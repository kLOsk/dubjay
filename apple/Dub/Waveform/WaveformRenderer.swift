//
//  WaveformRenderer.swift
//  Dub
//
//  Metal renderer for the deck waveform — Serato-faithful baseline.
//
//  Owns:
//    • `MTLDevice` + `MTLCommandQueue`
//    • one render pipeline state (vertex + fragment from `Shaders.metal`)
//    • triple-buffered uniforms (one slot per inflight frame, two
//      regions packed per slot for the past + future draws)
//    • two append-only ring buffers — `chunks` (broadband peaks)
//      and `bandChunks` (8 × f32 RMS per FFT hop) — written each
//      frame from `DubEngine.peaksExtend` / `bandPeaksExtend`
//
//  Renders directly to the MTKView drawable in a single render pass.
//  No HDR, no bloom, no tonemap, no onset confidence, no filtered-
//  peaks gate: that was the M10.5h–p stack which made kicks read
//  worse than Serato. The Rust DSP machinery for those features
//  (onset detection in `dub-peaks`, LF/MF/HF filter in
//  `dub-peaks::filtered`) is still alive for a future polish phase —
//  this renderer simply doesn't bind it.
//
//  Threading: all renderer work runs on the main thread. `MTKView`
//  invokes `draw(in:)` on the main thread when `isPaused == false`
//  and `enableSetNeedsDisplay == false` (our configuration in
//  `WaveformView`).
//

import Foundation
import Metal
import MetalKit
import simd

import DubCore

/// CPU-side mirror of `Shaders.metal`'s `Uniforms` struct. Field
/// order, type, and padding match exactly so we `memcpy` it into
/// the uniforms buffer with no per-frame allocation.
///
/// Nine 4-byte fields = 36 bytes total. Padded out to 64 bytes by
/// the per-region stride below (see `uniformStridePerRegion`).
private struct WaveformUniforms {
    /// First *raw* broadband chunk for this region (ring offset).
    /// The shader multiplies `chunkInWindow × chunksPerColumn` and
    /// adds this base to address its aggregation window.
    var chunkOffset: UInt32
    /// Number of *drawn columns* in this region's strip. Each
    /// drawn column emits 2 vertices and aggregates
    /// `chunksPerColumn` raw chunks under the hood.
    var chunksVisible: UInt32
    /// `> 0` ⇒ past region draw; `0` ⇒ future region draw. Mirrors
    /// `chunksAbove` on the host so the shader can pick the right
    /// time-→-NDC mapping without an extra flag.
    var chunksAbovePlayhead: UInt32
    var yScale: Float
    var samplesPerPeakChunk: UInt32
    /// First *raw* band chunk for this region (ring offset).
    var bandChunkOffset: UInt32
    var samplesPerBandChunk: UInt32
    var bandCapacity: UInt32
    /// 0 = vertical (PRD §9.1 default), 1 = horizontal (Prep mode).
    var orientation: UInt32
    /// Raw broadband chunks aggregated into one drawn column. ≥ 1.
    /// Set to `chunksPerPixel` (= 2) so each drawn column maps to
    /// exactly one drawable pixel along the time axis — the
    /// trapezoidal strip slices are then ≥ 1 px tall and don't
    /// stair-step into a sub-pixel comb pattern.
    var chunksPerColumn: UInt32
}

/// Renderer orientation. Vertical is the Performance-mode default
/// (PRD §9.1, time → y, playhead 25 % from the top); horizontal is
/// Prep mode (M10.8, time → x, playhead 25 % from the left).
public enum WaveformOrientation: UInt32 {
    case vertical = 0
    case horizontal = 1
}

/// Visible palette options. Collapsed to a single Serato-faithful
/// look for the post-strip-down baseline. The enum is kept (vs.
/// removing the field entirely from the public API) so a future
/// polish phase can add variants back without churning every call
/// site that passes a `WaveformPalette` parameter through.
public enum WaveformPalette: UInt32, CaseIterable, Identifiable {
    case serato = 0

    public var id: UInt32 { rawValue }
    public var displayName: String { "Serato-faithful" }
}

/// 12-byte mirror of `PeakChunk` for memory-layout assertions.
/// Generated UniFFI bindings return chunks as `Data`; we treat that
/// Data as `[PeakChunk]` via `withUnsafeBytes(_:)`.
private struct PeakChunkLayout {
    var minSample: Float
    var maxSample: Float
    var rms: Float
}

/// 32-byte mirror of `BandPeakChunk`. Matches
/// `#[repr(C)] pub struct BandPeakChunk { pub rms_per_band: [f32; 8] }`.
private struct BandPeakChunkLayout {
    var b0: Float; var b1: Float; var b2: Float; var b3: Float
    var b4: Float; var b5: Float; var b6: Float; var b7: Float
}

@MainActor
final class WaveformRenderer: NSObject {

    // MARK: Configuration

    /// Power-of-two number of broadband chunks the GPU ring buffer
    /// can hold. 2^20 ≈ 23 min of audio at 48 kHz / 64-sample
    /// chunks — sized so the entire offline-decoded peak set of
    /// any realistically-long DJ track fits without head-wrap
    /// collisions during a seek back to start. Power-of-two so the
    /// shader's modulo compiles to a bitmask. **Keep in sync with
    /// the `(1048576u - 1u)` mask in `Shaders.metal`.**
    static let chunkCapacity: Int = 1_048_576

    /// Power-of-two number of band chunks. 2^17 → ~1 400 s at
    /// 48 kHz / 512-sample band chunks, matching the broadband
    /// ring's coverage.
    static let bandChunkCapacity: Int = 131_072

    /// Standard Metal "three frames in flight" CPU queue depth.
    static let maxFramesInFlight: Int = 3

    /// Amplitude scale in NDC. 0.95 leaves a small gutter so peaks
    /// don't kiss the deck-column edge.
    private static let yScale: Float = 0.95

    /// Fraction of the deck column reserved for the *past* region
    /// (above the playhead per PRD §9.1).
    static let pastRegionFraction: Double = 0.25

    /// Raw broadband chunks per drawable pixel along the time axis.
    /// ~2.67 ms / px at 48 kHz / 64-sample chunks → a typical
    /// ~640 px-tall deck column shows ≈ 1.7 s of audio.
    nonisolated private static let chunksPerPixel: Double = 2.0

    /// Raw chunks aggregated into one drawn column. Set equal to
    /// `chunksPerPixel` so the geometry emits one trapezoidal
    /// slice per drawable pixel — the Mixxx-style per-pixel `max()`
    /// over a `chunksPerColumn`-sized data window happens in the
    /// vertex shader. This eliminates the sub-pixel comb pattern
    /// the un-aggregated strip produced when the raw chunk
    /// cadence is finer than 1 chunk per pixel.
    nonisolated private static let chunksPerColumn: UInt32 = 2

    /// Default broadband samples-per-chunk emitted by `dub-peaks`'s
    /// stream tap (M9.5b). Used for the host-side gesture-→-secs
    /// helper; the actual cadence the renderer uses is captured
    /// lazily from the first non-empty FFI payload.
    nonisolated public static let defaultSamplesPerPeakChunk: UInt32 = 64

    /// 4× MSAA on the drawable. The waveform geometry is a stack of
    /// trapezoid slices with sub-pixel edge slopes at high zoom;
    /// MSAA stops them stair-stepping into a "venetian blind"
    /// pattern. 4 samples is cheap on Apple Silicon.
    nonisolated public static let sampleCount: Int = 4

    /// Audio seconds represented by one pixel along the time axis,
    /// given the engine's current sample rate. Mirror of the
    /// renderer's `chunksPerPixel × samplesPerPeakChunk / sampleRate`
    /// formula so a click-scrub gesture lands on the same chunk
    /// the user clicked.
    nonisolated public static func secsPerPixel(
        sampleRate: UInt32,
        samplesPerPeakChunk: UInt32 = defaultSamplesPerPeakChunk
    ) -> Double {
        let sr = max(1.0, Double(sampleRate))
        return chunksPerPixel * Double(samplesPerPeakChunk) / sr
    }

    /// Byte stride between the past-region and future-region
    /// uniform slots inside one per-frame uniform buffer. The
    /// `Uniforms` struct itself is 36 bytes naturally; we round to
    /// 64 to satisfy the 32-byte `setVertexBuffer(offset:)`
    /// constant-buffer alignment Metal guarantees on every Apple
    /// GPU family.
    nonisolated public static let uniformStridePerRegion: Int = 64

    // MARK: Dependencies

    let device: MTLDevice
    private let commandQueue: MTLCommandQueue

    /// Single render pipeline: `waveformVertex` + `waveformFragment`
    /// writing straight to the MTKView's `bgra8Unorm` drawable
    /// (with MSAA resolve).
    private let waveformPipeline: MTLRenderPipelineState

    /// Bounded queue depth via semaphore. Prevents the CPU from
    /// writing into a uniform buffer the GPU is still reading.
    private let inflightSemaphore = DispatchSemaphore(value: maxFramesInFlight)

    /// Triple-buffered uniforms — one slot per inflight frame, two
    /// regions (past + future) packed per buffer at offsets 0 and
    /// `uniformStridePerRegion`.
    private var uniformBuffers: [MTLBuffer] = []
    private var uniformIndex: Int = 0

    /// Append-only ring buffer of `PeakChunk`s. Shared storage so
    /// we memcpy directly from the FFI `Data` blob — zero-copy on
    /// Apple Silicon.
    private let chunksBuffer: MTLBuffer

    /// Append-only ring buffer of `BandPeakChunk`s. Parallel to
    /// `chunksBuffer`; the vertex shader looks up the matching band
    /// chunk for each broadband instance.
    private let bandChunksBuffer: MTLBuffer

    // MARK: Engine binding

    private let engine: DubEngine
    private let deckIdx: UInt64

    /// How many broadband chunks have been written into the ring so
    /// far. Modulo `chunkCapacity` gives the ring offset.
    private(set) var totalChunksAppended: UInt64 = 0

    /// How many band chunks have been written into the ring so far.
    private(set) var totalBandChunksAppended: UInt64 = 0

    /// Cached `peaks_len()` from the previous poll.
    private var lastSeenPeaksLen: UInt64 = 0

    /// Cached `band_peaks_len()` from the previous poll. Tracked
    /// independently because the two streams advance at different
    /// cadences (one band chunk per 8 broadband chunks).
    private var lastSeenBandPeaksLen: UInt64 = 0

    /// Cached `peaks_generation()` from the previous poll. When the
    /// engine swaps a deck's `PeakSource` (Thru → File on load,
    /// File → File on reload) this bumps; the renderer wipes its
    /// ring + cadence cache and re-ingests from chunk 0.
    private var lastSeenPeaksGeneration: UInt64 = 0

    /// Cached chunk cadences (samples per chunk). Read once on the
    /// first non-empty poll; broadband / band lookup ratio in the
    /// shader depends on these.
    ///
    /// **Unit warning**: peak chunks are originally cadenced in
    /// **track** frames (e.g. 64 frames at 44.1 kHz). When the
    /// engine SR ≠ track SR, `round(peakDurSecs × engineSR)`
    /// introduces a ~0.5 %-per-chunk systematic error that
    /// compounds over the track length. We avoid that by using
    /// `peakChunkDurationSecs` (f64, exact) directly for the
    /// cumulative `elapsed_secs → playhead_chunk` mapping. The
    /// integer fields below are kept only for the band cross-ref
    /// math, which sees small visible-region chunk indices where
    /// the rounded error stays imperceptible.
    private var samplesPerPeakChunk: UInt32 = 64
    private var samplesPerBandChunk: UInt32 = 512

    /// Real-time duration of one broadband peak chunk in seconds —
    /// the **exact** value as reported by the engine. The
    /// authoritative source for `elapsed_secs → playhead_chunk`.
    private var peakChunkDurationSecs: Double = 0.0

    /// Active palette. Single-valued in the post-strip-down baseline
    /// but kept as a property so a future polish phase can add
    /// branches without re-plumbing the view.
    var palette: WaveformPalette = .serato

    /// Orientation. `.vertical` is Performance mode (PRD §9.1);
    /// `.horizontal` is Prep mode (M10.8).
    var orientation: WaveformOrientation = .vertical

    // MARK: Init

    init(device: MTLDevice, engine: DubEngine, deckIdx: UInt64 = 0) throws {
        self.device = device
        self.engine = engine
        self.deckIdx = deckIdx

        guard let queue = device.makeCommandQueue() else {
            throw NSError(
                domain: "WaveformRenderer", code: 1,
                userInfo: [NSLocalizedDescriptionKey: "Metal command queue allocation failed"])
        }
        self.commandQueue = queue
        self.commandQueue.label = "dub.waveform.cmdqueue"

        let library: MTLLibrary
        do {
            library = try device.makeDefaultLibrary(bundle: Bundle.main)
        } catch {
            throw NSError(
                domain: "WaveformRenderer", code: 2,
                userInfo: [
                    NSLocalizedDescriptionKey: "Default Metal library load failed: \(error)"
                ])
        }
        guard let vertexFn = library.makeFunction(name: "waveformVertex"),
              let fragmentFn = library.makeFunction(name: "waveformFragment")
        else {
            throw NSError(
                domain: "WaveformRenderer", code: 3,
                userInfo: [
                    NSLocalizedDescriptionKey:
                        "Metal functions (waveformVertex / waveformFragment) not found in default library"
                ])
        }

        // Single pass: waveform → MTKView drawable (bgra8Unorm,
        // 4× MSAA). MTKView allocates the multisample texture
        // when its `sampleCount` matches this pipeline's
        // `rasterSampleCount` and `framebufferOnly == false`.
        let waveformDescriptor = MTLRenderPipelineDescriptor()
        waveformDescriptor.label = "dub.waveform.pipeline"
        waveformDescriptor.vertexFunction = vertexFn
        waveformDescriptor.fragmentFunction = fragmentFn
        waveformDescriptor.colorAttachments[0].pixelFormat = .bgra8Unorm
        waveformDescriptor.colorAttachments[0].isBlendingEnabled = false
        waveformDescriptor.rasterSampleCount = WaveformRenderer.sampleCount
        self.waveformPipeline = try device.makeRenderPipelineState(
            descriptor: waveformDescriptor)

        let chunkBytes = WaveformRenderer.chunkCapacity * MemoryLayout<PeakChunkLayout>.stride
        guard let chunks = device.makeBuffer(length: chunkBytes, options: .storageModeShared)
        else {
            throw NSError(
                domain: "WaveformRenderer", code: 4,
                userInfo: [NSLocalizedDescriptionKey: "Chunks MTLBuffer allocation failed"])
        }
        chunks.label = "dub.waveform.chunks"
        chunks.contents().initializeMemory(as: UInt8.self, repeating: 0, count: chunkBytes)
        self.chunksBuffer = chunks

        let bandChunkBytes =
            WaveformRenderer.bandChunkCapacity * MemoryLayout<BandPeakChunkLayout>.stride
        guard let bandChunks = device.makeBuffer(
            length: bandChunkBytes, options: .storageModeShared)
        else {
            throw NSError(
                domain: "WaveformRenderer", code: 4,
                userInfo: [
                    NSLocalizedDescriptionKey: "Band chunks MTLBuffer allocation failed"
                ])
        }
        bandChunks.label = "dub.waveform.bandChunks"
        bandChunks.contents().initializeMemory(
            as: UInt8.self, repeating: 0, count: bandChunkBytes)
        self.bandChunksBuffer = bandChunks

        let uniformStride = WaveformRenderer.uniformStridePerRegion
        let uniformBytesPerBuffer = uniformStride * 2
        var uniforms: [MTLBuffer] = []
        for idx in 0..<WaveformRenderer.maxFramesInFlight {
            guard let buf = device.makeBuffer(
                length: uniformBytesPerBuffer, options: .storageModeShared)
            else {
                throw NSError(
                    domain: "WaveformRenderer", code: 5,
                    userInfo: [NSLocalizedDescriptionKey: "Uniform MTLBuffer allocation failed"])
            }
            buf.label = "dub.waveform.uniforms[\(idx)]"
            uniforms.append(buf)
        }
        self.uniformBuffers = uniforms

        super.init()
    }

    /// Drop any cached state. Called by `WaveformView` when the
    /// engine is stopped or the deck rebinds.
    func reset() {
        totalChunksAppended = 0
        totalBandChunksAppended = 0
        lastSeenPeaksLen = 0
        lastSeenBandPeaksLen = 0
        lastSeenPeaksGeneration = 0
        samplesPerPeakChunk = 64
        samplesPerBandChunk = 512
        peakChunkDurationSecs = 0.0
        let chunkBytes = WaveformRenderer.chunkCapacity * MemoryLayout<PeakChunkLayout>.stride
        chunksBuffer.contents().initializeMemory(
            as: UInt8.self, repeating: 0, count: chunkBytes)
        let bandChunkBytes =
            WaveformRenderer.bandChunkCapacity * MemoryLayout<BandPeakChunkLayout>.stride
        bandChunksBuffer.contents().initializeMemory(
            as: UInt8.self, repeating: 0, count: bandChunkBytes)
    }

    // MARK: MTKViewDelegate-style entry points

    func drawableSizeWillChange(_ size: CGSize) {
        _ = size
    }

    /// Per-frame work. Polls the engine for new chunks, uploads
    /// them into the rings, and records a single render pass to
    /// the MTKView drawable.
    func draw(in view: MTKView) {
        inflightSemaphore.wait()
        let releaseSemaphore: () -> Void = { [weak self] in
            self?.inflightSemaphore.signal()
        }

        // 0. Detect a PeakSource swap on this deck. When the engine
        //    swaps Thru → File (drag-and-drop load) or File → File
        //    (reload) the per-deck generation counter bumps; we
        //    wipe the ring and re-ingest the new source from
        //    chunk 0. Doing this before the ingest pull is critical:
        //    the new source's chunk count is typically smaller than
        //    what we last observed from Thru, so the length-
        //    monotonicity check in `ingestNewChunks` would
        //    otherwise silently no-op and the renderer would keep
        //    drawing stale Thru capture forever.
        let currentGeneration = engine.peaksGeneration(deckIdx: deckIdx)
        if currentGeneration != lastSeenPeaksGeneration {
            reset()
            lastSeenPeaksGeneration = currentGeneration
        }

        ingestNewChunks()
        ingestNewBandChunks()

        // 2. Compute the visible window.
        //
        // Piecewise layout: the time axis is whichever drawable
        // dimension time flows along. Vertical → height; Horizontal
        // → width. The playhead lives at 25 % from the leading edge
        // (top in vertical, left in horizontal) with a past region
        // covering 25 % of the axis and a future region the
        // remaining 75 %.
        let drawableSize = view.drawableSize
        let timeAxisPixels: Int
        switch orientation {
        case .vertical:
            timeAxisPixels = max(1, Int(drawableSize.height))
        case .horizontal:
            timeAxisPixels = max(1, Int(drawableSize.width))
        }
        let pastPixels =
            max(1, Int((Double(timeAxisPixels) * WaveformRenderer.pastRegionFraction).rounded()))
        let futurePixels =
            max(0, Int((Double(timeAxisPixels)
                * (1.0 - WaveformRenderer.pastRegionFraction)).rounded()))
        // 2× zoom-in: each drawn column spans **two** drawable
        // pixels along the time axis, so total visible time
        // halves to ≈ 0.93 s (≈ 2 beats at 128 BPM) and individual
        // transients double in apparent length on screen — what
        // the eye registers as a "fat" kick in Serato. The
        // per-column `chunksPerColumn` aggregation is preserved
        // (2 raw chunks per drawn column → 1 trapezoid every 2 px,
        // no sub-pixel comb).
        let pixelsPerDrawnColumn = 2
        let drawnAbovePixels = pastPixels / pixelsPerDrawnColumn
        let drawnBelowPixels = futurePixels / pixelsPerDrawnColumn
        let agg = Int(WaveformRenderer.chunksPerColumn)

        // Playhead chunk + chunks past it.
        let pos = engine.position(deckIdx: deckIdx)
        let peaksLenGlobal = totalChunksAppended
        let hasFuture = pos.hasTrack
        let playheadChunk: UInt64
        if hasFuture {
            // File mode. Map elapsed seconds → chunk via the exact
            // f64 chunk duration to bypass the integer-rounded
            // ~0.5 %-per-chunk drift the old code accumulated.
            if peakChunkDurationSecs > 0 {
                let chunkF = (pos.elapsedSecs / peakChunkDurationSecs).rounded(.down)
                let chunkClamped = max(0.0, min(chunkF, Double(peaksLenGlobal &- 1)))
                playheadChunk = UInt64(chunkClamped)
            } else {
                playheadChunk = peaksLenGlobal == 0 ? 0 : peaksLenGlobal &- 1
            }
        } else {
            // Thru mode (or empty deck). The newest chunk is the
            // playhead.
            playheadChunk = peaksLenGlobal == 0 ? 0 : peaksLenGlobal &- 1
        }

        let chunksAvailableBehind: Int
        let chunksAvailableAhead: Int
        if peaksLenGlobal == 0 {
            chunksAvailableBehind = 0
            chunksAvailableAhead = 0
        } else {
            chunksAvailableBehind = min(Int(playheadChunk) + 1, WaveformRenderer.chunkCapacity)
            if hasFuture {
                chunksAvailableAhead = min(
                    Int(peaksLenGlobal &- 1 &- playheadChunk),
                    WaveformRenderer.chunkCapacity)
            } else {
                chunksAvailableAhead = 0
            }
        }

        // Cap drawn columns by what's actually available, rounding
        // down so the shader never reads partial-aggregation chunks
        // past the end of the buffered region.
        let drawnAbove =
            max(0, min(drawnAbovePixels, chunksAvailableBehind / agg))
        let drawnBelow =
            max(0, min(drawnBelowPixels, chunksAvailableAhead / agg))
        // Raw chunk count behind the playhead — needed to derive
        // the past region's `chunkOffset`. The future region starts
        // at `playheadChunk + 1` regardless of aggregation, so no
        // analogous quantity is needed there.
        let rawAbove = drawnAbove * agg
        let hasContent = peaksLenGlobal > 0 && (drawnAbove + drawnBelow) > 0

        guard let drawable = view.currentDrawable,
              let passDescriptor = view.currentRenderPassDescriptor
        else {
            releaseSemaphore()
            return
        }

        // Per-region chunk + band offsets. The shader's vertex
        // stage addresses raw chunks as `chunkOffset + chunkInWindow
        // × chunksPerColumn`, so `chunkOffset` is in *raw* units
        // here.
        let pastFirstGlobal: UInt64 =
            (playheadChunk &+ 1) &- UInt64(rawAbove)
        let pastFirstRingOffset =
            Int(pastFirstGlobal % UInt64(WaveformRenderer.chunkCapacity))
        let futureFirstGlobal: UInt64 = playheadChunk &+ 1
        let futureFirstRingOffset =
            Int(futureFirstGlobal % UInt64(WaveformRenderer.chunkCapacity))

        let bandPerSample = max(UInt64(samplesPerBandChunk), 1)
        let pastFirstSample = pastFirstGlobal &* UInt64(samplesPerPeakChunk)
        let pastFirstBandGlobal = pastFirstSample / bandPerSample
        let pastFirstBandRingOffset =
            Int(pastFirstBandGlobal % UInt64(WaveformRenderer.bandChunkCapacity))
        let futureFirstSample = futureFirstGlobal &* UInt64(samplesPerPeakChunk)
        let futureFirstBandGlobal = futureFirstSample / bandPerSample
        let futureFirstBandRingOffset =
            Int(futureFirstBandGlobal % UInt64(WaveformRenderer.bandChunkCapacity))

        // 3. Fill both region slots in the per-frame uniform buffer.
        //
        // Past draw sets `chunksAbovePlayhead = chunksAbove` (> 0).
        // Future draw sets `chunksAbovePlayhead = 0`. The shader
        // picks the right time-axis mapping from this flag.
        let pastUniforms = WaveformUniforms(
            chunkOffset: UInt32(pastFirstRingOffset),
            chunksVisible: UInt32(drawnAbove),
            chunksAbovePlayhead: UInt32(drawnAbove),
            yScale: WaveformRenderer.yScale,
            samplesPerPeakChunk: samplesPerPeakChunk,
            bandChunkOffset: UInt32(pastFirstBandRingOffset),
            samplesPerBandChunk: samplesPerBandChunk,
            bandCapacity: UInt32(WaveformRenderer.bandChunkCapacity),
            orientation: orientation.rawValue,
            chunksPerColumn: WaveformRenderer.chunksPerColumn)
        let futureUniforms = WaveformUniforms(
            chunkOffset: UInt32(futureFirstRingOffset),
            chunksVisible: UInt32(drawnBelow),
            chunksAbovePlayhead: 0,
            yScale: WaveformRenderer.yScale,
            samplesPerPeakChunk: samplesPerPeakChunk,
            bandChunkOffset: UInt32(futureFirstBandRingOffset),
            samplesPerBandChunk: samplesPerBandChunk,
            bandCapacity: UInt32(WaveformRenderer.bandChunkCapacity),
            orientation: orientation.rawValue,
            chunksPerColumn: WaveformRenderer.chunksPerColumn)
        let uniformBuffer = uniformBuffers[uniformIndex]
        let uniformStride = WaveformRenderer.uniformStridePerRegion
        let bufBase = uniformBuffer.contents()
        bufBase.withMemoryRebound(to: WaveformUniforms.self, capacity: 1) { ptr in
            ptr.pointee = pastUniforms
        }
        bufBase.advanced(by: uniformStride).withMemoryRebound(
            to: WaveformUniforms.self, capacity: 1) { ptr in
            ptr.pointee = futureUniforms
        }

        // 4. Record one render pass.
        guard let commandBuffer = commandQueue.makeCommandBuffer() else {
            releaseSemaphore()
            return
        }
        commandBuffer.label = "dub.waveform.commandBuffer"
        commandBuffer.addCompletedHandler { _ in releaseSemaphore() }

        // The MTKView's pass descriptor already has the correct
        // colour attachment (the drawable with MSAA resolve when
        // `view.sampleCount == 4`). Force `.clear` + dark deck
        // background so a frame with no chunks still renders the
        // base colour.
        passDescriptor.colorAttachments[0].loadAction = .clear
        passDescriptor.colorAttachments[0].storeAction =
            (WaveformRenderer.sampleCount > 1) ? .multisampleResolve : .store
        passDescriptor.colorAttachments[0].clearColor =
            MTLClearColor(red: 0.07, green: 0.07, blue: 0.08, alpha: 1.0)

        guard let encoder = commandBuffer.makeRenderCommandEncoder(descriptor: passDescriptor)
        else {
            releaseSemaphore()
            return
        }
        encoder.label = "dub.waveform.pass"
        encoder.setRenderPipelineState(waveformPipeline)
        if hasContent {
            encoder.setVertexBuffer(chunksBuffer, offset: 0, index: 1)
            encoder.setVertexBuffer(bandChunksBuffer, offset: 0, index: 2)

            if drawnAbove > 0 {
                encoder.setVertexBuffer(uniformBuffer, offset: 0, index: 0)
                encoder.drawPrimitives(
                    type: .triangleStrip, vertexStart: 0,
                    vertexCount: 2 * drawnAbove)
            }
            if drawnBelow > 0 {
                encoder.setVertexBuffer(
                    uniformBuffer, offset: uniformStride, index: 0)
                encoder.drawPrimitives(
                    type: .triangleStrip, vertexStart: 0,
                    vertexCount: 2 * drawnBelow)
            }
        }
        encoder.endEncoding()

        commandBuffer.present(drawable)
        commandBuffer.commit()

        uniformIndex = (uniformIndex + 1) % WaveformRenderer.maxFramesInFlight
    }

    // MARK: Ingestion

    /// Pull any newly-available `PeakChunk`s and memcpy them into
    /// the GPU ring. Bounded work: at most `chunkCapacity` new
    /// chunks fit; older entries wrap.
    private func ingestNewChunks() {
        let currentLen = engine.peaksLen(deckIdx: deckIdx)
        if currentLen <= lastSeenPeaksLen {
            return
        }
        if lastSeenPeaksLen == 0 {
            captureChunkCadences()
        }
        let availableNew = currentLen &- lastSeenPeaksLen
        let cappedNew = min(availableNew, UInt64(WaveformRenderer.chunkCapacity))
        let startIdx = currentLen &- cappedNew
        let data = engine.peaksExtend(deckIdx: deckIdx, startIdx: startIdx)
        if data.isEmpty {
            lastSeenPeaksLen = currentLen
            return
        }

        let chunkStride = MemoryLayout<PeakChunkLayout>.stride
        let newChunkCount = data.count / chunkStride
        guard newChunkCount > 0, data.count % chunkStride == 0 else {
            lastSeenPeaksLen = currentLen
            return
        }

        let ringBytes = WaveformRenderer.chunkCapacity * chunkStride
        let dstBase = chunksBuffer.contents()

        data.withUnsafeBytes { (rawSrc: UnsafeRawBufferPointer) in
            guard let srcBase = rawSrc.baseAddress else { return }
            let firstSlot = Int(startIdx % UInt64(WaveformRenderer.chunkCapacity))
            let bytesToWrite = newChunkCount * chunkStride
            let firstByteOffset = firstSlot * chunkStride

            if firstByteOffset + bytesToWrite <= ringBytes {
                memcpy(dstBase.advanced(by: firstByteOffset), srcBase, bytesToWrite)
            } else {
                let bytesBeforeWrap = ringBytes - firstByteOffset
                memcpy(dstBase.advanced(by: firstByteOffset), srcBase, bytesBeforeWrap)
                memcpy(
                    dstBase,
                    srcBase.advanced(by: bytesBeforeWrap),
                    bytesToWrite - bytesBeforeWrap)
            }
        }

        totalChunksAppended = currentLen
        lastSeenPeaksLen = currentLen
    }

    /// Mirror of [`ingestNewChunks`] for the parallel band stream.
    /// No-ops cleanly when band capture is disabled (`band_peaks_len`
    /// stays at 0 forever); the shader sees the zero-initialised
    /// ring and the fragment falls back to a faint white from the
    /// per-channel normalisation floor.
    private func ingestNewBandChunks() {
        let currentLen = engine.bandPeaksLen(deckIdx: deckIdx)
        if currentLen <= lastSeenBandPeaksLen {
            return
        }
        let availableNew = currentLen &- lastSeenBandPeaksLen
        let cappedNew = min(availableNew, UInt64(WaveformRenderer.bandChunkCapacity))
        let startIdx = currentLen &- cappedNew
        let data = engine.bandPeaksExtend(deckIdx: deckIdx, startIdx: startIdx)
        if data.isEmpty {
            lastSeenBandPeaksLen = currentLen
            return
        }

        let chunkStride = MemoryLayout<BandPeakChunkLayout>.stride
        let newChunkCount = data.count / chunkStride
        guard newChunkCount > 0, data.count % chunkStride == 0 else {
            lastSeenBandPeaksLen = currentLen
            return
        }

        let ringBytes = WaveformRenderer.bandChunkCapacity * chunkStride
        let dstBase = bandChunksBuffer.contents()

        data.withUnsafeBytes { (rawSrc: UnsafeRawBufferPointer) in
            guard let srcBase = rawSrc.baseAddress else { return }
            let firstSlot = Int(startIdx % UInt64(WaveformRenderer.bandChunkCapacity))
            let bytesToWrite = newChunkCount * chunkStride
            let firstByteOffset = firstSlot * chunkStride

            if firstByteOffset + bytesToWrite <= ringBytes {
                memcpy(dstBase.advanced(by: firstByteOffset), srcBase, bytesToWrite)
            } else {
                let bytesBeforeWrap = ringBytes - firstByteOffset
                memcpy(dstBase.advanced(by: firstByteOffset), srcBase, bytesBeforeWrap)
                memcpy(
                    dstBase,
                    srcBase.advanced(by: bytesBeforeWrap),
                    bytesToWrite - bytesBeforeWrap)
            }
        }

        totalBandChunksAppended = currentLen
        lastSeenBandPeaksLen = currentLen
    }

    /// Snapshot the engine's reported broadband + band chunk cadences
    /// on the first non-empty poll. Cached so subsequent draws skip
    /// the FFI cost. Falls back to the M9 / M9.5b defaults
    /// (64 / 512) if the engine returns 0 for either accessor.
    private func captureChunkCadences() {
        let sr = engine.sampleRate()
        if sr == 0 {
            return
        }
        let srD = Double(sr)
        let peakDur = engine.peaksChunkDurationSecs(deckIdx: deckIdx)
        let bandDur = engine.bandPeaksChunkDurationSecs(deckIdx: deckIdx)
        if peakDur > 0 {
            peakChunkDurationSecs = peakDur
            let samples = Int((peakDur * srD).rounded())
            if samples > 0 {
                samplesPerPeakChunk = UInt32(samples)
            }
        }
        if bandDur > 0 {
            let samples = Int((bandDur * srD).rounded())
            if samples > 0 {
                samplesPerBandChunk = UInt32(samples)
            }
        }
    }
}
