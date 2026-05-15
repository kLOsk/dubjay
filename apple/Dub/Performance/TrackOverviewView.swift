//
//  TrackOverviewView.swift
//  Dub
//
//  M10.5c per-deck Track Overview. A thin vertical strip
//  (`DubLayout.deckOverviewWidth` ≈ 36 px) on the deck's *outside*
//  edge showing the *whole* track top→bottom with a playhead
//  bracket at the current position. Click-to-jump seeks the deck
//  in File mode (PRD §6.1); Timecode click-jump waits on M10.6's
//  Panic Play wiring.
//
//  Design notes (PRD §9.6.1):
//      • Vertical orientation, time runs **top → bottom**. This
//        matches Serato's overview (and the convention every DJ
//        already knows). The *playing waveform* is bottom-→-top
//        because that has to mirror platter rotation under the
//        playhead; the *overview* is a static map of the whole
//        track and doesn't have a playhead-vs-hand-motion
//        constraint, so the top-→-bottom reading order wins for
//        glance-ability.
//      • Rendered via SwiftUI `Canvas`, *not* Metal. The overview
//        is low-cadence (redraws only when the playhead chunk
//        changes ≈ 30 Hz) and fully-known-up-front (entire peak
//        array fetched once at load). Adding a second Metal
//        renderer would double our shader inventory for zero
//        performance benefit; `Canvas` keeps the pipeline simple.
//      • Decimated to a fixed bucket count (`Self.bucketCount`)
//        regardless of strip height. The bucket cap caps both
//        memory and draw cost; the Canvas's own scaling handles
//        the strip-height variation.
//      • Source-swap aware via `model.engine.peaksGeneration` —
//        a Thru → File / File → File swap forces a re-decimation
//        on the next render, same signal the playing-waveform
//        renderer uses to reset its ring.
//

import SwiftUI

import DubCore

/// One decimated amplitude value per overview bucket (M10.5r
/// rebuild). Carries the full broadband peak + RMS shape so the
/// renderer can paint the same two-tone envelope the Metal
/// playing-waveform uses: a bright outer hull at `peak`, a darker
/// inner core at `rms`. Matches the visual vocabulary of the main
/// strip without sharing any of its Metal pipeline.
private struct OverviewBucket {
    /// Outer envelope amplitude — `max(|min|, |max|)` across the
    /// bucket's chunk range, clamped to `[0, 1]`.
    var peak: Float
    /// Inner RMS — averaged over the bucket's chunk range, also
    /// clamped to `[0, 1]`. Always `<= peak` by construction.
    var rms: Float
}

/// Background padding around the bar field. The user-facing fix
/// from the M10.5t "no warping at the start and end" feedback —
/// without it the bars touch the top / bottom (or left / right)
/// edges of the strip and read as a solid block, especially on
/// loud-throughout material. The padding doubles as a hit-test
/// dead-zone so a stray click at the very edge doesn't snap to
/// `0` or `durationSecs` — clicks inside the padding are clamped
/// to the nearest bar.
private enum OverviewLayout {
    /// Padding (in points) reserved as dark background at each end
    /// of the time axis. 8 pt at 2× DPR = 16 device pixels = three
    /// bar widths at the default 480-bucket cap, so the empty
    /// edges read as visibly intentional rather than as cropping.
    static let endPadding: CGFloat = 8
}

struct TrackOverviewView: View {

    @ObservedObject var model: WaveformAppModel
    let side: DeckSide
    let deckIdx: UInt64
    /// Time-axis orientation. `.vertical` is the canonical
    /// Performance-mode column (top → bottom). `.horizontal`
    /// stacks the overview as a thin band across the top of the
    /// Prep-mode horizontal playing waveform (left → right).
    /// Defaults to `.vertical` so every existing call site keeps
    /// rendering unchanged.
    var orientation: WaveformOrientation = .vertical

    /// Bucket count for the decimated overview. 480 is enough to
    /// resolve every visible pixel on a typical 600 px-tall strip
    /// at 2× DPR (1200 device pixels) without being so dense that
    /// the bars merge into a smear. Tunable knob; smaller values
    /// look cleaner on very short tracks.
    private static let bucketCount: Int = 480

    /// Decimated peak data. `nil` until the deck has a track and we
    /// have a peak count to read from the engine. `[]` is the
    /// "loaded but empty / not enough data yet" state — the strip
    /// renders an empty background.
    @State private var buckets: [OverviewBucket]? = nil

    /// `peaks_generation` value that produced `buckets`. When the
    /// engine's current generation differs we know the source has
    /// swapped (Thru → File on `load_track`, or File → File on a
    /// second load) and re-decimate.
    @State private var lastSeenGeneration: UInt64 = 0

    private var deckState: DeckState {
        switch side {
        case .a: return model.deckA
        case .b: return model.deckB
        }
    }

    var body: some View {
        // GeometryReader gives us the strip's actual rendered
        // height inside the closure — which we need both for the
        // Canvas's draw math and for click-to-jump's fraction
        // calculation. SwiftUI gestures don't expose the view
        // bounds; reading them off the geo proxy is the
        // idiomatic workaround.
        GeometryReader { geo in
            Canvas { ctx, size in
                drawBackground(ctx: ctx, size: size)
                if let buckets, !buckets.isEmpty {
                    drawBars(ctx: ctx, size: size, buckets: buckets)
                    drawPlayhead(ctx: ctx, size: size)
                } else {
                    drawEmptyState(ctx: ctx, size: size)
                }
            }
            .contentShape(Rectangle())
            .gesture(
                // Click-to-jump only on the overview. The overview
                // is a "where am I in the whole track" map, not a
                // fine-positioning tool — continuous drag-scrub +
                // audio-under-cursor lives on the zoomed waveform
                // (PRD §6.1 / §9.6). Single-tap on the overview
                // seeks the deck to that absolute track position
                // and leaves transport alone.
                DragGesture(minimumDistance: 0)
                    .onEnded { value in
                        handleClickJump(at: value.location, in: geo.size)
                    })
        }
        .modifier(OverviewSizing(orientation: orientation))
        .onAppear(perform: reloadIfStale)
        .onChange(of: deckState.sourceURL) { _ in reloadIfStale() }
        .onChange(of: deckState.hasTrack) { _ in reloadIfStale() }
        // The 30 Hz position poll mutates `elapsedSecs` — that's
        // what advances the playhead bracket. We don't need a
        // separate timer; SwiftUI re-evaluates `body` on the
        // @Published deck-state change.
    }

    // MARK: - Drawing

    private func drawBackground(ctx: GraphicsContext, size: CGSize) {
        let rect = CGRect(origin: .zero, size: size)
        ctx.fill(Path(rect), with: .color(DubColor.surface1))
        // 1-px hairline marking the seam against the playing
        // waveform. Vertical mode: seam on the inner edge (right
        // for deck A, left for deck B). Horizontal Prep mode: the
        // overview sits *above* the playing strip so the seam is
        // along the overview's bottom edge.
        let seam: CGRect
        switch orientation {
        case .vertical:
            seam = side == .a
                ? CGRect(x: size.width - 1, y: 0, width: 1, height: size.height)
                : CGRect(x: 0, y: 0, width: 1, height: size.height)
        case .horizontal:
            seam = CGRect(x: 0, y: size.height - 1, width: size.width, height: 1)
        }
        ctx.fill(Path(seam), with: .color(DubColor.divider))
    }

    private func drawBars(ctx: GraphicsContext, size: CGSize, buckets: [OverviewBucket]) {
        let n = buckets.count
        guard n > 0 else { return }
        // Two-tone envelope matching the Metal playing-waveform's
        // Serato-faithful look (M10.5r refresh): bright outer hull
        // at `peak`, slightly transparent darker core at `rms`.
        //
        // M10.5t: bars live inside `[axisStart, axisEnd]` so the
        // very-first and very-last bar don't kiss the strip edges
        // (the "warping" the user reported). `axisLength` is the
        // axis-aligned length minus 2× endPadding.
        let peakColor = peakBarColor()
        let rmsColor = rmsBarColor()
        let pad = OverviewLayout.endPadding
        switch orientation {
        case .vertical:
            let axisStart = pad
            let axisLength = max(0, size.height - 2 * pad)
            let centreX = size.width * 0.5
            let halfW = size.width * 0.5 - 2
            var peakPath = Path()
            var rmsPath = Path()
            for (i, bucket) in buckets.enumerated() {
                let y0 = axisStart + axisLength * CGFloat(i) / CGFloat(n)
                let y1 = axisStart + axisLength * CGFloat(i + 1) / CGFloat(n)
                let height = max(1, y1 - y0)
                let peakW = max(1, CGFloat(bucket.peak.clamped01) * halfW)
                peakPath.addRect(CGRect(
                    x: centreX - peakW, y: y0,
                    width: peakW * 2, height: height))
                let rmsW = max(0.5, CGFloat(bucket.rms.clamped01) * halfW)
                rmsPath.addRect(CGRect(
                    x: centreX - rmsW, y: y0,
                    width: rmsW * 2, height: height))
            }
            ctx.fill(peakPath, with: .color(peakColor))
            ctx.fill(rmsPath, with: .color(rmsColor))
        case .horizontal:
            let axisStart = pad
            let axisLength = max(0, size.width - 2 * pad)
            let centreY = size.height * 0.5
            let halfH = size.height * 0.5 - 2
            var peakPath = Path()
            var rmsPath = Path()
            for (i, bucket) in buckets.enumerated() {
                let x0 = axisStart + axisLength * CGFloat(i) / CGFloat(n)
                let x1 = axisStart + axisLength * CGFloat(i + 1) / CGFloat(n)
                let width = max(1, x1 - x0)
                let peakH = max(1, CGFloat(bucket.peak.clamped01) * halfH)
                peakPath.addRect(CGRect(
                    x: x0, y: centreY - peakH,
                    width: width, height: peakH * 2))
                let rmsH = max(0.5, CGFloat(bucket.rms.clamped01) * halfH)
                rmsPath.addRect(CGRect(
                    x: x0, y: centreY - rmsH,
                    width: width, height: rmsH * 2))
            }
            ctx.fill(peakPath, with: .color(peakColor))
            ctx.fill(rmsPath, with: .color(rmsColor))
        }
    }

    private func drawPlayhead(ctx: GraphicsContext, size: CGSize) {
        guard let fraction = playheadFraction() else { return }
        let chevronSize: CGFloat = 4
        let pad = OverviewLayout.endPadding
        switch orientation {
        case .vertical:
            let axisLength = max(0, size.height - 2 * pad)
            let y = pad + axisLength * CGFloat(fraction)
            let line = CGRect(x: 0, y: max(0, y - 0.5),
                              width: size.width, height: 1)
            ctx.fill(Path(line), with: .color(DubColor.playheadAccent))
            let leftChevron = Path { p in
                p.move(to: CGPoint(x: 0, y: y - chevronSize))
                p.addLine(to: CGPoint(x: chevronSize, y: y))
                p.addLine(to: CGPoint(x: 0, y: y + chevronSize))
                p.closeSubpath()
            }
            let rightChevron = Path { p in
                p.move(to: CGPoint(x: size.width, y: y - chevronSize))
                p.addLine(to: CGPoint(x: size.width - chevronSize, y: y))
                p.addLine(to: CGPoint(x: size.width, y: y + chevronSize))
                p.closeSubpath()
            }
            ctx.fill(leftChevron, with: .color(DubColor.playheadAccent))
            ctx.fill(rightChevron, with: .color(DubColor.playheadAccent))
        case .horizontal:
            let axisLength = max(0, size.width - 2 * pad)
            let x = pad + axisLength * CGFloat(fraction)
            let line = CGRect(x: max(0, x - 0.5), y: 0,
                              width: 1, height: size.height)
            ctx.fill(Path(line), with: .color(DubColor.playheadAccent))
            let topChevron = Path { p in
                p.move(to: CGPoint(x: x - chevronSize, y: 0))
                p.addLine(to: CGPoint(x: x, y: chevronSize))
                p.addLine(to: CGPoint(x: x + chevronSize, y: 0))
                p.closeSubpath()
            }
            let bottomChevron = Path { p in
                p.move(to: CGPoint(x: x - chevronSize, y: size.height))
                p.addLine(to: CGPoint(x: x, y: size.height - chevronSize))
                p.addLine(to: CGPoint(x: x + chevronSize, y: size.height))
                p.closeSubpath()
            }
            ctx.fill(topChevron, with: .color(DubColor.playheadAccent))
            ctx.fill(bottomChevron, with: .color(DubColor.playheadAccent))
        }
    }

    /// Compute the playhead's fractional position **on the same
    /// chunk grid the bars are laid out on**. This is the M10.5t
    /// fix for the "overview drifts towards the end of the song"
    /// feedback: the previous code used `elapsedSecs / durationSecs`
    /// as the playhead fraction, which is *almost* but not exactly
    /// the same as the bar grid's denominator. The bar grid is laid
    /// out on `peaksLen × chunkDurationSecs` (= `chunkCount *
    /// samples_per_chunk / track_sr`), whereas `durationSecs` is
    /// `track.frames() / track_sr`. The two differ by up to one
    /// chunk's worth of frames at the very end of the file (the
    /// offline decimator flushes a partial last chunk so the bar
    /// array covers `≥` track frames). For typical 44.1 kHz tracks
    /// the difference is sub-millisecond and bounded, but the user
    /// perceives any drift between "where the bracket sits" and
    /// "which bar represents the audible material" as an off-by-N
    /// error in the visible mapping. Computing the playhead on the
    /// *exact same grid as the bars* eliminates the entire class.
    ///
    /// Mirrors the M10.5n principle used by the main Metal
    /// waveform: convert time → chunk index via `chunkDurationSecs`
    /// (the f64 the engine reports directly), not via the
    /// round-tripped engine-SR sample count.
    ///
    /// Falls back to `elapsedSecs / durationSecs` when peak data
    /// isn't loaded yet (e.g. Thru mode, fresh deck before
    /// `reloadIfStale` finishes); returns `nil` when nothing
    /// useful can be computed.
    private func playheadFraction() -> Double? {
        let elapsed = deckState.elapsedSecs
        let peaksLen = model.engine.peaksLen(deckIdx: deckIdx)
        let chunkDur = model.engine.peaksChunkDurationSecs(deckIdx: deckIdx)
        if peaksLen > 0 && chunkDur > 0 {
            let totalSecs = Double(peaksLen) * chunkDur
            guard totalSecs > 0 else { return nil }
            return max(0, min(1, elapsed / totalSecs))
        }
        guard deckState.durationSecs > 0 else { return nil }
        return max(0, min(1, elapsed / deckState.durationSecs))
    }

    private func drawEmptyState(ctx: GraphicsContext, size: CGSize) {
        // Faint dashed midline along the strip's *time* axis so
        // the empty state reads as a container, not a missing
        // element. Runs top → bottom in vertical mode, left →
        // right in horizontal mode.
        let dash: Path
        switch orientation {
        case .vertical:
            let x = size.width * 0.5
            dash = Path { p in
                p.move(to: CGPoint(x: x, y: 0))
                p.addLine(to: CGPoint(x: x, y: size.height))
            }
        case .horizontal:
            let y = size.height * 0.5
            dash = Path { p in
                p.move(to: CGPoint(x: 0, y: y))
                p.addLine(to: CGPoint(x: size.width, y: y))
            }
        }
        ctx.stroke(
            dash,
            with: .color(DubColor.divider),
            style: StrokeStyle(lineWidth: 1, dash: [3, 4]))
    }

    /// Outer-envelope colour for the overview's peak hull. M10.5r
    /// refresh: matches the playing-waveform's deck tint at a
    /// slightly reduced saturation so the overview still reads as
    /// secondary chrome, but with the same hue as the main strip
    /// so the two pieces visually agree. The pre-M10.5r muted
    /// `deckAOverview` / `deckBOverview` tones read too brown /
    /// teal-grey to feel related to the bright deck tint.
    private func peakBarColor() -> Color {
        DubColor.deckTint(side).opacity(0.78)
    }

    /// Inner RMS-core colour. Brighter version of the peak tint to
    /// give the envelope a two-tone look identical to the Metal
    /// playing-waveform's outer / inner split. Sits *on top of* the
    /// peak fill, so its opacity stacks with the peak's — keep it
    /// near 1.0 to read clean.
    private func rmsBarColor() -> Color {
        DubColor.deckTint(side).opacity(0.95)
    }

    // MARK: - Click-to-jump

    /// Click-to-jump on the overview (PRD §6.1 / §9.6). Maps the
    /// click's position along the strip's time axis to an absolute
    /// track position and seeks the deck there. Transport state is
    /// left alone — a paused deck stays paused at the new position,
    /// a playing deck keeps playing from the new position.
    ///
    /// Two M10.5t fixes layered on the previous behaviour:
    ///
    /// 1. **Padding-aware.** The click is measured in *axis* space
    ///    (between `endPadding` and `size - endPadding`), matching
    ///    the layout the bars and the playhead bracket use. A
    ///    click inside the padding clamps to the nearest bar
    ///    rather than snapping to `0` or `durationSecs`.
    /// 2. **Chunk-grid-canonical seek.** The seek target is
    ///    computed as `fraction × peaksLen × chunkDurationSecs`,
    ///    not `fraction × durationSecs`. The bars are laid out on
    ///    the peak-chunk grid (`peaksLen × chunkDurationSecs`
    ///    seconds total) so the click must map back through the
    ///    *same* grid — clicking the visual position of bar `b`
    ///    must seek to the audio time bar `b` actually represents,
    ///    regardless of any sub-millisecond delta between the bar
    ///    grid's total and `track.frames() / track_sr`. Mirrors
    ///    the M10.5n root-cause-fix principle for the main
    ///    waveform: peaks are cadenced in track frames, do all
    ///    seek/playhead math on that grid.
    ///
    /// Also goes through `WaveformAppModel.seekDeck` so the
    /// playhead bracket moves on the same SwiftUI tick as the
    /// click instead of waiting up to 33 ms for the next 30 Hz
    /// position poll.
    ///
    /// Works in both Performance and Prep mode. In two-deck
    /// Timecode the seek lands instantly and the timecode driver
    /// re-locks on the next confident sample.
    private func handleClickJump(at point: CGPoint, in size: CGSize) {
        guard deckState.hasTrack, deckState.durationSecs > 0 else { return }
        let pad = OverviewLayout.endPadding
        let fraction: Double
        switch orientation {
        case .vertical:
            let axisLength = size.height - 2 * pad
            guard axisLength > 0 else { return }
            let local = max(0, min(axisLength, point.y - pad))
            fraction = Double(local / axisLength)
        case .horizontal:
            let axisLength = size.width - 2 * pad
            guard axisLength > 0 else { return }
            let local = max(0, min(axisLength, point.x - pad))
            fraction = Double(local / axisLength)
        }
        let seekSecs: Double
        let peaksLen = model.engine.peaksLen(deckIdx: deckIdx)
        let chunkDur = model.engine.peaksChunkDurationSecs(deckIdx: deckIdx)
        if peaksLen > 0 && chunkDur > 0 {
            let totalSecs = Double(peaksLen) * chunkDur
            seekSecs = fraction * totalSecs
        } else {
            // Pre-peaks fallback (e.g. Thru-mode source): use the
            // header's reported duration. Same math as before the
            // M10.5t fix; only fires when peak data is missing.
            seekSecs = fraction * deckState.durationSecs
        }
        model.seekDeck(side: side, absoluteSecs: seekSecs)
    }

    // MARK: - Decimation

    /// Pull peaks via FFI and decimate to `bucketCount` buckets.
    /// Idempotent given the same (`hasTrack`, `sourceURL`,
    /// generation) tuple; cheap enough to call from
    /// `.onChange(of: sourceURL)` and `.onAppear` without
    /// debouncing.
    private func reloadIfStale() {
        let currentGen = model.engine.peaksGeneration(deckIdx: deckIdx)
        // No track → drop any cached buckets so the empty-state
        // path renders. This also covers engine-stopped, where
        // `peaks_generation` returns 0 and `hasTrack` is false.
        guard deckState.hasTrack else {
            buckets = nil
            lastSeenGeneration = currentGen
            return
        }
        let len = model.engine.peaksLen(deckIdx: deckIdx)
        guard len > 0 else {
            buckets = []
            lastSeenGeneration = currentGen
            return
        }
        // Pull the entire broadband peak array. `peaks_extend`
        // with start_idx = 0 returns every chunk that has been
        // produced so far; for File-mode sources that's the
        // whole track (computed offline at load time per M10.5a).
        let data = model.engine.peaksExtend(deckIdx: deckIdx, startIdx: 0)
        buckets = Self.decimate(data: data, bucketCount: Self.bucketCount)
        lastSeenGeneration = currentGen
    }

    /// Pure-function decimator. Takes the FFI's packed
    /// `PeakChunk` byte buffer (12 bytes per chunk: min, max, rms
    /// — three f32 little-endian) and reduces it to `bucketCount`
    /// (`peak`, `rms`) pairs. Per-bucket `peak` is the max
    /// `max(|min|, |max|)` across its chunk range; per-bucket
    /// `rms` is the *RMS-of-RMS* (sqrt-of-mean-of-squares) across
    /// the same range, which preserves loudness when chunks are
    /// aggregated.
    fileprivate static func decimate(data: Data, bucketCount: Int) -> [OverviewBucket] {
        let stride = MemoryLayout<Float>.size * 3 // f32 × 3
        let chunkCount = data.count / stride
        guard chunkCount > 0, bucketCount > 0 else { return [] }
        var out = [OverviewBucket](
            repeating: OverviewBucket(peak: 0, rms: 0),
            count: bucketCount)
        data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            guard let base = raw.baseAddress else { return }
            for b in 0..<bucketCount {
                // `[start, end)` chunk indices for this bucket.
                let start = (b * chunkCount) / bucketCount
                let endRaw = ((b + 1) * chunkCount) / bucketCount
                let end = max(start + 1, endRaw)
                var peak: Float = 0
                var rmsAccum: Float = 0
                var rmsN: Int = 0
                for i in start..<min(end, chunkCount) {
                    let p = base.advanced(by: i * stride)
                        .assumingMemoryBound(to: Float.self)
                    let mn = p[0]
                    let mx = p[1]
                    let rms = p[2]
                    let a = max(abs(mn), abs(mx))
                    if a > peak { peak = a }
                    rmsAccum += rms * rms
                    rmsN += 1
                }
                let rmsAvg: Float = rmsN > 0
                    ? (rmsAccum / Float(rmsN)).squareRoot()
                    : 0
                out[b] = OverviewBucket(peak: peak, rms: rmsAvg)
            }
        }
        return out
    }
}

private extension Float {
    /// Saturating clamp to `[0, 1]` for amplitude / fraction maths.
    /// Kept on `Float` (not `BinaryFloatingPoint`) to avoid the
    /// generic-conformance cost — every caller in this file uses
    /// `Float` directly.
    var clamped01: Float {
        max(0, min(1, self))
    }
}

/// Pin the overview to its orientation-appropriate intrinsic
/// dimension: a fixed width (filling height) in vertical mode, a
/// fixed height (filling width) in horizontal Prep-mode mode.
private struct OverviewSizing: ViewModifier {
    let orientation: WaveformOrientation
    func body(content: Content) -> some View {
        switch orientation {
        case .vertical:
            content
                .frame(width: DubLayout.deckOverviewWidth)
                .frame(maxHeight: .infinity)
        case .horizontal:
            content
                .frame(height: DubLayout.deckOverviewHeight)
                .frame(maxWidth: .infinity)
        }
    }
}

#Preview {
    TrackOverviewView(model: WaveformAppModel(), side: .a, deckIdx: 0)
        .frame(width: DubLayout.deckOverviewWidth, height: 600)
}
