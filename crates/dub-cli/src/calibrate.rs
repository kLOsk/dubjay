//! `dub calibrate` — auto-detecting per-rig calibration (M5.4.2 +
//! M5.4.3 speed pass).
//!
//! ## Default flow (single-phase, M5.4.3)
//!
//! One phase, no manual prompts:
//!
//! 1. **Carrier.** Wait until the input shows a stable carrier (high
//!    confidence, near-unity rate, for ≥ [`STABLE_BLOCKS`]
//!    consecutive blocks). Then capture [`Opts::carrier_secs`]
//!    seconds of measurement. From P5/P50/P95 of amplitude +
//!    confidence we derive thresholds via
//!    [`crate::calibration::derive_thresholds`], stamp a
//!    [`RigFingerprint`] for future cartridge-swap detection, and
//!    write JSON to `~/.dub/calibration/<device>_deck_<idx>_<format>.json`.
//!
//! Lift stats are persisted as zeros for schema compatibility (and
//! the loader already tolerates that — see
//! [`crate::calibration::derive_thresholds`]'s SNR-skip path). The
//! M5.4.3 hand-tuning analysis showed the carrier shape carries the
//! threshold info: `amplitude = carrier_p5 * 0.5` matched the
//! user's hand-found SL3 threshold within 1 % regardless of lift
//! noise level. Lift was always the SNR safety net, never the
//! signal source.
//!
//! ## Two-phase flow (`--two-phase`, legacy / diagnostic)
//!
//! `dub calibrate --two-phase` runs the M5.4.2 sequence: carrier,
//! then **lift** (wait for stylus-up signature; capture
//! [`Opts::lift_secs`] seconds), then derive with the SNR safety
//! check enabled. Slower (≈ 25 s vs ≈ 5 s on a typical rig) but
//! catches stylus / preamp / cabling problems that produce low
//! SNR. Useful when troubleshooting "calibration succeeded but
//! the deck won't engage at runtime".
//!
//! ## Why auto-detection rather than "press Enter"
//!
//! The user requested the lowest-friction calibration possible —
//! Traktor-style "press calibrate, do the thing". The decoder
//! already gives us everything we need to detect "the user is
//! showing me carrier" vs "the user is showing me silence", so
//! auto-progression is straightforward and the UX is genuinely
//! zero-input: drop the needle → wait ~3 s → done.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use dub_audio::AudioInput;
use dub_timecode::{Decoder, Format};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::calibration::{
    default_calibration_dir, derive_thresholds, format_string, measurement_stats_from_samples,
    snr_margin, Calibration, CalibrationMeasurements, MeasurementStats, RigFingerprint,
    SCHEMA_VERSION, SNR_FAIL_THRESHOLD, SNR_WARN_THRESHOLD,
};
use crate::input_cmds::{parse_input_args, InputArgs};

/// Block size used by the calibrator. Pinned to match
/// `dub timecode-deck` — the lift policy's sticky window is
/// block-counted, and we want the calibration thresholds to
/// produce identical behavior at playback time. See
/// [`crate::scope`] for the same rationale.
const BLOCK_FRAMES: usize = 256;

/// Carrier-detection threshold: confidence the decoder must report
/// to consider the current block "carrier present" (M5.4.3 tightened
/// from 0.85 → 0.90 so [`STABLE_BLOCKS`] at 2 is unambiguous).
/// 0.90 is well above the ~0.85 ceiling that handling/rumble noise
/// can briefly produce, and clean rigs sit at 0.97-0.99 carrier
/// confidence so detection still triggers within ~10 ms of needle
/// drop. The user's deck-B SL3 carrier_conf_p5 ≈ 0.96 passes this
/// criterion comfortably.
const CARRIER_DETECT_CONF: f32 = 0.90;

/// Carrier-detection rate band: |rate - 1.0| ≤ this. 0.10 allows for
/// slight pitch fader off-zero (typical turntable users keep ±2 % at
/// rest, well within the band) and platter spin-up wobble.
const CARRIER_DETECT_RATE_BAND: f64 = 0.10;

/// Number of consecutive blocks meeting the carrier criteria before
/// we declare "stable carrier" (M5.4.3 reduced from 5 → 2). 2 blocks
/// @ 256 frames @ 48 kHz ≈ 11 ms — fast enough to feel instant on a
/// known-good rig. Safe at 2 because [`CARRIER_DETECT_CONF`] was
/// tightened to 0.90 in the same pass: 2 consecutive blocks of
/// `conf ≥ 0.90 && |rate-1| < 0.10` cannot be produced by any of
/// the M5.3 / M5.4.1 / M5.4.4 captured noise patterns (handling,
/// dust ticks, brief stylus touches) — the rate gate alone catches
/// transient stylus motion because handling produces near-zero or
/// wildly varying rate, never the unity rate of a steady spin.
const STABLE_BLOCKS: u32 = 2;

/// Lift-detection amplitude threshold (RMS). Carriers through any
/// reasonable rig sit ≥ 0.05; lift drops to ~0.0001-0.005 depending
/// on ambient. 0.005 is a comfortably conservative cutoff above
/// almost any handling/rumble noise.
const LIFT_DETECT_AMP: f32 = 0.005;

/// Number of consecutive blocks below [`LIFT_DETECT_AMP`] before we
/// declare "lift detected". 10 blocks ≈ 53 ms. Ignores brief drop-
/// outs (dust ticks, brief inter-track gap on the record).
const LIFT_BLOCKS: u32 = 10;

/// Default timeout for each detection phase used by the standalone
/// `dub calibrate` command. After this long without hitting the
/// criteria, the calibrator aborts with a clear error — almost always
/// the user forgot to drop the needle, or the wrong channels are
/// configured.
///
/// **Not used by `dub timecode-deck`** (M5.4.5): startup-time
/// calibrators run with [`MeasureOptions::detect_timeout_secs`]
/// = `None` so the deck-B calibrator can wait indefinitely during a
/// DJ takeover (the incoming DJ may not get access to deck B for many
/// minutes after the app is launched).
const DETECT_TIMEOUT_SECS: f64 = 30.0;

/// Status-line refresh rate during waits + captures. 4 Hz is
/// readable + low-CPU; rolling carriage-return overwrites the
/// previous line so the terminal stays clean.
const STATUS_REFRESH_MS: u64 = 250;

/// Parsed `dub calibrate` options.
#[derive(Debug, Clone)]
struct Opts {
    input: InputArgs,
    format: Format,
    /// Engine deck index this calibration is for (0 = deck A, 1 =
    /// deck B). Defaults to 0 — `dub calibrate` opens a single
    /// 2-channel input and probes pair 0 regardless. The `deck`
    /// number is purely for the on-disk filename + the
    /// `deck_index` field inside the JSON, so a user calibrating
    /// deck B's separate hardware (different cartridge, different
    /// turntable) on the same SL3 picks up the right file at
    /// `dub timecode-deck` startup. See M5.4.4 in the PRD.
    deck: u32,
    /// Length of the carrier capture phase. Default 3.0 s (M5.4.3
    /// — see module docs); long enough for stable percentiles
    /// within < 1 % of the M5.4.2-era 10 s capture, short enough
    /// to feel responsive.
    carrier_secs: f64,
    /// Length of the lift capture phase. Used only when
    /// [`Opts::two_phase`] is `true`. Default 5.0 s preserved
    /// from M5.4.2 for comparable two-phase output.
    lift_secs: f64,
    /// Detection timeout for both waits. Always `Some` for the
    /// standalone `dub calibrate` command (default
    /// [`DETECT_TIMEOUT_SECS`]); `dub timecode-deck` startup
    /// calibrators set this to `None` for the takeover use case
    /// (M5.4.5).
    detect_timeout_secs: Option<f64>,
    /// Override the on-disk save location. `None` = standard
    /// `~/.dub/calibration/<device_key>.json` path.
    output: Option<PathBuf>,
    /// Skip the save step (compute + print results only). Useful
    /// when iterating on threshold formulas without polluting the
    /// real calibration dir.
    no_save: bool,
    /// Run the legacy carrier+lift two-phase calibration (M5.4.2
    /// flow) instead of the M5.4.3 single-phase default. Slower
    /// (≈ 25 s vs ≈ 5 s on a typical rig) but enables the SNR
    /// safety check in [`derive_thresholds`] — useful when the
    /// rig is suspected of having a stylus / preamp / cabling
    /// problem that single-phase wouldn't catch.
    two_phase: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            input: InputArgs::default(),
            format: Format::SeratoCv02,
            deck: 0,
            carrier_secs: DEFAULT_CARRIER_SECS,
            lift_secs: DEFAULT_LIFT_SECS,
            detect_timeout_secs: Some(DETECT_TIMEOUT_SECS),
            output: None,
            no_save: false,
            two_phase: false,
        }
    }
}

/// Default carrier capture duration. M5.4.3 reduced this from 10.0 s
/// (M5.4.2 era) to 3.0 s after fixture analysis showed the long-run
/// percentiles converge within < 1 % by ≈ 2 s on a steady spin. We
/// keep 3 s as the published default for a small safety margin.
const DEFAULT_CARRIER_SECS: f64 = 3.0;

/// Default lift capture duration in `--two-phase` mode. Preserved
/// from M5.4.2 so two-phase output is directly comparable to old
/// calibration files. Not used in the M5.4.3 single-phase default.
const DEFAULT_LIFT_SECS: f64 = 5.0;

fn parse_opts(args: &[String]) -> Result<Opts> {
    // Reuse the shared input-args parser (--device, --input-channels,
    // --sr, --buffer-size). `--duration` from the shared parser is
    // ignored here; calibrate's per-phase durations come from
    // --carrier-secs / --lift-secs instead.
    let (input_args, leftover) = parse_input_args(args)?;
    let mut opts = Opts {
        input: input_args,
        ..Opts::default()
    };

    let mut iter = leftover.into_iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--carrier-secs" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("--carrier-secs expects a number"))?;
                opts.carrier_secs = v
                    .parse::<f64>()
                    .with_context(|| format!("--carrier-secs {v}"))?;
            }
            "--lift-secs" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("--lift-secs expects a number"))?;
                opts.lift_secs = v
                    .parse::<f64>()
                    .with_context(|| format!("--lift-secs {v}"))?;
            }
            "--detect-timeout" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("--detect-timeout expects a number"))?;
                let parsed = v
                    .parse::<f64>()
                    .with_context(|| format!("--detect-timeout {v}"))?;
                opts.detect_timeout_secs = Some(parsed);
            }
            "--format" => {
                let v = iter.next().ok_or_else(|| {
                    anyhow!("--format expects 'serato-cv02', 'traktor-mk1', or 'traktor-mk2'")
                })?;
                opts.format = Format::from_cli_arg(v.as_str()).ok_or_else(|| {
                    anyhow!(
                        "unknown --format '{v}' (supported: serato-cv02, traktor-mk1, traktor-mk2)"
                    )
                })?;
            }
            "-o" | "--output" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("--output expects a path"))?;
                opts.output = Some(PathBuf::from(v));
            }
            "--deck" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("--deck expects 0 or 1"))?;
                let deck = v.parse::<u32>().with_context(|| format!("--deck {v}"))?;
                if deck >= 2 {
                    return Err(anyhow!(
                        "--deck must be 0 or 1 (engine has 2 decks today); got {deck}"
                    ));
                }
                opts.deck = deck;
            }
            "--no-save" => {
                opts.no_save = true;
            }
            "--two-phase" => {
                // M5.4.3 opt-out: run the legacy carrier+lift flow
                // with the SNR safety check. Slower; rarely needed.
                opts.two_phase = true;
            }
            other if other.starts_with("--") => {
                return Err(anyhow!("unknown calibrate flag: {other}"));
            }
            other => {
                return Err(anyhow!("unexpected positional arg: {other}"));
            }
        }
    }

    if opts.carrier_secs <= 0.0 || opts.lift_secs <= 0.0 {
        return Err(anyhow!(
            "--carrier-secs and --lift-secs must be > 0 (got {} and {})",
            opts.carrier_secs,
            opts.lift_secs
        ));
    }
    if matches!(opts.detect_timeout_secs, Some(t) if t <= 0.0) {
        return Err(anyhow!("--detect-timeout must be > 0"));
    }
    Ok(opts)
}

/// `dub calibrate [--input-channels N,M] [...]` — entry point.
///
/// # Errors
/// Audio-device open failures, detection timeouts, low-SNR rigs
/// (likely cartridge / cabling issue), JSON write failures.
pub fn run(args: &[String]) -> Result<()> {
    let opts = parse_opts(args)?;
    let mut input =
        AudioInput::start_with_options(&opts.input.to_options()).context("opening input device")?;
    let channels = input.channels() as usize;
    if channels < 2 {
        return Err(anyhow!(
            "calibrate requires a stereo input (got {channels} channel{}); \
             use --input-channels N,M to pick L,R",
            if channels == 1 { "" } else { "s" }
        ));
    }

    println!(
        "calibrating: device='{}' sr={} Hz channels={channels} deck={} format={} mode={}",
        input.device_name(),
        input.sample_rate(),
        opts.deck,
        format_string(opts.format),
        if opts.two_phase {
            "two-phase (carrier + lift, M5.4.2 legacy)"
        } else {
            "single-phase (carrier only, M5.4.3 default)"
        },
    );
    println!();

    // M5.4.5 plumbing: the calibrator now takes a HeapCons<f32>
    // directly (so it can run on a worker thread without holding a
    // borrow on the AudioInput). `dub calibrate` opens a dedicated
    // 2-channel AudioInput so pair 0 is the only consumer; we lift
    // it out and hand it to `measure_inline`. The `--deck` flag
    // still controls *only* the on-disk metadata so a user
    // calibrating deck B's separate hardware on the same SL3
    // (different cartridge, different turntable) gets a
    // `..._deck_1_<format>.json` file that `dub timecode-deck`
    // will pick up automatically.
    let inputs = MeasurementInputs {
        device_name: input.device_name().to_string(),
        input_sample_rate: input.sample_rate(),
        deck_index: opts.deck,
        format: opts.format,
    };
    let mut consumer = input
        .take_consumer_pair(0)
        .ok_or_else(|| anyhow!("AudioInput pair 0 consumer already taken"))?;
    let cal = measure_inline(
        &mut consumer,
        &inputs,
        MeasureOptions {
            carrier_secs: opts.carrier_secs,
            lift_secs: opts.lift_secs,
            detect_timeout_secs: opts.detect_timeout_secs,
            two_phase: opts.two_phase,
        },
    )?;

    print_summary(&cal);

    if opts.no_save {
        println!();
        println!("--no-save set — results not persisted.");
        return Ok(());
    }

    let path = match opts.output {
        Some(p) => p,
        None => Calibration::path_for(
            input.device_name(),
            opts.deck,
            opts.format,
            &default_calibration_dir().context("resolving default calibration dir")?,
        ),
    };
    cal.save(&path)
        .with_context(|| format!("saving calibration to {}", path.display()))?;

    println!();
    println!("saved → {}", path.display());
    Ok(())
}

/// Per-call options for [`measure_inline`]. Bundled to keep the
/// function signature stable across the M5.4.2 (two-phase) →
/// M5.4.3 (single-phase default) → M5.4.5 (parallel calibrators)
/// → future tuning passes.
#[derive(Debug, Clone, Copy)]
pub struct MeasureOptions {
    pub carrier_secs: f64,
    /// Used only when `two_phase == true`. Ignored otherwise.
    pub lift_secs: f64,
    /// Detection-phase timeout. `Some(secs)` aborts each wait
    /// (`wait_for_stable_carrier`, `wait_for_lift`) after `secs`
    /// of no-progress; `None` waits indefinitely.
    ///
    /// `dub calibrate` always passes `Some(30.0)` so a forgotten
    /// needle doesn't hang the CLI. `dub timecode-deck` startup
    /// calibrators (M5.4.5) pass `None` to support the DJ-takeover
    /// flow: deck B's calibrator may sit waiting for many minutes
    /// until the previous DJ vacates the turntable.
    pub detect_timeout_secs: Option<f64>,
    /// `false` (M5.4.3 default) → carrier-only single-phase.
    /// `true`  → legacy carrier+lift two-phase with SNR check.
    pub two_phase: bool,
}

/// Bundle of metadata the calibrator needs for the on-disk
/// [`Calibration`] record. Pulled from the [`AudioInput`] by the
/// caller and passed through; M5.4.5 introduced this split so
/// [`measure_inline`] can run on a worker thread that *doesn't*
/// own the [`AudioInput`] (only the [`HeapCons`] consumer end).
///
/// The fields here are the bare minimum needed by the calibration
/// JSON schema — the audio data flows through `consumer` separately.
#[derive(Debug, Clone)]
pub struct MeasurementInputs {
    /// Hardware label (e.g. `"SL 3"`) — recorded in
    /// [`Calibration::device_name`] for diagnostics.
    pub device_name: String,
    /// Engine sample rate. The decoder is built at this SR;
    /// downstream consumers expect input audio to match.
    pub input_sample_rate: f32,
    /// Engine deck index this calibration is for (0 = A, 1 = B).
    /// Written to [`Calibration::deck_index`] and the on-disk
    /// filename key.
    pub deck_index: u32,
    /// Timecode format being calibrated. The decoder is built for
    /// this format; the calibration result is per-format because
    /// different vinyl have different reference amplitudes.
    pub format: Format,
}

/// Run a calibration on a [`HeapCons<f32>`] consumer end of an
/// input ringbuffer. Returns the fully populated [`Calibration`]
/// without saving — the caller decides where (and whether) to
/// persist.
///
/// **M5.4.5 signature change.** Pre-M5.4.5 this took
/// `&mut AudioInput + pair_idx`; the exclusive borrow forced
/// sequential calibration (only one calibrator at a time can hold
/// the AudioInput). Now it takes a [`HeapCons<f32>`] for one pair
/// directly, so two calibrators on two different consumers can
/// run on two different threads with no shared mutable state.
/// The [`MeasurementInputs`] bundle carries the metadata that
/// previously came off `AudioInput` (device name, SR, etc.). The
/// caller — `dub calibrate` for the single-deck case,
/// `dub timecode-deck` startup for the two-deck case — owns the
/// [`AudioInput`], pulls these values off it once, and hands them
/// to each calibrator thread alongside its consumer.
///
/// In single-phase mode (M5.4.3 default,
/// [`MeasureOptions::two_phase`] = false), only the carrier phase
/// runs; lift stats are written as [`MeasurementStats::zero()`]
/// for schema compatibility. In two-phase mode
/// ([`MeasureOptions::two_phase`] = true), the legacy M5.4.2 flow
/// runs (carrier + lift) and the SNR safety net rejects rigs
/// below [`SNR_FAIL_THRESHOLD`].
///
/// Used by both `dub calibrate` (saves to
/// `~/.dub/calibration/...`) and `dub timecode-deck`'s startup
/// parallel-calibration path (saves to the same location, but the
/// user didn't explicitly ask for it). Sharing this code path
/// means "you got auto-calibrated" produces a JSON file
/// indistinguishable from "you ran `dub calibrate`".
///
/// # Errors
/// Detection timeout (only when
/// [`MeasureOptions::detect_timeout_secs`] = `Some`), 0-block
/// capture, or — in two-phase mode only — SNR below
/// [`SNR_FAIL_THRESHOLD`] (rejected by [`derive_thresholds`]).
pub fn measure_inline(
    consumer: &mut ringbuf::HeapCons<f32>,
    inputs: &MeasurementInputs,
    opts: MeasureOptions,
) -> Result<Calibration> {
    let sr = inputs.input_sample_rate;
    let mut decoder = Decoder::new(inputs.format, sr);

    let step_label = if opts.two_phase {
        "step 1/2: carrier"
    } else {
        "step 1/1: carrier"
    };
    println!("{step_label}  —  spin the record at 33\u{2153} on a clean section");
    wait_for_stable_carrier(consumer, &mut decoder, opts.detect_timeout_secs)?;
    println!();
    let (carrier_amps, carrier_confs) =
        capture_phase(consumer, &mut decoder, opts.carrier_secs, "carrier")?;
    println!();

    let mut carrier_amps_owned = carrier_amps;
    let mut carrier_confs_owned = carrier_confs;
    let carrier = measurement_stats_from_samples(&mut carrier_amps_owned, &mut carrier_confs_owned);

    let lift = if opts.two_phase {
        println!("step 2/2: lift     —  lift the needle off and rest it");
        wait_for_lift(consumer, &mut decoder, opts.detect_timeout_secs)?;
        println!();
        let (lift_amps, lift_confs) =
            capture_phase(consumer, &mut decoder, opts.lift_secs, "lift")?;
        println!();
        let mut lift_amps_owned = lift_amps;
        let mut lift_confs_owned = lift_confs;
        measurement_stats_from_samples(&mut lift_amps_owned, &mut lift_confs_owned)
    } else {
        // Single-phase: lift is unmeasured, persist zeros for schema
        // compatibility. `n_blocks == 0` is the load-bearing signal
        // to derive_thresholds that the SNR check should be skipped
        // (it was always a lift-derived safety net, never the
        // signal source — see calibration.rs module docs).
        MeasurementStats::zero()
    };

    let snr = snr_margin(&carrier, &lift);
    let Some(thresholds) = derive_thresholds(&carrier, &lift) else {
        // Only reachable in two-phase mode; single-phase always
        // returns `Some` because `lift.n_blocks == 0`.
        return Err(anyhow!(
            "low SNR ({:.1}\u{00d7} < {:.0}\u{00d7} minimum) — likely cartridge, stylus, \
             or cabling problem. Check the needle, preamp gain, and that \
             you've selected the right input channels (SL3 deck A is 3,4).\n\
             carrier amp_p5 = {:.4}, lift amp_p95 = {:.4}",
            snr,
            SNR_FAIL_THRESHOLD,
            carrier.amplitude_p5,
            lift.amplitude_p95,
        ));
    };

    let fingerprint = RigFingerprint {
        carrier_amp_p50: carrier.amplitude_p50,
        carrier_amp_p95: carrier.amplitude_p95,
        carrier_conf_p50: carrier.confidence_p50,
    };

    let calibrated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("formatting calibration timestamp")?;

    Ok(Calibration {
        schema_version: SCHEMA_VERSION,
        device_name: inputs.device_name.clone(),
        deck_index: inputs.deck_index,
        format: format_string(inputs.format).to_string(),
        calibrated_at,
        input_sample_rate: sr,
        #[allow(clippy::cast_possible_truncation)]
        block_frames: BLOCK_FRAMES as u32,
        fingerprint,
        thresholds,
        measurements: CalibrationMeasurements { carrier, lift },
        snr_margin: snr,
    })
}

/// Wait until [`STABLE_BLOCKS`] consecutive blocks meet the carrier-
/// detection criteria, or `timeout_secs` elapses (when `Some`).
///
/// While waiting, prints a rolling 4 Hz status line so the user
/// sees what the decoder is observing in real time.
fn wait_for_stable_carrier(
    consumer: &mut ringbuf::HeapCons<f32>,
    decoder: &mut Decoder,
    timeout_secs: Option<f64>,
) -> Result<()> {
    use ringbuf::traits::Consumer as _;

    let mut consecutive: u32 = 0;
    let block_samples = BLOCK_FRAMES * 2;
    let mut acc = vec![0_f32; block_samples * 4];
    let mut acc_len = 0_usize;

    let start = Instant::now();
    let timeout = timeout_secs.map(Duration::from_secs_f64);
    let mut last_status = Instant::now() - Duration::from_millis(STATUS_REFRESH_MS);

    let mut last_amp = 0.0_f32;
    let mut last_conf = 0.0_f32;
    let mut last_rate = 0.0_f64;

    loop {
        if let Some(t) = timeout {
            if start.elapsed() > t {
                eprintln!();
                return Err(anyhow!(
                    "no stable carrier detected within {} s.\n\
                     Common causes: needle not on the record, wrong --input-channels \
                     (SL3 deck A is 3,4), or platter not spinning at unity. \
                     Last seen: conf={last_conf:.2} rate={last_rate:+.3} amp={last_amp:.4}",
                    t.as_secs_f64()
                ));
            }
        }
        let space = acc.len() - acc_len;
        if space > 0 {
            let n = consumer.pop_slice(&mut acc[acc_len..acc_len + space]);
            acc_len += n;
        }
        while acc_len >= block_samples {
            let out = decoder.process(&acc[..block_samples]);
            last_amp = out.amplitude;
            last_conf = out.confidence;
            last_rate = out.rate;

            let stable = out.confidence >= CARRIER_DETECT_CONF
                && (out.rate - 1.0).abs() < CARRIER_DETECT_RATE_BAND;
            if stable {
                consecutive = consecutive.saturating_add(1);
                if consecutive >= STABLE_BLOCKS {
                    print_status_line(&format!(
                        "  \u{2713} stable carrier (conf={last_conf:.2} rate={last_rate:+.3}× amp={last_amp:.4})",
                    ));
                    return Ok(());
                }
            } else {
                consecutive = 0;
            }
            acc.copy_within(block_samples..acc_len, 0);
            acc_len -= block_samples;
        }
        if last_status.elapsed() >= Duration::from_millis(STATUS_REFRESH_MS) {
            print_status_line(&format!(
                "  \u{231b} waiting for carrier... [conf={last_conf:.2} rate={last_rate:+.3}× amp={last_amp:.4}] {}/{}",
                consecutive, STABLE_BLOCKS,
            ));
            last_status = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Wait until [`LIFT_BLOCKS`] consecutive blocks have amplitude
/// below [`LIFT_DETECT_AMP`], or `timeout_secs` elapses (when `Some`).
fn wait_for_lift(
    consumer: &mut ringbuf::HeapCons<f32>,
    decoder: &mut Decoder,
    timeout_secs: Option<f64>,
) -> Result<()> {
    use ringbuf::traits::Consumer as _;

    let mut consecutive: u32 = 0;
    let block_samples = BLOCK_FRAMES * 2;
    let mut acc = vec![0_f32; block_samples * 4];
    let mut acc_len = 0_usize;

    let start = Instant::now();
    let timeout = timeout_secs.map(Duration::from_secs_f64);
    let mut last_status = Instant::now() - Duration::from_millis(STATUS_REFRESH_MS);

    let mut last_amp = 0.0_f32;

    loop {
        if let Some(t) = timeout {
            if start.elapsed() > t {
                eprintln!();
                return Err(anyhow!(
                    "no lift detected within {} s.\n\
                     Lift the stylus off the record and place it on its rest. \
                     Last seen amp = {last_amp:.4}",
                    t.as_secs_f64()
                ));
            }
        }
        let space = acc.len() - acc_len;
        if space > 0 {
            let n = consumer.pop_slice(&mut acc[acc_len..acc_len + space]);
            acc_len += n;
        }
        while acc_len >= block_samples {
            let out = decoder.process(&acc[..block_samples]);
            last_amp = out.amplitude;

            if out.amplitude < LIFT_DETECT_AMP {
                consecutive = consecutive.saturating_add(1);
                if consecutive >= LIFT_BLOCKS {
                    print_status_line(&format!("  \u{2713} lift detected (amp={last_amp:.4})",));
                    return Ok(());
                }
            } else {
                consecutive = 0;
            }
            acc.copy_within(block_samples..acc_len, 0);
            acc_len -= block_samples;
        }
        if last_status.elapsed() >= Duration::from_millis(STATUS_REFRESH_MS) {
            print_status_line(&format!(
                "  \u{231b} waiting for lift... [amp={last_amp:.4}] {}/{}",
                consecutive, LIFT_BLOCKS,
            ));
            last_status = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Capture `secs` seconds of decoder output, returning per-block
/// amplitude + confidence vectors. Print a rolling progress
/// indicator at 4 Hz.
fn capture_phase(
    consumer: &mut ringbuf::HeapCons<f32>,
    decoder: &mut Decoder,
    secs: f64,
    label: &str,
) -> Result<(Vec<f32>, Vec<f32>)> {
    use ringbuf::traits::Consumer as _;

    let block_samples = BLOCK_FRAMES * 2;
    let mut acc = vec![0_f32; block_samples * 4];
    let mut acc_len = 0_usize;

    let mut amps = Vec::<f32>::new();
    let mut confs = Vec::<f32>::new();

    let start = Instant::now();
    let dur = Duration::from_secs_f64(secs);
    let mut last_status = Instant::now() - Duration::from_millis(STATUS_REFRESH_MS);

    while start.elapsed() < dur {
        let space = acc.len() - acc_len;
        if space > 0 {
            let n = consumer.pop_slice(&mut acc[acc_len..acc_len + space]);
            acc_len += n;
        }
        while acc_len >= block_samples {
            let out = decoder.process(&acc[..block_samples]);
            amps.push(out.amplitude);
            confs.push(out.confidence);
            acc.copy_within(block_samples..acc_len, 0);
            acc_len -= block_samples;
        }
        if last_status.elapsed() >= Duration::from_millis(STATUS_REFRESH_MS) {
            let elapsed = start.elapsed().as_secs_f64();
            let progress = (elapsed / secs).clamp(0.0, 1.0);
            let bar_w = 20_usize;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let filled = (progress * bar_w as f64).round() as usize;
            let bar: String = "\u{2588}".repeat(filled) + &"\u{2591}".repeat(bar_w - filled);
            print_status_line(&format!(
                "  \u{231b} capturing {label}... [{bar}] {:>3.0}%",
                progress * 100.0
            ));
            last_status = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    if amps.is_empty() {
        return Err(anyhow!(
            "{label} capture produced 0 blocks — input device delivered no \
             samples in {secs:.0} s. Check microphone permission and device \
             selection."
        ));
    }

    print_status_line(&format!(
        "  \u{2713} {label} captured ({} blocks)",
        amps.len()
    ));
    Ok((amps, confs))
}

/// Update the current line in place via `\r`. Pads to 100 chars so
/// shorter status lines fully overwrite longer ones (no leftover
/// glyphs from a previous status).
fn print_status_line(s: &str) {
    eprint!("\r{s:<100}");
    let _ = std::io::stderr().flush();
}

/// Print the per-rig summary banner — derived thresholds, raw
/// percentiles, fingerprint, and (when lift was measured) SNR margin
/// with appropriate warnings.
fn print_summary(cal: &Calibration) {
    let t = &cal.thresholds;
    let m = &cal.measurements;
    println!("derived thresholds:");
    println!("  engage     {:.3}", t.engage);
    println!(
        "  disengage  {:.3}  (M5.3 default; not measured in v1)",
        t.disengage
    );
    println!("  amplitude  {:.4}", t.amplitude);
    println!(
        "  sticky     {} blocks  (M5.3 default; not measured in v1)",
        t.sticky_blocks_to_disengage
    );
    println!();
    println!("measurements:");
    println!(
        "  carrier  amp p5/p50/p95 = {:.4} / {:.4} / {:.4}    conf p5/p50/p95 = {:.3} / {:.3} / {:.3}",
        m.carrier.amplitude_p5,
        m.carrier.amplitude_p50,
        m.carrier.amplitude_p95,
        m.carrier.confidence_p5,
        m.carrier.confidence_p50,
        m.carrier.confidence_p95,
    );
    if m.lift.n_blocks > 0 {
        println!(
            "  lift     amp p5/p50/p95 = {:.4} / {:.4} / {:.4}    conf p5/p50/p95 = {:.3} / {:.3} / {:.3}",
            m.lift.amplitude_p5,
            m.lift.amplitude_p50,
            m.lift.amplitude_p95,
            m.lift.confidence_p5,
            m.lift.confidence_p50,
            m.lift.confidence_p95,
        );
    } else {
        println!("  lift     not measured (single-phase mode; --two-phase to enable SNR check)");
    }
    println!();
    println!(
        "fingerprint  amp_p50 = {:.4}  amp_p95 = {:.4}  conf_p50 = {:.3}",
        cal.fingerprint.carrier_amp_p50,
        cal.fingerprint.carrier_amp_p95,
        cal.fingerprint.carrier_conf_p50,
    );
    println!();

    // SNR rating + warnings. Three bands so the user knows whether
    // their rig will work in louder venues without re-calibration.
    // Only meaningful when lift was actually measured — single-phase
    // mode skips this entirely (lift.n_blocks == 0 ⇒ snr_margin
    // trivially returns INFINITY, which would print "excellent" and
    // mislead the user).
    if m.lift.n_blocks > 0 {
        let snr = cal.snr_margin;
        let snr_label = if snr >= 200.0 {
            "excellent"
        } else if snr >= SNR_WARN_THRESHOLD {
            "good"
        } else if snr >= SNR_FAIL_THRESHOLD {
            "low"
        } else {
            "fail" // shouldn't reach here — derive_thresholds rejected it
        };
        println!("SNR margin: {snr:.0}\u{00d7} ({snr_label})");
        if snr < SNR_WARN_THRESHOLD {
            println!(
                "  \u{26A0} warning: SNR below {:.0}\u{00d7} — clubs / loud venues may \
                 require recalibration in place. Consider checking preamp gain \
                 or stylus condition.",
                SNR_WARN_THRESHOLD
            );
        } else if snr < 200.0 {
            println!(
                "  \u{2139} note: SNR is healthy but not abundant; recalibrate \
                 at the venue if you experience ghost-noise during lifts."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parse_opts_default_deck_is_zero() {
        // No `--deck` flag → opts.deck = 0 (the legacy default).
        let opts = parse_opts(&s(&["--input-channels", "3,4"])).unwrap();
        assert_eq!(opts.deck, 0);
    }

    #[test]
    fn parse_opts_deck_flag_round_trips() {
        let opts = parse_opts(&s(&["--input-channels", "5,6", "--deck", "1"])).unwrap();
        assert_eq!(opts.deck, 1);
    }

    #[test]
    fn parse_opts_deck_flag_rejects_out_of_range() {
        // `--deck 2` is rejected because the engine only has 2 decks.
        // Better than silently widening to a u32 range — a typo
        // (e.g. `--deck 12` instead of `--deck 1 2`) would have
        // gotten the user a useless `~/.dub/calibration/SL_3_deck_12_*.json`
        // file that `dub timecode-deck` can never load.
        let r = parse_opts(&s(&["--input-channels", "3,4", "--deck", "2"]));
        assert!(r.is_err(), "--deck 2 must be rejected");
        let r = parse_opts(&s(&["--input-channels", "3,4", "--deck", "99"]));
        assert!(r.is_err(), "--deck 99 must be rejected");
    }

    #[test]
    fn parse_opts_deck_flag_rejects_non_numeric() {
        let r = parse_opts(&s(&["--input-channels", "3,4", "--deck", "B"]));
        assert!(r.is_err(), "letters not accepted; user must use 0/1");
    }

    #[test]
    fn parse_opts_default_is_single_phase() {
        // M5.4.3: single-phase is the default. A bare `dub calibrate
        // --input-channels 3,4` must NOT pull in the legacy lift
        // capture, which would silently bring back the ~25 s wall
        // time the user complained about.
        let opts = parse_opts(&s(&["--input-channels", "3,4"])).unwrap();
        assert!(
            !opts.two_phase,
            "M5.4.3 default must be single-phase (carrier only)"
        );
    }

    #[test]
    fn parse_opts_two_phase_flag_round_trips() {
        // Opt-in to the legacy two-phase flow for diagnostics.
        let opts = parse_opts(&s(&["--input-channels", "3,4", "--two-phase"])).unwrap();
        assert!(opts.two_phase);
    }

    #[test]
    fn parse_opts_default_carrier_secs_is_3() {
        // M5.4.3 reduced the default from 10 s → 3 s. Pin the
        // value so a future "make it 4 s for safety" tweak is a
        // visible PRD-level change rather than a silent default
        // shift.
        let opts = parse_opts(&s(&["--input-channels", "3,4"])).unwrap();
        assert!(
            (opts.carrier_secs - 3.0).abs() < 1e-9,
            "default carrier_secs should be 3.0, got {}",
            opts.carrier_secs
        );
    }

    #[test]
    fn carrier_detect_constants_match_m543_targets() {
        // Lock the M5.4.3 detection-loop tightening in place. If
        // any of these regress (e.g. STABLE_BLOCKS bumped back to
        // 5), the calibration wall time grows back; if
        // CARRIER_DETECT_CONF drops below 0.90, 2-block detection
        // becomes prone to handling-noise false positives.
        assert_eq!(STABLE_BLOCKS, 2, "STABLE_BLOCKS must be 2 (M5.4.3)");
        assert!(
            (CARRIER_DETECT_CONF - 0.90).abs() < 1e-6,
            "CARRIER_DETECT_CONF must be 0.90 (M5.4.3)"
        );
        assert!(
            (CARRIER_DETECT_RATE_BAND - 0.10).abs() < 1e-9,
            "CARRIER_DETECT_RATE_BAND stays 0.10 — handles \u{00b1}10 % pitch fader"
        );
    }
}
