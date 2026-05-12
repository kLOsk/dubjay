//
//  PerformanceView.swift
//  Dub
//
//  Top-level performance layout per PRD §9.2.
//
//  Layout (top → bottom):
//
//      ┌─ status strip ──────────────────────────────────────────┐
//      │ DUB · 48.0 kHz · LIVE         21:47   🔋 87%             │
//      ├──────────────────────────────┬──────────────────────────┤
//      │ deck A header (3 rows in     │ deck B header            │
//      │ File mode — adds track-time) │                          │
//      ├──────────────────────────────┼──────────────────────────┤
//      │                              │                          │
//      │  Metal waveform A            │  Metal waveform B        │
//      │  (or idle pane if A          │  (or idle pane if B      │
//      │   offline)                   │   offline)               │
//      │                              │                          │
//      │   playhead at 25 % from top, deck-tinted hairline       │
//      │                                                         │
//      ├─ FX bar placeholder (M15 / M16 / M17) ──────────────────┤
//      ├─ library / FS browser (M10.5b) ─────────────────────────┤
//      └─────────────────────────────────────────────────────────┘
//
//  M10.5b deck panes accept Finder-drop URLs onto each pane,
//  surface a 200 ms red overlay when a load fails because the target
//  deck is currently playing (PRD §5.5 + §6.4), and render the Metal
//  waveform whenever the deck is *either* live Thru *or* has a File
//  track loaded — not just when Thru is capturing.
//

import SwiftUI
import UniformTypeIdentifiers
import DubCore

/// Top-level performance surface. Driven by `WaveformAppModel`.
struct PerformanceView: View {

    @ObservedObject var model: WaveformAppModel

    var body: some View {
        VStack(spacing: 0) {
            statusStrip
            deckHeaders
            Rectangle().fill(DubColor.divider).frame(height: 1)
            waveformRegion
            Rectangle().fill(DubColor.divider).frame(height: 1)
            FXBarPlaceholder()
            Rectangle().fill(DubColor.divider).frame(height: 1)
            FileBrowserView(model: model)
        }
        .background(DubColor.surface0)
    }

    // MARK: - Status strip

    private var statusStrip: some View {
        StatusStripContainer(
            engineVersion: engineVersion(),
            sampleRate: model.engine.sampleRate(),
            isRunning: model.isRunning)
    }

    // MARK: - Deck headers

    private var deckHeaders: some View {
        HStack(spacing: 1) {
            DeckHeader(side: .a, state: headerState(side: .a))
            DeckHeader(side: .b, state: headerState(side: .b))
        }
        .background(DubColor.divider)
    }

    private func headerState(side: DeckSide) -> DeckHeaderState {
        let enabled: Bool
        switch side {
        case .a: enabled = deckAEnabled
        case .b: enabled = deckBEnabled
        }
        let deck = (side == .a) ? model.deckA : model.deckB
        return DeckHeaderState.from(
            side: side,
            deckState: deck,
            engineRunning: model.isRunning,
            deckEnabled: enabled,
            thruMode: model.engineMode == .timecode,
            isMaster: model.masterDeck == side)
    }

    // MARK: - Waveform region

    /// Centre region: always two side-by-side deck panes (PRD §9.2
    /// symmetric layout invariant). Each pane renders its own Metal
    /// waveform when its deck has *any* source (Thru capture or File
    /// track); otherwise an idle placeholder. The layout never
    /// collapses to a single pane.
    private var waveformRegion: some View {
        HStack(spacing: 1) {
            deckPane(side: .a, deckIdx: 0, enabled: deckAEnabled)
            deckPane(side: .b, deckIdx: 1, enabled: deckBEnabled)
        }
        .frame(minHeight: DubLayout.waveformMinHeight)
        .background(DubColor.divider)
    }

    /// One deck's pane — Metal waveform when the deck has any
    /// source, idle placeholder otherwise. The pane is the drop
    /// target for Finder-drag file loads (PRD §5.5) and surfaces
    /// the 200 ms red flash when a load fails because the target
    /// deck is currently playing.
    @ViewBuilder
    private func deckPane(side: DeckSide, deckIdx: UInt64, enabled: Bool) -> some View {
        let deckState = (side == .a) ? model.deckA : model.deckB
        let hasSource = enabled && (deckState.hasTrack
                                    || (model.engineMode == .timecode && model.isRunning))
        ZStack {
            if hasSource {
                WaveformView(
                    engine: model.engine, deckIdx: deckIdx,
                    palette: model.palette, side: side)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(DubColor.surface0)
            } else {
                idlePane(side: side)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            loadErrorOverlay(side: side, deckState: deckState)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onDrop(of: [.fileURL], isTargeted: nil) { providers in
            handleDrop(side: side, providers: providers)
        }
    }

    /// Red flash overlay surfaced for ~200 ms when a load is
    /// rejected because the deck is currently playing. The exact
    /// expiry timestamp lives on `DeckState.errorFlashUntil`; we
    /// rely on the 30 Hz poll inside the model to clear the field
    /// (which republishes and removes the overlay).
    @ViewBuilder
    private func loadErrorOverlay(side: DeckSide, deckState: DeckState) -> some View {
        if let until = deckState.errorFlashUntil, until > Date() {
            ZStack {
                DubColor.stateError.opacity(0.55)
                Text("DECK IS PLAYING — LIFT THE NEEDLE")
                    .font(DubFont.caps)
                    .tracking(1.5)
                    .foregroundStyle(.white)
                    .padding(DubSpacing.lg)
                    .background(DubColor.stateError.opacity(0.95))
                    .clipShape(RoundedRectangle(cornerRadius: DubRadius.panel))
            }
            .allowsHitTesting(false)
            .transition(.opacity)
            .animation(.easeOut(duration: 0.15), value: until)
        }
    }

    private func handleDrop(side: DeckSide, providers: [NSItemProvider]) -> Bool {
        guard let provider = providers.first else { return false }
        _ = provider.loadObject(ofClass: URL.self) { url, _ in
            guard let url else { return }
            Task { @MainActor in
                _ = model.loadTrack(side: side, url: url)
                model.play(side: side)
            }
        }
        return true
    }

    /// Is deck A enabled for the current engine mode?
    private var deckAEnabled: Bool {
        switch model.engineMode {
        case .timecode: return model.isRunning
        case .prep:     return model.isRunning
        }
    }

    /// Is deck B enabled for the current engine mode? In Prep mode
    /// deck B is intentionally off (PRD §3.1 — Prep is a
    /// single-deck shell).
    private var deckBEnabled: Bool {
        switch model.engineMode {
        case .timecode: return model.isRunning && model.twoDeckMode
        case .prep:     return false
        }
    }

    /// Idle pane content — a 1-px deck-tinted playhead hairline at
    /// 25 % from the top (so the canonical orientation reads from
    /// the moment the app launches, even before any audio plays),
    /// plus a context-appropriate hint.
    private func idlePane(side: DeckSide) -> some View {
        GeometryReader { geo in
            ZStack(alignment: .topLeading) {
                DubColor.surface0
                Rectangle()
                    .fill(DubColor.deckTint(side).opacity(0.35))
                    .frame(width: geo.size.width, height: 1)
                    .offset(y: geo.size.height
                        * CGFloat(WaveformRenderer.pastRegionFraction))
                VStack(spacing: DubSpacing.sm) {
                    Text(side.label)
                        .font(DubFont.caps)
                        .tracking(1.2)
                        .foregroundStyle(DubColor.deckTint(side).opacity(0.7))
                    Text(idleCaption(side: side))
                        .font(DubFont.caps)
                        .tracking(0.6)
                        .foregroundStyle(DubColor.textSecondary)
                    Text(idleHint(side: side))
                        .font(DubFont.body)
                        .foregroundStyle(DubColor.textPlaceholder)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, DubSpacing.lg)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
            }
        }
    }

    private func idleCaption(side: DeckSide) -> String {
        switch side {
        case .a:
            return model.isRunning ? "DECK STOPPED" : "ENGINE STOPPED"
        case .b:
            if !model.isRunning { return "ENGINE STOPPED" }
            switch model.engineMode {
            case .timecode: return "SINGLE-DECK MODE"
            case .prep:     return "PREP MODE — DECK B OFF"
            }
        }
    }

    private func idleHint(side: DeckSide) -> String {
        switch side {
        case .a:
            if !model.isRunning {
                return "Open Preferences (⌘,) to pick an input device and start."
            }
            return "Drag an audio file here, or press Space to load the browser selection."
        case .b:
            if !model.isRunning {
                return "Open Preferences (⌘,) to start the engine."
            }
            switch model.engineMode {
            case .timecode:
                return "Drag a file here, or configure deck B's channels in Preferences (⌘,) for Thru."
            case .prep:
                return "Prep mode shows a single deck. Switch to Performance in Preferences for two decks."
            }
        }
    }
}

#Preview("Performance — idle") {
    PerformanceView(model: WaveformAppModel())
        .frame(width: 1440, height: 900)
}
