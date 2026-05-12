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
    /// Callback the status-strip gear button hits to open the
    /// Preferences sheet — owned by `MainView`, passed down so
    /// `PerformanceView` itself stays free of sheet bindings.
    let openPreferences: () -> Void

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
            isRunning: model.isRunning,
            lastError: model.lastError,
            openPreferences: openPreferences)
    }

    // MARK: - Deck headers

    @ViewBuilder
    private var deckHeaders: some View {
        if model.engineMode == .prep {
            DeckHeader(side: .a, state: headerState(side: .a))
                .background(DubColor.divider)
        } else {
            HStack(spacing: 1) {
                DeckHeader(side: .a, state: headerState(side: .a))
                DeckHeader(side: .b, state: headerState(side: .b))
            }
            .background(DubColor.divider)
        }
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

    /// Centre region. **Two-deck modes** keep the §9.2 symmetric
    /// layout invariant (both deck panes side-by-side, idle
    /// placeholder when one deck has no source). **Prep mode**
    /// collapses to a single full-width deck-A pane — Prep mode
    /// is a single-deck shell (PRD §3.1 / M10.8); a phantom "OFF"
    /// deck-B pane is just noise.
    @ViewBuilder
    private var waveformRegion: some View {
        if model.engineMode == .prep {
            deckPane(side: .a, deckIdx: 0, enabled: deckAEnabled)
                .frame(minHeight: DubLayout.waveformMinHeight)
                .background(DubColor.divider)
        } else {
            HStack(spacing: 1) {
                deckPane(side: .a, deckIdx: 0, enabled: deckAEnabled)
                deckPane(side: .b, deckIdx: 1, enabled: deckBEnabled)
            }
            .frame(minHeight: DubLayout.waveformMinHeight)
            .background(DubColor.divider)
        }
    }

    /// Column width the playing waveform strip is rendered at.
    /// Serato Scratch Live's playing waveform is ≈ 110–140 px tall
    /// in its horizontal orientation; translated into our vertical
    /// bottom-→-top scroll that maps to a ≈ 160 px-wide strip
    /// (`DubLayout.deckColumnWidth`). Prep mode gets a wider strip
    /// (`deckColumnWidthPrep`) because the single-deck surface has
    /// more horizontal real estate to spend; M10.5c will rotate
    /// that strip 90° into a true horizontal layout.
    private var waveformColumnWidth: CGFloat {
        model.engineMode == .prep
            ? DubLayout.deckColumnWidthPrep
            : DubLayout.deckColumnWidth
    }

    /// One deck's pane — Metal waveform when the deck has any
    /// source, idle placeholder otherwise. The pane (drop target,
    /// background, error-flash zone) spans the full half-window
    /// width, but the waveform *strip* itself is width-capped and
    /// centred. The remaining horizontal space is reserved for the
    /// M10.5c Track-Overview waveform and per-deck info chips.
    /// PRD §5.5: the pane is the drop target for Finder-drag file
    /// loads; PRD §6.4: the pane surfaces the 200 ms red flash when
    /// a load fails because the target deck is currently playing.
    @ViewBuilder
    private func deckPane(side: DeckSide, deckIdx: UInt64, enabled: Bool) -> some View {
        let deckState = (side == .a) ? model.deckA : model.deckB
        let hasSource = enabled && (deckState.hasTrack
                                    || (model.engineMode == .timecode && model.isRunning))
        ZStack {
            HStack(spacing: 0) {
                Spacer(minLength: 0)
                Group {
                    if hasSource {
                        WaveformView(
                            engine: model.engine, deckIdx: deckIdx,
                            palette: model.palette, side: side)
                            .background(DubColor.surface0)
                    } else {
                        idlePane(side: side)
                    }
                }
                .frame(width: waveformColumnWidth)
                .frame(maxHeight: .infinity)
                Spacer(minLength: 0)
            }
            loadErrorOverlay(side: side, deckState: deckState)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        // macOS 13+ Transferable drop API. The legacy `.onDrop(of:
        // [.fileURL])` path silently failed on Finder drags here
        // because the system doesn't auto-coerce
        // `NSItemProvider`-backed URLs to a typed payload; using
        // `dropDestination(for: URL.self)` routes the drop through
        // `Transferable`'s `URL` conformance which understands
        // `public.file-url` and `public.url` natively. Drops from
        // Finder and from the in-app FileBrowserView both arrive
        // here as `[URL]`.
        .dropDestination(for: URL.self) { urls, _ in
            guard let url = urls.first else { return false }
            Task { @MainActor in
                if model.loadTrack(side: side, url: url) {
                    model.play(side: side)
                }
            }
            return true
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
    PerformanceView(model: WaveformAppModel(), openPreferences: {})
        .frame(width: 1440, height: 900)
}
