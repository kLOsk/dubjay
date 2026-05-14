//
//  Tokens.swift
//  Dub
//
//  M10.3 design tokens — the single source of truth for app colour,
//  typography, spacing, and corner radii. Mirrors the `00 Tokens`
//  page in the Figma exploration sandbox, but **code wins** when the
//  two disagree (see PRD §9 workflow note).
//
//  Why an enum-namespaced constant set instead of `Color(.token)`
//  asset catalogues:
//
//  1. Asset catalogues need Xcode UI to edit; tokens live next to the
//     code that uses them and are diff-friendly.
//  2. We can express derived values (e.g. `DubColor.deckTint(.a)`) as
//     plain functions, which a catalogue can't.
//  3. Snapshot tests (M18) will diff token *values*, not asset names.
//
//  Convention: every name is qualified (`DubColor.surface0`, never
//  bare `surface0`) so reading code requires no import knowledge.
//

import SwiftUI

// MARK: - Colour

/// Dark-mode-only neutral + accent palette.
///
/// Dub is a performance tool used in dim rooms; we don't ship a
/// light mode. `Color` values are `sRGB` with no alpha unless a
/// `*Alpha*` suffix is present.
enum DubColor {

    // ----- Neutrals -------------------------------------------------
    // The "surface" ramp is the workhorse for backgrounds. Each step
    // is ~6 % L* brighter than the previous in OKLCH, so adjacent
    // steps are distinguishable on a calibrated screen but never
    // jarring side-by-side. Picked to avoid the "computer dashboard"
    // greyscale that plagues most DJ apps.

    /// Window background. Slightly warm-black, so the screen reads
    /// as "stage" rather than "spreadsheet."
    static let surface0 = Color(hex: 0x0B0C0F)

    /// Deck header / status strip background.
    static let surface1 = Color(hex: 0x14161B)

    /// Inner panel — FX bar, library frame.
    static let surface2 = Color(hex: 0x1B1E24)

    /// Hover / selected row.
    static let surface3 = Color(hex: 0x252932)

    /// Hairline divider on a `surface0` background.
    static let divider = Color(hex: 0x2B2F38)

    // ----- Text -----------------------------------------------------

    /// Track titles, numeric BPM, primary buttons.
    static let textPrimary = Color(hex: 0xE6E8EC)

    /// Artist, metadata captions, units (`BPM`, `KEY`, `PITCH`).
    static let textSecondary = Color(hex: 0x9099A3)

    /// Tertiary detail (timestamps, format codecs).
    static let textTertiary = Color(hex: 0x6B7079)

    /// Placeholder em-dash content (`—`) before a feature lands.
    static let textPlaceholder = Color(hex: 0x4A4F58)

    // ----- Deck accents --------------------------------------------
    // Deliberately *muted* — the M10.2 waveform palettes carry the
    // chromatic load. Header / chip accents only need to disambiguate
    // deck A vs B at a glance, not compete with the waveform colour.
    // Final accent hue is a M18 polish decision; these are the
    // committed M10.3 starting values.

    static let deckATint = Color(hex: 0xC49157)
    static let deckBTint = Color(hex: 0x5A8088)

    /// Pick a deck's accent by deck index. Asserts in debug for
    /// out-of-range indices so misuse fails loudly instead of
    /// silently falling back to deck A.
    static func deckTint(_ deck: DeckSide) -> Color {
        switch deck {
        case .a: return deckATint
        case .b: return deckBTint
        }
    }

    // ----- State ---------------------------------------------------

    /// "OK / locked / live" indicator.
    static let stateLocked = Color(hex: 0x6FB04A)

    /// "Tentative / searching" indicator.
    static let stateTentative = Color(hex: 0xD9A33A)

    /// Clip / error / destructive.
    static let stateError = Color(hex: 0xD45C5C)

    // ----- Overview strip (M10.5c) ---------------------------------

    /// Deck A's amplitude colour in the Track Overview strip.
    /// Same hue family as `deckATint` but with reduced saturation
    /// so the overview reads as secondary chrome — it must not
    /// compete visually with the playing-waveform palette.
    static let deckAOverview = Color(hex: 0x806341)

    /// Deck B's amplitude colour in the Track Overview strip.
    static let deckBOverview = Color(hex: 0x3F5A60)

    /// Playhead-bracket tint on the Track Overview strip. Bright
    /// neutral so it pops against both deck-tinted amplitude bars
    /// without picking a side.
    static let playheadAccent = Color(hex: 0xF0E8D8)

    /// Returns the per-deck overview bar tint.
    static func deckOverview(_ deck: DeckSide) -> Color {
        switch deck {
        case .a: return deckAOverview
        case .b: return deckBOverview
        }
    }
}

// MARK: - Deck-side handle

/// Tiny enum to keep deck identity type-safe in call sites that
/// don't need the full `UInt64` deck index.
enum DeckSide: Hashable {
    case a
    case b

    var ffiDeckIdx: UInt64 {
        switch self {
        case .a: return 0
        case .b: return 1
        }
    }

    var label: String {
        switch self {
        case .a: return "DECK A"
        case .b: return "DECK B"
        }
    }
}

// MARK: - Typography

/// Type ramp. Sizes are in points (SwiftUI points = 1/72 inch at
/// 1× backing scale). All weights live inside the system rounded
/// stack — we don't bundle a custom font yet (Inter would add ~3 MB
/// to the app + a font-loading dance the M10.3 milestone doesn't
/// justify). Switching to Inter is a one-line change to
/// `DubFont.baseFontName` if the M18 polish pass calls for it.
enum DubFont {

    private static let baseFontDesign: Font.Design = .default

    /// Display — used only for the wordmark in the status strip.
    static let display = Font.system(size: 18, weight: .semibold, design: baseFontDesign)

    /// Track titles in the deck header (`Stakes Is High`).
    static let title = Font.system(size: 16, weight: .semibold, design: baseFontDesign)

    /// Large numeric stat (BPM, pitch %) — distinct face to avoid
    /// confusion with track text.
    static let numericLarge = Font.system(size: 18, weight: .medium, design: .monospaced)

    /// Inline numeric (key, pitch ±).
    static let numericInline = Font.system(size: 13, weight: .medium, design: .monospaced)

    /// Body text — artist, library cells.
    static let body = Font.system(size: 13, weight: .regular, design: baseFontDesign)

    /// All-caps labels (`PITCH`, `BPM`, `KEY`, `DECK A`).
    static let caps = Font.system(size: 10, weight: .semibold, design: baseFontDesign)

    /// Micro caption (format chips, "fingerprint pending").
    static let micro = Font.system(size: 10, weight: .regular, design: baseFontDesign)
}

// MARK: - Spacing scale

/// 4-px base scale. Use the named constants below rather than raw
/// numbers; layout fixes in the M10.2-Figma debugging round were
/// largely caused by mixing 6/8/12 padding inconsistently.
enum DubSpacing {
    static let xs: CGFloat = 4
    static let sm: CGFloat = 8
    static let md: CGFloat = 12
    static let lg: CGFloat = 16
    static let xl: CGFloat = 24
    static let xxl: CGFloat = 32
}

// MARK: - Corner radii

enum DubRadius {
    /// Pills, chips, keycaps.
    static let pill: CGFloat = 999

    /// Inner panel (FX module box, library row).
    static let panel: CGFloat = 6

    /// Outer card (whole deck header background, FX bar background).
    static let card: CGFloat = 10
}

// MARK: - Layout constants

/// Performance View major regions, per PRD §9.2. Sized so the demo
/// at 1440×900 matches the Figma reference; the layout flexes with
/// the window thanks to SwiftUI auto-layout, but these are the
/// "natural" heights everything is balanced around.
enum DubLayout {
    static let statusStripHeight: CGFloat = 28
    static let deckHeaderHeight: CGFloat = 92
    static let fxBarHeight: CGFloat = 100
    static let libraryMinHeight: CGFloat = 200
    static let waveformMinHeight: CGFloat = 280

    /// Width of the vertical playing-waveform column in Performance
    /// (Timecode) mode. Sized to match Serato Scratch Live's
    /// playing-waveform height (≈ 110–140 px in its horizontal
    /// layout) — translated into our bottom-→-top vertical
    /// orientation, the *width* of each deck's strip is the Serato-
    /// equivalent dimension. The remaining horizontal space inside
    /// the deck pane is reserved for the deck header (already
    /// rendered above the strip), the M10.5c per-deck Track-
    /// Overview waveform, and future per-deck info chips (track
    /// time, RPM, key-lock, beatgrid offset).
    static let deckColumnWidth: CGFloat = 80

    /// Height of the horizontal playing-waveform strip in Prep
    /// mode. ≈ half the vertical-mode `waveformMinHeight`, sized
    /// so the strip is tall enough to read transient envelopes
    /// comfortably but short enough that the surrounding region
    /// has room for the M10.5c Track-Overview waveform + cue
    /// markers + beatgrid affordances that ship alongside.
    static let waveformPrepHeight: CGFloat = 140

    /// Height of the horizontal Track-Overview band in Prep mode.
    /// The same ratio to `waveformPrepHeight` (≈ 0.45) that
    /// `deckOverviewWidth` (≈ 36 px) has to `deckColumnWidth`
    /// (80 px) in Performance mode, so the overview reads as the
    /// secondary chrome it is rather than dominating the strip.
    static let deckOverviewHeight: CGFloat = 60

    /// Width of the horizontal playing-waveform in Prep mode (M10.5c).
    /// Prep mode rotates the strip 90° — it's a single horizontal
    /// band across the top of the single-deck surface, sized
    /// generously so a track-prep DJ can read beat-grids and place
    /// hot cues comfortably, but bounded so the surrounding region
    /// has room for the (forthcoming) horizontal overview + cue
    /// strip + waveform-zoom controls. Read as **height** in Prep
    /// mode, since the strip runs left-to-right there.
    static let deckColumnWidthPrep: CGFloat = 280

    /// Width of the per-deck Track Overview strip (M10.5c) — the
    /// thin vertical waveform on each deck's *outside* edge
    /// showing the whole track top→bottom with a playhead bracket
    /// at the current position. PRD §9.6.1: ≈ 36 px wide. Click-
    /// to-jump per §6.1 (File mode always; Timecode gated on
    /// Panic Play in M10.6).
    static let deckOverviewWidth: CGFloat = 36

    /// Horizontal padding between the overview strip and the
    /// playing-waveform column. Just enough breathing room for the
    /// playhead bracket on the overview to not collide visually
    /// with the playing strip's edge.
    static let deckOverviewGap: CGFloat = 12
}

// MARK: - Color hex initialiser

extension Color {

    /// Construct a `Color` from a 24-bit RGB hex literal, e.g.
    /// `Color(hex: 0x14161B)`. Bytes are interpreted in the sRGB
    /// colour space.
    ///
    /// We use this rather than `Color(red:green:blue:)` so the
    /// token table reads as the same hex strings designers ship.
    init(hex: UInt32, opacity: Double = 1.0) {
        let r = Double((hex >> 16) & 0xFF) / 255.0
        let g = Double((hex >> 8) & 0xFF) / 255.0
        let b = Double(hex & 0xFF) / 255.0
        self.init(.sRGB, red: r, green: g, blue: b, opacity: opacity)
    }
}
