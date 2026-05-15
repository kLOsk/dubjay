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

/// M10.5s vinyl-style scratch callbacks for the zoomed waveform.
/// Replaces the M10.5r seek-and-play loop with a rate-driven
/// scratch: the view reports the cursor's running offset from the
/// drag start (in audio seconds) and the host
/// (`WaveformAppModel.scratch*`) integrates that into a playback
/// rate. Mouse-still ⇒ offset stops changing ⇒ rate falls to 0 ⇒
/// silence, exactly like a stylus on a stationary platter.
///
/// Constructed as a value type so SwiftUI's diffing treats it as
/// stable across renders (the captured closures may differ, but
/// the surrounding `WaveformView` already rebuilds them per render
/// via `scrubHandler`-style factories, which is fine).
struct WaveformScrubHandler {
    /// Called on the first `onChanged` event of a drag, before any
    /// `onOffsetChanged`. Host captures pre-scratch transport,
    /// engages Panic Play in Timecode mode, freezes the playhead.
    let onBegan: () -> Void
    /// Called on every subsequent `onChanged` event with the
    /// cursor's running offset (in audio seconds) from the drag's
    /// start point. Positive = forward; negative = reverse. Host
    /// integrates these via a polling timer into a rate.
    let onOffsetChanged: (TimeInterval) -> Void
    /// Called on `onEnded`. Host stops the polling timer, restores
    /// pre-scratch transport, cancels Panic Play if engaged.
    let onEnded: () -> Void
}

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
    /// M10.6a / M10.5r continuous-scrub handler (PRD §6.1).
    ///
    /// When non-nil, the view installs a `DragGesture` on top of the
    /// Metal layer that fires on every `onChanged` event — the
    /// pointer's offset from the playhead is converted to a signed
    /// seconds-offset and forwarded to the host, which feeds the
    /// engine via `WaveformAppModel.scrubAudioSeek`. The host owns
    /// the play/pause-around-scrub bookkeeping so audio plays under
    /// the cursor.
    ///
    /// The pre-M10.5r single-tap-only behaviour (fire on `onEnded`)
    /// felt unresponsive because the waveform didn't move with the
    /// mouse. Continuous drag fixes that and is the user-asked-for
    /// "find the exact position of a kick" workflow. Set to `nil`
    /// when the host doesn't want a scrub gesture (e.g. Thru-mode
    /// panes where there's no track to scrub).
    let scrubHandler: WaveformScrubHandler?

    init(engine: DubEngine, deckIdx: UInt64 = 0,
         palette: WaveformPalette = .serato,
         side: DeckSide = .a,
         orientation: WaveformOrientation = .vertical,
         scrubHandler: WaveformScrubHandler? = nil) {
        self.engine = engine
        self.deckIdx = deckIdx
        self.palette = palette
        self.side = side
        self.orientation = orientation
        self.scrubHandler = scrubHandler
    }

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .topLeading) {
                WaveformMetalView(
                    engine: engine, deckIdx: deckIdx,
                    palette: palette, orientation: orientation)
                if let handler = scrubHandler {
                    scrubGestureOverlay(in: geo.size, handler: handler)
                }
                zeroCrossingOverlay(in: geo.size)
                playheadOverlay(in: geo.size)
            }
        }
    }

    /// M10.5e zero-crossing hairline. A 1-px line running along the
    /// amplitude=0 axis (i.e. *perpendicular* to the playhead,
    /// parallel to the time axis). Helps the eye read the bar's
    /// symmetry around silence, anchors the strip visually when
    /// the waveform is sparse, and — since M10.5t — provides the
    /// visible "needle is on the platter" baseline in the lead-in /
    /// lead-out empty-groove regions (PRD §9.6). Before M10.5t it
    /// used `DubColor.divider.opacity(0.55)`, which against a
    /// pure-black silent region rendered effectively invisible
    /// (~0x171A1F vs 0x000000); a Serato comparison made it
    /// obvious the dark groove needed a properly-visible
    /// centerline. White at ~20 % opacity matches the Serato
    /// reference: clearly visible against black, almost entirely
    /// hidden under the bars (which are centred on this axis), so
    /// it doesn't read as a separate UI element. Drawn underneath
    /// the playhead overlay so the deck-tinted playhead always
    /// wins where they cross.
    @ViewBuilder
    private func zeroCrossingOverlay(in size: CGSize) -> some View {
        let tint = Color.white.opacity(0.22)
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

    /// Transparent hit-test layer that drives the M10.5s vinyl-
    /// style scratch. We report the cursor's running offset (in
    /// audio seconds) from the drag's start position; the host
    /// (`WaveformAppModel.scratch*`) derives a smoothed playback
    /// rate from the per-event Δoffset / Δt (M10.5t rework — the
    /// earlier 60 Hz timer polled snapshots of the offset, which
    /// aliased against the typical 60–120 Hz cursor-event rate
    /// and produced audible "jumping" on a steady drag). When the
    /// mouse is held still the cursor stops emitting events; the
    /// host's stall watchdog ramps the deck rate toward zero
    /// within ~25 ms, so a stationary mouse plays silence just
    /// like a record under a stationary stylus.
    ///
    /// Sits *under* the playhead overlay in the ZStack so the 1-px
    /// hairline doesn't eat gesture pixels (it has
    /// `allowsHitTesting(false)` anyway, but the order keeps the
    /// rendering intuition clean: gesture surface below, chrome on
    /// top).
    @ViewBuilder
    private func scrubGestureOverlay(
        in size: CGSize,
        handler: WaveformScrubHandler
    ) -> some View {
        let secsPerPixel = Double(WaveformRenderer.secsPerPixel(
            sampleRate: engine.sampleRate()))
        Color.clear
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { value in
                        let deltaPx: CGFloat
                        switch orientation {
                        case .vertical:
                            // Time runs top → bottom under the
                            // stylus in the Metal renderer, so a
                            // downward drag = forward in time =
                            // positive offset. Matches "drag the
                            // record forward".
                            deltaPx = value.location.y - value.startLocation.y
                        case .horizontal:
                            // Past = left, future = right, and forward
                            // playback scrolls the waveform leftward
                            // through the playhead (PRD §9.6). So a
                            // leftward drag mirrors a platter being
                            // pushed forward: the user's finger moves
                            // with the content, and the audio under
                            // the playhead advances. Invert the sign
                            // here so leftward = positive offset =
                            // forward rate.
                            deltaPx = value.startLocation.x - value.location.x
                        }
                        let offsetSecs = Double(deltaPx) * secsPerPixel
                        // `onBegan` is idempotent on the host side
                        // (the model's `scratchBegin` ignores
                        // repeats), so we don't need to dedupe
                        // here — the lazy-begin pattern fires it
                        // on every `onChanged` until the host has
                        // captured pre-scratch state.
                        handler.onBegan()
                        handler.onOffsetChanged(offsetSecs)
                    }
                    .onEnded { _ in
                        handler.onEnded()
                    }
            )
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
