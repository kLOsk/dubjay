//
//  Shaders.metal
//  Dub
//
//  M10-B vertex + M10.1 multi-colour fragment shaders for the
//  broadband waveform view.
//
//  Vertex stage (M10-B): one instance per `PeakChunk`; instanced
//  quads (4 verts each, drawn as a triangle strip) form the
//  familiar Serato min/max bars. The vertex shader also looks up
//  the matching `BandPeakChunk` (8 × f32 RMS, one per log-spaced
//  band) and forwards it to the fragment stage.
//
//  Fragment stage (M10.1): mixes the 8 perceptual-band loudness
//  values into RGB:
//
//      R = mean(b[0], b[1])                 (kick / bass: 30-159 Hz)
//      G = mean(b[2], b[3], b[4])           (mids / vox: 159-1934 Hz)
//      B = mean(b[5], b[6], b[7])           (highs:      1934-16000 Hz)
//
//  Band boundaries are determined by `dub_spectral`'s log-spaced
//  layout (30 Hz – 16 kHz, 8 bands). The mapping above is the
//  default M10.1 palette; M10.2 will surface palette presets via
//  a SwiftUI sub-view.
//
//  Layout / sizing (M10.4 vertical + M10.5b single-pass past+future):
//
//  Each chunk is rendered as a **horizontal** bar at a fixed vertical
//  position. The deck column is a *tall* MTKView; chunks stack
//  vertically with time running bottom → top (forward play = waveform
//  marches upward through the playhead — see PRD §9.1).
//
//  Vertical NDC ranges (NDC y goes from -1 = bottom to +1 = top):
//      • Past region:   y ∈ [+0.5, +1.0]  (top 25 % of viewport)
//      • Playhead line: y =  +0.5         (25 % from the top; rendered
//                                          as an overlay by `WaveformView`)
//      • Future region: y ∈ [-1.0, +0.5)  (bottom 75 % of viewport)
//
//  The renderer dispatches a single draw with `chunksVisible` total
//  instances. The first `chunksAbovePlayhead` instances land in the
//  past region (top 25 %); the remaining `chunksVisible -
//  chunksAbovePlayhead` instances land in the future region (bottom
//  75 %). The shader does the piecewise NDC mapping below.
//
//  iid 0                       → oldest visible past chunk (top of
//                                viewport, y → +1.0)
//  iid chunksAbovePlayhead - 1 → newest past chunk (just above the
//                                playhead, y → +0.5)
//  iid chunksAbovePlayhead     → first future chunk (just below the
//                                playhead, y → +0.5⁻)
//  iid chunksVisible - 1       → last future chunk (bottom of
//                                viewport, y → -1.0)
//
//  This single-pass scheme works regardless of mode:
//    • Thru mode: `chunksAbovePlayhead == chunksVisible` (no future
//      data), the shader's past branch fires for every instance, the
//      future region renders empty — identical to M10.4 behaviour.
//    • File mode: `chunksAbovePlayhead > 0` AND `chunksAbovePlayhead
//      < chunksVisible`, both branches fire.
//    • Track-just-started: `chunksAbovePlayhead == 0`, only future
//      branch fires; the past region is empty (no audio has played
//      yet), which is correct.
//
//  Per-instance NDC math:
//      Let n_above = max(1, chunksAbovePlayhead)
//          n_below = max(1, chunksVisible - chunksAbovePlayhead)
//
//      If iid < chunksAbovePlayhead:           // past
//          fy      = (iid + 0.5) / n_above     in [0, 1]
//          yCenter = +1.0 - fy * 0.5           in [+1.0, +0.5]
//          dy      = 0.25 / n_above
//      Else:                                   // future
//          j       = iid - chunksAbovePlayhead
//          fy      = (j + 0.5) / n_below       in [0, 1]
//          yCenter = +0.5 - fy * 1.5           in [+0.5, -1.0]
//          dy      = 0.75 / n_below
//
//      x = sample amplitude * yScale  in [-yScale, +yScale]
//
//  Band chunk lookup:
//
//  Broadband chunks tick once per `samplesPerPeakChunk` (default
//  64) audio samples; band chunks tick once per
//  `samplesPerBandChunk` (default 512). For broadband instance
//  `k`, the corresponding band index is:
//
//      bandLocal = (k * samplesPerPeakChunk + samplesPerPeakChunk/2)
//                  / samplesPerBandChunk
//      bandRing  = (bandChunkOffset + bandLocal) % bandCapacity
//
//  Renderer guarantees both ring buffers' offsets refer to the
//  same point in audio-time (their writes are coupled to the same
//  M9 mono-downmix tap), so this math always lands on a band
//  chunk that's actually been written.

#include <metal_stdlib>
using namespace metal;

// Mirrors `#[repr(C)] PeakChunk` from crates/dub-peaks/src/lib.rs.
// Same memory layout: three IEEE-754 f32s, little-endian on both ARM64
// and x86_64 macOS, identical alignment.
struct PeakChunk {
    float minSample;
    float maxSample;
    float rms;
};

// Mirrors `#[repr(C)] BandPeakChunk { rms_per_band: [f32; 8] }`.
// Eight perceptual RMS values per chunk, one per log-spaced band.
struct BandPeakChunk {
    float band[8];
};

struct Uniforms {
    // First broadband chunk to render (ring index).
    uint  chunkOffset;
    // Total chunks visible across both past and future regions.
    // `instance_id` ranges over [0, chunksVisible).
    uint  chunksVisible;
    // Number of `chunksVisible` instances assigned to the past region
    // (top 25 % strip). The remaining `chunksVisible -
    // chunksAbovePlayhead` instances render in the future region
    // (bottom 75 %). 0 = no past (track just started); == chunksVisible
    // = no future (Thru mode behaviour). See file-level layout doc.
    uint  chunksAbovePlayhead;
    // Horizontal amplitude scale applied to min/max before NDC.
    // 0.95 keeps the bars off the very edge of the viewport
    // horizontally (name kept as `yScale` for ABI continuity — the
    // M10.4 rotation swaps the *axis* this value applies to but
    // keeps the wire layout identical).
    float yScale;
    // Number of audio samples per broadband chunk (M9 default 64).
    uint  samplesPerPeakChunk;
    // First band chunk to render (ring index).
    uint  bandChunkOffset;
    // Number of audio samples per band chunk (M9.5b default 512).
    uint  samplesPerBandChunk;
    // Total band-chunk ring capacity (power-of-two).
    uint  bandCapacity;
    // M10.2 palette index. 0 = Serato-faithful (the M10.1 default),
    // 1 = high-contrast, 2 = monochrome.
    uint  palette;
};

struct VertexOut {
    float4 position [[position]];
    float  rms;
    // Per-band RMS values forwarded to the fragment shader. Metal
    // pipelines all four lanes of float4 even if we only need 3
    // (R/G/B) values, so we pack as two float4s and let the fragment
    // mix them.
    float4 bandLow;   // b0, b1, b2, b3
    float4 bandHigh;  // b4, b5, b6, b7
    // M10.2 honest-state flags. All four corners of a quad come
    // from the same instance, so even though [[position]]-driven
    // rasterizer interpolation happens, every per-fragment value
    // collapses to the per-instance constant we wrote in the
    // vertex stage.
    //   flags.x = 1.0 if this chunk is clipping (|peak| >= 0.98)
    //   flags.y = 1.0 if this chunk is essentially silent
    //              (|min| + |max| < 1e-3 AND rms < 1e-4)
    //   flags.z = palette index (as float, rounded in fragment).
    //   flags.w = reserved.
    float4 flags;
};

vertex VertexOut waveformVertex(
    uint vid                       [[vertex_id]],
    uint iid                       [[instance_id]],
    constant Uniforms& u           [[buffer(0)]],
    constant PeakChunk* chunks     [[buffer(1)]],
    constant BandPeakChunk* bands  [[buffer(2)]]
) {
    // Read the broadband chunk this instance belongs to. Past-the-end
    // accesses are guarded by the renderer.
    PeakChunk c = chunks[u.chunkOffset + iid];

    // Honest-state flags. Computed once per instance so the
    // fragment shader doesn't need the original min/max/rms (it
    // only sees the post-rasteriser interpolated VertexOut).
    float maxAbs = max(fabs(c.minSample), fabs(c.maxSample));
    float clipping = (maxAbs >= 0.98) ? 1.0 : 0.0;
    float silence  = ((fabs(c.minSample) + fabs(c.maxSample) < 1e-3) &&
                      (c.rms < 1e-4)) ? 1.0 : 0.0;

    // Treat empty chunks (no samples yet) as a centred near-zero
    // bar so the leading edge of the waveform draws as a hairline
    // rather than a hidden zero-thickness triangle.
    float lo = c.minSample;
    float hi = c.maxSample;
    if (hi - lo < 1e-5) {
        lo = -1e-4;
        hi =  1e-4;
    }

    // M10.5b piecewise NDC mapping. iid < chunksAbovePlayhead lands
    // in the past region (top 25 %); iid >= chunksAbovePlayhead in
    // the future region (bottom 75 %). See file-level doc for the
    // full layout math.
    float ndcY;
    float dy;
    if (iid < u.chunksAbovePlayhead) {
        float n_above = float(max(1u, u.chunksAbovePlayhead));
        float fy = (float(iid) + 0.5) / n_above;
        ndcY = 1.0 - fy * 0.5;
        dy   = 0.25 / n_above;
    } else {
        uint chunksBelow = u.chunksVisible - u.chunksAbovePlayhead;
        float n_below = float(max(1u, chunksBelow));
        uint j = iid - u.chunksAbovePlayhead;
        float fy = (float(j) + 0.5) / n_below;
        ndcY = 0.5 - fy * 1.5;
        dy   = 0.75 / n_below;
    }

    // Quad corners (triangle strip vertex order, post-rotation):
    //   0: bottom-left, 1: bottom-right, 2: top-left, 3: top-right
    // Vertex-id bit layout (kept identical to the M10-B horizontal
    // version for ABI continuity):
    //   bit 1 (`vid & 2u`) selects the *amplitude extreme*:
    //                       cleared → low (left edge of bar)
    //                       set     → high (right edge of bar)
    //   bit 0 (`vid & 1u`) selects the *time-edge* of the chunk:
    //                       cleared → older edge (top of bar in
    //                                 vertical layout = y_center + dy)
    //                       set     → newer edge (bottom of bar
    //                                 in vertical layout = y_center
    //                                 - dy)
    //
    // Triangle strip vertex order with this mapping still produces
    // a single non-self-intersecting quad — the strip's winding
    // doesn't matter because we don't enable back-face culling for
    // the waveform pipeline.
    float x = (vid & 2u) ? (hi * u.yScale) : (lo * u.yScale);
    float y = (vid & 1u) ? (ndcY - dy) : (ndcY + dy);

    // Map the broadband instance to its containing band chunk.
    // Half-sample offset (+ samplesPerPeakChunk/2) picks the band
    // chunk that overlaps the *centre* of this peak chunk, which is
    // less prone to off-by-one drift at chunk boundaries than the
    // strict integer-division mapping.
    uint sampleCentre = iid * u.samplesPerPeakChunk + (u.samplesPerPeakChunk >> 1u);
    uint bandLocal = (u.samplesPerBandChunk == 0u) ? 0u
                                                   : (sampleCentre / u.samplesPerBandChunk);
    uint bandRing = (u.bandChunkOffset + bandLocal) % max(1u, u.bandCapacity);
    BandPeakChunk b = bands[bandRing];

    VertexOut out;
    out.position = float4(x, y, 0.0, 1.0);
    out.rms = c.rms;
    out.bandLow  = float4(b.band[0], b.band[1], b.band[2], b.band[3]);
    out.bandHigh = float4(b.band[4], b.band[5], b.band[6], b.band[7]);
    out.flags    = float4(clipping, silence, float(u.palette), 0.0);
    return out;
}

// Pre-compute the (r, g, b) loudness mix common to every palette.
// Indices match `dub-peaks`'s 8-band layout, 30 Hz - 16 kHz:
//   b0..b1 → red    (sub-bass + bass:           30 - 159 Hz)
//   b2..b4 → green  (low-mids + mids + presence: 159 - 1934 Hz)
//   b5..b7 → blue   (highs + air:               1934 - 16000 Hz)
inline float3 bandMix(float4 bandLow, float4 bandHigh) {
    float r = 0.5 * (bandLow.x + bandLow.y);
    float g = (bandLow.z + bandLow.w + bandHigh.x) * (1.0 / 3.0);
    float b = (bandHigh.y + bandHigh.z + bandHigh.w) * (1.0 / 3.0);
    // Channel-side gain. Without this, bass tends to dominate (it
    // routinely lives at 0.4-0.8 RMS in compressed magnitudes
    // because there are fewer FFT bins per low band, so each bin
    // carries more weight). Tuned so a balanced track lands near
    // (r, g, b) ≈ (0.5, 0.5, 0.5).
    return float3(r * 1.2, g * 1.8, b * 2.4);
}

// Normalise + brightness-floor a colour vector. Silence (max <
// `silenceThreshold`) collapses to a configurable neutral grey;
// otherwise we rescale so the brightest channel sits at
// `targetBrightness` × `saturate(maxC)`. Keeps the bar visible
// without saturating to pure RGB primaries.
inline float3 normaliseColour(float3 colour, float silenceGrey, float brightnessFloor) {
    float maxC = max(max(colour.r, colour.g), colour.b);
    if (maxC < 0.05) {
        return float3(silenceGrey);
    }
    return colour / maxC * mix(brightnessFloor, 1.0, saturate(maxC));
}

fragment float4 waveformFragment(VertexOut in [[stage_in]]) {
    // M10.2 honest-state. Clipping always wins (top-priority
    // visualisation); silence is rendered as the palette's neutral
    // grey before the colour mix even runs.
    bool clipping = in.flags.x > 0.5;
    bool silence  = in.flags.y > 0.5;
    uint palette  = uint(round(in.flags.z));

    if (clipping) {
        // Pure red so a peak-clipped bar is unmistakable. The
        // user is expected to act on this (turn the gain down on
        // the offending deck).
        return float4(1.0, 0.05, 0.05, 1.0);
    }
    if (silence) {
        // Honest dropout: thin, dim grey hairline. The amplitude
        // shape (essentially zero) already conveys silence; the
        // colour just stops painting hue, so a stretch of silence
        // is visually distinct from a stretch of fully-saturated
        // mid signal.
        return float4(0.18, 0.18, 0.20, 1.0);
    }

    float3 colour = bandMix(in.bandLow, in.bandHigh);

    if (palette == 1u) {
        // High-contrast palette. Pushes the colour primaries
        // harder by squaring the per-channel value (boosts strong
        // bands, suppresses weak ones), then rescales. Useful in
        // bright rooms / projector-driven club setups where the
        // M10.1 default washes out.
        colour = colour * colour;
        colour = normaliseColour(colour, 0.30, 0.55);
    } else if (palette == 2u) {
        // Monochrome palette. Collapses all hue information and
        // shows the broadband RMS as a single near-white tone.
        // Equivalent to the M10-B look — useful as a "honest"
        // amplitude-only reference when the colour layer is
        // misleading.
        float intensity = clamp(0.35 + in.rms * 1.6, 0.35, 1.0);
        return float4(intensity, intensity, intensity, 1.0);
    } else {
        // 0 = Serato-faithful (the M10.1 default).
        colour = normaliseColour(colour, 0.35, 0.45);
    }

    // Final RMS-driven luminance: louder bar = brighter colour.
    // Keeps the visual amplitude shape from M10-B; the bands only
    // affect *hue*. clamp avoids over-saturation on transients.
    float luminance = clamp(0.45 + in.rms * 1.6, 0.45, 1.0);
    colour *= luminance;

    return float4(colour, 1.0);
}
