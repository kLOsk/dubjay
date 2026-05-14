//
//  DeckHeader.swift
//  Dub
//
//  Two-row strip above each deck's waveform region per PRD §9.2.
//  M10.5b adds a track-time line + a `MASTER` chip so the FS-browser
//  Space-load + master-deck semantics from §6.4 are visible at a
//  glance.
//
//  Row 1: deck label · source pill · MASTER chip · track title · artist · format chip
//  Row 2: pitch · BPM · key · FX chip
//  Row 3: (track loaded) Play/Pause · Restart · track time / total · remaining
//
//  M10.6a (Casual Play UI, PRD §6.1.3): the time row gains a left-
//  aligned transport-glyph cluster — Play/Pause toggle + Restart —
//  so the DJ can start file playback by mouse before a set begins
//  (or pause / restart it). Glyphs render exactly when `timeRow`
//  renders (i.e. a file track is loaded), which covers both the
//  Prep-mode single-deck shell and the Casual-Play-pre-Timecode
//  case in two-deck Timecode mode. Transport callbacks are passed
//  in from `PerformanceView` via a `DeckHeaderCallbacks` value.
//

import SwiftUI
import DubCore

/// Driving state for one deck header. Pure function of the model
/// (engine status + DeckState + master-deck flag) — the view does no
/// derivation of its own, which keeps it trivially snapshot-testable
/// in M18.
struct DeckHeaderState: Equatable {

    /// Whether the engine has an active source on this deck (Thru
    /// capture *or* a loaded File track). Drives the source pill's
    /// "ON / OFF" treatment.
    let isLive: Bool

    /// What the deck is currently sourcing audio from.
    let source: Source

    let trackTitle: String?
    let trackArtist: String?

    /// Format / SR caption ("MP3 · 44.1 kHz · stereo"). `nil` for
    /// Thru / off decks.
    let formatChip: String?

    /// File-mode time-row (elapsed / total + remaining). `nil` for
    /// Thru / off decks — no canonical playhead concept in Thru
    /// mode (timecode drives the rate).
    let timeRow: TimeRow?

    /// Whether this deck is the current master (PRD §6.4).
    let isMaster: Bool

    /// M10.6a: whether the deck is currently advancing the playhead.
    /// Drives the Play / Pause toggle in the transport-glyph cluster
    /// (PRD §6.1.3 Casual Play). Independent from `timeRow != nil`
    /// — `timeRow` says "a file is loaded so render the time
    /// indicators"; `isPlaying` says "the engine is advancing
    /// elapsed time right now". A paused-mid-track deck has
    /// `timeRow != nil` and `isPlaying == false` — the Play glyph
    /// shows.
    let isPlaying: Bool

    /// M10.6c: whether the engine has Panic Play engaged on this
    /// deck (PRD §6.1.2). When `true` the source pill flips to the
    /// `.tcHold` variant ("TC · HOLD" / amber dot) and the
    /// transport-cluster primary button renders the "re-engage
    /// timecode" icon. Authoritative source is the engine via
    /// `PositionInfo.isPanicPlay` (30 Hz poll); the model also sets
    /// it optimistically on `panic(side:)` for zero-frame UI
    /// latency.
    let isPanicPlay: Bool

    /// M10.6d: whether the transport cluster's primary button should
    /// behave as a Serato-style INT/ABS toggle (engage / cancel
    /// Panic Play) rather than as a Play/Pause toggle. True iff
    /// the engine is in Timecode mode with a loaded track — Panic
    /// Play needs audible audio to recover *to* (PRD §6.1.2) so
    /// the toggle is meaningless without a track, and Prep mode
    /// never engages timecode in the first place. In Timecode mode
    /// the toggle subsumes Casual Play: tap once to engage internal
    /// playback (= start the track when the platter is silent / mid-
    /// fix), tap again to hand control back to the timecode driver.
    let useTimecodeToggle: Bool

    enum Source: Equatable {
        case off
        case thru
        case timecode
        case file
        /// M10.6c. Engine mode is Timecode, a file track is the
        /// audio source, but Panic Play is engaged so the deck is
        /// decoupled from its timecode input and holding the last-
        /// known velocity (PRD §6.1.2). Renders as `TC · HOLD` with
        /// an amber dot.
        case tcHold
        /// M10.5d. A `load_track` FFI call is in flight on this
        /// deck (decode + offline peaks running on a background
        /// `Task.detached`). Renders as `LOADING…` with an amber
        /// dot — supersedes `.file` / `.tcHold` while the load is
        /// running so the user sees the deck is busy.
        case loading
    }

    struct TimeRow: Equatable {
        let elapsedText: String   // "01:23"
        let totalText: String     // "03:45"
        let remainingText: String // "-02:22"
    }

    /// Convenience: idle / cold-launch state.
    static let idle = DeckHeaderState(
        isLive: false, source: .off,
        trackTitle: nil, trackArtist: nil, formatChip: nil,
        timeRow: nil, isMaster: false, isPlaying: false,
        isPanicPlay: false, useTimecodeToggle: false
    )
}

/// M10.6a transport callbacks the deck header invokes when the user
/// clicks Play / Pause / Restart in the time row. Kept off
/// `DeckHeaderState` so the state value stays `Equatable` (closures
/// aren't). `PerformanceView` constructs an instance per render that
/// forwards into `WaveformAppModel.{play, pause, restart}(side:)`.
struct DeckHeaderCallbacks {
    /// Casual-Play start (Prep mode + track loaded + paused).
    var onPlay:    () -> Void = {}
    /// Casual-Play pause (Prep mode + track loaded + playing).
    var onPause:   () -> Void = {}
    /// M10.6d INT/ABS toggle. Used by the transport cluster when
    /// the engine is in Timecode mode with a track loaded: tap
    /// engages Panic Play (internal playback at last-known rate);
    /// tap-while-engaged cancels it (hand back to timecode driver).
    /// `PerformanceView` routes this to
    /// `WaveformAppModel.panicToggle(side:)`.
    var onPanicToggle: () -> Void = {}

    /// No-op fallback used by the cold-launch / preview state where
    /// no model is wired in yet.
    static let noop = DeckHeaderCallbacks()
}

/// The deck header. Stateless — caller supplies a `DeckHeaderState`
/// per render.
struct DeckHeader: View {

    let side: DeckSide
    let state: DeckHeaderState
    /// M10.6a Casual-Play transport callbacks. Defaults to no-op so
    /// the cold-launch / preview path doesn't have to wire anything.
    var callbacks: DeckHeaderCallbacks = .noop

    var body: some View {
        VStack(alignment: .leading, spacing: DubSpacing.sm) {
            row1
            row2
            if let time = state.timeRow {
                timeRow(time)
            }
        }
        .padding(.horizontal, DubSpacing.lg)
        .padding(.vertical, DubSpacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .frame(minHeight: DubLayout.deckHeaderHeight)
        .background(DubColor.surface1)
    }

    // MARK: - Row 1 — identity

    @ViewBuilder
    private var row1: some View {
        HStack(spacing: DubSpacing.md) {
            deckLabel
            sourcePill
            if state.isMaster {
                masterChip
            }
            if let title = state.trackTitle {
                Text(title)
                    .font(DubFont.title)
                    .foregroundStyle(DubColor.textPrimary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            } else {
                placeholderText("—", font: DubFont.title)
            }
            if let artist = state.trackArtist {
                Text("· \(artist)")
                    .font(DubFont.body)
                    .foregroundStyle(DubColor.textSecondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
            if let chip = state.formatChip {
                formatChipView(chip)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Row 2 — live stats

    @ViewBuilder
    private var row2: some View {
        HStack(spacing: DubSpacing.lg) {
            statColumn(label: "PITCH", value: "—")
            statColumn(label: "BPM",   value: "—")
            statColumn(label: "KEY",   value: "—")
            Spacer(minLength: 0)
            fxChip
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Row 3 — track time + transport glyphs (track loaded)

    @ViewBuilder
    private func timeRow(_ time: DeckHeaderState.TimeRow) -> some View {
        HStack(spacing: DubSpacing.md) {
            transportGlyphs
            Text(time.elapsedText)
                .font(DubFont.numericInline)
                .foregroundStyle(DubColor.textPrimary)
            Text("/")
                .font(DubFont.numericInline)
                .foregroundStyle(DubColor.textTertiary)
            Text(time.totalText)
                .font(DubFont.numericInline)
                .foregroundStyle(DubColor.textSecondary)
            Spacer(minLength: 0)
            Text(time.remainingText)
                .font(DubFont.numericInline)
                .foregroundStyle(DubColor.textSecondary)
        }
        .monospacedDigit()
    }

    /// Transport-cluster primary button (PRD §6.1).
    ///
    /// Sits left of the elapsed-time numbers in Row 3, and only
    /// renders because `timeRow(_:)` only renders when a track is
    /// loaded — no button in Thru mode where there's no canonical
    /// playhead. Branches on `useTimecodeToggle`:
    ///
    /// * Prep mode (`useTimecodeToggle == false`): classic
    ///   Play/Pause toggle. Drives `onPlay` / `onPause` per
    ///   `isPlaying`.
    /// * Timecode mode + track loaded (`useTimecodeToggle == true`):
    ///   Serato-style INT/ABS toggle. Drives `onPanicToggle` either
    ///   way; the icon flips between `play.fill` (currently following
    ///   platter — tap to play internally) and `opticaldisc.fill`
    ///   amber (currently internal — tap to re-engage timecode).
    ///   Subsumes Casual Play: a paused-in-Timecode deck still
    ///   shows `play.fill`, and tapping engages Panic Play which
    ///   starts internal playback at last-known rate — fixing the
    ///   "Play does nothing in Timecode mode" bug where the prior
    ///   `engine.play` call was instantly overwritten by the next
    ///   `DropoutHoldRate` block.
    ///
    /// The Restart button from the M10.6a draft is gone: the
    /// Track Overview strip's click-to-top handles seek-to-zero,
    /// and Panic Play handles "keep playing through a glitch", so
    /// we don't need a third glyph.
    private var transportGlyphs: some View {
        HStack(spacing: DubSpacing.sm) {
            primaryButton
        }
    }

    @ViewBuilder
    private var primaryButton: some View {
        if state.useTimecodeToggle {
            timecodeToggleButton
        } else {
            playPauseButton
        }
    }

    /// Prep-mode Play/Pause toggle (PRD §6.1.3).
    private var playPauseButton: some View {
        transportButton(
            systemName: state.isPlaying ? "pause.fill" : "play.fill",
            accessibilityLabel: state.isPlaying ? "Pause" : "Play",
            tint: DubColor.textPrimary,
            background: DubColor.surface2,
            action: state.isPlaying ? callbacks.onPause : callbacks.onPlay)
    }

    /// Timecode-mode INT/ABS toggle (PRD §6.1.2 / M10.6d). Amber
    /// tint + background while panic is engaged so the button
    /// visually agrees with the `TC · HOLD` source-pill amber dot.
    private var timecodeToggleButton: some View {
        transportButton(
            systemName: state.isPanicPlay
                ? "opticaldisc.fill"
                : "play.fill",
            accessibilityLabel: state.isPanicPlay
                ? "Re-engage timecode"
                : "Play internally (disengage timecode)",
            tint: state.isPanicPlay
                ? DubColor.stateTentative
                : DubColor.textPrimary,
            background: state.isPanicPlay
                ? DubColor.stateTentative.opacity(0.15)
                : DubColor.surface2,
            action: callbacks.onPanicToggle)
    }

    @ViewBuilder
    private func transportButton(
        systemName: String,
        accessibilityLabel: String,
        tint: Color,
        background: Color,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: systemName)
                .symbolRenderingMode(.monochrome)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(tint)
                .frame(width: 20, height: 20)
                .background(background)
                .clipShape(RoundedRectangle(cornerRadius: 3, style: .continuous))
        }
        .buttonStyle(.borderless)
        .accessibilityLabel(accessibilityLabel)
    }

    // MARK: - Subviews

    private var deckLabel: some View {
        Text(side.label)
            .font(DubFont.caps)
            .tracking(1.2)
            .foregroundStyle(DubColor.deckTint(side))
    }

    /// Pill: bullet + source name. Pill colour follows live state
    /// (locked green when capturing / playing, secondary grey when
    /// idle) — a quick at-a-distance "is the deck running?" tell.
    private var sourcePill: some View {
        HStack(spacing: DubSpacing.xs) {
            Circle()
                .fill(sourcePillDotColor)
                .frame(width: 6, height: 6)
            Text(sourcePillLabel)
                .font(DubFont.caps)
                .tracking(0.6)
                .foregroundStyle(DubColor.textSecondary)
        }
        .padding(.horizontal, DubSpacing.sm)
        .padding(.vertical, 3)
        .background(DubColor.surface2)
        .clipShape(Capsule())
    }

    /// MASTER chip — visible only on the master deck. Anchors the
    /// "which deck does Space-load avoid" UI affordance from PRD §6.4.
    private var masterChip: some View {
        Text("MASTER")
            .font(DubFont.caps)
            .tracking(0.8)
            .foregroundStyle(DubColor.deckTint(side))
            .padding(.horizontal, DubSpacing.sm)
            .padding(.vertical, 2)
            .overlay(
                Capsule(style: .continuous)
                    .stroke(DubColor.deckTint(side), lineWidth: 1)
            )
    }

    private var sourcePillLabel: String {
        switch state.source {
        case .off:      return "OFF"
        case .thru:     return state.isLive ? "THRU · LIVE" : "THRU"
        case .timecode: return state.isLive ? "TIMECODE · LIVE" : "TIMECODE"
        case .file:     return "FILE"
        case .tcHold:   return "TC · HOLD"
        case .loading:  return "LOADING…"
        }
    }

    private var sourcePillDotColor: Color {
        guard state.isLive else { return DubColor.textPlaceholder }
        switch state.source {
        case .off:      return DubColor.textPlaceholder
        case .thru:     return DubColor.stateLocked
        case .timecode: return DubColor.stateLocked
        case .file:     return DubColor.stateTentative
        case .tcHold:   return DubColor.stateTentative
        case .loading:  return DubColor.stateTentative
        }
    }

    @ViewBuilder
    private func formatChipView(_ text: String) -> some View {
        Text(text)
            .font(DubFont.micro)
            .foregroundStyle(DubColor.textTertiary)
            .padding(.horizontal, DubSpacing.sm)
            .padding(.vertical, 2)
            .background(DubColor.surface2)
            .clipShape(RoundedRectangle(cornerRadius: 3, style: .continuous))
    }

    @ViewBuilder
    private func statColumn(label: String, value: String) -> some View {
        HStack(spacing: DubSpacing.sm) {
            Text(label)
                .font(DubFont.caps)
                .tracking(0.8)
                .foregroundStyle(DubColor.textSecondary)
            Text(value)
                .font(DubFont.numericInline)
                .foregroundStyle(
                    value == "—" ? DubColor.textPlaceholder : DubColor.textPrimary
                )
        }
    }

    private var fxChip: some View {
        HStack(spacing: DubSpacing.xs) {
            Text("FX")
                .font(DubFont.caps)
                .tracking(0.8)
                .foregroundStyle(DubColor.textSecondary)
            Text("—")
                .font(DubFont.numericInline)
                .foregroundStyle(DubColor.textPlaceholder)
        }
        .padding(.horizontal, DubSpacing.sm)
        .padding(.vertical, 3)
        .overlay(
            RoundedRectangle(cornerRadius: DubRadius.panel, style: .continuous)
                .stroke(DubColor.divider, lineWidth: 1)
        )
    }

    @ViewBuilder
    private func placeholderText(_ text: String, font: Font) -> some View {
        Text(text)
            .font(font)
            .foregroundStyle(DubColor.textPlaceholder)
    }
}

// MARK: - Time formatting

/// Format a duration in seconds as `MM:SS` (or `HH:MM:SS` for tracks
/// over 60 minutes — DJ mix-files do exist). Returns `"--:--"` for
/// negative / NaN inputs so we never crash on a transient bad poll.
enum DeckTimeFormat {
    static func format(_ secs: Double, signed: Bool = false) -> String {
        guard secs.isFinite, secs >= 0 else { return "--:--" }
        let total = Int(secs)
        let hh = total / 3600
        let mm = (total / 60) % 60
        let ss = total % 60
        let sign = signed ? "-" : ""
        if hh > 0 {
            return String(format: "%@%02d:%02d:%02d", sign, hh, mm, ss)
        }
        return String(format: "%@%02d:%02d", sign, mm, ss)
    }
}

// MARK: - Derivation from DeckState

extension DeckHeaderState {

    /// Build a header state from the model's per-deck snapshot plus
    /// the engine-wide flags. Keeps all derivation in one place so
    /// the view stays declarative.
    static func from(
        side: DeckSide,
        deckState: DeckState,
        engineRunning: Bool,
        deckEnabled: Bool,
        thruMode: Bool,
        isMaster: Bool
    ) -> DeckHeaderState {
        guard engineRunning, deckEnabled else { return .idle }

        // M10.5d: cold load (no previous track) — render the
        // header with the new title + LOADING pill but no time row
        // (duration is unknown until decode completes). The
        // transport-toggle is gated off until `hasTrack` flips
        // true.
        if deckState.isLoading, !deckState.hasTrack {
            return DeckHeaderState(
                isLive: true,
                source: .loading,
                trackTitle: deckState.displayName,
                trackArtist: nil,
                formatChip: nil,
                timeRow: nil,
                isMaster: isMaster,
                isPlaying: false,
                isPanicPlay: false,
                useTimecodeToggle: false)
        }

        if deckState.hasTrack {
            let time = DeckHeaderState.TimeRow(
                elapsedText: DeckTimeFormat.format(deckState.elapsedSecs),
                totalText:   DeckTimeFormat.format(deckState.durationSecs),
                remainingText: DeckTimeFormat.format(deckState.remainingSecs, signed: true))
            // M10.6c: in Timecode mode + Panic Play engaged, the
            // source pill flips from FILE → TC · HOLD (PRD §6.1.2).
            // M10.5d: a replace-load (new file decoded while the
            // previous one is still resident) shows the LOADING
            // pill but keeps the old time row + transport-toggle
            // available — the previous track stays audible /
            // visible until the new peaks swap in at decode
            // completion (one frame after the engine bumps
            // `peak_generation_seq`).
            let inPanic = thruMode && deckState.isPanicPlay
            let source: Source
            if deckState.isLoading {
                source = .loading
            } else if inPanic {
                source = .tcHold
            } else {
                source = .file
            }
            return DeckHeaderState(
                isLive: true,
                source: source,
                trackTitle: deckState.displayName,
                trackArtist: nil,
                formatChip: deckState.formatChip,
                timeRow: time,
                isMaster: isMaster,
                isPlaying: deckState.isPlaying,
                isPanicPlay: inPanic,
                useTimecodeToggle: thruMode)
        }

        if thruMode {
            // Timecode engine mode + no File track loaded → the deck
            // is in "Real Record" Thru mode. The pill reads
            // `TIMECODE` because that's the *engine mode* the user
            // picked (PRD §1: "real records are first-class citizens
            // via Thru mode auto-detection") — even though M5.6's
            // actual timecode decoder isn't wired through the UI
            // yet, this is the milestone the surface advertises.
            //
            // No transport toggle here: panic needs a loaded track
            // to recover *to* (PRD §6.1.2). The button only appears
            // once the DJ has loaded a file onto the deck.
            return DeckHeaderState(
                isLive: true,
                source: .timecode,
                trackTitle: "Real Record",
                trackArtist: "capturing live",
                formatChip: nil,
                timeRow: nil,
                isMaster: isMaster,
                isPlaying: false,
                isPanicPlay: false,
                useTimecodeToggle: false)
        }

        return DeckHeaderState(
            isLive: false,
            source: .off,
            trackTitle: nil,
            trackArtist: nil,
            formatChip: nil,
            timeRow: nil,
            isMaster: false,
            isPlaying: false,
            isPanicPlay: false,
            useTimecodeToggle: false)
    }
}

#Preview("Deck A — idle") {
    DeckHeader(side: .a, state: .idle)
        .frame(width: 720)
        .background(DubColor.surface0)
        .padding()
}

#Preview("Deck A — live Thru, master") {
    DeckHeader(side: .a, state: DeckHeaderState(
        isLive: true, source: .thru,
        trackTitle: "Real Record", trackArtist: "capturing live",
        formatChip: nil, timeRow: nil,
        isMaster: true, isPlaying: false,
        isPanicPlay: false, useTimecodeToggle: false))
        .frame(width: 720)
        .background(DubColor.surface0)
        .padding()
}

#Preview("Deck B — File, mid-track") {
    DeckHeader(side: .b, state: DeckHeaderState(
        isLive: true, source: .file,
        trackTitle: "Stakes Is High",
        trackArtist: nil,
        formatChip: "MP3 · 44.1 kHz · stereo",
        timeRow: DeckHeaderState.TimeRow(
            elapsedText: "01:23",
            totalText: "03:45",
            remainingText: "-02:22"),
        isMaster: false, isPlaying: true,
        isPanicPlay: false, useTimecodeToggle: true))
        .frame(width: 720)
        .background(DubColor.surface0)
        .padding()
}

#Preview("Deck B — Timecode, Panic Play engaged") {
    DeckHeader(side: .b, state: DeckHeaderState(
        isLive: true, source: .tcHold,
        trackTitle: "Stakes Is High",
        trackArtist: nil,
        formatChip: "MP3 · 44.1 kHz · stereo",
        timeRow: DeckHeaderState.TimeRow(
            elapsedText: "01:23",
            totalText: "03:45",
            remainingText: "-02:22"),
        isMaster: true, isPlaying: true,
        isPanicPlay: true, useTimecodeToggle: true))
        .frame(width: 720)
        .background(DubColor.surface0)
        .padding()
}
