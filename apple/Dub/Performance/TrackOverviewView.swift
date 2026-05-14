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

/// One decimated amplitude value per overview bucket. Mirrors the
/// `(min_sample, max_sample)` shape of `PeakChunk` so the bars
/// drawn around the strip's centreline have the same visual
/// vocabulary as the playing waveform (mirrored top / bottom).
private struct OverviewBucket {
    /// Normalised positive amplitude — `max(abs(min), abs(max))`
    /// across the bucket's chunk range, then clamped to `[0, 1]`.
    var amplitude: Float
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
                DragGesture(minimumDistance: 0)
                    .onEnded { value in
                        handleTap(at: value.location, in: geo.size)
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
        let color = barColor()
        switch orientation {
        case .vertical:
            // Bars distributed top → bottom, mirrored around the
            // strip's *vertical* centreline.
            let centreX = size.width * 0.5
            let halfW = size.width * 0.5 - 2
            let h = size.height
            for (i, bucket) in buckets.enumerated() {
                let y0 = h * CGFloat(i) / CGFloat(n)
                let y1 = h * CGFloat(i + 1) / CGFloat(n)
                let amp = CGFloat(min(max(bucket.amplitude, 0), 1))
                let w = max(1, amp * halfW)
                let rect = CGRect(x: centreX - w, y: y0,
                                  width: w * 2, height: max(1, y1 - y0))
                ctx.fill(Path(rect), with: .color(color))
            }
        case .horizontal:
            // Bars distributed left → right, mirrored around the
            // strip's *horizontal* centreline.
            let centreY = size.height * 0.5
            let halfH = size.height * 0.5 - 2
            let w = size.width
            for (i, bucket) in buckets.enumerated() {
                let x0 = w * CGFloat(i) / CGFloat(n)
                let x1 = w * CGFloat(i + 1) / CGFloat(n)
                let amp = CGFloat(min(max(bucket.amplitude, 0), 1))
                let h = max(1, amp * halfH)
                let rect = CGRect(x: x0, y: centreY - h,
                                  width: max(1, x1 - x0), height: h * 2)
                ctx.fill(Path(rect), with: .color(color))
            }
        }
    }

    private func drawPlayhead(ctx: GraphicsContext, size: CGSize) {
        // No playhead until we have a duration to normalise
        // against. In Thru mode `durationSecs == 0` so this path
        // never fires — the overview just shows the static
        // amplitude trace.
        guard deckState.durationSecs > 0 else { return }
        let fraction = max(0, min(1, deckState.elapsedSecs / deckState.durationSecs))
        let chevronSize: CGFloat = 4
        switch orientation {
        case .vertical:
            let y = size.height * CGFloat(fraction)
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
            let x = size.width * CGFloat(fraction)
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

    private func barColor() -> Color {
        // Match the playing-waveform deck-tint — but at reduced
        // saturation so the overview reads as secondary chrome,
        // not a competing surface. M10.5b uses palette-driven
        // tints; the M10.5c overview cribs from the same source
        // but flattens the per-band split (overview is broadband
        // only).
        switch side {
        case .a: return DubColor.deckAOverview
        case .b: return DubColor.deckBOverview
        }
    }

    // MARK: - Click-to-jump

    /// PRD §6.1 click-to-jump on the overview. Allowed in File
    /// mode always; in Timecode mode allowed only when Panic Play
    /// is engaged on this deck (M10.6c — once the engine is
    /// decoupled from the platter via Panic Play, mouse-driven
    /// seeks no longer fight the timecode rate). We treat both
    /// single-deck Timecode + Prep as "File-mode-ish" because
    /// there's only one deck and the user has clearly loaded a
    /// file; the strict §6.1 Timecode-gating applies only to two-
    /// deck Timecode where the live timecode signal is the
    /// authority on playback rate.
    private func handleTap(at point: CGPoint, in size: CGSize) {
        guard deckState.hasTrack, deckState.durationSecs > 0 else { return }
        let isTwoDeckTimecode = model.twoDeckMode && model.engineMode == .timecode
        if isTwoDeckTimecode, !deckState.isPanicPlay {
            // Two-deck Timecode without Panic Play: the platter is
            // the authority on rate + position. A mouse seek here
            // would race the next decode update + click-through
            // — silently no-op per PRD §6.1.
            return
        }
        let fraction: Double
        switch orientation {
        case .vertical:
            guard size.height > 0 else { return }
            fraction = min(max(Double(point.y / size.height), 0), 1)
        case .horizontal:
            guard size.width > 0 else { return }
            fraction = min(max(Double(point.x / size.width), 0), 1)
        }
        let seekSecs = fraction * deckState.durationSecs
        do {
            try model.engine.seek(deckIdx: deckIdx, positionSecs: seekSecs)
        } catch {
            model.surfaceError("Seek failed: \(error.localizedDescription)")
        }
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
    /// — three f32 little-endian) and reduces it to
    /// `bucketCount` amplitude values. Each bucket's amplitude is
    /// the max of `|min|` and `|max|` over its chunk range.
    fileprivate static func decimate(data: Data, bucketCount: Int) -> [OverviewBucket] {
        let stride = MemoryLayout<Float>.size * 3 // f32 × 3
        let chunkCount = data.count / stride
        guard chunkCount > 0, bucketCount > 0 else { return [] }
        var out = [OverviewBucket](repeating: OverviewBucket(amplitude: 0), count: bucketCount)
        data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            guard let base = raw.baseAddress else { return }
            for b in 0..<bucketCount {
                // `[start, end)` chunk indices for this bucket.
                let start = (b * chunkCount) / bucketCount
                let endRaw = ((b + 1) * chunkCount) / bucketCount
                let end = max(start + 1, endRaw)
                var peak: Float = 0
                for i in start..<min(end, chunkCount) {
                    let p = base.advanced(by: i * stride)
                        .assumingMemoryBound(to: Float.self)
                    let mn = p[0]
                    let mx = p[1]
                    let a = max(abs(mn), abs(mx))
                    if a > peak { peak = a }
                }
                out[b] = OverviewBucket(amplitude: peak)
            }
        }
        return out
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
