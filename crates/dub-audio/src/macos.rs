//! macOS CoreAudio output via Default Output AudioUnit.
//!
//! The Default Output AudioUnit is the simplest path to "play sound out of
//! the user's speakers". It tracks the user's selected output device
//! automatically (System Settings → Sound → Output) without us having to
//! enumerate devices. For v1 of Dub that's exactly what we want; per-device
//! selection lands when we add the audio settings UI (M11+).
//!
//! Latency: the IO buffer size on macOS defaults to ~256–512 frames, which
//! at 48 kHz is 5–11 ms. The PRD targets < 8 ms; we tighten this with
//! `kAudioDevicePropertyBufferFrameSize` once the engine is stable. M1.4
//! verifies the path works end-to-end at the device's default buffer size.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use coreaudio::audio_unit::audio_format::LinearPcmFlags;
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::stream_format::StreamFormat;
use coreaudio::audio_unit::{AudioUnit, Element, IOType, SampleFormat, Scope};

use dub_engine::{Engine, RealtimeContext};

use crate::{AudioError, DeviceInfo};

/// Query the system's current default output device for its sample rate
/// and channel count, without committing to playback.
///
/// Use this to size the [`Engine`] correctly before calling
/// [`AudioOutput::start`].
///
/// # Errors
///
/// Returns [`AudioError::Device`] if the audio unit cannot be opened or its
/// properties cannot be queried.
pub fn query_default_output() -> Result<DeviceInfo, AudioError> {
    let audio_unit = AudioUnit::new(IOType::DefaultOutput)?;
    let sample_rate = audio_unit.sample_rate()?;
    // The Default Output AudioUnit is configurable to our format; we query
    // here just so the caller has a sensible default if they want to match
    // the device. We always render stereo regardless.
    Ok(DeviceInfo {
        #[allow(clippy::cast_possible_truncation)]
        sample_rate: sample_rate as f32,
        channels: 2,
    })
}

/// A live audio output: drives the engine's render method from CoreAudio's
/// real-time thread until dropped.
///
/// The engine is owned exclusively by the render callback for the lifetime
/// of this `AudioOutput`. There's no cross-thread access. To control
/// playback while it's running, send commands via a lock-free channel
/// (TBD; lands with M2 transport).
pub struct AudioOutput {
    audio_unit: AudioUnit,
    callback_count: Arc<AtomicU64>,
    sample_rate: f32,
}

impl AudioOutput {
    /// Open the default output device, configure it for interleaved f32
    /// stereo at the engine's sample rate, install a render callback that
    /// drives `engine.render`, and start playback.
    ///
    /// The engine is moved into the render closure. Stop playback by
    /// dropping the returned [`AudioOutput`]; this stops the AudioUnit
    /// and drops the engine off the audio thread.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError`] if the device cannot be opened, the stream
    /// format cannot be set, or the callback cannot be installed.
    pub fn start(engine: Engine) -> Result<Self, AudioError> {
        let mut audio_unit = AudioUnit::new(IOType::DefaultOutput)?;

        // Force interleaved f32 stereo at the engine's sample rate. The
        // Default Output AudioUnit will SRC internally if the device is
        // running at a different rate; PRD §4.1.5 calls for the engine
        // matching the device, so the caller is expected to have built
        // the engine at the device's rate (use `query_default_output`).
        let engine_sr = engine.sample_rate();
        let format = StreamFormat {
            sample_rate: f64::from(engine_sr),
            sample_format: SampleFormat::F32,
            flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
            channels: 2,
        };
        audio_unit.set_stream_format(format, Scope::Input, Element::Output)?;

        let callback_count = Arc::new(AtomicU64::new(0));
        let cb_count = callback_count.clone();

        // The engine moves into the closure. From this point on it lives
        // on the audio thread; the main thread cannot touch it directly.
        let mut engine = engine;
        let mut rt = RealtimeContext::new();

        audio_unit.set_render_callback(
            move |args: render_callback::Args<data::Interleaved<f32>>| {
                // RT thread. No allocation, no locks, no syscalls.
                // engine.render is verified alloc-free by tests; the
                // AtomicU64::fetch_add is wait-free.
                engine.render(&mut rt, args.data.buffer);
                cb_count.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )?;

        audio_unit.start()?;

        Ok(Self {
            audio_unit,
            callback_count,
            sample_rate: engine_sr,
        })
    }

    /// The engine's configured sample rate (matches the AudioUnit's stream rate).
    #[must_use]
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Number of times the render callback has fired since `start`.
    /// Useful for tests and diagnostics.
    #[must_use]
    pub fn callback_count(&self) -> u64 {
        self.callback_count.load(Ordering::Relaxed)
    }

    /// Stop the AudioUnit explicitly. `Drop` does this too; calling stop
    /// directly is useful when you want to deterministically wait for the
    /// last callback to finish before tearing down the parent struct.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Device`] if CoreAudio refuses to stop.
    pub fn stop(&mut self) -> Result<(), AudioError> {
        self.audio_unit.stop()?;
        Ok(())
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        // Best-effort stop. If CoreAudio errors here it's already on its
        // way out; there's no useful recovery action and we mustn't panic
        // in Drop.
        let _ = self.audio_unit.stop();
    }
}
