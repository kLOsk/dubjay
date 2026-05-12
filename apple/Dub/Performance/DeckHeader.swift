//
//  DeckHeader.swift
//  Dub
//
//  M10.3 deck header — the two-row strip that sits above each
//  deck's waveform region. Layout mirrors PRD §9.2 and the Figma
//  exploration; **code wins** when the two disagree (PRD §9 note).
//
//  Row 1: deck label · source pill · track title · artist · format chip
//  Row 2: pitch · BPM · key · FX chip
//
//  Live data hooked up in M10.3:
//
//  * source pill — bound to whether the engine is running + the
//    deck's mode (always THRU in M10.3; M11 introduces FILE; M5
//    already exposes TC at the engine layer but the FFI accessor
//    isn't wired yet).
//
//  Placeholders (em-dash content) until a follow-up:
//
//  * BPM        — needs `DubEngine.bpmState(deck)` FFI accessor
//                 over M8's already-running tracker (trivial; not
//                 in M10.3 scope).
//  * pitch %    — needs deck-rate FFI; depends on M13 plumbing.
//  * key        — depends on M14 key-detection (or library import
//                 metadata once M11 lands).
//  * FX chip    — depends on M15 / M16 Smart FX engine state.
//

import SwiftUI
import DubCore

/// Driving state for one deck header. Everything the row renders
/// comes from here; the view is otherwise a pure function of this
/// struct, which keeps it trivially snapshot-testable once the
/// M18 test infrastructure is in place.
struct DeckHeaderState: Equatable {

    /// Whether the engine has an active Thru capture for this deck.
    /// Drives the source pill's "ON / OFF" treatment.
    let isLive: Bool

    /// What the deck is currently sourcing audio from. M10.3 is
    /// always `.thru` when `isLive == true` (M11 will add `.file`).
    let source: Source

    /// Track display name. M10.3 has no library yet, so this stays
    /// as the friendly placeholder for the "live capture" mode.
    let trackTitle: String?

    /// Artist / metadata caption. `nil` when there's no library
    /// metadata to show.
    let trackArtist: String?

    /// Format / SR caption ("FLAC · 44.1 kHz"). `nil` until M11.
    let formatChip: String?

    enum Source: Equatable {
        case off
        case thru
        case timecode
        case file
    }

    /// Convenience: idle state, deck on, engine not running.
    static let idle = DeckHeaderState(
        isLive: false,
        source: .off,
        trackTitle: nil,
        trackArtist: nil,
        formatChip: nil
    )

    /// Convenience: live Thru capture, no library context.
    static let liveThru = DeckHeaderState(
        isLive: true,
        source: .thru,
        trackTitle: "Real Record",
        trackArtist: "capturing live",
        formatChip: nil
    )
}

/// The two-row deck header. Stateless — caller supplies a
/// `DeckHeaderState` per render.
struct DeckHeader: View {

    let side: DeckSide
    let state: DeckHeaderState

    var body: some View {
        VStack(alignment: .leading, spacing: DubSpacing.sm) {
            row1
            row2
        }
        .padding(.horizontal, DubSpacing.lg)
        .padding(.vertical, DubSpacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .frame(height: DubLayout.deckHeaderHeight)
        .background(DubColor.surface1)
    }

    // MARK: - Row 1 — identity

    @ViewBuilder
    private var row1: some View {
        HStack(spacing: DubSpacing.md) {
            deckLabel
            sourcePill
            if let title = state.trackTitle {
                Text(title)
                    .font(DubFont.title)
                    .foregroundStyle(DubColor.textPrimary)
                    .lineLimit(1)
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

    // MARK: - Subviews

    private var deckLabel: some View {
        Text(side.label)
            .font(DubFont.caps)
            .tracking(1.2)
            .foregroundStyle(DubColor.deckTint(side))
    }

    /// Pill: bullet + source name. The pill colour follows live state
    /// (locked green when capturing, secondary grey when idle) — a
    /// quick at-a-distance "is the deck running?" tell.
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

    /// Inline `LABEL  VALUE` pair. Label is small-caps secondary;
    /// value is numeric medium. Value is the M10.3-placeholder
    /// em-dash if `"—"`.
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

#Preview("Deck A — idle") {
    DeckHeader(side: .a, state: .idle)
        .frame(width: 720)
        .background(DubColor.surface0)
        .padding()
}

#Preview("Deck A — live Thru") {
    DeckHeader(side: .a, state: .liveThru)
        .frame(width: 720)
        .background(DubColor.surface0)
        .padding()
}

#Preview("Deck B — live Thru") {
    DeckHeader(side: .b, state: .liveThru)
        .frame(width: 720)
        .background(DubColor.surface0)
        .padding()
}
