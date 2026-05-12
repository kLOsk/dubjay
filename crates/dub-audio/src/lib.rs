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
pub use macos::{
    has_external_audio_interface, list_input_devices, query_default_input, query_default_output,
    AudioInput, AudioOutput, BufferFrameRange, InputDeviceInfo, InputOptions, OutputOptions,
};

/// Library version reported by the crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Information about an audio output device.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// CoreAudio device name (e.g. `"SL 3"`, `"Traktor Audio 6"`,
    /// `"MacBook Pro Speakers"`). Used by the CLI's known-device
    /// table (M5.5.2) to decide on per-deck output channel routing.
    pub device_name: String,
    /// Sample rate in Hz (e.g. 48000.0).
    pub sample_rate: f32,
    /// Physical output-channel count of this device (6 for an SL3,
    /// 4 for a Traktor Audio 6, 2 for a MacBook's built-in
    /// speakers). Up to M5.5.1 this was hardcoded to 2 because
    /// `AudioOutput::start` only opened a stereo AU; M5.5.2 surfaces
    /// the real count so the CLI can route deck audio to the right
    /// physical pair.
    pub channels: u32,
    /// Current device buffer size, in frames per render callback.
    pub buffer_frames: u32,
    /// Allowed buffer-size range for this device (frames).
    #[cfg(target_os = "macos")]
    pub buffer_frame_range: BufferFrameRange,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!VERSION.is_empty());
    }
}
