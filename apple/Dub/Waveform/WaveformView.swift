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

    init(engine: DubEngine, deckIdx: UInt64 = 0,
         palette: WaveformPalette = .seratoFaithful,
         side: DeckSide = .a) {
        self.engine = engine
        self.deckIdx = deckIdx
        self.palette = palette
        self.side = side
    }

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .topLeading) {
                WaveformMetalView(
                    engine: engine, deckIdx: deckIdx, palette: palette)
                playheadOverlay(in: geo.size)
            }
        }
    }

    /// 1-px deck-tinted hairline at 25 % from the top of the column,
    /// marking the "now playing" position the Metal renderer
    /// addresses (PRD §9.1 / §9.6). Tinted in the deck's accent
    /// colour so the two columns stay disambiguated at a glance.
    private func playheadOverlay(in size: CGSize) -> some View {
        let playheadY = size.height * CGFloat(WaveformRenderer.pastRegionFraction)
        return Rectangle()
            .fill(DubColor.deckTint(side))
            .frame(width: size.width, height: 1)
            .offset(y: playheadY)
            .allowsHitTesting(false)
    }
}

/// Bare `MTKView` host — the SwiftUI/AppKit bridge. Separated from
/// `WaveformView` so the playhead overlay can live in pure SwiftUI
/// without forcing the `NSViewRepresentable` to host both layers.
private struct WaveformMetalView: NSViewRepresentable {

    let engine: DubEngine
    let deckIdx: UInt64
    let palette: WaveformPalette

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

        if let device = MTLCreateSystemDefaultDevice() {
            mtkView.device = device
            do {
                let renderer = try WaveformRenderer(
                    device: device, engine: engine, deckIdx: deckIdx)
                renderer.palette = palette
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
        // Push the current palette into the renderer. Cheap — just a
        // property assignment; the next draw frame picks it up via
        // the uniforms buffer.
        context.coordinator.renderer?.palette = palette
    }
}
