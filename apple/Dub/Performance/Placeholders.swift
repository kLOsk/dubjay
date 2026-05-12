//
//  Placeholders.swift
//  Dub
//
//  M10.3 placeholders for regions that future milestones will
//  populate with real content:
//
//  * `FXBarPlaceholder`   — split per-deck, lit by M15 (Echo-Out) /
//                            M16 (Dub Siren) / M17 (Sampler + Quick
//                            Scratch).
//  * `LibraryPlaceholder` — lit by M11 (Serato import).
//
//  Each placeholder is a visible-but-honest dim block with a tiny
//  caption naming the milestone that owes it real content. The
//  goal is to make "M15 hasn't shipped yet" obvious to anyone
//  looking at the running app, and to make the layout sizes wrong
//  *now* if they would be wrong later.
//

import SwiftUI

/// Two-column FX bar placeholder — one column per deck — sized to
/// `DubLayout.fxBarHeight`. The real implementation arrives across
/// M15 / M16 / M17; until then we show the slot allocation so the
/// outer layout is finalised.
struct FXBarPlaceholder: View {
    var body: some View {
        HStack(spacing: 1) {
            deckColumn(.a)
            deckColumn(.b)
        }
        .frame(height: DubLayout.fxBarHeight)
        .background(DubColor.divider)
    }

    @ViewBuilder
    private func deckColumn(_ side: DeckSide) -> some View {
        HStack(spacing: DubSpacing.md) {
            modulePlaceholder("ECHO-OUT", milestone: "M15")
            modulePlaceholder("DUB SIREN", milestone: "M16")
            scratchPlaceholder(side: side)
            samplerPlaceholder(side: side)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, DubSpacing.lg)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(DubColor.surface2)
    }

    @ViewBuilder
    private func modulePlaceholder(_ label: String, milestone: String) -> some View {
        VStack(alignment: .leading, spacing: DubSpacing.xs) {
            Text(label)
                .font(DubFont.caps)
                .tracking(0.8)
                .foregroundStyle(DubColor.textSecondary)
            Text(milestone)
                .font(DubFont.micro)
                .foregroundStyle(DubColor.textPlaceholder)
        }
        .padding(.horizontal, DubSpacing.md)
        .padding(.vertical, DubSpacing.sm)
        .frame(width: 96, height: 56, alignment: .topLeading)
        .background(DubColor.surface1)
        .clipShape(RoundedRectangle(cornerRadius: DubRadius.panel, style: .continuous))
    }

    @ViewBuilder
    private func scratchPlaceholder(side: DeckSide) -> some View {
        // Q/W for A, E/R for B (matches the Figma exploration and
        // PRD §7 Quick Scratch keymap intent — final binding lives
        // with M17).
        let keys = (side == .a) ? ["Q", "W"] : ["E", "R"]
        keyCapGroup(label: "SCRATCH", keys: keys, milestone: "M17")
    }

    @ViewBuilder
    private func samplerPlaceholder(side: DeckSide) -> some View {
        let keys = (side == .a) ? ["A", "S"] : ["D", "F"]
        keyCapGroup(label: "SAMPLER", keys: keys, milestone: "M17")
    }

    @ViewBuilder
    private func keyCapGroup(label: String, keys: [String], milestone: String) -> some View {
        VStack(alignment: .leading, spacing: DubSpacing.xs) {
            HStack(spacing: DubSpacing.xs) {
                Text(label)
                    .font(DubFont.caps)
                    .tracking(0.8)
                    .foregroundStyle(DubColor.textSecondary)
                Text(milestone)
                    .font(DubFont.micro)
                    .foregroundStyle(DubColor.textPlaceholder)
            }
            HStack(spacing: DubSpacing.xs) {
                ForEach(keys, id: \.self) { k in keyCap(k) }
            }
        }
        .padding(.horizontal, DubSpacing.sm)
        .padding(.vertical, DubSpacing.xs)
    }

    @ViewBuilder
    private func keyCap(_ glyph: String) -> some View {
        Text(glyph)
            .font(.system(size: 12, weight: .semibold, design: .monospaced))
            .foregroundStyle(DubColor.textTertiary)
            .frame(width: 28, height: 28)
            .background(DubColor.surface1)
            .clipShape(RoundedRectangle(cornerRadius: 4, style: .continuous))
    }
}

/// Library / browser placeholder. Single block with a centred
/// caption.
struct LibraryPlaceholder: View {
    var body: some View {
        ZStack {
            DubColor.surface1
            VStack(spacing: DubSpacing.sm) {
                Text("LIBRARY")
                    .font(DubFont.caps)
                    .tracking(1.2)
                    .foregroundStyle(DubColor.textSecondary)
                Text("Lit by M11 — Serato library import")
                    .font(DubFont.body)
                    .foregroundStyle(DubColor.textPlaceholder)
            }
        }
        .frame(minHeight: DubLayout.libraryMinHeight)
    }
}

#Preview("FX bar") {
    FXBarPlaceholder()
        .frame(width: 1440)
}

#Preview("Library") {
    LibraryPlaceholder()
        .frame(width: 1440, height: 240)
}
