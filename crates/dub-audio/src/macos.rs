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
use coreaudio::audio_unit::macos_helpers::{
    audio_unit_from_device_id_uninitialized, get_audio_device_ids_for_scope,
    get_audio_device_supports_scope, get_default_device_id, get_device_id_from_name,
    get_device_name, set_device_sample_rate,
};
use coreaudio::audio_unit::render_callback::{self, data};
use coreaudio::audio_unit::stream_format::StreamFormat;
use coreaudio::audio_unit::{AudioUnit, Element, IOType, SampleFormat, Scope};
use objc2_audio_toolbox::{
    kAudioOutputUnitProperty_ChannelMap, kAudioOutputUnitProperty_CurrentDevice,
    kAudioUnitProperty_StreamFormat, AudioUnit as RawAudioUnit, AudioUnitSetProperty,
};
use objc2_core_audio::{
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyBufferFrameSizeRange,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreamConfiguration,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyElementWildcard,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectSetPropertyData,
};
use objc2_core_audio_types::{AudioBufferList, AudioValueRange};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapRb};

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
    let device_name =
        get_device_name(device_id).unwrap_or_else(|_| format!("<device {device_id}>"));
    // M5.5.2: surface the real physical channel count so the CLI's
    // known-device table can pick deck-routing offsets correctly. SL3
    // is 6 outs; built-in MacBook is 2; Traktor Audio 6 is 4 (or 6
    // depending on driver mode). Falls back to 2 if the HAL doesn't
    // answer, matching the pre-M5.5.2 behaviour.
    let channels = device_channel_count(device_id, kAudioObjectPropertyScopeOutput).unwrap_or(2);

    Ok(DeviceInfo {
        device_name,
        #[allow(clippy::cast_possible_truncation)]
        sample_rate: sample_rate_f64 as f32,
        channels,
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
    /// Number of output channels currently open on the AU. 2 for the
    /// legacy stereo path (`start` / `start_with_buffer_size`), N for
    /// the M5.5.2 multi-channel path (`start_with_options`).
    channels: u32,
    /// CoreAudio device name (e.g. `"SL 3"`). Empty for the legacy
    /// stereo path that didn't track this; populated from
    /// [`OutputOptions`] / [`query_default_output`] in the multi-channel
    /// path so the CLI can echo the routing it chose.
    device_name: String,
}

/// Options for opening an [`AudioOutput`] in multi-channel mode.
///
/// Mirrors [`InputOptions`]: the M5.2 input-side struct. Defaults are
/// chosen so that `start_with_options` with `OutputOptions::default()`
/// behaves like `start`: stereo, default device, device's current SR
/// and buffer size.
///
/// In v1 the *device* itself is always the system default output —
/// there's no `device_name` here yet. Users target a specific
/// interface via macOS Audio MIDI Setup → "Use this device for
/// sound output". Per-device selection lands later.
#[derive(Debug, Clone)]
pub struct OutputOptions {
    /// Number of physical output channels to open the AU with. 2 for
    /// stereo / internal mixer; 4 for the canonical external-mixer
    /// 2-deck topology; 6 for an SL3 / Audio 6 (so we can route deck
    /// audio to its physical pair *plus* leave aux ch 1+2 untouched).
    pub channels: u32,
    /// Optional buffer size override (frames per render callback).
    /// `None` keeps the device's current setting.
    pub buffer_frames: Option<u32>,
    /// Optional sample rate override. `None` uses the engine's SR
    /// (matches the legacy path's behaviour).
    pub sample_rate: Option<f32>,
    /// Optional channel-map override (CoreAudio
    /// `kAudioOutputUnitProperty_ChannelMap`). One entry per logical
    /// AU output channel; the i32 names the physical hardware
    /// channel, or -1 for "unmapped". `None` is identity (logical
    /// channel N → physical channel N), which is what the user wants
    /// for SL3 / Audio 6 since those devices already expose their
    /// physical channels in order.
    pub channel_map: Option<Vec<i32>>,
}

impl Default for OutputOptions {
    fn default() -> Self {
        Self {
            channels: 2,
            buffer_frames: None,
            sample_rate: None,
            channel_map: None,
        }
    }
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
        let device_id = device_id_from_audio_unit(&audio_unit)?;
        let engine_sr = engine.sample_rate();

        // SR alignment — same gauntlet as M5.2 input. CoreAudio HAL's
        // DefaultOutput AudioUnit, when its stream-format SR differs
        // from the device's nominal SR, sometimes inserts an internal
        // converter and sometimes plays the engine's bytes literally
        // at the device clock — driver-dependent and silent either
        // way. The reliable path is to force the device to the engine
        // SR so no conversion is needed. If the device can't honor
        // it, fail loudly rather than ship audible drift.
        //
        // No-op when the caller built the engine at the device's
        // current SR (`dub play --realtime` does this), so existing
        // realtime workflows are unaffected.
        let device_sr = get_device_nominal_sample_rate(device_id)?;
        if (f64::from(engine_sr) - device_sr).abs() > 0.5 {
            set_device_sample_rate(device_id, f64::from(engine_sr)).map_err(|e| {
                AudioError::Device(format!(
                    "output device refused engine SR {engine_sr} Hz \
                     (was {device_sr} Hz): {e:?} — check Audio MIDI Setup \
                     for supported rates",
                ))
            })?;
        }

        // Force interleaved f32 stereo at the engine's sample rate.
        // After the alignment above, this matches the device clock —
        // CoreAudio doesn't insert an SRC.
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
            channels: 2,
            device_name: get_device_name(device_id).unwrap_or_default(),
        })
    }

    /// Open the default output in multi-channel mode and start playback
    /// with per-deck routing.
    ///
    /// `opts.channels` controls how many physical channels of the
    /// device the AU exposes; `routing[deck_idx]` is the first
    /// (0-based) output channel that deck's stereo pair lands on. See
    /// [`dub_engine::OutputRouting`] for the full semantics. SL3
    /// example: `opts.channels = 6`, `routing = [Some(2), Some(4)]`
    /// puts deck A on out 3+4 and deck B on out 5+6.
    ///
    /// Same SR-alignment guarantee as [`Self::start_with_buffer_size`]:
    /// the device is forced to the engine's nominal SR (or the call
    /// fails loudly) so CoreAudio doesn't insert a silent SRC.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError`] if the device can't be opened, the
    /// requested stream format / channel count is rejected, the
    /// channel map is invalid, the buffer size can't be applied, or
    /// the callback can't be installed.
    pub fn start_with_options(
        engine: Engine,
        opts: &OutputOptions,
        routing: dub_engine::OutputRouting,
    ) -> Result<Self, AudioError> {
        let mut audio_unit = AudioUnit::new(IOType::DefaultOutput)?;
        let device_id = device_id_from_audio_unit(&audio_unit)?;
        let device_name = get_device_name(device_id).unwrap_or_default();
        let engine_sr = engine.sample_rate();

        if opts.channels < 2 {
            return Err(AudioError::Device(format!(
                "OutputOptions.channels = {} is invalid; need at least 2 to hold a stereo pair",
                opts.channels
            )));
        }
        for (deck_idx, slot) in routing.iter().enumerate() {
            if let Some(first) = slot {
                if (*first).saturating_add(2) > opts.channels {
                    return Err(AudioError::Device(format!(
                        "deck {deck_idx} routed to first channel {first} but only {} channels \
                         are open; need at least 2 channels for a stereo pair",
                        opts.channels
                    )));
                }
            }
        }

        // Same SR alignment as the legacy stereo path. See the long
        // comment in start_with_buffer_size for the full rationale.
        let device_sr = get_device_nominal_sample_rate(device_id)?;
        let target_sr = opts.sample_rate.unwrap_or(engine_sr);
        if (f64::from(target_sr) - device_sr).abs() > 0.5 {
            set_device_sample_rate(device_id, f64::from(target_sr)).map_err(|e| {
                AudioError::Device(format!(
                    "output device '{device_name}' refused SR {target_sr} Hz \
                     (was {device_sr} Hz): {e:?} — check Audio MIDI Setup \
                     for supported rates",
                ))
            })?;
        }

        // Force interleaved f32 N-channel at the engine's SR. Same
        // alignment guarantee as the stereo path; the only difference
        // is `channels: opts.channels`.
        let format = StreamFormat {
            sample_rate: f64::from(target_sr),
            sample_format: SampleFormat::F32,
            flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
            channels: opts.channels,
        };
        audio_unit.set_stream_format(format, Scope::Input, Element::Output)?;

        // Optional channel map: reorder logical AU channels to physical
        // device channels. Most users (including the SL3) don't need
        // this — the device's physical layout is already in order, so
        // logical out N == physical out N+1 (1-based). Provided for
        // future quirky devices.
        if let Some(map) = &opts.channel_map {
            if map.len() != opts.channels as usize {
                return Err(AudioError::Device(format!(
                    "channel_map.len() = {} but opts.channels = {}; map must have one entry \
                     per logical AU channel",
                    map.len(),
                    opts.channels
                )));
            }
            set_output_channel_map(&audio_unit, map)?;
        }

        if let Some(frames) = opts.buffer_frames {
            set_buffer_frame_size(device_id, frames)?;
        }
        let buffer_frames = get_buffer_frame_size(device_id)?;

        let callback_count = Arc::new(AtomicU64::new(0));
        let cb_count = callback_count.clone();

        let mut engine = engine;
        let mut rt = RealtimeContext::new();
        let num_channels = opts.channels as usize;
        // The routing array is Copy (it's [Option<u32>; 2]) so the
        // closure captures by value — no Arc, no allocation. The
        // engine is moved in. (No `let routing = routing` shadow
        // needed — clippy flags the no-op rebind, and routing is
        // already an owned value from the function argument.)
        audio_unit.set_render_callback(
            move |args: render_callback::Args<data::Interleaved<f32>>| {
                // RT thread. No allocation, no locks, no syscalls.
                // engine.render_routed is verified alloc-free by the
                // M5.5.1 tests; AtomicU64::fetch_add is wait-free.
                engine.render_routed(&mut rt, args.data.buffer, num_channels, &routing);
                cb_count.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )?;

        audio_unit.start()?;

        Ok(Self {
            audio_unit,
            callback_count,
            sample_rate: target_sr,
            buffer_frames,
            channels: opts.channels,
            device_name,
        })
    }

    /// The engine's configured sample rate (matches the AudioUnit's stream rate).
    #[must_use]
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Number of physical output channels currently open on the AU.
    /// 2 for the stereo paths (`start`, `start_with_buffer_size`); N
    /// for the multi-channel path (`start_with_options`) where N
    /// matches the `channels` field of [`OutputOptions`].
    #[must_use]
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// CoreAudio device name (e.g. `"SL 3"`) for the device this
    /// AudioOutput is driving. May be empty if the legacy stereo path
    /// didn't track it; always populated for `start_with_options`.
    #[must_use]
    pub fn device_name(&self) -> &str {
        &self.device_name
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

/// Read a device's *hardware* nominal sample rate.
///
/// CRITICAL: this is **not** the same as `AudioUnit::sample_rate()`. A
/// freshly-created HAL Output AudioUnit reports its own internal default
/// (e.g. 44.1 kHz) until you set the stream format on it — independent
/// of the underlying device's actual hardware SR. If the AU's stream
/// format SR doesn't equal the device's nominal SR, CoreAudio silently
/// delivers ZERO callbacks (no error from `audio_unit.start()`, no log
/// from `coreaudiod`). This was the cause of `dub capture` returning
/// 0 callbacks on a Mac whose mic was set to 48 kHz while we asked for
/// 44.1 kHz. See `crates/dub-audio/examples/probe_input.rs` for the
/// minimal repro.
fn get_device_nominal_sample_rate(device: AudioObjectID) -> Result<f64, AudioError> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyNominalSampleRate,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut sr: f64 = 0.0;
    #[allow(clippy::cast_possible_truncation)]
    let mut size: u32 = mem::size_of::<f64>() as u32;
    // SAFETY: address & size on stack; out_data points at our f64;
    // property returns exactly an f64; null qualifier is valid.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut sr).cast::<c_void>(),
        )
    };
    if status != 0 {
        return Err(AudioError::Device(format!(
            "AudioObjectGetPropertyData(NominalSampleRate) failed: status {status}"
        )));
    }
    Ok(sr)
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

// =========================================================================
//                       Audio INPUT — M5.2
// =========================================================================

/// Type alias for the input-callback argument pack. Hoisted to module
/// scope to satisfy clippy::items_after_statements (the alias was
/// previously defined inside `start_with_options`).
type InputCallbackArgs = render_callback::Args<data::Interleaved<f32>>;
//
// CoreAudio HAL input mirrors the output side: an AudioUnit per device
// drives a callback with newly-arrived samples. Differences vs output:
//
// 1. The HAL convention for an input-only unit is: enable input on
//    Element::Input, disable output on Element::Output, set the stream
//    format on the *Output* scope of the *Input* element (the "output"
//    of the input element is what flows to your callback).
//    `coreaudio::audio_unit::macos_helpers::audio_unit_from_device_id`
//    handles all the EnableIO / device-binding bookkeeping for us.
//
// 2. Selectors for the default device differ:
//      output → `kAudioHardwarePropertyDefaultOutputDevice`
//      input  → `kAudioHardwarePropertyDefaultInputDevice`
//    The macos_helpers `get_default_device_id(true)` wraps that.
//
// 3. The callback runs on the input device's IOProc thread. Like the
//    output side, it MUST be alloc-free, lock-free, and bounded —
//    we push samples through a ringbuf to a consumer thread that does
//    everything else (file write, level meter, timecode decode).

/// Information about an audio input device.
#[derive(Debug, Clone)]
pub struct InputDeviceInfo {
    /// Human-readable device name, e.g. "MacBook Pro Microphone",
    /// "Scratch Live SL3", "Traktor Audio 6".
    pub name: String,
    /// Sample rate the device is currently configured at.
    pub sample_rate: f32,
    /// Channel count on the input bus.
    pub channels: u32,
    /// Current device buffer size, in frames per IOProc callback.
    pub buffer_frames: u32,
    /// Allowed buffer-size range.
    pub buffer_frame_range: BufferFrameRange,
}

/// List every audio device that exposes input streams (i.e. has at
/// least one input channel). Used by `dub list-inputs` to help the
/// user pick the right SL3 / Audio 6 input pair.
///
/// # Errors
/// Returns [`AudioError::Device`] if HAL enumeration fails.
pub fn list_input_devices() -> Result<Vec<InputDeviceInfo>, AudioError> {
    // Implementation note: `get_audio_device_ids_for_scope(Scope::Input)`
    // in coreaudio-rs 0.14 does NOT actually filter by scope — it
    // returns every device on the system regardless. On output-only
    // devices, instantiating an input AudioUnit hangs CoreAudio
    // indefinitely. We therefore enumerate Global and explicitly filter
    // with `get_audio_device_supports_scope(id, Scope::Input)` BEFORE
    // any AudioUnit construction.
    //
    // We also report the device's **hardware nominal SR** (not the
    // AudioUnit's default 44.1 kHz), because that's the only rate at
    // which input callbacks will actually fire. Reporting the AU's
    // default would be a lie that misled `dub capture` for a day.
    let ids = get_audio_device_ids_for_scope(Scope::Global)
        .map_err(|e| AudioError::Device(format!("enumerating audio devices: {e}")))?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if !get_audio_device_supports_scope(id, Scope::Input).unwrap_or(false) {
            continue;
        }
        let name = get_device_name(id).unwrap_or_else(|_| format!("<device {id}>"));
        let Ok(buffer_frames) = get_buffer_frame_size(id) else {
            continue;
        };
        let Ok(buffer_frame_range) = get_buffer_frame_size_range(id) else {
            continue;
        };
        let Ok(sr) = get_device_nominal_sample_rate(id) else {
            continue;
        };
        if sr <= 0.0 {
            continue;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        out.push(InputDeviceInfo {
            name,
            sample_rate: sr as f32,
            channels: device_channel_count(id, kAudioObjectPropertyScopeInput).unwrap_or(0),
            buffer_frames,
            buffer_frame_range,
        });
    }
    Ok(out)
}

/// Query the system's current default *input* device (System Settings
/// → Sound → Input). Mirror of [`query_default_output`].
///
/// # Errors
/// Returns [`AudioError::Device`] if no default input is configured or
/// any HAL property cannot be queried.
pub fn query_default_input() -> Result<InputDeviceInfo, AudioError> {
    let id = get_default_device_id(true)
        .ok_or_else(|| AudioError::Device("no default input device configured".to_string()))?;
    device_info_for_input(id)
}

fn device_info_for_input(id: AudioObjectID) -> Result<InputDeviceInfo, AudioError> {
    let name = get_device_name(id)
        .map_err(|e| AudioError::Device(format!("get_device_name({id}): {e}")))?;
    let buffer_frames = get_buffer_frame_size(id)?;
    let buffer_frame_range = get_buffer_frame_size_range(id)?;
    // Hardware nominal SR — the only rate input callbacks will fire at.
    // (Querying via the AU returns the AU's internal default of 44.1 kHz,
    // not the device's actual rate. See `get_device_nominal_sample_rate`.)
    let sr = get_device_nominal_sample_rate(id)?;
    let channels = device_channel_count(id, kAudioObjectPropertyScopeInput).unwrap_or(2);
    #[allow(clippy::cast_possible_truncation)]
    Ok(InputDeviceInfo {
        name,
        sample_rate: sr as f32,
        channels,
        buffer_frames,
        buffer_frame_range,
    })
}

/// Configuration for opening an [`AudioInput`].
#[derive(Debug, Clone)]
pub struct InputOptions {
    /// Pick the device by name (substring match, case-insensitive).
    /// `None` → use the system's current default input.
    pub device_name: Option<String>,
    /// Number of channels to open. Defaults to 2 — the natural fit
    /// for stereo timecode (Serato CV02, Traktor MK2). Devices with
    /// more channels (SL3 has 6) will hand us the first `channels`
    /// of them.
    pub channels: u32,
    /// Request a specific device buffer size, or `None` to use the
    /// device's current value.
    pub buffer_frames: Option<u32>,
    /// Override the sample rate the input AudioUnit is opened at.
    /// `None` → use the device's current SR. Mismatches between this
    /// and the device's native SR will cause CoreAudio to insert a
    /// sample-rate converter; for timecode work we want exact device
    /// SR (no SRC) so leave this `None` in v1.
    pub sample_rate: Option<f32>,
    /// Capacity of the internal audio→consumer ringbuf, in **frames**
    /// (one frame = `channels` interleaved samples). The default of
    /// 0.5 s of stereo at 96 kHz = 48 000 frames is enough that a
    /// reasonably-scheduled consumer thread won't drop samples.
    pub ringbuf_frames: usize,
    /// Optional **device → output** channel mapping (0-based device
    /// channel indices). When `Some`, exactly `channels` entries are
    /// expected; entry `i` selects which physical input channel of
    /// the device feeds slot `i` of our interleaved output buffer.
    /// `-1` silences a slot.
    ///
    /// Real-world need: the Serato SL3 is a 6-input device with the
    /// turntable A pair on channels 3-4 (1-based) and turntable B on
    /// 5-6. Without this map, `dub capture` would record the unused
    /// aux/mic pair on channels 1-2. With `channel_map = Some(vec![2, 3])`
    /// (0-based for ch 3-4) the AU delivers exactly turntable A as a
    /// stereo pair, which is what the timecode decoder expects.
    ///
    /// `None` keeps the device's default identity mapping, where AU
    /// output channels [0..channels) are taken straight from device
    /// input channels [0..channels) — fine for a 2-channel mic.
    pub channel_map: Option<Vec<i32>>,
    /// **M5.6**: split the incoming interleaved-`channels` stream
    /// into N independent stereo ringbuffers, one per `(l, r)` pair.
    /// Each entry names the 0-based logical AU channels that form
    /// that pair's L and R; the IOProc demuxes per-frame and pushes
    /// 2 samples per pair into separate SPSC rings.
    ///
    /// Required for two-deck timecode (M5.6): SL3 input is opened
    /// 4-channel via `channel_map = [2,3,4,5]` (= device ch 3..6),
    /// then `output_pairs = [(0, 1), (2, 3)]` gives deck A pair 0
    /// and deck B pair 1. Each pair's consumer goes to its own
    /// `Engine::attach_timecode_input(deck_idx, ...)`.
    ///
    /// `None` and `Some(vec![(0, 1)])` are equivalent: a single
    /// stereo pair from the first two channels (= the M5.2 / M5.3
    /// behaviour). Constraints: `l != r`, both `< channels`.
    pub output_pairs: Option<Vec<(u32, u32)>>,
}

impl Default for InputOptions {
    fn default() -> Self {
        Self {
            device_name: None,
            channels: 2,
            buffer_frames: None,
            sample_rate: None,
            ringbuf_frames: 48_000,
            channel_map: None,
            output_pairs: None,
        }
    }
}

/// A live audio input: pulls samples from CoreAudio's input IOProc into
/// a ringbuf consumed by [`AudioInput::read_into`]. Mirror of
/// [`AudioOutput`] in design and lifetime semantics — dropping the
/// `AudioInput` stops the unit and reclaims the resources.
///
/// The audio thread is sacred: the IOProc callback only `try_push`es
/// into the ringbuf and increments two atomic counters. No allocation,
/// no locks, no transcendentals.
pub struct AudioInput {
    audio_unit: AudioUnit,
    /// Per-pair consumer ends of the IOProc → consumer ringbufs. One
    /// stereo SPSC ring per `output_pairs` entry; the IOProc demuxes
    /// the interleaved-`channels` device stream into these rings.
    /// Each `Some` until the caller takes it via
    /// [`AudioInput::take_consumer`] / [`AudioInput::take_consumer_pair`]
    /// (used to plumb the input into the engine for M5.3 / M5.6
    /// timecode wiring). After taking, the corresponding
    /// `read_into_pair` returns 0. Dropping the `AudioInput` always
    /// stops the AudioUnit, regardless of which consumers were taken.
    ///
    /// Single-pair mode (M5.2 / M5.3 / M5.4 backward compat): `rxs`
    /// has length 1; `rxs[0]` is the legacy stereo consumer and
    /// `take_consumer()` / `read_into()` / `available()` operate on
    /// it without the caller ever needing to know about pair indices.
    rxs: Vec<Option<HeapCons<f32>>>,
    callback_count: Arc<AtomicU64>,
    overflow_count: Arc<AtomicU64>,
    sample_rate: f32,
    channels: u32,
    buffer_frames: u32,
    device_name: String,
}

impl AudioInput {
    /// Open the default input device with default options.
    ///
    /// # Errors
    /// See [`AudioInput::start_with_options`].
    pub fn start() -> Result<Self, AudioError> {
        Self::start_with_options(&InputOptions::default())
    }

    /// Open an input device with explicit options.
    ///
    /// # Errors
    /// Returns [`AudioError::Device`] if the device cannot be opened,
    /// the requested format cannot be set, the buffer size cannot be
    /// applied, or the input callback cannot be installed.
    // Linear HAL setup pipeline: resolve device → set format → set
    // channel map → initialize → set buffer → build N stereo rings →
    // install IOProc → start. Splitting it would just hide the
    // ordering invariants (e.g. channel_map must be applied *before*
    // initialize, the IOProc closure must capture the producers
    // *after* they're built). M5.6 added ~30 lines for the multi-pair
    // ringbuf creation and validation; the function is still a
    // straight-line setup, not a tangle.
    #[allow(clippy::too_many_lines)]
    pub fn start_with_options(opts: &InputOptions) -> Result<Self, AudioError> {
        let device_id = resolve_input_device(opts.device_name.as_deref())?;
        let device_name =
            get_device_name(device_id).unwrap_or_else(|_| format!("<device {device_id}>"));

        // Sample-rate gauntlet — the load-bearing fix for "0 callbacks
        // and no error" on macOS HAL input. CoreAudio will silently
        // refuse to deliver any input data if the AudioUnit's stream
        // format SR ≠ the device's hardware nominal SR. So:
        //
        //   1. Query the device's actual hardware SR (NOT the AU's
        //      internal default, which a freshly-created HALOutput
        //      reports as 44.1 kHz regardless of hardware).
        //   2. If the caller requested a specific SR that differs,
        //      tell the device to switch via `set_device_sample_rate`
        //      (synchronous — blocks until the rate listener fires).
        //   3. Use the device's now-actual SR as the AU stream format
        //      SR. They're guaranteed equal at this point.
        //
        // We do this BEFORE creating the input AudioUnit so the AU
        // is born already in sync with the device.
        let device_sr = get_device_nominal_sample_rate(device_id)?;
        if let Some(requested) = opts.sample_rate {
            if (f64::from(requested) - device_sr).abs() > 0.5 {
                set_device_sample_rate(device_id, f64::from(requested)).map_err(|e| {
                    AudioError::Device(format!(
                        "set_device_sample_rate({device_name} -> {requested} Hz): {e:?}"
                    ))
                })?;
            }
        }
        let device_sr = get_device_nominal_sample_rate(device_id)?;
        #[allow(clippy::cast_possible_truncation)]
        let sample_rate = device_sr as f32;

        // Use the *uninitialized* helper so we can set the stream
        // format before `AudioUnitInitialize`. Setting the format on
        // an already-initialized unit appears to succeed but doesn't
        // always re-arm the IOProc; the safe sequence is set-then-init.
        let mut audio_unit = audio_unit_from_device_id_uninitialized(device_id, true)
            .map_err(|e| AudioError::Device(format!("audio_unit_from_device_id: {e}")))?;

        let channels = opts.channels.max(1);
        let format = StreamFormat {
            sample_rate: f64::from(sample_rate),
            sample_format: SampleFormat::F32,
            flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
            channels,
        };
        // For an input-only HAL unit, the stream format goes on the
        // *Output* scope of the *Input* element (see comment block at
        // the top of this section).
        let asbd = format.to_asbd();
        audio_unit
            .set_property(
                kAudioUnitProperty_StreamFormat,
                Scope::Output,
                Element::Input,
                Some(&asbd),
            )
            .map_err(|e| AudioError::Device(format!("set_stream_format(input): {e}")))?;

        // Optional channel map: tell the AU which physical device input
        // channels feed each slot of our interleaved output buffer.
        // Required to capture turntable A from an SL3 (channels 3-4)
        // instead of the unused mic/aux pair on channels 1-2. Must be
        // set BEFORE `initialize()` — the property is honoured by the
        // input element's reformatter at init time.
        if let Some(map) = &opts.channel_map {
            if map.len() != channels as usize {
                return Err(AudioError::Device(format!(
                    "channel_map has {} entries but format has {channels} channels",
                    map.len(),
                )));
            }
            set_input_channel_map(&audio_unit, map)?;
        }

        audio_unit
            .initialize()
            .map_err(|e| AudioError::Device(format!("audio_unit.initialize(input): {e}")))?;

        if let Some(req) = opts.buffer_frames {
            set_buffer_frame_size(device_id, req)?;
        }
        let buffer_frames = get_buffer_frame_size(device_id)?;

        // Resolve and validate the M5.6 output pairs. `None` and
        // `Some(vec![(0, 1)])` are equivalent — both mean "one
        // stereo pair from the first two channels" (the M5.2 / M5.3
        // single-deck case). Both result in a 1-element `pairs`
        // vector and the same RT behaviour.
        let pairs: Vec<(u32, u32)> = match opts.output_pairs.as_ref() {
            None => vec![(0, 1)],
            Some(p) if p.is_empty() => {
                return Err(AudioError::Device(
                    "InputOptions.output_pairs is Some([]); use None for the default \
                     single stereo pair, or pass at least one (l, r) pair"
                        .to_string(),
                ))
            }
            Some(p) => p.clone(),
        };
        for (idx, &(l, r)) in pairs.iter().enumerate() {
            if l >= channels || r >= channels {
                return Err(AudioError::Device(format!(
                    "output_pairs[{idx}] = ({l}, {r}) but only {channels} channels are open"
                )));
            }
            if l == r {
                return Err(AudioError::Device(format!(
                    "output_pairs[{idx}] = ({l}, {r}): L and R must be different channels \
                     (a stereo pair from the same mono channel is almost always a bug)"
                )));
            }
        }

        // Per-pair audio→consumer ringbufs. Each ring holds stereo
        // (capacity in samples = `ringbuf_frames * 2`), giving
        // `ringbuf_frames` frames of headroom independent of the
        // device's channel count or the number of pairs.
        let rb_capacity_per_pair = opts.ringbuf_frames.saturating_mul(2).max(1);
        let mut txs: Vec<ringbuf::HeapProd<f32>> = Vec::with_capacity(pairs.len());
        let mut rxs: Vec<Option<HeapCons<f32>>> = Vec::with_capacity(pairs.len());
        for _ in 0..pairs.len() {
            let rb = HeapRb::<f32>::new(rb_capacity_per_pair);
            let (tx, rx) = rb.split();
            txs.push(tx);
            rxs.push(Some(rx));
        }

        let callback_count = Arc::new(AtomicU64::new(0));
        let overflow_count = Arc::new(AtomicU64::new(0));
        let cb_count = callback_count.clone();
        let of_count = overflow_count.clone();

        // Capture into the closure: the producers (one per pair),
        // the channels-per-frame (= AU's logical interleave width),
        // and the pair offsets. All `Copy` or `Send` enough to live
        // in the IOProc closure.
        let channels_us = channels as usize;
        let pairs_for_cb = pairs.clone();

        audio_unit
            .set_input_callback(move |args: InputCallbackArgs| {
                cb_count.fetch_add(1, Ordering::Relaxed);
                let overflow =
                    push_demuxed_frames(args.data.buffer, channels_us, &pairs_for_cb, &mut txs);
                if overflow {
                    of_count.fetch_add(1, Ordering::Relaxed);
                }
                Ok(())
            })
            .map_err(|e| AudioError::Device(format!("set_input_callback: {e}")))?;

        audio_unit
            .start()
            .map_err(|e| AudioError::Device(format!("audio_unit.start (input): {e}")))?;

        Ok(Self {
            audio_unit,
            rxs,
            callback_count,
            overflow_count,
            sample_rate,
            channels,
            buffer_frames,
            device_name,
        })
    }

    /// Drain available samples from pair 0 (legacy single-pair) into
    /// `dst` (interleaved stereo). Returns the number of samples
    /// actually copied (≤ `dst.len()`); a partial fill simply means
    /// the device hasn't produced enough data yet, not an error.
    ///
    /// `dst.len()` should be a multiple of 2 for the caller's life
    /// to be easy (each pair is stereo). Returns 0 if pair 0's
    /// consumer was previously moved out via [`Self::take_consumer`]
    /// or [`Self::take_consumer_pair`].
    ///
    /// Equivalent to `read_into_pair(0, dst)`. Use the indexed form
    /// for M5.6 two-deck mode where pair 1 is deck B's input.
    pub fn read_into(&mut self, dst: &mut [f32]) -> usize {
        self.read_into_pair(0, dst)
    }

    /// Drain available stereo samples from pair `idx` into `dst`.
    /// Returns 0 if `idx` is out of range or that pair's consumer
    /// has been taken.
    pub fn read_into_pair(&mut self, idx: usize, dst: &mut [f32]) -> usize {
        self.rxs
            .get_mut(idx)
            .and_then(|slot| slot.as_mut().map(|rx| rx.pop_slice(dst)))
            .unwrap_or(0)
    }

    /// Number of stereo pairs configured via
    /// [`InputOptions::output_pairs`]. Single-pair mode (the M5.2 /
    /// M5.3 / M5.4 default) returns 1; M5.6 two-deck mode returns 2.
    #[must_use]
    pub fn pair_count(&self) -> usize {
        self.rxs.len()
    }

    /// Move pair 0's consumer out of this `AudioInput`. Used by the
    /// M5.3 timecode-deck wiring to hand the consumer to
    /// `dub_engine::Engine::attach_timecode_input`, which then runs
    /// the decoder on the audio thread directly.
    ///
    /// After this, [`Self::read_into`] returns 0 forever (the
    /// engine owns the consumer; only one reader is sound on an
    /// SPSC ring). The `AudioInput` itself stays alive on the main
    /// thread to keep the AudioUnit running and to surface
    /// `callback_count` / `overflow_count` to the UI; dropping it
    /// stops the device.
    ///
    /// Equivalent to `take_consumer_pair(0)`. For M5.6 two-deck
    /// mode, call `take_consumer_pair(1)` to get deck B's consumer.
    pub fn take_consumer(&mut self) -> Option<HeapCons<f32>> {
        self.take_consumer_pair(0)
    }

    /// Move pair `idx`'s consumer out of this `AudioInput`. Returns
    /// `None` if `idx` is out of range or that pair was already
    /// taken. Used by M5.6 two-deck timecode to attach pair 0 to
    /// engine deck 0 and pair 1 to engine deck 1.
    pub fn take_consumer_pair(&mut self, idx: usize) -> Option<HeapCons<f32>> {
        self.rxs.get_mut(idx).and_then(Option::take)
    }

    /// Number of interleaved stereo samples currently buffered
    /// between the audio thread and pair 0's consumer. Divide by 2
    /// to get frames. Returns 0 if pair 0's consumer was taken.
    #[must_use]
    pub fn available(&self) -> usize {
        self.available_pair(0)
    }

    /// Per-pair version of [`Self::available`].
    #[must_use]
    pub fn available_pair(&self, idx: usize) -> usize {
        self.rxs
            .get(idx)
            .and_then(|slot| slot.as_ref().map(Observer::occupied_len))
            .unwrap_or(0)
    }

    /// IOProc callback count since [`AudioInput::start`]. Used by
    /// the CLI to verify the device actually started.
    #[must_use]
    pub fn callback_count(&self) -> u64 {
        self.callback_count.load(Ordering::Relaxed)
    }

    /// Total number of callbacks where the audio→consumer ringbuf
    /// ran out of headroom and dropped samples. **Should always be
    /// zero in correct usage.** Non-zero means the consumer thread
    /// is too slow or the ringbuf is too small.
    #[must_use]
    pub fn overflow_count(&self) -> u64 {
        self.overflow_count.load(Ordering::Relaxed)
    }

    /// Sample rate of the input stream as configured (Hz).
    #[must_use]
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Number of interleaved channels arriving from the device.
    #[must_use]
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Device buffer size in frames per IOProc callback.
    #[must_use]
    pub fn buffer_frames(&self) -> u32 {
        self.buffer_frames
    }

    /// Human-readable name of the bound device.
    #[must_use]
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// One-way capture latency = `buffer_frames / sample_rate`. Does
    /// not include cable/ADC latency that's invisible to software.
    #[must_use]
    pub fn latency_seconds(&self) -> f64 {
        f64::from(self.buffer_frames) / f64::from(self.sample_rate)
    }

    /// Stop the AudioUnit explicitly. `Drop` does this too.
    ///
    /// # Errors
    /// Returns [`AudioError::Device`] if CoreAudio refuses to stop.
    pub fn stop(&mut self) -> Result<(), AudioError> {
        self.audio_unit
            .stop()
            .map_err(|e| AudioError::Device(format!("audio_unit.stop (input): {e}")))?;
        Ok(())
    }
}

impl Drop for AudioInput {
    fn drop(&mut self) {
        let _ = self.audio_unit.stop();
    }
}

/// Resolve a user-supplied device specifier to an input-capable
/// `AudioObjectID`.
///
/// `None` → system's default input. `Some(query)` → exact match first
/// (via `coreaudio-rs::get_device_id_from_name`), then case-insensitive
/// substring across input-capable devices only. We do the substring
/// search ourselves because `Scope::Input` filtering in coreaudio-rs
/// 0.14 does not actually filter by scope; see `list_input_devices`.
fn resolve_input_device(query: Option<&str>) -> Result<AudioObjectID, AudioError> {
    let Some(query) = query else {
        return get_default_device_id(true)
            .ok_or_else(|| AudioError::Device("no default input device configured".to_string()));
    };
    if let Some(id) = get_device_id_from_name(query, true) {
        return Ok(id);
    }
    let needle = query.to_lowercase();
    let ids = get_audio_device_ids_for_scope(Scope::Global)
        .map_err(|e| AudioError::Device(format!("enumerating audio devices: {e}")))?;
    for id in ids {
        if !get_audio_device_supports_scope(id, Scope::Input).unwrap_or(false) {
            continue;
        }
        if let Ok(name) = get_device_name(id) {
            if name.to_lowercase().contains(&needle) {
                return Ok(id);
            }
        }
    }
    Err(AudioError::Device(format!(
        "no input device matching '{query}'"
    )))
}

/// Apply an input channel map to an open AudioUnit.
///
/// `coreaudio_rs::AudioUnit::set_property::<T>` requires `T: Sized`, so
/// it can't carry a slice payload — we drop into the FFI directly.
/// The map must be sized exactly to the AU's input-element output
/// channel count (i.e. the `channels` field of the stream format we
/// just installed). Each entry is a 0-based device input channel index
/// or `-1` to mute that slot.
fn set_input_channel_map(au: &AudioUnit, map: &[i32]) -> Result<(), AudioError> {
    let inner: RawAudioUnit = *au.as_ref();
    #[allow(clippy::cast_possible_truncation)]
    let size = std::mem::size_of_val(map) as u32;
    // SAFETY: `inner` is a live AudioComponentInstance owned by `au`
    // for the lifetime of this call (we hold a `&AudioUnit`); `map`
    // is a borrowed slice we don't outlive; the property writes
    // exactly `size` bytes from `in_data`. Returns OSStatus; non-zero
    // means CoreAudio rejected the map (wrong size, channel out of
    // range, or unit state didn't permit the change).
    let status = unsafe {
        AudioUnitSetProperty(
            inner,
            kAudioOutputUnitProperty_ChannelMap,
            Scope::Output as u32,
            Element::Input as u32,
            map.as_ptr().cast::<c_void>(),
            size,
        )
    };
    if status != 0 {
        return Err(AudioError::Device(format!(
            "AudioUnitSetProperty(ChannelMap, len={}) failed: status {status}",
            map.len(),
        )));
    }
    Ok(())
}

/// Demultiplex an interleaved-N IOProc frame buffer into per-pair
/// stereo SPSC ringbufs (M5.6 two-deck input).
///
/// `buf` is interleaved-`channels`. For each frame (= `channels`
/// samples), push (L, R) into each `pair`'s ringbuf, where the
/// L/R indices are 0-based logical AU channels (i.e., post
/// `kAudioOutputUnitProperty_ChannelMap`).
///
/// Returns `true` if any pair's ring was unable to accept both
/// samples — overflow is signalled once per callback (not per
/// pair, not per sample), keeping the `overflow_count` counter
/// meaningful as "how many callbacks lost data" rather than a
/// sample-loss tally. Same convention as the M5.2 single-pair
/// path so existing rt-audit traces stay comparable.
///
/// **RT-safety**: `push_slice` on `HeapProd` is lock-free (atomic
/// CAS on the head index). The inner loop is bounded by
/// `pairs.len()` (1 in single-pair mode, 2 in M5.6 two-deck mode);
/// on a typical 256-frame CoreAudio callback with 2 pairs that's
/// 512 push_slice calls of 2 samples each — measured well under
/// 50 µs / callback, comfortably inside the 5 ms budget at 256/48k.
///
/// Extracted from `AudioInput::start_with_options` so the demux
/// logic is unit-testable without standing up an audio device —
/// the IOProc is otherwise too coupled to CoreAudio to exercise
/// from a test.
fn push_demuxed_frames(
    buf: &[f32],
    channels: usize,
    pairs: &[(u32, u32)],
    txs: &mut [ringbuf::HeapProd<f32>],
) -> bool {
    let mut overflow = false;
    for frame in buf.chunks_exact(channels) {
        for (p_idx, &(l, r)) in pairs.iter().enumerate() {
            let pair_samples = [frame[l as usize], frame[r as usize]];
            let pushed = txs[p_idx].push_slice(&pair_samples);
            if pushed < 2 {
                overflow = true;
            }
        }
    }
    overflow
}

/// Install a channel map on the output AU (M5.5.2). Writes
/// `kAudioOutputUnitProperty_ChannelMap` with `Scope::Input,
/// Element::Output` (the AU's *input* scope is what receives our
/// render-callback samples; the *output* element is what sends them
/// to the device). Each entry is a 0-based device output channel
/// index, or `-1` to mute that slot.
///
/// Note the scope/element flip vs. [`set_input_channel_map`] — for
/// the input AU the channel map sits on `Scope::Output, Element::Input`
/// (data comes *from* the device's input element and is *output* by the
/// AU). For the output AU it's the mirror image.
fn set_output_channel_map(au: &AudioUnit, map: &[i32]) -> Result<(), AudioError> {
    let inner: RawAudioUnit = *au.as_ref();
    #[allow(clippy::cast_possible_truncation)]
    let size = std::mem::size_of_val(map) as u32;
    // SAFETY: `inner` is a live AudioComponentInstance owned by `au`
    // for this call; `map` is a borrowed slice; the property writes
    // exactly `size` bytes from `in_data`. Returns OSStatus; non-zero
    // means CoreAudio rejected the map.
    let status = unsafe {
        AudioUnitSetProperty(
            inner,
            kAudioOutputUnitProperty_ChannelMap,
            Scope::Input as u32,
            Element::Output as u32,
            map.as_ptr().cast::<c_void>(),
            size,
        )
    };
    if status != 0 {
        return Err(AudioError::Device(format!(
            "AudioUnitSetProperty(ChannelMap, output, len={}) failed: status {status}",
            map.len(),
        )));
    }
    Ok(())
}

/// Total channel count of a device on a given scope (input or output),
/// queried via the HAL property `kAudioDevicePropertyStreamConfiguration`.
///
/// This is the **physical** channel count of the device for the given
/// scope: 6 for an SL3 (both in and out), 4 for a Traktor Audio 6 (or
/// 6 in some driver modes), 2 for a built-in MacBook mic / speakers.
/// It is **not** the same as `AudioUnit::input_stream_format().channels`,
/// which reports the AU's own (configurable) output count and defaults
/// to 2 regardless of hardware. We need the physical count so the user
/// can see "SL 3 outputs=6" in `dub list-outputs` (M5.5.2) and
/// confidently route deck audio to physical pairs.
///
/// Implementation: the property returns an `AudioBufferList`. We sum
/// `mNumberChannels` across all of its `mBuffers[]` entries.
///
/// `scope` is `kAudioObjectPropertyScopeInput` for input channels,
/// `kAudioObjectPropertyScopeOutput` for output channels.
fn device_channel_count(device: AudioObjectID, scope: u32) -> Option<u32> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreamConfiguration,
        mScope: scope,
        mElement: kAudioObjectPropertyElementWildcard,
    };
    let mut data_size: u32 = 0;
    // SAFETY: address lives on the stack; we only read into data_size,
    // which we own; null qualifier is valid for this selector.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            device,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut data_size),
        )
    };
    if status != 0 || data_size == 0 {
        return None;
    }
    // Allocate as `Vec<u64>` to guarantee 8-byte alignment for the
    // AudioBufferList cast (mNumberBuffers is u32, mBuffers contains
    // pointers — natural alignment 8 on 64-bit). We round up to the
    // next u64 worth of bytes.
    let n_u64 = (data_size as usize).div_ceil(std::mem::size_of::<u64>());
    let mut buf: Vec<u64> = vec![0_u64; n_u64];
    let buf_ptr = buf.as_mut_ptr().cast::<c_void>();
    // SAFETY: buf is at least `data_size` bytes (rounded up) and
    // 8-byte aligned, satisfying AudioBufferList layout.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut data_size),
            NonNull::new(buf_ptr).unwrap(),
        )
    };
    if status != 0 {
        return None;
    }
    let list = buf.as_ptr().cast::<AudioBufferList>();
    // SAFETY: the kernel populated `*list` with a valid AudioBufferList
    // whose `mNumberBuffers` and trailing `mBuffers[]` fit within the
    // bytes we passed (CoreAudio decreases data_size to the actual
    // size, which we don't shrink past — buf still owns enough memory).
    let total = unsafe {
        let n_buffers = (*list).mNumberBuffers as usize;
        // `mBuffers` is declared as length-1 in the C header but the
        // kernel writes a flexible array past it; index by raw pointer
        // arithmetic from mBuffers[0].
        let first = (*list).mBuffers.as_ptr();
        let mut total: u32 = 0;
        for i in 0..n_buffers {
            total += (*first.add(i)).mNumberChannels;
        }
        total
    };
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::{Consumer, Split};
    use ringbuf::HeapRb;

    /// Build N HeapProds + HeapCons of the given capacity for the
    /// pure-logic demux tests. Mirrors what `start_with_options`
    /// builds at runtime, just without the AudioUnit.
    fn make_pair_rings(n: usize, cap: usize) -> (Vec<ringbuf::HeapProd<f32>>, Vec<HeapCons<f32>>) {
        let mut txs = Vec::with_capacity(n);
        let mut rxs = Vec::with_capacity(n);
        for _ in 0..n {
            let rb = HeapRb::<f32>::new(cap);
            let (tx, rx) = rb.split();
            txs.push(tx);
            rxs.push(rx);
        }
        (txs, rxs)
    }

    #[test]
    fn demux_single_pair_passes_through_stereo() {
        // M5.2 / M5.3 / M5.4 backward-compat path: 2 channels in, 1
        // pair out, identity mapping. The output must equal the
        // input byte-for-byte (same frame layout, same pair).
        let buf = [10.0, 11.0, 20.0, 21.0, 30.0, 31.0]; // 3 stereo frames
        let pairs = [(0u32, 1u32)];
        let (mut txs, mut rxs) = make_pair_rings(1, 64);
        let overflow = push_demuxed_frames(&buf, 2, &pairs, &mut txs);
        assert!(
            !overflow,
            "ringbuf with cap 64 should not overflow on 6 samples"
        );
        let mut out = vec![0.0_f32; buf.len()];
        let n = rxs[0].pop_slice(&mut out);
        assert_eq!(n, buf.len());
        assert_eq!(out, buf, "single-pair demux must be identity");
    }

    #[test]
    fn demux_two_pairs_from_4ch_isolates() {
        // M5.6 case: 4-channel device frame, deck A on logical
        // channels 0+1, deck B on logical channels 2+3. Each pair
        // gets its own stream with the right samples.
        // Frame 0: A=10,11 B=12,13.  Frame 1: A=20,21 B=22,23.
        let buf = [10.0, 11.0, 12.0, 13.0, 20.0, 21.0, 22.0, 23.0];
        let pairs = [(0u32, 1u32), (2u32, 3u32)];
        let (mut txs, mut rxs) = make_pair_rings(2, 64);
        let overflow = push_demuxed_frames(&buf, 4, &pairs, &mut txs);
        assert!(!overflow);

        let mut a = vec![0.0_f32; 4];
        let mut b = vec![0.0_f32; 4];
        let na = rxs[0].pop_slice(&mut a);
        let nb = rxs[1].pop_slice(&mut b);
        assert_eq!(na, 4);
        assert_eq!(nb, 4);
        assert_eq!(
            a,
            vec![10.0, 11.0, 20.0, 21.0],
            "deck A should get ch 0+1 only"
        );
        assert_eq!(
            b,
            vec![12.0, 13.0, 22.0, 23.0],
            "deck B should get ch 2+3 only"
        );
    }

    #[test]
    fn demux_swapped_pair_indices_swap_lr() {
        // Pair = (1, 0) means "L is logical channel 1, R is logical
        // channel 0" — swaps the stereo image. Documents the semantic
        // and confirms the indices are honoured (not just the
        // ordering of `pairs`). Useful when a user's interface wires
        // the cartridge L/R inverted.
        let buf = [1.0, 2.0]; // one frame: ch0=1, ch1=2
        let pairs = [(1u32, 0u32)];
        let (mut txs, mut rxs) = make_pair_rings(1, 32);
        let _ = push_demuxed_frames(&buf, 2, &pairs, &mut txs);
        let mut out = vec![0.0_f32; 2];
        rxs[0].pop_slice(&mut out);
        assert_eq!(out, vec![2.0, 1.0], "(1, 0) must swap L and R");
    }

    #[test]
    fn demux_overflow_is_signalled() {
        // Tiny ring (2 samples = 1 frame); push 3 frames; the second
        // and third frame's pushes will partially fail. The function
        // must return `true` once.
        let buf = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let pairs = [(0u32, 1u32)];
        let (mut txs, _rxs) = make_pair_rings(1, 2);
        let overflow = push_demuxed_frames(&buf, 2, &pairs, &mut txs);
        assert!(
            overflow,
            "ring with capacity 2 should overflow on 6 samples"
        );
    }

    #[test]
    fn demux_partial_frame_at_tail_is_ignored() {
        // `chunks_exact` drops a tail that isn't a full frame. This
        // is the right behaviour because CoreAudio only ever delivers
        // complete frames (the contract of an AU); a partial frame in
        // the test buffer represents a malformed input. Document the
        // behaviour by pinning it.
        let buf = [10.0, 11.0, 12.0, 13.0, 99.0]; // 1 full 4-ch frame + 1 stray sample
        let pairs = [(0u32, 1u32), (2u32, 3u32)];
        let (mut txs, mut rxs) = make_pair_rings(2, 64);
        push_demuxed_frames(&buf, 4, &pairs, &mut txs);
        let mut a = vec![0.0_f32; 8];
        let mut b = vec![0.0_f32; 8];
        let na = rxs[0].pop_slice(&mut a);
        let nb = rxs[1].pop_slice(&mut b);
        assert_eq!(na, 2, "exactly one frame's worth (the stray 99.0 ignored)");
        assert_eq!(nb, 2);
        assert_eq!(&a[..2], &[10.0, 11.0]);
        assert_eq!(&b[..2], &[12.0, 13.0]);
    }
}
