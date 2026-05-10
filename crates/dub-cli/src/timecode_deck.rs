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
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

use dub_audio::AudioInput;
use dub_engine::{
    Engine, TimecodeInputConfig, DEFAULT_AMPLITUDE_THRESHOLD, DEFAULT_CONFIDENCE_THRESHOLD,
    DEFAULT_DISENGAGE_THRESHOLD, DEFAULT_STICKY_BLOCKS_TO_DISENGAGE,
};
use dub_io::Track;
use dub_timecode::Format;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::calibrate::{measure_inline, probe_carrier};
use crate::calibration::{
    default_calibration_dir, Calibration, CalibrationThresholds, DEFAULT_FINGERPRINT_TOLERANCE,
};
use crate::device_profiles;
use crate::input_cmds::{parse_input_args, InputArgs};

/// Default duration if `--duration` isn't given. 60 s is comfortably
/// long for a tactile validation run; the user can Ctrl-C earlier.
const DEFAULT_RUN_SECS: f64 = 60.0;

/// Length of the auto-startup carrier probe used to validate the
/// saved fingerprint against the rig in front of the user. 3 s
/// gives stable percentiles within < 1 % of the long-run values
/// from the original calibration, so it's a meaningful comparison
/// without holding the user up.
const PROBE_SECS: f64 = 3.0;

/// Detection timeout for the auto-startup probe. 30 s gives the
/// user time to walk to the turntable and drop the needle without
/// the timecode-deck startup feeling rushed.
const PROBE_DETECT_TIMEOUT_SECS: f64 = 30.0;

/// Length of the auto-startup full calibration phases. Match
/// `dub calibrate`'s defaults — auto-calibration produces a
/// JSON file indistinguishable from a manual `dub calibrate` run.
const AUTO_CARRIER_SECS: f64 = 10.0;
const AUTO_LIFT_SECS: f64 = 5.0;
const AUTO_DETECT_TIMEOUT_SECS: f64 = 30.0;

/// Surface a warning when the saved calibration is older than this.
/// 30 days is long enough that "set up at home, played one gig two
/// weeks ago" doesn't trigger a warning, but short enough to flag
/// "dusty stylus from six months of disuse" before the user gets
/// surprised by ghost noise.
const STALE_CALIBRATION_DAYS: f64 = 30.0;

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
    /// Per-threshold explicit overrides. `None` = auto-resolve from
    /// the saved calibration (or auto-calibrate if missing /
    /// fingerprint mismatch); `Some(v)` = take this value verbatim,
    /// independent of calibration. Partial overrides are supported
    /// so the user can pin one knob (e.g. amplitude=0.05 to test a
    /// loud venue) and let the rest auto-resolve.
    confidence: Option<f32>,
    disengage: Option<f32>,
    sticky_blocks: Option<u32>,
    amplitude_threshold: Option<f32>,
    /// Force fresh full measurement even if a matching calibration
    /// JSON exists. Use after a known cartridge / cabling change.
    recalibrate: bool,
    /// Skip the fingerprint probe at startup. Faster (~3 s saved)
    /// but loses rig-swap detection. Use only when iterating on
    /// other things and you know the rig is unchanged.
    no_probe: bool,
    /// Skip calibration entirely — fall back to the M5.3 defaults
    /// regardless of what's on disk. Mostly useful for regression
    /// testing the M5.3 path or for first-time users who want to
    /// hear the deck immediately without touching the calibrator.
    no_calibrate: bool,
    /// Wall-clock duration to run before stopping. Distinct from
    /// `InputArgs::duration` because we want timecode-deck to default
    /// to 60 s, not the 5 s default of capture/levels.
    duration_secs: f64,
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
    let mut recalibrate = false;
    let mut no_probe = false;
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
            "--recalibrate" => {
                recalibrate = true;
                i += 1;
            }
            "--no-probe" => {
                no_probe = true;
                i += 1;
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
             [--output-buffer-size FRAMES] [--recalibrate] [--no-probe] [--no-calibrate] \
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
    if recalibrate && no_calibrate {
        return Err(anyhow!(
            "--recalibrate and --no-calibrate are mutually exclusive"
        ));
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
        duration_secs: input.duration.unwrap_or(DEFAULT_RUN_SECS),
        input,
        deck_b_input_channels,
        format,
        output_buffer_size,
        confidence,
        disengage,
        sticky_blocks,
        amplitude_threshold,
        recalibrate,
        no_probe,
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
    let mut engine = Engine::new(engine_sr, engine_block);
    println!(
        "engine:       sr={engine_sr} Hz block={engine_block} frames\n\
         output:       device sr={} Hz (target {engine_sr} Hz) buffer={} frames",
        device.sample_rate, device.buffer_frames,
    );

    // 4. Configure deck 0 with track A; in two-deck mode also load
    //    track B onto deck 1. Crucially we do NOT set_playing(true)
    //    on either deck — the decoder will do that per-deck on the
    //    first locked block (see `Engine::drive_timecode_inputs`).
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

    // 4b. Resolve thresholds: load saved calibration if present and
    //     fingerprint matches, auto-calibrate otherwise. CLI flag
    //     overrides applied last so partial overrides work
    //     (calibration handles 3 of 4 thresholds, user pins one).
    //
    //     Calibration must happen BEFORE we hand the input consumer
    //     to the engine, because the calibration helpers consume
    //     samples from the same input device. After
    //     `take_consumer()` is called, the input is engine-owned.
    //
    //     **Two-deck note (M5.6)**: calibration probes pair 0
    //     (deck A) only, and the same thresholds are reused for
    //     deck B. This is correct when both decks have matched
    //     cartridges (the common case). For mismatched cartridges,
    //     M5.4.4 will add per-deck named profiles; today the user
    //     can pass `--no-probe` and pin individual thresholds via
    //     `--confidence` etc. See ARCHITECTURE.md / M5.6.
    let resolved = resolve_thresholds(&mut input, &opts)?;

    // 5. Hand each input pair's consumer to its deck. Pair 0 always
    //    goes to engine deck 0 (deck A); pair 1, when present (M5.6
    //    two-deck mode), goes to deck 1.
    let rx_a = input
        .take_consumer_pair(0)
        .ok_or_else(|| anyhow!("AudioInput pair 0 consumer already taken"))?;
    let cfg = |format: Format| TimecodeInputConfig {
        format,
        input_sample_rate: input_sr,
        // CoreAudio output blocks vary; 4096 is a safe upper bound.
        max_block_frames: 4096,
        confidence_threshold: resolved.engage,
        disengage_threshold: resolved.disengage,
        sticky_blocks_to_disengage: resolved.sticky_blocks_to_disengage,
        amplitude_threshold: resolved.amplitude,
    };
    engine
        .attach_timecode_input(0, rx_a, cfg(opts.format))
        .context("attaching timecode input to deck 0")?;
    if two_deck {
        let rx_b = input
            .take_consumer_pair(1)
            .ok_or_else(|| anyhow!("AudioInput pair 1 consumer already taken"))?;
        engine
            .attach_timecode_input(1, rx_b, cfg(opts.format))
            .context("attaching timecode input to deck 1")?;
    }
    println!(
        "timecode:     format={} ({:.0} Hz carrier) engage={:.3} disengage={:.3} \
         sticky={} blocks amp_floor={:.4}{}",
        opts.format.cli_name(),
        opts.format.carrier_hz(),
        resolved.engage,
        resolved.disengage,
        resolved.sticky_blocks_to_disengage,
        resolved.amplitude,
        if two_deck {
            " (deck A + B share calibration; M5.4.4 will add per-deck profiles)"
        } else {
            ""
        },
    );

    // 6. Resolve output routing: known-device auto-detect, manual
    //    per-deck flags, or the M4 internal-mixer fallback. See
    //    `resolve_output_routing` for the full priority order.
    let routing = resolve_output_routing(&device, &opts)?;
    println!("{}", routing.describe());

    // 7. Move the engine onto the audio thread. From here, AudioOutput
    //    drives Engine::render_routed which drives the decoder which
    //    drives deck transport — no main-thread participation in the
    //    audio path.
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
    println!(
        "running for {:.1} s — drop the needle and play.",
        opts.duration_secs
    );
    println!("(Ctrl-C to stop early)");

    // 7. Sleep the wall-clock duration, sampling stats every 0.5 s so
    //    the user gets live feedback.
    let start = Instant::now();
    let total = Duration::from_secs_f64(opts.duration_secs);
    let mut next_tick = start + Duration::from_millis(500);
    while start.elapsed() < total {
        let now = Instant::now();
        if now >= next_tick {
            let cb = output.callback_count();
            let in_cb = input.callback_count();
            let in_of = input.overflow_count();
            print_stats(&output, &input, cb, in_cb, in_of);
            next_tick += Duration::from_millis(500);
        }
        // Coarse sleep — the polling rate above is ≥ 2 Hz.
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

/// Build [`dub_audio::InputOptions`] for the input AU, supporting
/// both single-deck (M5.3) and two-deck (M5.6) modes.
///
/// In single-deck mode this is a thin wrapper around
/// `InputArgs::to_options()` — the legacy path, untouched.
///
/// In two-deck mode (`deck_b_channels = Some([5, 6])`):
///
/// 1. The AU is opened with `channels = 4` (or however many we need
///    to span both decks' channels).
/// 2. `channel_map` is `[a_l-1, a_r-1, b_l-1, b_r-1]` — 0-based
///    device channel indices for the SL3-style "deck A on 3+4,
///    deck B on 5+6" layout.
/// 3. `output_pairs = [(0, 1), (2, 3)]` — both pairs are stereo
///    contiguous in the AU's logical (post-channel-map) frame, so
///    pair indices map cleanly to logical positions.
///
/// Validation: deck A and deck B input pairs must not overlap (a
/// shared channel between decks is almost always a bug).
fn build_input_options(
    input: &InputArgs,
    deck_b_channels: Option<&[u32]>,
) -> Result<dub_audio::InputOptions> {
    let mut opts = input.to_options();
    let Some(b) = deck_b_channels else {
        return Ok(opts);
    };
    let a = input.input_channels.as_deref().ok_or_else(|| {
        anyhow!("two-deck mode requires --input-channels for deck A (e.g. 3,4 for SL3 deck A)")
    })?;
    if a.len() != 2 {
        return Err(anyhow!(
            "two-deck mode requires --input-channels to be a pair (got {} channels)",
            a.len()
        ));
    }
    if b.len() != 2 {
        return Err(anyhow!(
            "--deck-b-input-channels must be a pair (got {} channels)",
            b.len()
        ));
    }
    let overlap = a.iter().any(|c| b.contains(c));
    if overlap {
        return Err(anyhow!(
            "--input-channels {a:?} and --deck-b-input-channels {b:?} share a channel; \
             each deck needs its own stereo pair (SL3: 3,4 + 5,6)"
        ));
    }
    // Combine into a 4-channel logical AU layout: [a_l, a_r, b_l, b_r],
    // converted from 1-based to 0-based.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let channel_map: Vec<i32> = a.iter().chain(b.iter()).map(|&c| (c as i32) - 1).collect();
    opts.channels = 4;
    opts.channel_map = Some(channel_map);
    opts.output_pairs = Some(vec![(0, 1), (2, 3)]);
    Ok(opts)
}

/// Resolved output routing. Captured ahead of `AudioOutput::start_with_options`
/// so we can print a clear "what we chose, and why" line before any
/// audio starts — saves the user from wondering why deck B is silent
/// on an unknown interface.
struct ResolvedOutputRouting {
    /// Total channels to open the AU with.
    channels: u32,
    /// Per-deck routing handed to `Engine::render_routed`.
    routing: dub_engine::OutputRouting,
    /// Human-readable summary, printed at startup.
    summary: String,
}

impl ResolvedOutputRouting {
    fn describe(&self) -> &str {
        &self.summary
    }
}

/// Resolve the M5.5.2 output routing in priority order:
///
/// 1. `--internal-mixer` → 2-ch internal mixer (debug only). Loud and
///    explicit; mutually exclusive with all other routing flags.
/// 2. Explicit `--deck-a-out-ch` + `--deck-b-out-ch` → manual routing
///    over `--output-channels` (or the device's reported channel
///    count). Most permissive — works for unknown devices.
/// 3. `--device-profile NAME` → look up the profile by exact pattern
///    and apply its routing. Useful when the system default is the
///    wrong device.
/// 4. Auto-detect by `device.device_name` against
///    `device_profiles::KNOWN_DEVICES`. The path users hit when they
///    plug in their SL3 and run `dub timecode-deck` with no flags.
/// 5. Fallback (unknown device, no flags) → 2-ch internal mixer with a
///    loud warning. Matches Serato's "preparation mode" semantics for
///    laptop-only situations: the user can hear playback but should
///    not run a live set.
fn resolve_output_routing(
    device: &dub_audio::DeviceInfo,
    opts: &Opts,
) -> Result<ResolvedOutputRouting> {
    if opts.internal_mixer {
        return Ok(ResolvedOutputRouting {
            channels: 2,
            routing: dub_engine::INTERNAL_MIXER_ROUTING,
            summary: "output routing: internal mixer (2 ch, both decks → ch 1+2)\n\
                 ⚠️  --internal-mixer is debug-only; not for live performance"
                .to_string(),
        });
    }

    if let (Some(a), Some(b)) = (opts.deck_a_out_ch, opts.deck_b_out_ch) {
        let a0 = device_profiles::one_based_to_zero_based(a)
            .ok_or_else(|| anyhow!("--deck-a-out-ch must be ≥ 1 (1-based), got {a}"))?;
        let b0 = device_profiles::one_based_to_zero_based(b)
            .ok_or_else(|| anyhow!("--deck-b-out-ch must be ≥ 1 (1-based), got {b}"))?;
        let channels = opts.output_channels.unwrap_or(device.channels);
        if channels < 2 {
            return Err(anyhow!(
                "--output-channels must be ≥ 2; got {channels} (device reports {} ch)",
                device.channels
            ));
        }
        if a0 + 2 > channels || b0 + 2 > channels {
            return Err(anyhow!(
                "deck-a-out-ch={a} or deck-b-out-ch={b} doesn't fit in {channels} channels \
                 (each deck takes 2 channels). Pass --output-channels N if your device has \
                 more outputs than the default detected."
            ));
        }
        return Ok(ResolvedOutputRouting {
            channels,
            routing: [Some(a0), Some(b0)],
            summary: format!(
                "output routing: manual ({} ch, deck A → ch {}+{}, deck B → ch {}+{})",
                channels,
                a,
                a + 1,
                b,
                b + 1,
            ),
        });
    }

    let profile = if let Some(pattern) = opts.device_profile.as_deref() {
        device_profiles::profile_by_pattern(pattern).ok_or_else(|| {
            anyhow!(
                "--device-profile {pattern:?} not found in known-device table; \
                 known patterns: {}",
                device_profiles::KNOWN_DEVICES
                    .iter()
                    .map(|d| d.name_pattern)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?
    } else if let Some(p) = device_profiles::match_device(&device.device_name) {
        p
    } else {
        // Unknown device, no manual routing — fall back to internal
        // mixer with a loud warning. Per the M5.5.2 design call: this
        // is preparation-mode-equivalent (the user can audition tracks
        // but the routing isn't right for an external mixer).
        return Ok(ResolvedOutputRouting {
            channels: 2,
            routing: dub_engine::INTERNAL_MIXER_ROUTING,
            summary: format!(
                "output routing: unknown device '{}' — falling back to internal mixer.\n\
                 ⚠️  no recognised interface profile; deck audio is summed to ch 1+2.\n\
                 ⚠️  for an external mixer, pass --deck-a-out-ch / --deck-b-out-ch (1-based) \
                 + --output-channels N, or --device-profile <name> if your interface is \
                 listed in the known-device table",
                device.device_name
            ),
        });
    };

    let channels = opts.output_channels.unwrap_or(profile.output_channels);
    if profile.deck_a_first_channel + 2 > channels || profile.deck_b_first_channel + 2 > channels {
        return Err(anyhow!(
            "device profile '{}' wants {} channels but --output-channels {} is too small",
            profile.display_name,
            profile.output_channels,
            channels
        ));
    }
    let verified_note = if profile.verified {
        ""
    } else {
        "\n⚠️  this profile is unverified against real hardware — double-check the routing"
    };
    Ok(ResolvedOutputRouting {
        channels,
        routing: [
            Some(profile.deck_a_first_channel),
            Some(profile.deck_b_first_channel),
        ],
        summary: format!(
            "output routing: {} ({} ch, deck A → ch {}+{}, deck B → ch {}+{}){}",
            profile.display_name,
            channels,
            profile.deck_a_first_channel + 1,
            profile.deck_a_first_channel + 2,
            profile.deck_b_first_channel + 1,
            profile.deck_b_first_channel + 2,
            verified_note,
        ),
    })
}

/// Resolve the four lift-policy thresholds from (in priority order):
///
/// 1. Explicit CLI overrides (`--confidence`, `--amplitude-threshold`,
///    `--disengage-threshold`, `--sticky-blocks`). Applied last so
///    a partial override always wins over the auto-resolved value.
/// 2. Saved calibration JSON, validated against the current rig
///    via a brief carrier probe. Mismatch (cartridge swap, preamp
///    change, …) triggers automatic recalibration.
/// 3. A fresh full calibration if no JSON exists yet, or
///    `--recalibrate` was passed.
/// 4. The M5.3 defaults if `--no-calibrate` was passed (or the user
///    cancels the calibration flow).
///
/// This is the entry point for the user's "auto-detect different
/// rigs" requirement: even if the same SL3 is used across cartridge
/// swaps, the fingerprint catches the change at startup and the
/// thresholds are re-derived in place.
///
/// **Format-aware (M6 fix).** The calibration JSON path keys off
/// `(device_name, opts.format)` so a user with both Serato and
/// Traktor records on the same SL3 keeps independent calibrations
/// per format. An earlier draft of M6 hardcoded `Format::SeratoCv02`
/// here — which silently loaded the Serato JSON when the user passed
/// `--format traktor-mk1`, then ran the carrier probe at the wrong
/// nominal frequency (1 kHz Serato vs the 2 kHz the MK1 record was
/// actually emitting), so the probe got `rate ≈ 2.0×` and timed out
/// trying to find unity-rate stability. Fallback then used Serato
/// thresholds against MK1 audio, which worked passably (similar
/// signal levels) but was not the right calibration. Pinned by
/// the regression test
/// `resolve_thresholds_uses_opts_format_for_calibration_path` below.
fn resolve_thresholds(input: &mut AudioInput, opts: &Opts) -> Result<CalibrationThresholds> {
    let format = opts.format;
    let dir = default_calibration_dir().context("resolving default calibration dir")?;
    let path = calibration_path_for(input.device_name(), opts, &dir);

    // Bypass-everything modes first.
    if opts.no_calibrate {
        println!("calibration: skipped (--no-calibrate); using M5.3 defaults");
        return Ok(apply_overrides(default_thresholds(), opts));
    }

    // Force-fresh path. Same as "no file exists" but ignores any
    // existing JSON. We still save the new measurement (overwrites
    // the old file), preserving the always-on "what is this rig"
    // record on disk.
    if opts.recalibrate {
        println!("calibration: --recalibrate forced; running fresh measurement");
        let cal = run_full_calibration(input, format)?;
        save_calibration(&cal, &path);
        return Ok(apply_overrides(cal.thresholds, opts));
    }

    // Try to load. Missing → run a fresh calibration.
    let cal = match Calibration::load(&path) {
        Ok(c) => c,
        Err(_) => {
            println!(
                "calibration: no JSON at {} — running first-time calibration",
                path.display()
            );
            let cal = run_full_calibration(input, format)?;
            save_calibration(&cal, &path);
            return Ok(apply_overrides(cal.thresholds, opts));
        }
    };

    let age_days = calibration_age_days(&cal.calibrated_at);
    if age_days > STALE_CALIBRATION_DAYS {
        eprintln!(
            "  ⚠ calibration is {age_days:.0} days old (>{:.0}); consider \
             `dub timecode-deck ... --recalibrate` for the current venue.",
            STALE_CALIBRATION_DAYS
        );
    }

    // Probe path. Skipping the probe gives faster startup but no
    // rig-swap detection — explicit opt-in via --no-probe.
    if opts.no_probe {
        println!(
            "calibration: loaded {} (probe skipped); engage={:.3} amp={:.4}",
            path.display(),
            cal.thresholds.engage,
            cal.thresholds.amplitude
        );
        return Ok(apply_overrides(cal.thresholds, opts));
    }

    println!(
        "calibration: loaded {} (calibrated {})",
        path.display(),
        cal.calibrated_at
    );
    let observed = match probe_carrier(input, format, PROBE_SECS, PROBE_DETECT_TIMEOUT_SECS) {
        Ok(fp) => fp,
        Err(e) => {
            eprintln!("  ⚠ probe failed: {e:#}\n  using saved thresholds without verification");
            return Ok(apply_overrides(cal.thresholds, opts));
        }
    };
    let delta = cal.fingerprint.max_relative_delta(&observed);
    if cal
        .fingerprint
        .matches(&observed, DEFAULT_FINGERPRINT_TOLERANCE)
    {
        println!(
            "  ✓ fingerprint matches (max delta {:.1}%); engage={:.3} amp={:.4}",
            delta * 100.0,
            cal.thresholds.engage,
            cal.thresholds.amplitude
        );
        return Ok(apply_overrides(cal.thresholds, opts));
    }

    // Mismatch — auto-recalibrate. The user explicitly requested
    // this behavior: "the user can always play on a new cartridge,
    // it must work."
    println!(
        "  ✗ fingerprint differs by {:.1}% (saved {:.4}/{:.4}/{:.3} vs observed \
         {:.4}/{:.4}/{:.3}) — recalibrating",
        delta * 100.0,
        cal.fingerprint.carrier_amp_p50,
        cal.fingerprint.carrier_amp_p95,
        cal.fingerprint.carrier_conf_p50,
        observed.carrier_amp_p50,
        observed.carrier_amp_p95,
        observed.carrier_conf_p50,
    );
    let new_cal = run_full_calibration(input, format)?;
    save_calibration(&new_cal, &path);
    Ok(apply_overrides(new_cal.thresholds, opts))
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
fn apply_overrides(base: CalibrationThresholds, opts: &Opts) -> CalibrationThresholds {
    CalibrationThresholds {
        engage: opts.confidence.unwrap_or(base.engage),
        disengage: opts.disengage.unwrap_or(base.disengage),
        amplitude: opts.amplitude_threshold.unwrap_or(base.amplitude),
        sticky_blocks_to_disengage: opts
            .sticky_blocks
            .unwrap_or(base.sticky_blocks_to_disengage),
    }
}

/// Run a full calibration against an open `AudioInput` and return
/// the populated [`Calibration`]. A wrapper around
/// [`measure_inline`] that pins the auto-startup defaults.
fn run_full_calibration(input: &mut AudioInput, format: Format) -> Result<Calibration> {
    println!();
    println!("=== auto-calibration ===");
    let cal = measure_inline(
        input,
        format,
        AUTO_CARRIER_SECS,
        AUTO_LIFT_SECS,
        AUTO_DETECT_TIMEOUT_SECS,
    )?;
    println!(
        "  derived: engage={:.3} disengage={:.3} amp={:.4} sticky={} (SNR {:.0}×)",
        cal.thresholds.engage,
        cal.thresholds.disengage,
        cal.thresholds.amplitude,
        cal.thresholds.sticky_blocks_to_disengage,
        cal.snr_margin,
    );
    println!("=== end calibration ===");
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
/// the given input device + run options. Pure — no I/O, no audio
/// device access — so the format-passthrough is unit-testable
/// without a hardware fixture.
///
/// **Why this is its own helper.** An earlier draft of M6 inlined
/// this logic inside `resolve_thresholds` and silently used
/// `Format::SeratoCv02` instead of `opts.format`, which loaded the
/// wrong calibration JSON and ran the carrier probe at the wrong
/// nominal frequency when the user passed `--format traktor-mk1`.
/// Extracting this lets us pin the contract ("the path always
/// derives from `opts.format`") with a fast unit test that doesn't
/// need a real audio device.
fn calibration_path_for(
    device_name: &str,
    opts: &Opts,
    dir: &std::path::Path,
) -> std::path::PathBuf {
    Calibration::path_for(device_name, opts.format, dir)
}

/// Difference in days between `calibrated_at` (RFC-3339) and now.
/// Returns 0.0 if `calibrated_at` is unparseable so the freshness
/// warning never spuriously fires for older / future-schema files.
fn calibration_age_days(calibrated_at: &str) -> f64 {
    let parsed = OffsetDateTime::parse(calibrated_at, &Rfc3339).ok();
    let Some(t) = parsed else {
        return 0.0;
    };
    let now = OffsetDateTime::now_utc();
    let dur = now - t;
    dur.as_seconds_f64() / 86_400.0
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
            recalibrate: false,
            no_probe: false,
            no_calibrate: false,
            internal_mixer: false,
            device_profile: None,
            output_channels: None,
            deck_a_out_ch: None,
            deck_b_out_ch: None,
            duration_secs: 0.0,
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

    #[test]
    fn calibration_age_days_recent_is_near_zero() {
        let now = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
        let age = calibration_age_days(&now);
        assert!(age.abs() < 0.01, "expected near 0, got {age}");
    }

    #[test]
    fn calibration_age_days_unparseable_is_zero() {
        // Garbage string should be treated as "fresh" — we'd rather
        // miss a freshness warning than spuriously cry wolf.
        let age = calibration_age_days("not-a-date");
        assert!(age.abs() < f64::EPSILON);
    }

    #[test]
    fn calibration_age_days_30_days_ago_returns_30() {
        let past = OffsetDateTime::now_utc() - time::Duration::days(30);
        let s = past.format(&Rfc3339).unwrap();
        let age = calibration_age_days(&s);
        // Tolerance for sub-second clock drift across the test's
        // own runtime.
        assert!((age - 30.0).abs() < 0.01, "expected ~30, got {age}");
    }

    /// Regression: an earlier draft of M6 hardcoded
    /// `Format::SeratoCv02` in `resolve_thresholds` instead of
    /// reading `opts.format`, so `dub timecode-deck --format
    /// traktor-mk1` silently loaded the Serato calibration JSON
    /// (`SL_3_serato-cv02.json`) and ran the carrier probe at the
    /// 1 kHz Serato carrier — which produced `rate ≈ 2.0×` against
    /// the actual 2 kHz MK1 carrier and timed the probe out. This
    /// test pins that the path always derives from `opts.format`.
    #[test]
    fn calibration_path_uses_opts_format_serato() {
        let dir = std::path::Path::new("/tmp/dub-test");
        let opts = Opts {
            format: Format::SeratoCv02,
            ..opts_default()
        };
        let p = calibration_path_for("SL 3", &opts, dir);
        assert!(
            p.to_string_lossy().contains("serato-cv02"),
            "expected serato-cv02 in path, got {}",
            p.display()
        );
    }

    #[test]
    fn calibration_path_uses_opts_format_traktor_mk1() {
        let dir = std::path::Path::new("/tmp/dub-test");
        let opts = Opts {
            format: Format::TraktorMk1,
            ..opts_default()
        };
        let p = calibration_path_for("SL 3", &opts, dir);
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
        let p = calibration_path_for("SL 3", &opts, dir);
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
        // Three formats × one device = three independent JSON files.
        // Pin the disjointness so a future refactor can't accidentally
        // share calibration across formats.
        let dir = std::path::Path::new("/tmp/dub-test");
        let mut paths = vec![];
        for format in [Format::SeratoCv02, Format::TraktorMk1, Format::TraktorMk2] {
            let opts = Opts {
                format,
                ..opts_default()
            };
            paths.push(calibration_path_for("SL 3", &opts, dir));
        }
        // All three must be distinct.
        assert_ne!(paths[0], paths[1]);
        assert_ne!(paths[1], paths[2]);
        assert_ne!(paths[0], paths[2]);
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
    fn parse_opts_recalibrate_flag() {
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let opts = parse_opts(&s(&["--recalibrate", "--input-channels", "3,4", "t.wav"])).unwrap();
        assert!(opts.recalibrate);
        assert!(!opts.no_probe);
        assert!(!opts.no_calibrate);
    }

    #[test]
    fn parse_opts_recalibrate_and_no_calibrate_conflict() {
        let s = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let r = parse_opts(&s(&[
            "--recalibrate",
            "--no-calibrate",
            "--input-channels",
            "3,4",
            "t.wav",
        ]));
        assert!(r.is_err(), "mutually-exclusive flags should error");
    }

    // --- M5.5.2 output-routing resolution tests ------------------------

    #[test]
    fn resolve_output_routing_internal_mixer_flag() {
        let mut opts = opts_default();
        opts.internal_mixer = true;
        let r = resolve_output_routing(&dev("SL 3", 6), &opts).unwrap();
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
        let r = resolve_output_routing(&dev("Mystery USB DAC", 6), &opts).unwrap();
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
        let r = resolve_output_routing(&dev("Mystery USB DAC", 8), &opts).unwrap();
        assert_eq!(r.channels, 4);
        assert_eq!(r.routing, [Some(0), Some(2)]);
    }

    #[test]
    fn resolve_output_routing_manual_oob_errors() {
        let mut opts = opts_default();
        opts.deck_a_out_ch = Some(5);
        opts.deck_b_out_ch = Some(7);
        // Device only has 4 channels; deck B at ch 7 doesn't fit.
        let r = resolve_output_routing(&dev("Mystery USB DAC", 4), &opts);
        assert!(r.is_err(), "deck-b-out-ch=7 with 4ch device should error");
    }

    #[test]
    fn resolve_output_routing_auto_detects_sl3() {
        let opts = opts_default();
        let r = resolve_output_routing(&dev("Rane SL 3", 6), &opts).unwrap();
        assert_eq!(r.channels, 6);
        assert_eq!(r.routing, [Some(2), Some(4)]); // deck A 3+4, deck B 5+6
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
        let r = resolve_output_routing(&dev("Traktor Audio 6", 6), &opts).unwrap();
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
        let r = resolve_output_routing(&dev("MacBook Pro Speakers", 2), &opts).unwrap();
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
        // User has the SL3 connected but their default output is the
        // built-in MacBook (oversight). --device-profile lets them
        // pin the routing without changing macOS audio settings.
        let mut opts = opts_default();
        opts.device_profile = Some("SL 3".to_string());
        let r = resolve_output_routing(&dev("MacBook Pro Speakers", 2), &opts).unwrap();
        // We pin the SL3 profile but the *device* only has 2 outputs
        // — that's an error; the user must also pass --output-channels
        // or fix their default device. Pin the error semantic.
        // Actually the user's profile says SL3 (output_channels=6),
        // and we don't override-check against the device, we just
        // open the AU with `channels`. The macOS default-output AU
        // can still be opened with N channels even if the underlying
        // device has fewer (the AU aggregates), so this is the
        // user's own footgun. We pass through and let CoreAudio
        // reject if it must.
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
