//! macOS CoreAudio output via Default Output AudioUnit.
//!
//! The Default Output AudioUnit is the simplest path to "play sound out of
//! the user's speakers". It tracks the user's selected output device
//! automatically (System Settings → Sound → Output) without us having to
//! enumerate devices. For v1 of Dub that's exactly what we want; per-device
//! selection lands when we add the audio settings UI (M11+).
//!
//! Latency: macOS lets us request a device buffer size via
//! `kAudioDevicePropertyBufferFrameSize`. The PRD targets <8 ms one-way; a
//! 256-frame buffer at 48 kHz gives ~5.3 ms, 128 gives ~2.7 ms. The device
//! always clamps to its min/max, and we read back the actual value.

use std::ffi::c_void;
use std::mem;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use coreaudio::audio_unit::audio_format::LinearPcmFlags;
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::stream_format::StreamFormat;
use coreaudio::audio_unit::{AudioUnit, Element, IOType, SampleFormat, Scope};
use objc2_audio_toolbox::kAudioOutputUnitProperty_CurrentDevice;
use objc2_core_audio::{
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyBufferFrameSizeRange,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, AudioObjectGetPropertyData,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectSetPropertyData,
};
use objc2_core_audio_types::AudioValueRange;

use dub_engine::{Engine, RealtimeContext};

use crate::{AudioError, DeviceInfo};

/// Information about a device's allowed buffer-frame-size range.
#[derive(Debug, Clone, Copy)]
pub struct BufferFrameRange {
    /// Smallest buffer the device permits (frames).
    pub min: u32,
    /// Largest buffer the device permits (frames).
    pub max: u32,
}

/// Query the system's current default output device for its sample rate,
/// channel count, current buffer-frame size, and allowed buffer range,
/// without committing to playback.
///
/// Use this to size the [`Engine`] correctly before calling
/// [`AudioOutput::start`], and to inform the user of latency tradeoffs.
///
/// # Errors
///
/// Returns [`AudioError::Device`] if the audio unit cannot be opened or
/// any property cannot be queried.
pub fn query_default_output() -> Result<DeviceInfo, AudioError> {
    let audio_unit = AudioUnit::new(IOType::DefaultOutput)?;
    let sample_rate_f64 = audio_unit.sample_rate()?;
    let device_id = device_id_from_audio_unit(&audio_unit)?;
    let buffer_frames = get_buffer_frame_size(device_id)?;
    let range = get_buffer_frame_size_range(device_id)?;

    Ok(DeviceInfo {
        #[allow(clippy::cast_possible_truncation)]
        sample_rate: sample_rate_f64 as f32,
        channels: 2,
        buffer_frames,
        buffer_frame_range: range,
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
    buffer_frames: u32,
}

impl AudioOutput {
    /// Open the default output device with the device's current buffer size,
    /// configure it for interleaved f32 stereo at the engine's sample rate,
    /// install a render callback, and start playback.
    ///
    /// Convenience for callers who don't care about buffer size. Equivalent
    /// to `start_with_buffer_size(engine, None)`.
    ///
    /// # Errors
    ///
    /// See [`AudioOutput::start_with_buffer_size`].
    pub fn start(engine: Engine) -> Result<Self, AudioError> {
        Self::start_with_buffer_size(engine, None)
    }

    /// Open the default output, optionally request a specific buffer size,
    /// and start playback.
    ///
    /// `requested_buffer_frames`:
    ///
    /// - `None`: leave the device at its current buffer size.
    /// - `Some(n)`: ask the device to use `n` frames. The device clamps to
    ///   its own min/max range; check [`AudioOutput::buffer_frames`] for
    ///   the value that was actually applied.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError`] if the device cannot be opened, the stream
    /// format cannot be set, the buffer size cannot be applied, or the
    /// callback cannot be installed.
    pub fn start_with_buffer_size(
        engine: Engine,
        requested_buffer_frames: Option<u32>,
    ) -> Result<Self, AudioError> {
        let mut audio_unit = AudioUnit::new(IOType::DefaultOutput)?;

        // Force interleaved f32 stereo at the engine's sample rate. Per
        // PRD §4.1.5 the engine matches the device, so callers should
        // build the engine at `query_default_output().sample_rate`.
        let engine_sr = engine.sample_rate();
        let format = StreamFormat {
            sample_rate: f64::from(engine_sr),
            sample_format: SampleFormat::F32,
            flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
            channels: 2,
        };
        audio_unit.set_stream_format(format, Scope::Input, Element::Output)?;

        // Apply requested buffer size *before* installing the callback so
        // the first callback already runs at the new size. The device may
        // clamp; we read back the actual value.
        let device_id = device_id_from_audio_unit(&audio_unit)?;
        if let Some(frames) = requested_buffer_frames {
            set_buffer_frame_size(device_id, frames)?;
        }
        let buffer_frames = get_buffer_frame_size(device_id)?;

        let callback_count = Arc::new(AtomicU64::new(0));
        let cb_count = callback_count.clone();

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
            buffer_frames,
        })
    }

    /// The engine's configured sample rate (matches the AudioUnit's stream rate).
    #[must_use]
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Buffer size in frames per render callback, as accepted by the device.
    /// Multiply by 2 for the interleaved-stereo sample count.
    #[must_use]
    pub fn buffer_frames(&self) -> u32 {
        self.buffer_frames
    }

    /// One-way output latency = `buffer_frames / sample_rate`. Does **not**
    /// include input-capture latency (Thru mode adds another buffer of
    /// it; PRD §5.1) nor any DAC/cable latency we cannot observe from
    /// software.
    #[must_use]
    pub fn latency_seconds(&self) -> f64 {
        f64::from(self.buffer_frames) / f64::from(self.sample_rate)
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

// -----------------------------------------------------------------------
// Raw CoreAudio FFI helpers.
//
// These are the only `unsafe` blocks in dub-audio. They wrap two CoreAudio
// HAL calls — `AudioObjectGetPropertyData` and `AudioObjectSetPropertyData`
// — to read/write the buffer-frame-size on a device. The wrappers above
// expose only the safe interface.
//
// Soundness conditions for each call are documented inline; we hold them
// because:
//   - all `NonNull::from(&local)` references point to live stack data;
//   - `out_data` / `in_data` are correctly sized for the property's value
//     type (u32 for BufferFrameSize, AudioValueRange for the range query);
//   - `in_qualifier_data` is null and qualifier_size is 0 (this property
//     does not require qualification).
// -----------------------------------------------------------------------

fn device_id_from_audio_unit(au: &AudioUnit) -> Result<AudioObjectID, AudioError> {
    // get_property is the safe wrapper around AudioUnitGetProperty; it
    // returns the value bytes interpreted as `T`. Element::Output is the
    // I/O unit's output element; that's where the device binding lives.
    let device_id: AudioObjectID = au.get_property(
        kAudioOutputUnitProperty_CurrentDevice,
        Scope::Global,
        Element::Output,
    )?;
    Ok(device_id)
}

fn buffer_frame_size_address() -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyBufferFrameSize,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn buffer_frame_size_range_address() -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyBufferFrameSizeRange,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn get_buffer_frame_size(device: AudioObjectID) -> Result<u32, AudioError> {
    let address = buffer_frame_size_address();
    let mut value: u32 = 0;
    #[allow(clippy::cast_possible_truncation)]
    let mut size: u32 = mem::size_of::<u32>() as u32;
    // SAFETY: address and size live on the stack for the duration of the
    // call; out_data points at a u32 we own; the property returns exactly
    // a u32. qualifier null/0 is valid for this selector.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast::<c_void>(),
        )
    };
    if status != 0 {
        return Err(AudioError::Device(format!(
            "AudioObjectGetPropertyData(BufferFrameSize) failed: status {status}"
        )));
    }
    Ok(value)
}

fn set_buffer_frame_size(device: AudioObjectID, frames: u32) -> Result<(), AudioError> {
    let address = buffer_frame_size_address();
    let frames_value: u32 = frames;
    #[allow(clippy::cast_possible_truncation)]
    let data_size: u32 = mem::size_of::<u32>() as u32;
    // SAFETY: address and frames_value are stack values held for the call;
    // in_data is exactly u32-sized; qualifier null/0 is valid for this
    // selector. The kernel returns OSStatus; non-zero means the device
    // rejected the value (out of range, or not currently writable).
    let status = unsafe {
        AudioObjectSetPropertyData(
            device,
            NonNull::from(&address),
            0,
            ptr::null(),
            data_size,
            NonNull::from(&frames_value).cast::<c_void>(),
        )
    };
    if status != 0 {
        return Err(AudioError::Device(format!(
            "AudioObjectSetPropertyData(BufferFrameSize={frames}) failed: status {status}"
        )));
    }
    Ok(())
}

fn get_buffer_frame_size_range(device: AudioObjectID) -> Result<BufferFrameRange, AudioError> {
    let address = buffer_frame_size_range_address();
    let mut range = AudioValueRange {
        mMinimum: 0.0,
        mMaximum: 0.0,
    };
    #[allow(clippy::cast_possible_truncation)]
    let mut size: u32 = mem::size_of::<AudioValueRange>() as u32;
    // SAFETY: same conditions as get_buffer_frame_size, but the property
    // returns an AudioValueRange (two f64s) rather than a u32.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut range).cast::<c_void>(),
        )
    };
    if status != 0 {
        return Err(AudioError::Device(format!(
            "AudioObjectGetPropertyData(BufferFrameSizeRange) failed: status {status}"
        )));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(BufferFrameRange {
        min: range.mMinimum as u32,
        max: range.mMaximum as u32,
    })
}
