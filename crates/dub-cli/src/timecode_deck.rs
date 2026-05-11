//! `dub timecode-deck <track> --input-channels N,M [...]` —
//! the M5.3 live wiring demo.
//!
//! Wires:
//!
//! 1. `dub_audio::AudioInput` on the chosen input device (channel-mapped,
//!    e.g. SL3 deck A on `--input-channels 3,4`),
//! 2. The IOProc → consumer ringbuf moved into the engine via
//!    [`dub_engine::Engine::attach_timecode_input`],
//! 3. A track loaded onto deck 0 (off the audio thread),
//! 4. `dub_audio::AudioOutput` running the engine on the CoreAudio
//!    render thread.
//!
//! Result: real-platter timecode drives a loaded track in real time —
//! forward play plays forward, scratching scratches, lifting the
//! stylus mutes the deck.
//!
//! What this is **not**: a UI, a mixer, a calibration tool, or a
//! correctness reference. It's the smallest possible "make sound come
//! out from real timecode" rig so we can validate the live integration
//! before any of those higher-level concerns land. Stickiness on lift
//! is M5.4; multi-deck routing and external-mixer output is M5.5.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use ringbuf::HeapCons;

use crate::audio_routing::{build_input_options, resolve_output_routing, RoutingArgs};
use crate::calibrate::{measure_inline, MeasureOptions, MeasurementInputs};
use crate::calibration::{default_calibration_dir, Calibration, CalibrationThresholds};
use crate::input_cmds::{parse_input_args, InputArgs};
use dub_audio::AudioInput;
use dub_engine::{
    Engine, TimecodeInputConfig, DEFAULT_AMPLITUDE_THRESHOLD, DEFAULT_CONFIDENCE_THRESHOLD,
    DEFAULT_DISENGAGE_THRESHOLD, DEFAULT_STICKY_BLOCKS_TO_DISENGAGE,
};
use dub_io::Track;
use dub_timecode::Format;

// `--duration` is now optional: omitted means "run until Ctrl-C"
// (the takeover use case from M5.4.5 needs unbounded runtime — the
// incoming DJ may wait minutes for the previous DJ to vacate deck
// B). The legacy 60 s default belonged to the M5.3 "validation
// run" era; with the calibrator able to wait indefinitely for a
// carrier, a hard wall-clock exit would silently lose deck B's
// calibration window. Pass `--duration N` to keep the bounded
// behaviour for scripted/CI runs.

/// Length of the auto-startup full calibration phases. Pin the
/// M5.4.3 single-phase defaults so auto-calibration produces a
/// JSON file indistinguishable from a manual `dub calibrate` run.
/// `AUTO_LIFT_SECS` is unused on the default M5.4.3 single-phase
/// path; kept for parallelism with [`MeasureOptions`] in case the
/// internal call sites later need to opt into two-phase.
///
/// M5.4.5 deleted `AUTO_DETECT_TIMEOUT_SECS` from the production
/// path — `dub timecode-deck` now passes
/// [`MeasureOptions::detect_timeout_secs`] = `None` so deck B's
/// calibrator can wait indefinitely during a DJ takeover (the
/// incoming DJ may not get access to deck B for many minutes after
/// launching the app). The standalone `dub calibrate` command
/// keeps a 30 s timeout — see the
/// [`crate::calibrate::DETECT_TIMEOUT_SECS`] private constant.
const AUTO_CARRIER_SECS: f64 = 3.0;
const AUTO_LIFT_SECS: f64 = 5.0;

/// CLI options for `dub timecode-deck`. Built on top of the shared
/// [`InputArgs`] so the `--input-channels`/`--device`/`--sr` flags
/// are identical to `dub levels` and `dub capture`.
struct Opts {
    /// Deck A's track. Always required.
    track_a: PathBuf,
    /// Deck B's track. `Some` triggers two-deck mode (M5.6); `None`
    /// keeps the M5.3 single-deck behaviour. When `Some`,
    /// `deck_b_input_channels` must also be set.
    track_b: Option<PathBuf>,
    /// Deck A's input config (device, channels, SR, buffer). Built
    /// from `--input-channels`, `--device`, etc. (the shared
    /// `InputArgs` parser).
    input: InputArgs,
    /// Deck B's input channels (1-based device-channel indices),
    /// e.g. `--deck-b-input-channels 5,6` for SL3 deck B. `None`
    /// keeps single-deck mode.
    deck_b_input_channels: Option<Vec<u32>>,
    /// Timecode format for both decks. M6 added Traktor MK1 (2 kHz)
    /// and Traktor MK2 (2.5 kHz) alongside Serato CV02. The default
    /// stays Serato CV02 to keep M5.3+ invocations working without
    /// a flag. Per-deck mixed format (e.g. Serato A + Traktor B,
    /// or MK1 A + MK2 B) isn't supported in the CLI yet — the
    /// engine already handles it (every `attach_timecode_input`
    /// is per-deck), so adding `--deck-b-format` later is a
    /// one-line change.
    format: Format,
    /// Output buffer size hint for CoreAudio output (frames). Smaller
    /// = lower output latency. None means "device default".
    output_buffer_size: Option<u32>,
    /// M5.5.2 routing knobs. None = auto-resolve from the device name
    /// against the known-device table; Some(_) overrides for
    /// unknown devices or for testing alternative routings on a
    /// known one.
    /// Force the M4 internal mixer regardless of detected device.
    /// Mutually exclusive with `--deck-a-out-ch` / `--deck-b-out-ch`
    /// (mixing the two would silently change the routing semantics).
    internal_mixer: bool,
    /// Override the auto-detection by selecting a profile by its
    /// `name_pattern` (e.g. `--device-profile "SL 3"`). Useful when
    /// the user has multiple interfaces connected and the wrong one
    /// is the system default.
    device_profile: Option<String>,
    /// Explicit total output channel count. For unknown devices this
    /// is required when `--deck-a-out-ch`/`--deck-b-out-ch` are
    /// given; for known devices it overrides the profile's default
    /// (rare; mostly for debugging).
    output_channels: Option<u32>,
    /// 1-based first output channel for deck A's stereo pair (e.g.
    /// `--deck-a-out-ch 3` → ch 3+4). Mutually exclusive with
    /// `--internal-mixer`.
    deck_a_out_ch: Option<u32>,
    /// 1-based first output channel for deck B's stereo pair.
    deck_b_out_ch: Option<u32>,
    /// Per-threshold explicit overrides on top of the M5.4.6
    /// always-fresh calibration. `None` = take the value the
    /// calibrator just produced; `Some(v)` = override that knob
    /// verbatim. Partial overrides are first-class so the user
    /// can pin one knob (e.g. amplitude=0.05 to test a loud venue)
    /// and let the rest auto-resolve from the actual measurement.
    confidence: Option<f32>,
    disengage: Option<f32>,
    sticky_blocks: Option<u32>,
    amplitude_threshold: Option<f32>,
    /// Skip calibration entirely — fall back to the M5.3 defaults
    /// regardless. Mostly useful for regression testing the M5.3
    /// engine path or for first-time users who want to hear the
    /// deck immediately without touching the calibrator. Per-knob
    /// overrides still apply on top.
    no_calibrate: bool,
    /// Wall-clock duration to run before stopping. `None` = run
    /// until Ctrl-C (the default; matches the M5.4.5 takeover use
    /// case where the operator can't predict when deck B will be
    /// available). `Some(d)` = exit after `d` seconds, kept for
    /// scripted / CI smoke tests. Distinct from `InputArgs::duration`
    /// because levels/capture default to 5 s; here unset means
    /// unbounded, not "use the levels default".
    duration_secs: Option<f64>,
}

fn parse_opts(args: &[String]) -> Result<Opts> {
    // Pull threshold/calibration flags out before delegating to the
    // shared input-args parser; everything else (device, channels,
    // input-channels, sr, duration) goes through the shared path.
    let mut filtered: Vec<String> = Vec::with_capacity(args.len());
    let mut confidence: Option<f32> = None;
    let mut disengage: Option<f32> = None;
    let mut sticky_blocks: Option<u32> = None;
    let mut amplitude_threshold: Option<f32> = None;
    let mut output_buffer_size: Option<u32> = None;
    let mut no_calibrate = false;
    let mut internal_mixer = false;
    let mut device_profile: Option<String> = None;
    let mut output_channels: Option<u32> = None;
    let mut deck_a_out_ch: Option<u32> = None;
    let mut deck_b_out_ch: Option<u32> = None;
    let mut deck_b_input_channels: Option<Vec<u32>> = None;
    let mut format = Format::SeratoCv02;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--confidence" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--confidence expects a number"))?;
                confidence = Some(
                    v.parse::<f32>()
                        .with_context(|| format!("--confidence {v}"))?,
                );
                i += 2;
            }
            "--disengage-threshold" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--disengage-threshold expects a number"))?;
                disengage = Some(
                    v.parse::<f32>()
                        .with_context(|| format!("--disengage-threshold {v}"))?,
                );
                i += 2;
            }
            "--sticky-blocks" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--sticky-blocks expects an integer"))?;
                sticky_blocks = Some(
                    v.parse::<u32>()
                        .with_context(|| format!("--sticky-blocks {v}"))?,
                );
                i += 2;
            }
            "--amplitude-threshold" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--amplitude-threshold expects a number"))?;
                amplitude_threshold = Some(
                    v.parse::<f32>()
                        .with_context(|| format!("--amplitude-threshold {v}"))?,
                );
                i += 2;
            }
            "--output-buffer-size" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--output-buffer-size expects an integer"))?;
                output_buffer_size = Some(
                    v.parse::<u32>()
                        .with_context(|| format!("--output-buffer-size {v}"))?,
                );
                i += 2;
            }
            "--no-calibrate" => {
                no_calibrate = true;
                i += 1;
            }
            "--internal-mixer" => {
                internal_mixer = true;
                i += 1;
            }
            "--device-profile" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--device-profile expects a name"))?;
                device_profile = Some(v.clone());
                i += 2;
            }
            "--output-channels" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--output-channels expects an integer"))?;
                output_channels = Some(
                    v.parse::<u32>()
                        .with_context(|| format!("--output-channels {v}"))?,
                );
                i += 2;
            }
            "--deck-a-out-ch" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--deck-a-out-ch expects an integer (1-based)"))?;
                deck_a_out_ch = Some(
                    v.parse::<u32>()
                        .with_context(|| format!("--deck-a-out-ch {v}"))?,
                );
                i += 2;
            }
            "--deck-b-out-ch" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--deck-b-out-ch expects an integer (1-based)"))?;
                deck_b_out_ch = Some(
                    v.parse::<u32>()
                        .with_context(|| format!("--deck-b-out-ch {v}"))?,
                );
                i += 2;
            }
            "--format" => {
                let v = args.get(i + 1).ok_or_else(|| {
                    anyhow!("--format expects 'serato-cv02', 'traktor-mk1', or 'traktor-mk2'")
                })?;
                format = Format::from_cli_arg(v).ok_or_else(|| {
                    anyhow!(
                        "unknown --format '{v}' (supported: serato-cv02, traktor-mk1, \
                         traktor-mk2; bare 'traktor' is rejected as ambiguous — MK1 \
                         is 2 kHz carrier, MK2 is 2.5 kHz, picking the wrong one \
                         silently plays back at the wrong speed)"
                    )
                })?;
                i += 2;
            }
            "--deck-b-input-channels" => {
                let v = args.get(i + 1).ok_or_else(|| {
                    anyhow!(
                        "--deck-b-input-channels expects N,M (1-based, e.g. 5,6 for SL3 deck B)"
                    )
                })?;
                let parsed: Result<Vec<u32>, _> =
                    v.split(',').map(str::trim).map(str::parse::<u32>).collect();
                let chans = parsed.with_context(|| format!("--deck-b-input-channels {v}"))?;
                if chans.len() != 2 || chans.contains(&0) {
                    return Err(anyhow!(
                        "--deck-b-input-channels must be exactly two non-zero 1-based \
                         indices, e.g. 5,6; got {v:?}"
                    ));
                }
                deck_b_input_channels = Some(chans);
                i += 2;
            }
            _ => {
                filtered.push(args[i].clone());
                i += 1;
            }
        }
    }
    let (input, leftover) = parse_input_args(&filtered)?;
    let positional: Vec<&String> = leftover.iter().filter(|s| !s.starts_with("--")).collect();
    if positional.is_empty() {
        return Err(anyhow!(
            "usage: dub timecode-deck <track-a.wav> [<track-b.wav>] --input-channels N,M \
             [--deck-b-input-channels N,M] [--format serato-cv02|traktor-mk1|traktor-mk2] \
             [--device NAME] \
             [--duration SECS] [--confidence T] [--disengage-threshold T] \
             [--sticky-blocks N] [--amplitude-threshold T] \
             [--output-buffer-size FRAMES] [--no-calibrate] \
             [--internal-mixer | (--deck-a-out-ch N --deck-b-out-ch N [--output-channels N])] \
             [--device-profile NAME]"
        ));
    }
    if positional.len() > 2 {
        return Err(anyhow!(
            "timecode-deck takes 1 (single-deck) or 2 (two-deck) track paths; got {}",
            positional.len()
        ));
    }
    if let Some(unknown) = leftover.iter().find(|s| s.starts_with("--")) {
        return Err(anyhow!("unknown flag: {unknown}"));
    }
    if internal_mixer && (deck_a_out_ch.is_some() || deck_b_out_ch.is_some()) {
        return Err(anyhow!(
            "--internal-mixer and --deck-a-out-ch / --deck-b-out-ch are mutually \
             exclusive: internal-mixer pins both decks to ch 1+2"
        ));
    }
    if internal_mixer && device_profile.is_some() {
        return Err(anyhow!(
            "--internal-mixer and --device-profile are mutually exclusive"
        ));
    }
    // Mixed-set sanity: one of deck-a/deck-b without the other is
    // almost always a typo. Require both or neither so the routing
    // is symmetric and the user can see what they specified.
    if deck_a_out_ch.is_some() != deck_b_out_ch.is_some() {
        return Err(anyhow!(
            "--deck-a-out-ch and --deck-b-out-ch must be specified together"
        ));
    }

    // Two-deck mode requires *both* a second track AND
    // --deck-b-input-channels — one without the other is a config
    // error. M5.6 doesn't support timecode for deck B with no track
    // (would be silent) or a track for deck B with no timecode (no
    // way to drive transport).
    let track_b: Option<PathBuf> = positional.get(1).map(|p| PathBuf::from(*p));
    if track_b.is_some() != deck_b_input_channels.is_some() {
        return Err(anyhow!(
            "two-deck mode requires both <track-b.wav> AND --deck-b-input-channels N,M; \
             got track_b={} deck-b-input-channels={}",
            track_b.is_some(),
            deck_b_input_channels.is_some(),
        ));
    }

    Ok(Opts {
        track_a: PathBuf::from(positional[0]),
        track_b,
        duration_secs: input.duration,
        input,
        deck_b_input_channels,
        format,
        output_buffer_size,
        confidence,
        disengage,
        sticky_blocks,
        amplitude_threshold,
        no_calibrate,
        internal_mixer,
        device_profile,
        output_channels,
        deck_a_out_ch,
        deck_b_out_ch,
    })
}

/// Entry point dispatched from `main`.
///
/// # Errors
/// Track decode, audio device open, attach errors, or HAL failures.
pub fn run(args: &[String]) -> Result<()> {
    let opts = parse_opts(args)?;
    let two_deck = opts.track_b.is_some();

    // 1. Load the track(s) off the audio thread.
    let track_a = Track::load_from_path(&opts.track_a)
        .with_context(|| format!("loading track A {}", opts.track_a.display()))?;
    println!(
        "track A:      {} ({} frames @ {} Hz, {} ch, {:.3} s)",
        opts.track_a.display(),
        track_a.frames(),
        track_a.sample_rate(),
        track_a.channels(),
        track_a.duration_seconds()
    );
    let track_b = match opts.track_b.as_ref() {
        Some(p) => {
            let t = Track::load_from_path(p)
                .with_context(|| format!("loading track B {}", p.display()))?;
            println!(
                "track B:      {} ({} frames @ {} Hz, {} ch, {:.3} s)",
                p.display(),
                t.frames(),
                t.sample_rate(),
                t.channels(),
                t.duration_seconds()
            );
            Some(t)
        }
        None => None,
    };

    // 2. Open the input device. In two-deck mode (M5.6) we open
    //    one 4-channel AU with `output_pairs = [(0, 1), (2, 3)]`,
    //    giving deck A pair 0 and deck B pair 1; the IOProc demuxes
    //    per frame into two stereo SPSC ringbuffers. Single-deck
    //    keeps the M5.3 path (channels=2, output_pairs=None).
    //
    //    CoreAudio doesn't allow two AUs on the same physical input
    //    device — this demux is the only way to feed two timecode
    //    decoders without serialising on a single ringbuffer
    //    (which is SPSC by design).
    let input_opts = build_input_options(&opts.input, opts.deck_b_input_channels.as_deref())?;
    let mut input =
        AudioInput::start_with_options(&input_opts).context("opening input device for timecode")?;
    let input_sr = input.sample_rate();
    println!(
        "input:        device='{}' sr={input_sr} Hz channels={} buffer={} frames pairs={}",
        input.device_name(),
        input.channels(),
        input.buffer_frames(),
        input.pair_count(),
    );

    // 3. The engine MUST run at the input SR for v1 (no SR conversion
    //    between input and engine). The output device gets aligned to
    //    the same SR by `AudioOutput::start_with_buffer_size` below;
    //    we just print the *current* nominal here for the user's
    //    reference. If the output device can't honor `engine_sr`
    //    `AudioOutput` will fail loudly rather than ship audible drift.
    let device = dub_audio::query_default_output().context("querying default output")?;
    if (device.sample_rate - input_sr).abs() > 0.5 {
        println!(
            "note: output device currently at {} Hz, will be retuned to {input_sr} Hz \
             (engine SR) so playback runs on a single clock — no SRC.",
            device.sample_rate
        );
    }
    let engine_sr = input_sr;
    let engine_block = 256_usize;
    // M5.4.5: build the engine *with a handle* so we can attach
    // timecode inputs mid-stream via the SPSC command channel after
    // `AudioOutput::start_with_options` has consumed the engine.
    // Pre-M5.4.5 this used `Engine::new` (no handle) and attached
    // synchronously before starting the output — which forced
    // calibration to complete before any audio could play, the
    // exact UX failure that breaks the DJ-takeover use case.
    let (mut engine, mut handle) = Engine::new_with_handle(engine_sr, engine_block);
    println!(
        "engine:       sr={engine_sr} Hz block={engine_block} frames\n\
         output:       device sr={} Hz (target {engine_sr} Hz) buffer={} frames",
        device.sample_rate, device.buffer_frames,
    );

    // 4. Configure decks with their tracks. We do NOT set_playing —
    //    decks default to paused, and the timecode driver (when it
    //    eventually attaches) will drive transport from carrier
    //    state. Until then, both decks render silence into the
    //    output bus, which is fine: the user sees a working audio
    //    chain (output device alive, no clicks) while calibrators
    //    work in the background.
    {
        let deck = engine.deck_mut(0);
        deck.set_source(Arc::new(track_a));
        deck.set_gain(1.0);
    }
    if let Some(t) = track_b {
        let deck = engine.deck_mut(1);
        deck.set_source(Arc::new(t));
        deck.set_gain(1.0);
    }

    // 5. Take both input consumers up front so each can be moved
    //    into its own calibrator worker thread (M5.4.5). Pre-M5.4.5
    //    the calibrator borrowed the AudioInput exclusively and the
    //    consumers were only moved out *after* calibration; with
    //    parallel calibrators that's no longer an option.
    let device_name = input.device_name().to_string();
    let consumer_a = input
        .take_consumer_pair(0)
        .ok_or_else(|| anyhow!("AudioInput pair 0 consumer already taken"))?;
    let consumer_b = if two_deck {
        Some(
            input
                .take_consumer_pair(1)
                .ok_or_else(|| anyhow!("AudioInput pair 1 consumer already taken"))?,
        )
    } else {
        None
    };

    // 6. Resolve output routing (M5.5.2 — shared with `dub thru` in
    //    M7 via [`crate::audio_routing`]).
    let routing = resolve_output_routing(&device, &routing_args_from_opts(&opts))?;
    println!("{}", routing.describe());

    // 7. Move the engine onto the audio thread. Output stage
    //    starts producing audio immediately — both decks render
    //    silence (paused, no timecode attached) until their
    //    calibrators complete and call
    //    `handle.attach_timecode_input(...)` mid-stream.
    let output_opts = dub_audio::OutputOptions {
        channels: routing.channels,
        buffer_frames: opts.output_buffer_size,
        sample_rate: None,
        channel_map: None,
    };
    let output = dub_audio::AudioOutput::start_with_options(engine, &output_opts, routing.routing)
        .context("starting CoreAudio output for timecode-deck")?;
    let achieved = output.buffer_frames();
    let latency_ms = output.latency_seconds() * 1000.0;
    println!("output buffer: {achieved} frames -> {latency_ms:.2} ms one-way latency");
    println!();

    // 8. M5.4.5 parallel calibrators. Each deck's calibrator runs
    //    on its own worker thread, owning its `HeapCons<f32>`. As
    //    each calibrator completes it sends back the consumer +
    //    [`Calibration`] via the mpsc channel; main applies CLI
    //    overrides + builds [`TimecodeInputConfig`] + calls
    //    [`dub_engine::EngineHandle::attach_timecode_input`] —
    //    that deck goes live mid-stream, the other one keeps
    //    waiting.
    //
    //    Worker threads are *detached* (`std::thread::spawn` not
    //    `std::thread::scope`). On the takeover path a calibrator
    //    may sit indefinitely waiting for its carrier; using scope
    //    would block forever at scope exit. Detached threads are
    //    cleaned up by the OS at process termination — acceptable
    //    for a CLI tool that exits via duration timer or Ctrl-C.
    println!(
        "calibration:  starting per-deck calibrators in parallel (M5.4.5).\n\
         \u{2192} as each deck's carrier locks, that deck attaches and audio plays.\n\
         \u{2192} the other deck keeps waiting — no blocking on a deck you don't have access to."
    );
    println!("(Ctrl-C to stop early)");
    println!();

    let overrides = ThresholdOverrides::from_opts(&opts);
    let calibration_dir = default_calibration_dir().ok();
    let format = opts.format;

    type CalibratorMsg = (u32, Result<(HeapCons<f32>, Calibration)>);
    let (tx, rx) = mpsc::channel::<CalibratorMsg>();

    if opts.no_calibrate {
        // No-calibrate path: skip the workers entirely, attach
        // immediately with the M5.3 defaults (+ CLI overrides).
        // The decks go live the instant attach lands, mirroring
        // the no-calibrate behaviour of pre-M5.4.5.
        println!("calibration: skipped (--no-calibrate); attaching with M5.3 defaults");
        let thresholds = overrides.apply_to(default_thresholds());
        attach_timecode_via_handle(
            &mut handle,
            0,
            consumer_a,
            format,
            input_sr,
            &thresholds,
            "deck A attached (no-calibrate)",
        )?;
        if let Some(c_b) = consumer_b {
            attach_timecode_via_handle(
                &mut handle,
                1,
                c_b,
                format,
                input_sr,
                &thresholds,
                "deck B attached (no-calibrate)",
            )?;
        }
        // No calibrators in flight; rx will only ever yield "no
        // more senders" once we drop tx below.
        drop(tx);
    } else {
        // Spawn one worker per declared deck. Each worker owns its
        // consumer + the metadata bundle + the optional save path
        // — all of which are 'static-able after move.
        let inputs_a = MeasurementInputs {
            device_name: device_name.clone(),
            input_sample_rate: input_sr,
            deck_index: 0,
            format,
        };
        let save_a = calibration_dir
            .as_ref()
            .map(|d| calibration_path_for(&device_name, 0, &opts, d));
        let tx_a = tx.clone();
        thread::spawn(move || {
            let result = calibrate_one_deck(consumer_a, inputs_a, save_a);
            // Send may fail if main has already exited; that's
            // fine, the worker just drops its result.
            let _ = tx_a.send((0, result));
        });

        if let Some(c_b) = consumer_b {
            let inputs_b = MeasurementInputs {
                device_name: device_name.clone(),
                input_sample_rate: input_sr,
                deck_index: 1,
                format,
            };
            let save_b = calibration_dir
                .as_ref()
                .map(|d| calibration_path_for(&device_name, 1, &opts, d));
            let tx_b = tx.clone();
            thread::spawn(move || {
                let result = calibrate_one_deck(c_b, inputs_b, save_b);
                let _ = tx_b.send((1, result));
            });
        }
        // Drop main's sender so `rx.try_recv` returns
        // `Disconnected` once both workers have completed (or
        // exited with an error). If we held it, `try_recv` would
        // return `Empty` forever and we couldn't tell the
        // difference between "still running" and "finished".
        drop(tx);
    }

    match opts.duration_secs {
        Some(d) => println!("running for {d:.1} s — drop the needle and play."),
        None => println!("running until Ctrl-C — drop the needle and play."),
    }

    // 9. Main loop. Interleaves three jobs at ~20 Hz:
    //   (a) drain the calibrator-result channel and call
    //       handle.attach_timecode_input as each deck completes,
    //   (b) reclaim trash so old TimecodeInputs don't accumulate
    //       on a re-attach (cartridge swap, future M5.4.5+),
    //   (c) print live stats every 500 ms.
    let start = Instant::now();
    let total = opts.duration_secs.map(Duration::from_secs_f64);
    let mut next_tick = start + Duration::from_millis(500);
    while total.is_none_or(|t| start.elapsed() < t) {
        // (a) Process completed calibrators.
        loop {
            match rx.try_recv() {
                Ok((deck_idx, Ok((consumer, cal)))) => {
                    let label = deck_label(deck_idx);
                    let thresholds = overrides.apply_to(cal.thresholds);
                    println!(
                        "deck {label}:       format={} ({:.0} Hz) engage={:.3} disengage={:.3} \
                         sticky={} blocks amp_floor={:.4}",
                        format.cli_name(),
                        format.carrier_hz(),
                        thresholds.engage,
                        thresholds.disengage,
                        thresholds.sticky_blocks_to_disengage,
                        thresholds.amplitude,
                    );
                    if let Err(e) = attach_timecode_via_handle(
                        &mut handle,
                        deck_idx as usize,
                        consumer,
                        format,
                        input_sr,
                        &thresholds,
                        &format!("deck {label} attached, audio live"),
                    ) {
                        eprintln!("deck {label} attach failed: {e:#}");
                    }
                }
                Ok((deck_idx, Err(e))) => {
                    let label = deck_label(deck_idx);
                    eprintln!("deck {label} calibration failed: {e:#} — deck stays silent");
                }
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            }
        }

        // (b) Reclaim trash from any displaced TimecodeInputs (no-
        //     op today; future-proofs mid-set re-cal).
        let _ = handle.reclaim();

        // (c) Stats tick.
        let now = Instant::now();
        if now >= next_tick {
            let cb = output.callback_count();
            let in_cb = input.callback_count();
            let in_of = input.overflow_count();
            print_stats(&output, &input, cb, in_cb, in_of);
            next_tick += Duration::from_millis(500);
        }
        thread::sleep(Duration::from_millis(50));
    }

    // 8. Final summary.
    let elapsed = start.elapsed().as_secs_f64();
    let cb = output.callback_count();
    let in_cb = input.callback_count();
    let in_of = input.overflow_count();
    println!();
    println!("done — {elapsed:.3} s wall");
    println!("  output callbacks: {cb}");
    println!("  input  callbacks: {in_cb} (overflow={in_of})");
    if cb == 0 {
        anyhow::bail!("CoreAudio output never fired a callback — device probably failed");
    }
    if in_cb == 0 {
        anyhow::bail!(
            "input device delivered no callbacks. SR mismatch or TCC permissions? \
             See `dub levels --input-channels {:?}` for a quick check.",
            opts.input.input_channels.as_deref().unwrap_or(&[1, 2])
        );
    }
    println!("OK");
    Ok(())
}

/// Build a [`TimecodeInputConfig`] from the resolved per-deck
/// thresholds + format + engine SR, then push it through the
/// [`dub_engine::EngineHandle`] command channel for mid-stream
/// attach (M5.4.5). Prints `attach_msg` on success — gives the
/// user a clear "deck N is now live" signal.
fn attach_timecode_via_handle(
    handle: &mut dub_engine::EngineHandle,
    deck_idx: usize,
    consumer: HeapCons<f32>,
    format: Format,
    input_sr: f32,
    thresholds: &CalibrationThresholds,
    attach_msg: &str,
) -> Result<()> {
    let cfg = TimecodeInputConfig {
        format,
        input_sample_rate: input_sr,
        // CoreAudio output blocks vary; 4096 is a safe upper bound
        // matching the pre-M5.4.5 synchronous attach path.
        max_block_frames: 4096,
        confidence_threshold: thresholds.engage,
        disengage_threshold: thresholds.disengage,
        sticky_blocks_to_disengage: thresholds.sticky_blocks_to_disengage,
        amplitude_threshold: thresholds.amplitude,
    };
    handle
        .attach_timecode_input(deck_idx, consumer, cfg)
        .with_context(|| format!("attaching timecode input to deck {deck_idx}"))?;
    println!("{attach_msg}");
    Ok(())
}

fn print_stats(
    output: &dub_audio::AudioOutput,
    input: &AudioInput,
    out_cb: u64,
    in_cb: u64,
    in_of: u64,
) {
    // Single-line refresh on stderr — keeps stdout clean for `tee`.
    let buf_ms = (f64::from(output.buffer_frames()) / f64::from(output.sample_rate())) * 1000.0;
    // `available()` reports interleaved-stereo *samples* on pair 0;
    // each frame is 2 samples regardless of how many channels the
    // device delivers (the demux gives us one stereo pair per
    // ringbuf in M5.6).
    #[allow(clippy::cast_precision_loss)]
    let avail_frames = (input.available() as f64) / 2.0;
    eprintln!(
        "  out_cb={out_cb} buf={buf_ms:.2}ms in_cb={in_cb} in_overflow={in_of} \
         in_buffered={avail_frames:.0} frames"
    );
}

/// Adapt this subcommand's `Opts` into the shared
/// [`crate::audio_routing::RoutingArgs`] used by
/// [`resolve_output_routing`]. Just a field-by-field copy of the
/// routing-relevant flags; the resolver doesn't know about
/// `--track`, `--format`, or anything timecode-specific. Same
/// adapter pattern is used in `dub thru` (M7).
fn routing_args_from_opts(opts: &Opts) -> RoutingArgs {
    RoutingArgs {
        internal_mixer: opts.internal_mixer,
        deck_a_out_ch: opts.deck_a_out_ch,
        deck_b_out_ch: opts.deck_b_out_ch,
        output_channels: opts.output_channels,
        device_profile: opts.device_profile.clone(),
    }
}

/// Human-friendly label for an engine deck index. `0 → "A"`,
/// `1 → "B"`. Used in print statements so the user thinks in
/// SL1200-style deck letters while the internals stay numeric.
fn deck_label(deck_idx: u32) -> char {
    char::from(b'A' + u8::try_from(deck_idx).unwrap_or(0))
}

/// Resolve the four lift-policy thresholds for a single deck.
///
/// **M5.4.6 (always-fresh) flow:**
///
/// 1. **Explicit CLI overrides** (`--confidence`,
///    `--amplitude-threshold`, `--disengage-threshold`,
///    `--sticky-blocks`) are applied last so a partial override
///    always wins over the auto-resolved value.
/// 2. **`--no-calibrate`** — return the M5.3 defaults verbatim
///    (with overrides on top). Useful for testing the audio path
///    without hardware, or for first-time users who want to hear
///    the deck immediately.
/// 3. **Otherwise (default)** — run a fresh single-phase
///    calibration (M5.4.3) against the actual rig in front of the
///    user, save the result to
///    `~/.dub/calibration/<device>_deck_<idx>_<format>.json` as a
///    diagnostic artifact (best-effort; save failure is logged
///    but non-fatal), and return the derived thresholds.
///
/// **Why no probe / no JSON load (M5.4.6).** The pre-M5.4.6
/// design saved + fingerprint-probed-at-startup to skip the slow
/// recalibration path on repeat sessions. With M5.4.3 making
/// fresh calibration ≈ 3.5 s, the probe was only paying for
/// itself in the bedroom-DJ scenario (one fixed rig, repeat
/// sessions). For touring DJs every venue brings a different
/// turntable + cartridge, the fingerprint mismatches, and the
/// probe burns ~1.7 s confirming what we already know. Always-
/// measure-the-rig-in-front-of-you is simpler, faster on the
/// production path, and removes a class of "stale calibration
/// silently used" failure modes.
///
/// **Format-aware (M6 fix preserved).** Calibration runs against
/// `opts.format`, so two-deck mode with mixed formats (Serato on
/// A, Traktor on B) gets the right carrier per deck. The save
/// path keys off `(device_name, deck_idx, opts.format)`, so the
/// per-format diagnostic JSONs accumulate independently.
///
/// **Pair / deck duality (M5.6 + M5.4.4).** `pair_idx == deck_idx`
/// in two-deck mode because `dub timecode-deck` opens a
/// 4-channel input with `output_pairs[(0, 1), (2, 3)]` mapping
/// pair 0 → deck A, pair 1 → deck B. `run_full_calibration` reads
/// from `pair_idx = deck_idx` so each deck calibrates against its
/// own physical input pair.
/// Snapshot of the per-deck CLI override knobs that `resolve_thresholds`
/// needs to apply on top of the auto-derived calibration. Stripping
/// the worker thread's view of `Opts` to just these `Copy`/owned
/// values means the worker doesn't need to clone the entire `Opts`
/// (it carries `PathBuf`, `InputArgs`, etc., which are bigger and
/// irrelevant to threshold derivation).
#[derive(Debug, Clone, Copy)]
struct ThresholdOverrides {
    confidence: Option<f32>,
    disengage: Option<f32>,
    sticky_blocks: Option<u32>,
    amplitude_threshold: Option<f32>,
}

impl ThresholdOverrides {
    fn from_opts(opts: &Opts) -> Self {
        Self {
            confidence: opts.confidence,
            disengage: opts.disengage,
            sticky_blocks: opts.sticky_blocks,
            amplitude_threshold: opts.amplitude_threshold,
        }
    }

    fn apply_to(self, base: CalibrationThresholds) -> CalibrationThresholds {
        CalibrationThresholds {
            engage: self.confidence.unwrap_or(base.engage),
            disengage: self.disengage.unwrap_or(base.disengage),
            amplitude: self.amplitude_threshold.unwrap_or(base.amplitude),
            sticky_blocks_to_disengage: self
                .sticky_blocks
                .unwrap_or(base.sticky_blocks_to_disengage),
        }
    }
}

/// Run a fresh calibration on a single deck's input consumer in a
/// worker thread (M5.4.5). Returns the consumer back along with the
/// resulting [`Calibration`] so the caller can extract thresholds,
/// apply CLI overrides, and call
/// [`dub_engine::EngineHandle::attach_timecode_input`] to attach
/// this deck mid-stream.
///
/// **M5.4.5 worker entry point.** Designed to be moved by value
/// into a [`std::thread::spawn`] closure — `consumer`, `inputs`,
/// and `save_path` are all owned values with no borrows from the
/// main thread's stack. Two of these can run in parallel without
/// any shared mutable state.
///
/// The save step is best-effort — failure is logged via
/// [`save_calibration`] but never propagated, so a read-only home
/// directory or sandbox doesn't block the live performance flow.
/// The runtime no longer reads these files (M5.4.6) but we still
/// write them so a power user can inspect "what did this rig look
/// like" after the fact.
fn calibrate_one_deck(
    mut consumer: HeapCons<f32>,
    inputs: MeasurementInputs,
    save_path: Option<PathBuf>,
) -> Result<(HeapCons<f32>, Calibration)> {
    let cal = run_full_calibration(&mut consumer, &inputs)?;

    if let Some(path) = save_path {
        save_calibration(&cal, &path);
    }

    Ok((consumer, cal))
}

/// M5.3 defaults — the floor that every higher-priority source
/// (saved JSON, fresh measurement) overrides.
fn default_thresholds() -> CalibrationThresholds {
    CalibrationThresholds {
        engage: DEFAULT_CONFIDENCE_THRESHOLD,
        disengage: DEFAULT_DISENGAGE_THRESHOLD,
        amplitude: DEFAULT_AMPLITUDE_THRESHOLD,
        sticky_blocks_to_disengage: DEFAULT_STICKY_BLOCKS_TO_DISENGAGE,
    }
}

/// Apply per-knob CLI overrides on top of an auto-resolved set.
/// Each override replaces exactly one value, leaving the others
/// untouched — partial overrides ("auto-everything except force
/// amplitude=0.05") are first-class.
///
/// Thin wrapper over [`ThresholdOverrides::apply_to`] so the
/// existing override tests (which take an `Opts`) keep their
/// shape without coupling them to the [`Opts`] internals. The
/// production hot path uses `ThresholdOverrides` directly to keep
/// the worker-thread move set small.
#[cfg(test)]
fn apply_overrides(base: CalibrationThresholds, opts: &Opts) -> CalibrationThresholds {
    ThresholdOverrides::from_opts(opts).apply_to(base)
}

/// Run a full calibration against a single deck's input ringbuffer
/// consumer and return the populated [`Calibration`].
///
/// **M5.4.5** changed the parameter set from `(&mut AudioInput,
/// deck_idx, format)` to `(&mut HeapCons<f32>, &MeasurementInputs)`
/// so the function can run on a worker thread without an exclusive
/// borrow on the `AudioInput`. Now two of these can run in parallel,
/// one per deck, and the calibrator that finishes first attaches its
/// deck to the engine via the command channel — no waiting on the
/// other deck.
///
/// The `detect_timeout` is `None` (wait indefinitely) for the
/// `dub timecode-deck` startup path: in the DJ-takeover scenario
/// the incoming DJ launches the app while the previous DJ still
/// has the turntables, and deck B's calibrator may sit waiting for
/// many minutes. Once the carrier eventually appears (DJ drops a
/// record), the calibrator wakes up, completes, and attaches
/// mid-stream without disturbing whichever deck is already live.
fn run_full_calibration(
    consumer: &mut HeapCons<f32>,
    inputs: &MeasurementInputs,
) -> Result<Calibration> {
    println!();
    println!(
        "=== auto-calibration (deck {}) ===",
        deck_label(inputs.deck_index)
    );
    // M5.4.3: auto-calibration uses single-phase mode (carrier-only).
    // The two-phase opt-out is reachable from the `dub calibrate
    // --two-phase` CLI; auto-calibration prioritizes "drop the needle,
    // wait ~3 s, you're playing" over the SNR safety net (which
    // catches stylus / preamp / cabling problems but adds ~25 s the
    // user is unlikely to notice when something's already working).
    let cal = measure_inline(
        consumer,
        inputs,
        MeasureOptions {
            carrier_secs: AUTO_CARRIER_SECS,
            lift_secs: AUTO_LIFT_SECS,
            // M5.4.5: no timeout — the incoming DJ may not have
            // access to deck B's turntable for many minutes during
            // a takeover. We wait until the carrier appears.
            detect_timeout_secs: None,
            two_phase: false,
        },
    )?;
    println!(
        "  derived: engage={:.3} disengage={:.3} amp={:.4} sticky={} (SNR {:.0}\u{00d7})",
        cal.thresholds.engage,
        cal.thresholds.disengage,
        cal.thresholds.amplitude,
        cal.thresholds.sticky_blocks_to_disengage,
        cal.snr_margin,
    );
    println!(
        "=== end calibration (deck {}) ===",
        deck_label(inputs.deck_index)
    );
    println!();
    Ok(cal)
}

/// Save the calibration to disk; report failures as warnings rather
/// than fatal errors. The user's session can proceed even if disk
/// is full / read-only / sandboxed — they just lose the persistence
/// for next startup. This trade-off keeps the calibration flow
/// "always recoverable" for a live performance setup.
fn save_calibration(cal: &Calibration, path: &std::path::Path) {
    match cal.save(path) {
        Ok(()) => println!("  saved → {}", path.display()),
        Err(e) => eprintln!(
            "  ⚠ failed to save calibration to {}: {e:#}",
            path.display()
        ),
    }
}

/// Compute the on-disk path the calibration JSON should live at for
/// the given input device + deck index + run options. Pure — no I/O,
/// no audio device access — so the path-derivation contract is
/// unit-testable without a hardware fixture.
///
/// **Why this is its own helper.** An earlier draft of M6 inlined
/// the path computation inside `resolve_thresholds` and silently
/// used `Format::SeratoCv02` instead of `opts.format`, which loaded
/// the wrong calibration JSON and ran the carrier probe at the
/// wrong nominal frequency when the user passed `--format
/// traktor-mk1`. Extracting this helper means the contract ("the
/// path always derives from `(device, deck_idx, opts.format)`") is
/// pinned by fast unit tests that don't need a real audio device.
///
/// **Per-deck (M5.4.4).** `deck_idx` keys the path so deck A and
/// deck B on the same SL3 + same format end up in distinct files
/// (`SL_3_deck_0_serato-cv02.json` vs `SL_3_deck_1_serato-cv02
/// .json`). Pre-M5.4.4 callers used the format-only path and
/// shared deck B's thresholds with deck A; that's silently wrong
/// when the two cartridges differ.
fn calibration_path_for(
    device_name: &str,
    deck_idx: u32,
    opts: &Opts,
    dir: &std::path::Path,
) -> std::path::PathBuf {
    Calibration::path_for(device_name, deck_idx, opts.format, dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::SCHEMA_VERSION;

    fn opts_default() -> Opts {
        Opts {
            track_a: PathBuf::new(),
            track_b: None,
            input: InputArgs::default(),
            deck_b_input_channels: None,
            format: Format::SeratoCv02,
            output_buffer_size: None,
            confidence: None,
            disengage: None,
            sticky_blocks: None,
            amplitude_threshold: None,
            no_calibrate: false,
            internal_mixer: false,
            device_profile: None,
            output_channels: None,
            deck_a_out_ch: None,
            deck_b_out_ch: None,
            duration_secs: None,
        }
    }

    fn dev(name: &str, channels: u32) -> dub_audio::DeviceInfo {
        dub_audio::DeviceInfo {
            device_name: name.to_string(),
            sample_rate: 48_000.0,
            channels,
            buffer_frames: 256,
            #[cfg(target_os = "macos")]
            buffer_frame_range: dub_audio::BufferFrameRange { min: 64, max: 4096 },
        }
    }

    #[test]
    fn apply_overrides_replaces_only_set_fields() {
        let base = CalibrationThresholds {
            engage: 0.95,
            disengage: 0.50,
            amplitude: 0.12,
            sticky_blocks_to_disengage: 4,
        };
        let mut opts = opts_default();
        opts.amplitude_threshold = Some(0.05);
        let r = apply_overrides(base, &opts);
        // amplitude overridden, everything else preserved.
        assert!((r.amplitude - 0.05).abs() < 1e-6);
        assert!((r.engage - 0.95).abs() < 1e-6);
        assert!((r.disengage - 0.50).abs() < 1e-6);
        assert_eq!(r.sticky_blocks_to_disengage, 4);
    }

    #[test]
    fn apply_overrides_no_explicit_keeps_base() {
        let base = CalibrationThresholds {
            engage: 0.95,
            disengage: 0.50,
            amplitude: 0.12,
            sticky_blocks_to_disengage: 4,
        };
        let opts = opts_default();
        let r = apply_overrides(base, &opts);
        assert!((r.engage - base.engage).abs() < 1e-6);
        assert!((r.amplitude - base.amplitude).abs() < 1e-6);
    }

    #[test]
    fn apply_overrides_full_override_wins() {
        let base = CalibrationThresholds {
            engage: 0.95,
            disengage: 0.50,
            amplitude: 0.12,
            sticky_blocks_to_disengage: 4,
        };
        let mut opts = opts_default();
        opts.confidence = Some(0.80);
        opts.disengage = Some(0.40);
        opts.amplitude_threshold = Some(0.03);
        opts.sticky_blocks = Some(8);
        let r = apply_overrides(base, &opts);
        assert!((r.engage - 0.80).abs() < 1e-6);
        assert!((r.disengage - 0.40).abs() < 1e-6);
        assert!((r.amplitude - 0.03).abs() < 1e-6);
        assert_eq!(r.sticky_blocks_to_disengage, 8);
    }

    // M5.4.6 dropped `calibration_age_days` (and the surrounding
    // age-warning logic) along with the load-from-disk path. The
    // three calibration_age_days_* tests went with it.

    /// Regression: an earlier draft of M6 hardcoded
    /// `Format::SeratoCv02` in `resolve_thresholds` instead of
    /// reading `opts.format`, so `dub timecode-deck --format
    /// traktor-mk1` silently loaded the Serato calibration JSON
    /// and ran the carrier probe at the 1 kHz Serato carrier —
    /// which produced `rate ≈ 2.0×` against the actual 2 kHz MK1
    /// carrier and timed the probe out. This test pins that the
    /// path always derives from `opts.format` *and* the deck index.
    #[test]
    fn calibration_path_uses_opts_format_and_deck_serato() {
        let dir = std::path::Path::new("/tmp/dub-test");
        let opts = Opts {
            format: Format::SeratoCv02,
            ..opts_default()
        };
        let p0 = calibration_path_for("SL 3", 0, &opts, dir);
        let p1 = calibration_path_for("SL 3", 1, &opts, dir);
        assert!(
            p0.to_string_lossy().contains("serato-cv02"),
            "expected serato-cv02 in path, got {}",
            p0.display()
        );
        assert!(
            p0.to_string_lossy().contains("deck_0"),
            "expected deck_0 in path, got {}",
            p0.display()
        );
        assert!(
            p1.to_string_lossy().contains("deck_1"),
            "expected deck_1 in path, got {}",
            p1.display()
        );
        assert_ne!(p0, p1, "decks A and B must have distinct calibration paths");
    }

    #[test]
    fn calibration_path_uses_opts_format_traktor_mk1() {
        let dir = std::path::Path::new("/tmp/dub-test");
        let opts = Opts {
            format: Format::TraktorMk1,
            ..opts_default()
        };
        let p = calibration_path_for("SL 3", 0, &opts, dir);
        assert!(
            p.to_string_lossy().contains("traktor-mk1"),
            "expected traktor-mk1 in path, got {}",
            p.display()
        );
        assert!(
            !p.to_string_lossy().contains("serato"),
            "MK1 must not load the Serato JSON, got {}",
            p.display()
        );
    }

    #[test]
    fn calibration_path_uses_opts_format_traktor_mk2() {
        let dir = std::path::Path::new("/tmp/dub-test");
        let opts = Opts {
            format: Format::TraktorMk2,
            ..opts_default()
        };
        let p = calibration_path_for("SL 3", 0, &opts, dir);
        assert!(
            p.to_string_lossy().contains("traktor-mk2"),
            "expected traktor-mk2 in path, got {}",
            p.display()
        );
        assert!(
            !p.to_string_lossy().contains("traktor-mk1"),
            "MK2 must not collide with MK1, got {}",
            p.display()
        );
    }

    #[test]
    fn calibration_path_distinct_per_format_for_same_device() {
        // Three formats × one deck = three independent JSON files.
        // Pin the disjointness so a future refactor can't accidentally
        // share calibration across formats.
        let dir = std::path::Path::new("/tmp/dub-test");
        let mut paths = vec![];
        for format in [Format::SeratoCv02, Format::TraktorMk1, Format::TraktorMk2] {
            let opts = Opts {
                format,
                ..opts_default()
            };
            paths.push(calibration_path_for("SL 3", 0, &opts, dir));
        }
        assert_ne!(paths[0], paths[1]);
        assert_ne!(paths[1], paths[2]);
        assert_ne!(paths[0], paths[2]);
    }

    /// M5.4.4 regression: deck A's and deck B's calibration paths
    /// must be disjoint across every (format, deck_idx) combination
    /// the user can hit. The previous design borrowed deck A's
    /// thresholds for deck B silently — pin the per-deck split so
    /// that can't recur.
    #[test]
    fn calibration_path_distinct_per_deck_for_every_format() {
        let dir = std::path::Path::new("/tmp/dub-test");
        for format in [Format::SeratoCv02, Format::TraktorMk1, Format::TraktorMk2] {
            let opts = Opts {
                format,
                ..opts_default()
            };
            let a = calibration_path_for("SL 3", 0, &opts, dir);
            let b = calibration_path_for("SL 3", 1, &opts, dir);
            assert_ne!(
                a, b,
                "{format:?}: deck A path == deck B path; per-deck calibration is broken"
            );
        }
    }

    // M5.4.6 dropped legacy_calibration_path_for + the legacy
    // load-fallback; the runtime never reads disk on startup any
    // more. The historical `<device>_<format>.json` pattern is no
    // longer produced by any code path.

    #[test]
    fn deck_label_maps_index_to_letter() {
        assert_eq!(deck_label(0), 'A');
        assert_eq!(deck_label(1), 'B');
    }

    #[test]
    fn m543_auto_calibration_constants_are_fast() {
        // Lock the M5.4.3 single-phase wall-time tuning so a future
        // edit can't silently regress the always-fresh M5.4.6 path
        // back to the M5.4.2-era ~10 s carrier capture. (The M5.4.3
        // probe constants went away entirely with M5.4.6 — there's
        // no probe any more, so no `PROBE_SECS` to pin.)
        assert!(
            (AUTO_CARRIER_SECS - 3.0).abs() < 1e-9,
            "M5.4.3 AUTO_CARRIER_SECS must stay 3.0 (was 10.0)"
        );
    }

    #[test]
    fn parse_opts_explicit_thresholds_round_trip() {
        // Sanity check: --confidence on the CLI lands as
        // Some(_) (used to test the override path).
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let args = s(&[
            "--confidence",
            "0.93",
            "--amplitude-threshold",
            "0.04",
            "--input-channels",
            "3,4",
            "track.wav",
        ]);
        let opts = parse_opts(&args).unwrap();
        assert_eq!(opts.confidence, Some(0.93));
        assert_eq!(opts.amplitude_threshold, Some(0.04));
        assert!(opts.disengage.is_none());
        assert!(opts.sticky_blocks.is_none());
    }

    #[test]
    fn parse_opts_no_calibrate_flag() {
        // M5.4.6 reduced the calibration flag surface to a single
        // boolean: --no-calibrate. --recalibrate / --no-probe were
        // removed when the load-from-disk + fingerprint-probe path
        // was deleted. Pin the survivor so a future regression
        // ("oh let's add --recalibrate back") forces a deliberate
        // PRD revisit.
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let opts = parse_opts(&s(&["--no-calibrate", "--input-channels", "3,4", "t.wav"])).unwrap();
        assert!(opts.no_calibrate);
    }

    #[test]
    fn parse_opts_rejects_dropped_recalibrate_flag() {
        // --recalibrate was removed in M5.4.6 (always-fresh model).
        // The unknown-flag path should reject it cleanly so users
        // who pasted an old invocation get a useful error rather
        // than a silently-ignored flag.
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let r = parse_opts(&s(&["--recalibrate", "--input-channels", "3,4", "t.wav"]));
        assert!(r.is_err(), "removed --recalibrate should be rejected");
    }

    #[test]
    fn parse_opts_rejects_dropped_no_probe_flag() {
        // Same story as --recalibrate — --no-probe was the load-
        // fingerprint-but-skip-the-verification opt-in. The probe
        // is gone, so the flag is gone.
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let r = parse_opts(&s(&["--no-probe", "--input-channels", "3,4", "t.wav"]));
        assert!(r.is_err(), "removed --no-probe should be rejected");
    }

    // --- M5.5.2 output-routing resolution tests ------------------------

    #[test]
    fn resolve_output_routing_internal_mixer_flag() {
        let mut opts = opts_default();
        opts.internal_mixer = true;
        let r = resolve_output_routing(&dev("SL 3", 6), &routing_args_from_opts(&opts)).unwrap();
        assert_eq!(r.channels, 2);
        assert_eq!(r.routing, dub_engine::INTERNAL_MIXER_ROUTING);
        assert!(
            r.summary.contains("internal mixer") && r.summary.contains("debug-only"),
            "expected debug warning, got: {}",
            r.summary
        );
    }

    #[test]
    fn resolve_output_routing_manual_overrides() {
        let mut opts = opts_default();
        opts.deck_a_out_ch = Some(3);
        opts.deck_b_out_ch = Some(5);
        let r = resolve_output_routing(&dev("Mystery USB DAC", 6), &routing_args_from_opts(&opts))
            .unwrap();
        // Device has 6 channels by default → channels=6.
        assert_eq!(r.channels, 6);
        // 1-based → 0-based: 3 → 2, 5 → 4.
        assert_eq!(r.routing, [Some(2), Some(4)]);
        assert!(r.summary.contains("manual"), "got: {}", r.summary);
    }

    #[test]
    fn resolve_output_routing_manual_with_explicit_channels() {
        let mut opts = opts_default();
        opts.deck_a_out_ch = Some(1);
        opts.deck_b_out_ch = Some(3);
        opts.output_channels = Some(4);
        let r = resolve_output_routing(&dev("Mystery USB DAC", 8), &routing_args_from_opts(&opts))
            .unwrap();
        assert_eq!(r.channels, 4);
        assert_eq!(r.routing, [Some(0), Some(2)]);
    }

    #[test]
    fn resolve_output_routing_manual_oob_errors() {
        let mut opts = opts_default();
        opts.deck_a_out_ch = Some(5);
        opts.deck_b_out_ch = Some(7);
        // Device only has 4 channels; deck B at ch 7 doesn't fit.
        let r = resolve_output_routing(&dev("Mystery USB DAC", 4), &routing_args_from_opts(&opts));
        assert!(r.is_err(), "deck-b-out-ch=7 with 4ch device should error");
    }

    #[test]
    fn resolve_output_routing_auto_detects_sl3() {
        let opts = opts_default();
        let r =
            resolve_output_routing(&dev("Rane SL 3", 6), &routing_args_from_opts(&opts)).unwrap();
        assert_eq!(r.channels, 6);
        assert_eq!(r.routing, [Some(2), Some(4)]);
        assert!(r.summary.contains("Serato SL 3"), "got: {}", r.summary);
        assert!(
            !r.summary.contains("unverified"),
            "SL 3 is verified, should not warn: {}",
            r.summary
        );
    }

    #[test]
    fn resolve_output_routing_auto_detects_audio6_warns_unverified() {
        let opts = opts_default();
        let r = resolve_output_routing(&dev("Traktor Audio 6", 6), &routing_args_from_opts(&opts))
            .unwrap();
        assert_eq!(r.routing, [Some(0), Some(2)]);
        assert!(
            r.summary.contains("unverified"),
            "Audio 6 should warn until validated: {}",
            r.summary
        );
    }

    #[test]
    fn resolve_output_routing_unknown_device_falls_back_internal() {
        let opts = opts_default();
        let r = resolve_output_routing(
            &dev("MacBook Pro Speakers", 2),
            &routing_args_from_opts(&opts),
        )
        .unwrap();
        assert_eq!(r.channels, 2);
        assert_eq!(r.routing, dub_engine::INTERNAL_MIXER_ROUTING);
        assert!(
            r.summary.contains("unknown device") && r.summary.contains("internal mixer"),
            "expected fallback summary, got: {}",
            r.summary
        );
    }

    #[test]
    fn resolve_output_routing_device_profile_override() {
        let mut opts = opts_default();
        opts.device_profile = Some("SL 3".to_string());
        let r = resolve_output_routing(
            &dev("MacBook Pro Speakers", 2),
            &routing_args_from_opts(&opts),
        )
        .unwrap();
        // Profile says SL3 (output_channels=6); the AU can be opened
        // with N channels even if the physical device has fewer
        // (CoreAudio's default-output AU aggregates). User's own
        // footgun if it actually fails.
        assert_eq!(r.channels, 6);
        assert_eq!(r.routing, [Some(2), Some(4)]);
    }

    #[test]
    fn parse_opts_routing_flags_round_trip() {
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let opts = parse_opts(&s(&[
            "--deck-a-out-ch",
            "3",
            "--deck-b-out-ch",
            "5",
            "--output-channels",
            "6",
            "--input-channels",
            "3,4",
            "t.wav",
        ]))
        .unwrap();
        assert_eq!(opts.deck_a_out_ch, Some(3));
        assert_eq!(opts.deck_b_out_ch, Some(5));
        assert_eq!(opts.output_channels, Some(6));
    }

    #[test]
    fn parse_opts_internal_mixer_with_deck_flags_errors() {
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let r = parse_opts(&s(&[
            "--internal-mixer",
            "--deck-a-out-ch",
            "3",
            "--deck-b-out-ch",
            "5",
            "--input-channels",
            "3,4",
            "t.wav",
        ]));
        assert!(r.is_err(), "internal-mixer + deck flags must conflict");
    }

    #[test]
    fn parse_opts_partial_deck_flags_errors() {
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let r = parse_opts(&s(&[
            "--deck-a-out-ch",
            "3",
            "--input-channels",
            "3,4",
            "t.wav",
        ]));
        assert!(
            r.is_err(),
            "deck-a alone (without deck-b) must error to avoid asymmetric routing"
        );
    }

    /// Avoid unused-import lint when calibration types pull in.
    #[allow(dead_code)]
    fn _keep_schema_version_alive() {
        let _ = SCHEMA_VERSION;
    }

    // -------------------------------------------------------------
    // M5.6: two-deck timecode CLI + InputOptions builder
    // -------------------------------------------------------------

    #[test]
    fn parse_opts_two_deck_round_trip() {
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let opts = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--deck-b-input-channels",
            "5,6",
            "trackA.wav",
            "trackB.wav",
        ]))
        .expect("two-deck parse");
        assert_eq!(opts.track_a, PathBuf::from("trackA.wav"));
        assert_eq!(opts.track_b, Some(PathBuf::from("trackB.wav")));
        assert_eq!(
            opts.input.input_channels.as_deref(),
            Some(&[3_u32, 4_u32][..])
        );
        assert_eq!(
            opts.deck_b_input_channels.as_deref(),
            Some(&[5_u32, 6_u32][..])
        );
    }

    #[test]
    fn parse_opts_single_deck_still_works() {
        // Backward compat: M5.3 invocation must keep parsing with
        // track_b=None and deck_b_input_channels=None.
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let opts = parse_opts(&s(&["--input-channels", "3,4", "track.wav"])).expect("M5.3 parse");
        assert_eq!(opts.track_a, PathBuf::from("track.wav"));
        assert!(opts.track_b.is_none(), "no track B in single-deck mode");
        assert!(
            opts.deck_b_input_channels.is_none(),
            "no deck B input channels"
        );
    }

    #[test]
    fn parse_opts_track_b_without_deck_b_channels_errors() {
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let r = parse_opts(&s(&["--input-channels", "3,4", "trackA.wav", "trackB.wav"]));
        assert!(
            r.is_err(),
            "track B without --deck-b-input-channels must error (no way to drive transport)"
        );
    }

    #[test]
    fn parse_opts_deck_b_channels_without_track_b_errors() {
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let r = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--deck-b-input-channels",
            "5,6",
            "trackA.wav",
        ]));
        assert!(
            r.is_err(),
            "--deck-b-input-channels without track B must error (would be silent)"
        );
    }

    #[test]
    fn parse_opts_too_many_positionals_errors() {
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let r = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--deck-b-input-channels",
            "5,6",
            "a.wav",
            "b.wav",
            "c.wav",
        ]));
        assert!(r.is_err(), "3 tracks must error (only 1 or 2 supported)");
    }

    #[test]
    fn parse_opts_deck_b_channels_must_be_pair() {
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let r = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--deck-b-input-channels",
            "5,6,7",
            "a.wav",
            "b.wav",
        ]));
        assert!(
            r.is_err(),
            "stereo timecode needs exactly 2 channels per deck"
        );
    }

    #[test]
    fn parse_opts_deck_b_channels_zero_rejected() {
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let r = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--deck-b-input-channels",
            "0,1",
            "a.wav",
            "b.wav",
        ]));
        assert!(
            r.is_err(),
            "channel 0 is invalid (CLI flags are 1-based device indices)"
        );
    }

    #[test]
    fn build_input_options_single_deck_passthrough() {
        // Single-deck (no deck B) must produce the same options as
        // the existing M5.3 path — no `output_pairs`, channels=2,
        // channel_map = [2, 3] (1-based 3,4 → 0-based 2,3).
        let input = InputArgs {
            input_channels: Some(vec![3, 4]),
            ..InputArgs::default()
        };
        let opts = build_input_options(&input, None).expect("single-deck build");
        assert_eq!(opts.channels, 2);
        assert_eq!(opts.channel_map.as_deref(), Some(&[2_i32, 3_i32][..]));
        assert!(
            opts.output_pairs.is_none(),
            "single-deck must not set output_pairs (preserves M5.2 RT path)"
        );
    }

    #[test]
    fn build_input_options_two_deck_sl3_layout() {
        // SL3 reference layout: deck A on device 3+4, deck B on
        // device 5+6 → AU opens 4 channels, channel_map=[2,3,4,5],
        // output_pairs=[(0,1),(2,3)].
        let input = InputArgs {
            input_channels: Some(vec![3, 4]),
            ..InputArgs::default()
        };
        let opts = build_input_options(&input, Some(&[5, 6])).expect("two-deck build");
        assert_eq!(opts.channels, 4);
        assert_eq!(
            opts.channel_map.as_deref(),
            Some(&[2_i32, 3_i32, 4_i32, 5_i32][..]),
            "channel_map must be [a_l-1, a_r-1, b_l-1, b_r-1]"
        );
        assert_eq!(
            opts.output_pairs,
            Some(vec![(0_u32, 1_u32), (2_u32, 3_u32)]),
            "pairs must be (0,1) deck A and (2,3) deck B in logical AU coords"
        );
    }

    #[test]
    fn build_input_options_two_deck_overlap_errors() {
        // Deck A on 3+4 and Deck B on 4+5 share channel 4 — the
        // mixer can't physically route both decks to the same
        // input pair, so error early instead of producing a
        // silently-wrong calibration.
        let input = InputArgs {
            input_channels: Some(vec![3, 4]),
            ..InputArgs::default()
        };
        let r = build_input_options(&input, Some(&[4, 5]));
        assert!(r.is_err(), "overlapping deck pairs must error");
    }

    #[test]
    fn build_input_options_two_deck_requires_deck_a_channels() {
        // Asking for deck B's channels but not deck A's is
        // ambiguous — we don't know what to put on logical 0+1.
        // Force the user to be explicit.
        let input = InputArgs::default();
        let r = build_input_options(&input, Some(&[5, 6]));
        assert!(
            r.is_err(),
            "two-deck mode without --input-channels for deck A must error"
        );
    }

    #[test]
    fn build_input_options_two_deck_swapped_layout() {
        // Confirm the builder doesn't assume "deck A < deck B" —
        // a user with deck A on 5+6 and deck B on 3+4 (atypical
        // wiring) gets [4,5,2,3] which still gives the correct
        // logical layout: pair 0 = deck A.
        let input = InputArgs {
            input_channels: Some(vec![5, 6]),
            ..InputArgs::default()
        };
        let opts = build_input_options(&input, Some(&[3, 4])).expect("swapped build");
        assert_eq!(
            opts.channel_map.as_deref(),
            Some(&[4_i32, 5_i32, 2_i32, 3_i32][..])
        );
    }
}
