//! Error type for the audio I/O layer.

/// Errors returned from the audio output layer.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// CoreAudio (or the underlying platform API) returned an error during
    /// device open, configuration, or callback installation. The `String`
    /// preserves the platform's diagnostic; we don't need to round-trip
    /// the structured `coreaudio::Error` to callers (they can't act on it).
    #[error("audio device: {0}")]
    Device(String),

    /// The host platform is not supported in this build (only macOS is
    /// supported in v1).
    #[error("audio output not supported on this platform")]
    UnsupportedPlatform,

    /// The engine's expected sample rate could not be matched to the
    /// device. The engine and the device must agree on rate; resampling
    /// at the device boundary is deliberately not done in v1 (see PRD §4.1).
    #[error("sample rate mismatch: engine wants {engine_sr} Hz, device offered {device_sr} Hz")]
    SampleRateMismatch {
        /// Sample rate the engine was configured at.
        engine_sr: f32,
        /// Sample rate the device exposes.
        device_sr: f32,
    },
}

#[cfg(target_os = "macos")]
impl From<coreaudio::Error> for AudioError {
    fn from(e: coreaudio::Error) -> Self {
        Self::Device(e.to_string())
    }
}
