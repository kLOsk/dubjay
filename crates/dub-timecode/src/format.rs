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
/// In v1 only [`Format::SeratoCv02`] is fully decoded. [`Format::TraktorMk2`]
/// is enumerated so callers can write format-agnostic code, but the
/// decoder will reject it until M6 wires up the 23-bit position table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Serato Control Vinyl, 2nd edition (the only edition the
    /// Mixxx/xwax community recommends — earlier pressings have
    /// signal coverage issues toward the inner groove).
    SeratoCv02,

    /// Traktor MK2 timecode vinyl (Native Instruments).
    TraktorMk2,
}

impl Format {
    /// Carrier frequency in Hz. Per-vendor:
    ///
    /// - Serato CV02: 1000 Hz
    /// - Traktor MK2: 2000 Hz
    ///
    /// xwax's documentation explains why Serato chose the lower carrier:
    /// at high stylus velocities the cartridge's high-frequency response
    /// matters more than encoding density; 1 kHz still gives sub-ms
    /// position resolution while staying clear of the cartridge's
    /// resonance tail.
    #[must_use]
    pub fn carrier_hz(self) -> f32 {
        match self {
            Self::SeratoCv02 => 1000.0,
            Self::TraktorMk2 => 2000.0,
        }
    }

    /// Position-code length in bits. The full code is repeated linearly
    /// down the record; one full period spans `bits / carrier_hz`
    /// seconds. v1 doesn't use this (relative mode only) but exposing
    /// it keeps the type honest about future M6 work.
    #[must_use]
    pub fn position_bits(self) -> u32 {
        match self {
            Self::SeratoCv02 => 20,
            Self::TraktorMk2 => 23,
        }
    }

    /// Total length of side A in bits. ~712,000 bits for Serato 2nd Ed.
    /// at 1 kbit/s = ~712 seconds = ~12 minutes of usable signal.
    #[must_use]
    pub fn side_a_bits(self) -> u32 {
        match self {
            Self::SeratoCv02 => 712_000,
            Self::TraktorMk2 => 1_500_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carriers_distinct() {
        assert!((Format::SeratoCv02.carrier_hz() - Format::TraktorMk2.carrier_hz()).abs() > 1.0);
    }

    #[test]
    fn position_bits_sane() {
        for f in [Format::SeratoCv02, Format::TraktorMk2] {
            assert!(f.position_bits() >= 16 && f.position_bits() <= 32);
        }
    }

    #[test]
    fn side_a_at_least_5_minutes() {
        // Sanity: every supported format gives ≥ 5 min of usable signal.
        // Side A bits / carrier_hz = duration in seconds at unity speed.
        for f in [Format::SeratoCv02, Format::TraktorMk2] {
            let secs = f64::from(f.side_a_bits()) / f64::from(f.carrier_hz());
            assert!(secs >= 300.0, "{f:?} side-A only {secs:.0}s");
        }
    }
}
