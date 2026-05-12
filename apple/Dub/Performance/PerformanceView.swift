//
//  PerformanceView.swift
//  Dub
//
//  M10.3 top-level performance layout, M10.4-rotated. This is the
//  "real" UI the PRD §9.2 describes; `MainView` is now a thin
//  wrapper around it plus the `⌘,` Preferences sheet that holds
//  the dev tools.
//
//  Layout (top → bottom, after M10.4):
//
//      ┌─ status strip ──────────────────────────────────────────┐
//      │ DUB · 48.0 kHz · LIVE         21:47   🔋 87%             │
//      ├──────────────────────────────┬──────────────────────────┤
//      │ deck A header (rows)         │ deck B header (rows)     │
//      ├──────────────────────────────┼──────────────────────────┤
//      │                              │                          │
//      │  Metal waveform A            │  Metal waveform B        │
//      │  (or idle pane if A          │  (or idle pane if B      │
//      │   offline)                   │   offline)               │
//      │                              │                          │
//      │   playhead at 25 % from top, deck-tinted hairline       │
//      │                                                         │
//      ├─ FX bar placeholder (M15 / M16 / M17) ──────────────────┤
//      ├─ library placeholder (M11) ─────────────────────────────┤
//      └─────────────────────────────────────────────────────────┘
//
//  Live data wiring:
//
//  * status strip — engine sample rate + isRunning + live wall
//                   clock + battery (M10.4)
//  * deck headers — derive `DeckHeaderState` from `WaveformAppModel`
//                   (`isRunning`, `twoDeckMode`)
//  * waveform     — vertical Metal renderer per PRD §9.1; deck B
//                   pane is *always present* (idle placeholder
//                   when offline) so the layout stays symmetric
//                   (M10.4 layout invariant).
//

import SwiftUI
import DubCore

/// Top-level performance surface. Driven by `WaveformAppModel`,
/// which owns the `DubEngine` handle.
struct PerformanceView: View {

    @ObservedObject var model: WaveformAppModel

    var body: some View {
        VStack(spacing: 0) {
            statusStrip
            deckHeaders
            Rectangle()
                .fill(DubColor.divider)
                .frame(height: 1)
            waveformRegion
            Rectangle()
                .fill(DubColor.divider)
                .frame(height: 1)
            FXBarPlaceholder()
            Rectangle()
                .fill(DubColor.divider)
                .frame(height: 1)
            LibraryPlaceholder()
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
            DeckHeader(side: .a, state: deckHeaderState(side: .a))
            DeckHeader(side: .b, state: deckHeaderState(side: .b))
        }
        .background(DubColor.divider)
    }

    private func deckHeaderState(side: DeckSide) -> DeckHeaderState {
        let deckActive: Bool
        switch side {
        case .a: deckActive = model.isRunning
        case .b: deckActive = model.isRunning && model.twoDeckMode
        }
        if deckActive {
            return .liveThru
        }
        return .idle
    }

    // MARK: - Waveform region

    /// Centre region: **always** two side-by-side deck panes (PRD
    /// §9.2 symmetric layout invariant — M10.4). Each pane renders
    /// its own Metal waveform when its deck is live; otherwise an
    /// idle placeholder matching the deck header's `OFF` source-pill
    /// state. The layout never collapses to a single pane.
    private var waveformRegion: some View {
        HStack(spacing: 1) {
            deckPane(side: .a, deckIdx: 0, active: deckAActive)
            deckPane(side: .b, deckIdx: 1, active: deckBActive)
        }
        .frame(minHeight: DubLayout.waveformMinHeight)
        .background(DubColor.divider)
    }

    /// One deck's pane — Metal waveform when the deck is live, idle
    /// placeholder otherwise. The pane fills its half of the
    /// `HStack` equally so the two panes are always the same width
    /// regardless of which decks happen to be active.
    @ViewBuilder
    private func deckPane(side: DeckSide, deckIdx: UInt64, active: Bool) -> some View {
        if active {
            WaveformView(
                engine: model.engine, deckIdx: deckIdx,
                palette: model.palette, side: side)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(DubColor.surface0)
        } else {
            idlePane(side: side)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    /// Is deck A live? Single-deck mode counts as deck-A-only.
    private var deckAActive: Bool { model.isRunning }

    /// Is deck B live? Only in two-deck mode while the engine runs.
    private var deckBActive: Bool { model.isRunning && model.twoDeckMode }

    /// Idle pane content — a 1-px deck-tinted playhead hairline at
    /// 25 % from the top (so the canonical orientation reads from
    /// the moment the app launches, even before any audio plays),
    /// plus a context-appropriate hint. Distinct copy for "engine
    /// off entirely" (deck A) vs "engine running but this deck is
    /// off" (deck B in single-deck mode) so the operator knows
    /// which condition to fix.
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
            return model.isRunning ? "SINGLE-DECK MODE" : "ENGINE STOPPED"
        }
    }

    private func idleHint(side: DeckSide) -> String {
        switch side {
        case .a:
            return "Open Preferences (⌘,) to pick an input device and start."
        case .b:
            return model.isRunning
                ? "Configure deck B's channels in Preferences (⌘,) to bring it online."
                : "Open Preferences (⌘,) to start the engine."
        }
    }
}

#Preview("Performance — idle") {
    PerformanceView(model: WaveformAppModel())
        .frame(width: 1440, height: 900)
}
