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
//  Row 3: (File mode only) track time / total · remaining
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

    enum Source: Equatable {
        case off
        case thru
        case timecode
        case file
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
        timeRow: nil, isMaster: false
    )
}

/// The deck header. Stateless — caller supplies a `DeckHeaderState`
/// per render.
struct DeckHeader: View {

    let side: DeckSide
    let state: DeckHeaderState

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

    // MARK: - Row 3 — track time (File mode only)

    @ViewBuilder
    private func timeRow(_ time: DeckHeaderState.TimeRow) -> some View {
        HStack(spacing: DubSpacing.md) {
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
        case .timecode: return "TIMECODE"
        case .file:     return "FILE"
        }
    }

    private var sourcePillDotColor: Color {
        guard state.isLive else { return DubColor.textPlaceholder }
        switch state.source {
        case .off:      return DubColor.textPlaceholder
        case .thru:     return DubColor.stateLocked
        case .timecode: return DubColor.stateLocked
        case .file:     return DubColor.stateTentative
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

        if deckState.hasTrack {
            let time = DeckHeaderState.TimeRow(
                elapsedText: DeckTimeFormat.format(deckState.elapsedSecs),
                totalText:   DeckTimeFormat.format(deckState.durationSecs),
                remainingText: DeckTimeFormat.format(deckState.remainingSecs, signed: true))
            return DeckHeaderState(
                isLive: true,
                source: .file,
                trackTitle: deckState.displayName,
                trackArtist: nil,
                formatChip: deckState.formatChip,
                timeRow: time,
                isMaster: isMaster)
        }

        if thruMode {
            return DeckHeaderState(
                isLive: true,
                source: .thru,
                trackTitle: "Real Record",
                trackArtist: "capturing live",
                formatChip: nil,
                timeRow: nil,
                isMaster: isMaster)
        }

        return DeckHeaderState(
            isLive: false,
            source: .off,
            trackTitle: nil,
            trackArtist: nil,
            formatChip: nil,
            timeRow: nil,
            isMaster: false)
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
        formatChip: nil, timeRow: nil, isMaster: true))
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
        isMaster: false))
        .frame(width: 720)
        .background(DubColor.surface0)
        .padding()
}
