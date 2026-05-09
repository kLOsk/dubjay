//! Platform audio I/O for Dub.
//!
//! v1 is macOS-only via CoreAudio HAL (PRD §4.2: "v1 uses HAL via
//! coreaudio-rs, not cpal — we want lowest possible latency and direct
//! device control"). Windows ASIO and iOS are explicit follow-ons but
//! already shape this layer's API: anything that platform-specific stays
//! behind the [`AudioOutput`] facade so the engine never knows the host.
//!
//! Concurrency model:
//!
//! - Main thread (or a worker) builds a [`dub_engine::Engine`] off the
//!   audio thread, configures it (loads tracks, sets transport state),
//!   and then hands ownership to [`AudioOutput::start`].
//! - From that point the engine lives on the CoreAudio render thread.
//!   The main thread no longer has a handle to it. To control playback
//!   while it's running we need a lock-free command channel (ringbuf);
//!   that lands when M2 transport requires it. For M1 "play this WAV"
//!   the simpler one-shot API is enough.
//! - Dropping [`AudioOutput`] stops the AudioUnit and reclaims the
//!   engine via the closure's drop, so all RAII cleanup happens
//!   off the audio thread.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
// "CoreAudio", "AudioUnit", "ASIO", etc. are Apple/vendor product names, not
// Rust symbols. Clippy::doc_markdown wants them in backticks; that's wrong
// for English prose — we'd backtick-pollute every doc comment.
#![allow(clippy::doc_markdown)]

mod error;
#[cfg(target_os = "macos")]
mod macos;

pub use error::AudioError;

#[cfg(target_os = "macos")]
pub use macos::{query_default_output, AudioOutput};

/// Library version reported by the crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Information about an audio output device.
#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    /// Sample rate in Hz (e.g. 48000.0).
    pub sample_rate: f32,
    /// Channel count (output channels). Currently always 2.
    pub channels: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!VERSION.is_empty());
    }
}
