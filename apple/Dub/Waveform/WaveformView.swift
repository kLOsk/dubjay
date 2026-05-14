//
//  WaveformView.swift
//  Dub
//
//  M10-B SwiftUI wrapper around an `MTKView` driven by
//  `WaveformRenderer`. The view holds a single `DubEngine` reference
//  (passed in by `MainView`) and polls it for new peaks each frame.
//
//  Threading: `MTKView` invokes its delegate on the main thread when
//  configured with `isPaused = false` and `enableSetNeedsDisplay =
//  false`, which is what we do here. The renderer is `@MainActor`
//  isolated; SwiftUI lifecycle methods run on the main actor; no
//  cross-thread hazards.
//

import SwiftUI
import MetalKit
import DubCore

/// SwiftUI host for the broadband waveform. The `engine` parameter is
/// the shared `DubEngine` from `MainView`; this view never starts /
/// stops the engine — that's the picker's job.
///
/// M10.4 — the view is now a *vertical* column (PRD §9.1). The Metal
/// pipeline renders past peaks in the top 25 % of the drawable; this
/// wrapper overlays a deck-tinted playhead hairline at 25 % from the
/// top to mark the "now playing" position.
///
/// M10.5c-b — accepts an `orientation` parameter (defaults to
/// `.vertical` so every existing call site renders unchanged). The
/// `.horizontal` variant lays the same waveform out left → right
/// with the playhead 25 % from the left edge, in preparation for
/// the Prep-mode shell shipping in M10.8.
struct WaveformView: View {

    let engine: DubEngine
    let deckIdx: UInt64
    /// M10.2: current palette. Changes flow into the renderer via
    /// `updateNSView`; the renderer reads it on the next frame.
    let palette: WaveformPalette
    /// M10.4: which deck this view belongs to. Drives playhead tint
    /// + future affordances. Defaults to deck A so the existing
    /// preview & single-deck call sites keep working.
    let side: DeckSide
    /// M10.5c-b: time-axis orientation. Vertical is the Performance-
    /// mode default; horizontal is reserved for Prep mode (M10.8)
    /// and other inspector surfaces.
    let orientation: WaveformOrientation
    /// M10.6a click-scrub closure (PRD §6.1). When non-nil, the view
    /// installs a tap gesture on top of the Metal layer; clicking
    /// anywhere on the zoomed waveform calls the closure with a
    /// *signed* offset in seconds relative to the current playhead
    /// position (positive = forward, negative = backward). The host
    /// (`PerformanceView`) gates this — Prep mode wires it through
    /// `WaveformAppModel.scrub`; Timecode-mode panes pass `nil` per
    /// the PRD's "no fine-scrub on a timecode-controlled deck" rule.
    let onClickScrubRelativeSecs: ((TimeInterval) -> Void)?

    init(engine: DubEngine, deckIdx: UInt64 = 0,
         palette: WaveformPalette = .serato,
         side: DeckSide = .a,
         orientation: WaveformOrientation = .vertical,
         onClickScrubRelativeSecs: ((TimeInterval) -> Void)? = nil) {
        self.engine = engine
        self.deckIdx = deckIdx
        self.palette = palette
        self.side = side
        self.orientation = orientation
        self.onClickScrubRelativeSecs = onClickScrubRelativeSecs
    }

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .topLeading) {
                WaveformMetalView(
                    engine: engine, deckIdx: deckIdx,
                    palette: palette, orientation: orientation)
                if let callback = onClickScrubRelativeSecs {
                    scrubGestureOverlay(in: geo.size, callback: callback)
                }
                zeroCrossingOverlay(in: geo.size)
                playheadOverlay(in: geo.size)
            }
        }
    }

    /// M10.5e zero-crossing hairline. A faint 1-px line running
    /// along the amplitude=0 axis (i.e. *perpendicular* to the
    /// playhead, parallel to the time axis). Helps the eye read
    /// the bar's symmetry around silence and adds visual anchoring
    /// when the waveform is sparse. Drawn underneath the playhead
    /// overlay so the deck-tinted playhead always wins where they
    /// cross. Vertical mode: vertical line at x = mid-width.
    /// Horizontal mode: horizontal line at y = mid-height.
    @ViewBuilder
    private func zeroCrossingOverlay(in size: CGSize) -> some View {
        let tint = DubColor.divider.opacity(0.55)
        switch orientation {
        case .vertical:
            Rectangle()
                .fill(tint)
                .frame(width: 1, height: size.height)
                .offset(x: size.width * 0.5 - 0.5)
                .allowsHitTesting(false)
        case .horizontal:
            Rectangle()
                .fill(tint)
                .frame(width: size.width, height: 1)
                .offset(y: size.height * 0.5 - 0.5)
                .allowsHitTesting(false)
        }
    }

    /// Transparent hit-test layer that converts a click anywhere on
    /// the waveform to a signed seconds-offset from the playhead
    /// and forwards it to the host. Lives *under* the playhead
    /// overlay in the ZStack so the 1-px hairline doesn't eat
    /// gesture pixels (it has `allowsHitTesting(false)` anyway, but
    /// the order keeps the rendering intuition clean: gesture
    /// surface below, chrome on top).
    @ViewBuilder
    private func scrubGestureOverlay(
        in size: CGSize,
        callback: @escaping (TimeInterval) -> Void
    ) -> some View {
        Color.clear
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onEnded { value in
                        let secs = relativeSecs(from: value.location, in: size)
                        callback(secs)
                    }
            )
    }

    /// Convert a click location into a signed seconds-offset from
    /// the playhead. Uses the same `chunksPerPixel × samplesPerChunk
    /// / sampleRate` ratio the renderer uses to map chunk → pixel,
    /// so the gesture lands on the *visual* position the user
    /// clicked. Orientation-aware: vertical uses click y, horizontal
    /// uses click x.
    private func relativeSecs(from point: CGPoint, in size: CGSize) -> TimeInterval {
        let secsPerPixel = WaveformRenderer.secsPerPixel(
            sampleRate: engine.sampleRate())
        let fraction = CGFloat(WaveformRenderer.pastRegionFraction)
        switch orientation {
        case .vertical:
            let playheadY = size.height * fraction
            return TimeInterval((point.y - playheadY) * CGFloat(secsPerPixel))
        case .horizontal:
            let playheadX = size.width * fraction
            return TimeInterval((point.x - playheadX) * CGFloat(secsPerPixel))
        }
    }

    /// 1-px deck-tinted hairline marking the "now playing" position
    /// the Metal renderer addresses (PRD §9.1 / §9.6). Vertical
    /// orientation draws a horizontal hairline at 25 % from the top;
    /// horizontal orientation draws a vertical hairline at 25 % from
    /// the left. Tinted in the deck's accent colour so the two
    /// columns stay disambiguated at a glance.
    @ViewBuilder
    private func playheadOverlay(in size: CGSize) -> some View {
        let fraction = CGFloat(WaveformRenderer.pastRegionFraction)
        switch orientation {
        case .vertical:
            Rectangle()
                .fill(DubColor.deckTint(side))
                .frame(width: size.width, height: 1)
                .offset(y: size.height * fraction)
                .allowsHitTesting(false)
        case .horizontal:
            Rectangle()
                .fill(DubColor.deckTint(side))
                .frame(width: 1, height: size.height)
                .offset(x: size.width * fraction)
                .allowsHitTesting(false)
        }
    }
}

/// Bare `MTKView` host — the SwiftUI/AppKit bridge. Separated from
/// `WaveformView` so the playhead overlay can live in pure SwiftUI
/// without forcing the `NSViewRepresentable` to host both layers.
private struct WaveformMetalView: NSViewRepresentable {

    let engine: DubEngine
    let deckIdx: UInt64
    let palette: WaveformPalette
    let orientation: WaveformOrientation

    @MainActor
    final class Coordinator: NSObject, MTKViewDelegate {
        var renderer: WaveformRenderer?

        // MARK: MTKViewDelegate

        nonisolated func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {
            // Hop to the main actor — MTKView declares this callback
            // nonisolated but the renderer is @MainActor.
            Task { @MainActor [weak self] in
                self?.renderer?.drawableSizeWillChange(size)
            }
        }

        nonisolated func draw(in view: MTKView) {
            // MTKView with isPaused=false + setNeedsDisplay=false
            // invokes draw on the main thread already; we use the
            // actor-isolated entry point so the concurrency checker
            // is happy.
            MainActor.assumeIsolated {
                renderer?.draw(in: view)
            }
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeNSView(context: Context) -> MTKView {
        let mtkView = MTKView()
        mtkView.colorPixelFormat = .bgra8Unorm
        mtkView.clearColor = MTLClearColor(red: 0.07, green: 0.07, blue: 0.08, alpha: 1.0)
        mtkView.framebufferOnly = true
        mtkView.isPaused = false
        mtkView.enableSetNeedsDisplay = false
        mtkView.preferredFramesPerSecond = 60
        // 4× MSAA on the drawable. The waveform geometry is a stack
        // of trapezoid slices with sub-pixel edge slopes at high
        // zoom; without MSAA they stair-step into a visible
        // "venetian blind" pattern. MTKView allocates the
        // multisample texture itself when `sampleCount > 1` and the
        // render pass descriptor it hands us already has the
        // multisample → drawable resolve wired up.
        mtkView.sampleCount = WaveformRenderer.sampleCount

        if let device = MTLCreateSystemDefaultDevice() {
            mtkView.device = device
            do {
                let renderer = try WaveformRenderer(
                    device: device, engine: engine, deckIdx: deckIdx)
                renderer.palette = palette
                renderer.orientation = orientation
                context.coordinator.renderer = renderer
                mtkView.delegate = context.coordinator
            } catch {
                NSLog("WaveformView: renderer init failed: \(error.localizedDescription)")
            }
        } else {
            NSLog("WaveformView: MTLCreateSystemDefaultDevice() returned nil")
        }
        return mtkView
    }

    func updateNSView(_ nsView: MTKView, context: Context) {
        // Push the current palette + orientation into the renderer.
        // Cheap — just property assignments; the next draw frame
        // picks both up via the uniforms buffer. M10.5c-b
        // orientation changes also implicitly remap which drawable
        // dimension drives `chunksVisible` (see the orientation
        // switch in `WaveformRenderer.draw`).
        context.coordinator.renderer?.palette = palette
        context.coordinator.renderer?.orientation = orientation
    }
}
