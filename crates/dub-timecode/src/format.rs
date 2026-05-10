//! Per-vendor timecode-format constants.
//!
//! Sources:
//! - xwax/timecoder.c (BSD-licensed reference implementation; spec only —
//!   no code derived). Confirms CV02 carrier/resolution.
//! - Mixxx DVS internals series (mixxx.org/news/2021-2025) for the
//!   modulation scheme description.
//!
//! Numeric values here are the *nominal* parameters of the printed
//! signal on the vinyl. Real cartridges/turntables introduce ~1–3% RPM
//! drift and ±0.5 dB amplitude variation; the decoder absorbs both
//! through coherent block averaging (see `decoder.rs`).
//!
//! v1 ships Serato CV02 only (M5.1). Traktor MK2 is on the table for
//! M6 — the [`Format`] enum already enumerates it so the API doesn't
//! break when MK2 lands.

/// Supported timecode formats.
///
/// As of M6, all three relative-mode formats are decoded by the same
/// algorithm — only the nominal carrier frequency differs. Absolute-
/// mode (the bitstream riding on top of each carrier) still needs the
/// per-format position-bit table; deferred to a future v1.x milestone
/// since real-DJ scratching only needs relative mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Serato Control Vinyl, 2nd edition (the only edition the
    /// Mixxx/xwax community recommends — earlier pressings have
    /// signal coverage issues toward the inner groove).
    SeratoCv02,

    /// Traktor "MK1" timecode vinyl (= the original "Traktor Scratch"
    /// pressing, the one shipped with Final Scratch / Traktor Scratch
    /// v1, ca. 2005). Stereo quadrature carrier at 2 kHz with AM
    /// modulation — the same modulation family as Serato CV02, just
    /// at twice the carrier frequency. xwax calls this `traktor_a`
    /// in its definition table.
    ///
    /// Despite "MK1" being a retroactive name (NI never officially
    /// branded it that way; it became "MK1" only after MK2 shipped),
    /// it's still in widespread circulation among scratch DJs because
    /// the cartridges, the rigs, and the records last decades.
    TraktorMk1,

    /// Traktor "MK2" timecode vinyl (= "Traktor Scratch MK2",
    /// shipped ca. 2008, the current-production NI timecode pressing).
    /// Stereo carrier at **2.5 kHz** (note: not 2 kHz — that's MK1)
    /// with a non-standard *offset modulation* technique: the
    /// modulation rides as a vertical DC offset of the signal rather
    /// than as amplitude changes. The cartridge/preamp AC-couples
    /// the offset out before it reaches us, which is why our
    /// relative-mode decoder works on both AM and offset-modulated
    /// carriers without per-format branches — only `carrier_hz`
    /// changes.
    ///
    /// Verified empirically by the Mixxx community in 2013 (xwax
    /// bug 923389): the MK2 carrier is functionally a 2.5 kHz sine,
    /// quadrature stereo, and decodes through the standard CV02-
    /// style algorithm once the carrier frequency is right.
    /// Absolute-mode would need reverse-engineering of the offset-
    /// bitstream — out of scope for v1.
    TraktorMk2,
}

impl Format {
    /// Nominal carrier frequency in Hz. The decoder uses this to
    /// normalize the measured instantaneous frequency into a `rate`
    /// (`measured / nominal == 1.0` at unity playback).
    ///
    /// - Serato CV02: 1000 Hz (xwax `serato_2a`)
    /// - Traktor MK1: 2000 Hz (xwax `traktor_a`)
    /// - Traktor MK2: 2500 Hz (verified empirically, see enum docs)
    ///
    /// Why Serato chose 1 kHz while NI doubled it: at high stylus
    /// velocities the cartridge's high-frequency response matters
    /// more than encoding density; 1 kHz keeps resolution sub-ms
    /// while staying clear of the cartridge's resonance tail. NI
    /// traded that headroom for higher position resolution at
    /// normal speeds. Both are fine for relative mode; the higher
    /// carrier just costs us alias-band ceiling
    /// (`Nyquist / carrier`-fold at 48 kHz: 24× for Serato, 12× for
    /// MK1, 9.6× for MK2 — all comfortably above real scratching).
    #[must_use]
    pub fn carrier_hz(self) -> f32 {
        match self {
            Self::SeratoCv02 => 1000.0,
            Self::TraktorMk1 => 2000.0,
            Self::TraktorMk2 => 2500.0,
        }
    }

    /// Position-code length in bits. The full code is repeated linearly
    /// down the record; one full period spans `bits / carrier_hz`
    /// seconds. v1 doesn't use this (relative mode only) but exposing
    /// it keeps the type honest about future absolute-mode work.
    ///
    /// MK1 uses a 23-bit code (xwax `traktor_a.bits`); MK2's bit count
    /// is unknown publicly — the offset modulation hasn't been fully
    /// reverse-engineered. Pinned to 23 here as a placeholder that
    /// matches MK1 until someone validates MK2's bitstream.
    #[must_use]
    pub fn position_bits(self) -> u32 {
        match self {
            Self::SeratoCv02 => 20,
            Self::TraktorMk1 | Self::TraktorMk2 => 23,
        }
    }

    /// Total length of side A in bits. ~712,000 bits for Serato 2nd Ed.
    /// at 1 kbit/s = ~712 seconds = ~12 minutes of usable signal.
    #[must_use]
    pub fn side_a_bits(self) -> u32 {
        match self {
            Self::SeratoCv02 => 712_000,
            // Both Traktor generations are pressed on the same blank
            // (~12 minutes side A); the modulation differs but the
            // physical groove length is the same. Merged arm so a
            // future Traktor generation that *does* differ in length
            // shows up as a deliberate split, not a copy-paste.
            Self::TraktorMk1 | Self::TraktorMk2 => 1_500_000,
        }
    }

    /// Parse a CLI `--format` argument string. Returns the matched
    /// [`Format`] or `None` for unknown input.
    ///
    /// Accepted aliases (case-sensitive — keep CLI vocabulary
    /// predictable):
    ///
    /// - `serato-cv02`, `serato`, `cv02` → [`Format::SeratoCv02`]
    /// - `traktor-mk1`, `mk1` → [`Format::TraktorMk1`]
    /// - `traktor-mk2`, `mk2` → [`Format::TraktorMk2`]
    ///
    /// Note that the bare alias `traktor` is **deliberately not
    /// supported**: MK1 and MK2 are different carrier frequencies
    /// (2 kHz vs 2.5 kHz), so a user who types `--format traktor`
    /// without specifying which generation would get silent
    /// mis-routing — playback at 80% speed (MK2 vinyl decoded
    /// against an MK1 carrier nominal) is exactly the kind of
    /// "Dub is broken!" mystery we want to avoid. Force the user
    /// to know which record they own.
    ///
    /// Centralised here so every CLI subcommand (`scope`,
    /// `calibrate`, `timecode-deck`, `decode-timecode`) speaks the
    /// same vocabulary — adding a future format only requires one
    /// edit, and the test below pins every alias.
    #[must_use]
    pub fn from_cli_arg(s: &str) -> Option<Self> {
        match s {
            "serato-cv02" | "serato" | "cv02" => Some(Self::SeratoCv02),
            "traktor-mk1" | "mk1" => Some(Self::TraktorMk1),
            "traktor-mk2" | "mk2" => Some(Self::TraktorMk2),
            _ => None,
        }
    }

    /// Canonical CLI string for this format. Inverse of
    /// [`Self::from_cli_arg`] — `Format::from_cli_arg(f.cli_name())
    /// == Some(f)` for every format. Used by the calibration JSON
    /// to round-trip the format key.
    #[must_use]
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::SeratoCv02 => "serato-cv02",
            Self::TraktorMk1 => "traktor-mk1",
            Self::TraktorMk2 => "traktor-mk2",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carriers_distinct() {
        // Each supported format must have a unique carrier so the
        // decoder's rate-normalisation is unambiguous. Pin the
        // pairwise differences explicitly: a future format added at
        // an existing carrier (e.g. an MK3 at 2 kHz again) would
        // need a different distinguishing feature — this test
        // forces that conversation at PR time, not at run-time.
        let formats = [Format::SeratoCv02, Format::TraktorMk1, Format::TraktorMk2];
        for (i, a) in formats.iter().enumerate() {
            for b in &formats[i + 1..] {
                assert!(
                    (a.carrier_hz() - b.carrier_hz()).abs() > 1.0,
                    "{a:?} and {b:?} share carrier {}",
                    a.carrier_hz()
                );
            }
        }
    }

    #[test]
    fn traktor_mk1_is_2khz_mk2_is_25khz() {
        // Pin the empirically-verified carrier values so a typo (e.g.
        // accidentally setting MK2 to 2 kHz, which would silently
        // play MK2 vinyl back at ~80% speed) is caught at test time.
        // See enum docstrings for sources.
        assert!((Format::TraktorMk1.carrier_hz() - 2000.0).abs() < f32::EPSILON);
        assert!((Format::TraktorMk2.carrier_hz() - 2500.0).abs() < f32::EPSILON);
    }

    #[test]
    fn position_bits_sane() {
        for f in [Format::SeratoCv02, Format::TraktorMk1, Format::TraktorMk2] {
            assert!(f.position_bits() >= 16 && f.position_bits() <= 32);
        }
    }

    #[test]
    fn cli_arg_round_trips_every_format() {
        // Every variant must round-trip through cli_name → from_cli_arg
        // so the CLI vocabulary stays consistent with the calibration
        // JSON (which uses cli_name as the on-disk key).
        for f in [Format::SeratoCv02, Format::TraktorMk1, Format::TraktorMk2] {
            let parsed = Format::from_cli_arg(f.cli_name())
                .expect("cli_name → from_cli_arg must round-trip");
            assert_eq!(parsed, f, "{} did not round-trip", f.cli_name());
        }
    }

    #[test]
    fn cli_arg_accepts_aliases() {
        assert_eq!(
            Format::from_cli_arg("serato"),
            Some(Format::SeratoCv02),
            "'serato' is the friendly alias (only one Serato format supported)"
        );
        assert_eq!(Format::from_cli_arg("cv02"), Some(Format::SeratoCv02));
        assert_eq!(Format::from_cli_arg("mk1"), Some(Format::TraktorMk1));
        assert_eq!(Format::from_cli_arg("mk2"), Some(Format::TraktorMk2));
    }

    #[test]
    fn bare_traktor_is_rejected_to_avoid_ambiguity() {
        // The bare alias `traktor` would have to pick MK1 or MK2,
        // and a wrong pick silently plays back at the wrong speed
        // (MK2 vinyl decoded against an MK1 carrier = ~80% rate).
        // Force the user to be explicit. See docstring on
        // [`Format::from_cli_arg`].
        assert_eq!(Format::from_cli_arg("traktor"), None);
    }

    #[test]
    fn cli_arg_rejects_unknown() {
        assert_eq!(Format::from_cli_arg("rekordbox"), None);
        assert_eq!(Format::from_cli_arg(""), None);
        assert_eq!(
            Format::from_cli_arg("Serato"),
            None,
            "case-sensitive: SHOUTING is not a real DJ format"
        );
    }

    #[test]
    fn side_a_at_least_5_minutes() {
        // Sanity: every supported format gives ≥ 5 min of usable signal.
        // Side A bits / carrier_hz = duration in seconds at unity speed.
        for f in [Format::SeratoCv02, Format::TraktorMk1, Format::TraktorMk2] {
            let secs = f64::from(f.side_a_bits()) / f64::from(f.carrier_hz());
            assert!(secs >= 300.0, "{f:?} side-A only {secs:.0}s");
        }
    }
}
