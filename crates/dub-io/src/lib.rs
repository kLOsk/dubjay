//! Audio file I/O for Dub.
//!
//! Per PRD §4.4, tracks are decoded fully into RAM on load to support
//! sample-accurate, bidirectional playback. Forward and backward are
//! byte-for-byte symmetric in the engine. No per-block disk streaming.
//!
//! M1 ships WAV/PCM via symphonia. Other formats (MP3, FLAC, AIFF, ALAC,
//! AAC) land in subsequent milestones, gated by their respective symphonia
//! feature flags in the workspace `Cargo.toml`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

mod track;

pub use track::{read_metadata, LoadError, Track, TrackMetadata};

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
