//! Timecode-vinyl decoder for Dub.
//!
//! Supports **Serato CV02**, **Traktor MK1** and **Traktor MK2** in
//! relative-mode-only (PRD §5.4 / §6). All three share the same
//! decode algorithm — only the nominal carrier frequency differs
//! per format (1 / 2 / 2.5 kHz respectively). Absolute-mode decoding
//! is deferred to a future v1.x milestone.
//!
//! Pipeline:
//!
//! ```text
//!  AudioInput  ──► Decoder.process(stereo_block)  ──► DecodeOutput
//!  (M5.2)            (this crate, M5.1)                 │
//!                                                       ▼
//!                                                Engine deck transport
//!                                                  (rate, position)
//! ```
//!
//! [`signal::Generator`] produces synthetic timecode for tests and
//! offline diagnostics. [`decoder::Decoder`] consumes stereo audio
//! and emits per-block rate/position/amplitude/confidence — see the
//! algorithm note in `decoder.rs`.
//!
//! v1 design choice: relative mode only. We track *changes* in
//! position via the carrier phase, not absolute groove location. The
//! upside is that the decoder needs no AM-bitstream demodulation, no
//! 20-bit position lookup table, and no per-record calibration — a
//! drastically simpler v1 surface that nonetheless covers every
//! scratch DJ use case (PRD §5.4: "absolute mode is for digital
//! mixers we don't target in v1").
//!
//! License note: this is a **clean-room implementation** of the
//! published timecode-vinyl format documented by xwax (BSD) and the
//! Mixxx project. No xwax code is copied or derived — we re-implement
//! from the algorithm description because dub is GPL-3.0 and we want
//! attribution to remain unambiguous. See `format.rs` for the source
//! list.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
// Vendor product names ("Serato", "Traktor", "CV02", "MK2") are not
// Rust symbols; clippy::doc_markdown is wrong to demand backticks.
#![allow(clippy::doc_markdown)]

mod decoder;
mod format;
pub mod signal;

pub use decoder::{DecodeOutput, Decoder};
pub use format::Format;

/// Library version reported by the crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!VERSION.is_empty());
    }
}
