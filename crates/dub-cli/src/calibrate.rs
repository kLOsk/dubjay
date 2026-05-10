//! `dub calibrate` — auto-detecting per-rig calibration (M5.4.2).
//!
//! Two phases, no manual prompts:
//!
//! 1. **Carrier.** Wait until the input shows a stable carrier (high
//!    confidence, near-unity rate, low amplitude variance for ≥
//!    [`STABLE_BLOCKS`] consecutive blocks). Then capture
//!    [`Opts::carrier_secs`] seconds of measurement.
//! 2. **Lift.** Wait until amplitude drops below
//!    [`LIFT_DETECT_AMP`] for ≥ [`LIFT_BLOCKS`] consecutive blocks.
//!    Then capture [`Opts::lift_secs`] seconds.
//!
//! From the two phases we compute P5/P50/P95 percentiles for both
//! amplitude + confidence, derive the thresholds via
//! [`crate::calibration::derive_thresholds`], stamp a
//! [`RigFingerprint`] for future cartridge-swap detection, and write
//! the JSON to `~/.dub/calibration/<device>_<format>.json` (or the
//! `--output` override).
//!
//! ## Why auto-detection rather than "press Enter"
//!
//! The user requested the lowest-friction calibration possible —
//! Traktor-style "press calibrate, do the thing" rather than Serato-
//! style "press calibrate, do the thing, click next, do the next
//! thing". The decoder already gives us everything we need to detect
//! "the user is showing me carrier" vs "the user is showing me
//! silence", so auto-progression is straightforward and the UX is
//! genuinely zero-input: drop the needle → wait ~10 s → lift the
//! needle → wait ~5 s → done.

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
    snr_margin, Calibration, CalibrationMeasurements, RigFingerprint, SCHEMA_VERSION,
    SNR_FAIL_THRESHOLD, SNR_WARN_THRESHOLD,
};
use crate::input_cmds::{parse_input_args, InputArgs};

/// Block size used by the calibrator. Pinned to match
/// `dub timecode-deck` — the lift policy's sticky window is
/// block-counted, and we want the calibration thresholds to
/// produce identical behavior at playback time. See
/// [`crate::scope`] for the same rationale.
const BLOCK_FRAMES: usize = 256;

/// Carrier-detection threshold: confidence the decoder must report
/// to consider the current block "carrier present". Set well above
/// noise + handling artifacts but well below "perfect lock"; we
/// just need to recognize "the user dropped the needle".
const CARRIER_DETECT_CONF: f32 = 0.85;

/// Carrier-detection rate band: |rate - 1.0| ≤ this. 0.10 allows for
/// slight pitch fader off-zero (typical turntable users keep ±2 % at
/// rest, well within the band) and platter spin-up wobble.
const CARRIER_DETECT_RATE_BAND: f64 = 0.10;

/// Number of consecutive blocks meeting the carrier criteria before
/// we declare "stable carrier". 5 blocks @ 256 frames @ 48 kHz =
/// ~27 ms — short enough to be responsive, long enough to filter
/// brief touches of the needle on the record.
const STABLE_BLOCKS: u32 = 5;

/// Lift-detection amplitude threshold (RMS). Carriers through any
/// reasonable rig sit ≥ 0.05; lift drops to ~0.0001-0.005 depending
/// on ambient. 0.005 is a comfortably conservative cutoff above
/// almost any handling/rumble noise.
const LIFT_DETECT_AMP: f32 = 0.005;

/// Number of consecutive blocks below [`LIFT_DETECT_AMP`] before we
/// declare "lift detected". 10 blocks ≈ 53 ms. Ignores brief drop-
/// outs (dust ticks, brief inter-track gap on the record).
const LIFT_BLOCKS: u32 = 10;

/// Default timeout for each detection phase. After this long without
/// hitting the criteria, the calibrator aborts with a clear error
/// — almost always the user forgot to drop the needle, or the wrong
/// channels are configured.
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
    /// Length of the carrier capture phase.
    carrier_secs: f64,
    /// Length of the lift capture phase.
    lift_secs: f64,
    /// Detection timeout for both waits.
    detect_timeout_secs: f64,
    /// Override the on-disk save location. `None` = standard
    /// `~/.dub/calibration/<device_key>.json` path.
    output: Option<PathBuf>,
    /// Skip the save step (compute + print results only). Useful
    /// when iterating on threshold formulas without polluting the
    /// real calibration dir.
    no_save: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            input: InputArgs::default(),
            format: Format::SeratoCv02,
            carrier_secs: 10.0,
            lift_secs: 5.0,
            detect_timeout_secs: DETECT_TIMEOUT_SECS,
            output: None,
            no_save: false,
        }
    }
}

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
                opts.detect_timeout_secs = v
                    .parse::<f64>()
                    .with_context(|| format!("--detect-timeout {v}"))?;
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
            "--no-save" => {
                opts.no_save = true;
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
    if opts.detect_timeout_secs <= 0.0 {
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
        "calibrating: device='{}' sr={} Hz channels={channels} format={}",
        input.device_name(),
        input.sample_rate(),
        format_string(opts.format),
    );
    println!();

    let cal = measure_inline(
        &mut input,
        opts.format,
        opts.carrier_secs,
        opts.lift_secs,
        opts.detect_timeout_secs,
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

/// Run both phases of calibration against an already-open
/// [`AudioInput`]. Returns the fully populated [`Calibration`]
/// without saving — the caller decides where (and whether) to
/// persist.
///
/// Used by both `dub calibrate` (saves to
/// `~/.dub/calibration/...`) and `dub timecode-deck`'s startup
/// auto-calibration path (saves to the same location, but the user
/// didn't explicitly ask for it). Sharing this code path means
/// "you got auto-calibrated" produces a JSON file indistinguishable
/// from "you ran `dub calibrate`".
///
/// # Errors
/// Detection timeout (no carrier / no lift), 0-block capture, or
/// SNR below [`SNR_FAIL_THRESHOLD`] (rejected by
/// [`derive_thresholds`]).
pub fn measure_inline(
    input: &mut AudioInput,
    format: Format,
    carrier_secs: f64,
    lift_secs: f64,
    detect_timeout_secs: f64,
) -> Result<Calibration> {
    let sr = input.sample_rate();
    let mut decoder = Decoder::new(format, sr);

    println!("step 1/2: carrier  —  spin the record at 33⅓ on a clean section");
    wait_for_stable_carrier(input, &mut decoder, detect_timeout_secs)?;
    println!();
    let (carrier_amps, carrier_confs) =
        capture_phase(input, &mut decoder, carrier_secs, "carrier")?;
    println!();

    println!("step 2/2: lift     —  lift the needle off and rest it");
    wait_for_lift(input, &mut decoder, detect_timeout_secs)?;
    println!();
    let (lift_amps, lift_confs) = capture_phase(input, &mut decoder, lift_secs, "lift")?;
    println!();

    let mut carrier_amps_owned = carrier_amps;
    let mut carrier_confs_owned = carrier_confs;
    let carrier = measurement_stats_from_samples(&mut carrier_amps_owned, &mut carrier_confs_owned);
    let mut lift_amps_owned = lift_amps;
    let mut lift_confs_owned = lift_confs;
    let lift = measurement_stats_from_samples(&mut lift_amps_owned, &mut lift_confs_owned);

    let snr = snr_margin(&carrier, &lift);
    let Some(thresholds) = derive_thresholds(&carrier, &lift) else {
        return Err(anyhow!(
            "low SNR ({:.1}× < {:.0}× minimum) — likely cartridge, stylus, \
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
        device_name: input.device_name().to_string(),
        format: format_string(format).to_string(),
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

/// Briefly observe the carrier and return its fingerprint. Used by
/// `dub timecode-deck` startup to validate the saved fingerprint
/// without doing the full ~25 s carrier+lift calibration. Three
/// seconds of carrier capture (≈ 564 blocks @ 256 frames @ 48 kHz)
/// gives stable percentiles within a fraction of a percent of the
/// long-run values.
///
/// # Errors
/// Detection timeout (no carrier observed), 0-block capture.
pub fn probe_carrier(
    input: &mut AudioInput,
    format: Format,
    secs: f64,
    detect_timeout_secs: f64,
) -> Result<RigFingerprint> {
    let sr = input.sample_rate();
    let mut decoder = Decoder::new(format, sr);
    println!("probing carrier ({secs:.1} s)...");
    wait_for_stable_carrier(input, &mut decoder, detect_timeout_secs)?;
    println!();
    let (amps, confs) = capture_phase(input, &mut decoder, secs, "probe")?;
    println!();
    let mut amps_owned = amps;
    let mut confs_owned = confs;
    let stats = measurement_stats_from_samples(&mut amps_owned, &mut confs_owned);
    Ok(RigFingerprint {
        carrier_amp_p50: stats.amplitude_p50,
        carrier_amp_p95: stats.amplitude_p95,
        carrier_conf_p50: stats.confidence_p50,
    })
}

/// Wait until [`STABLE_BLOCKS`] consecutive blocks meet the carrier-
/// detection criteria, or `timeout_secs` elapses.
///
/// While waiting, prints a rolling 4 Hz status line so the user
/// sees what the decoder is observing in real time.
fn wait_for_stable_carrier(
    input: &mut AudioInput,
    decoder: &mut Decoder,
    timeout_secs: f64,
) -> Result<()> {
    let mut consecutive: u32 = 0;
    let block_samples = BLOCK_FRAMES * 2;
    let mut acc = vec![0_f32; block_samples * 4];
    let mut acc_len = 0_usize;

    let start = Instant::now();
    let timeout = Duration::from_secs_f64(timeout_secs);
    let mut last_status = Instant::now() - Duration::from_millis(STATUS_REFRESH_MS);

    let mut last_amp = 0.0_f32;
    let mut last_conf = 0.0_f32;
    let mut last_rate = 0.0_f64;

    loop {
        if start.elapsed() > timeout {
            eprintln!();
            return Err(anyhow!(
                "no stable carrier detected within {timeout_secs:.0} s.\n\
                 Common causes: needle not on the record, wrong --input-channels \
                 (SL3 deck A is 3,4), or platter not spinning at unity. \
                 Last seen: conf={last_conf:.2} rate={last_rate:+.3} amp={last_amp:.4}",
            ));
        }
        let space = acc.len() - acc_len;
        if space > 0 {
            let n = input.read_into(&mut acc[acc_len..acc_len + space]);
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
/// below [`LIFT_DETECT_AMP`], or [`Opts::detect_timeout_secs`]
/// elapses.
fn wait_for_lift(input: &mut AudioInput, decoder: &mut Decoder, timeout_secs: f64) -> Result<()> {
    let mut consecutive: u32 = 0;
    let block_samples = BLOCK_FRAMES * 2;
    let mut acc = vec![0_f32; block_samples * 4];
    let mut acc_len = 0_usize;

    let start = Instant::now();
    let timeout = Duration::from_secs_f64(timeout_secs);
    let mut last_status = Instant::now() - Duration::from_millis(STATUS_REFRESH_MS);

    let mut last_amp = 0.0_f32;

    loop {
        if start.elapsed() > timeout {
            eprintln!();
            return Err(anyhow!(
                "no lift detected within {timeout_secs:.0} s.\n\
                 Lift the stylus off the record and place it on its rest. \
                 Last seen amp = {last_amp:.4}",
            ));
        }
        let space = acc.len() - acc_len;
        if space > 0 {
            let n = input.read_into(&mut acc[acc_len..acc_len + space]);
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
    input: &mut AudioInput,
    decoder: &mut Decoder,
    secs: f64,
    label: &str,
) -> Result<(Vec<f32>, Vec<f32>)> {
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
            let n = input.read_into(&mut acc[acc_len..acc_len + space]);
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
/// percentiles, fingerprint, and SNR margin with appropriate
/// warnings.
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
    println!(
        "  lift     amp p5/p50/p95 = {:.4} / {:.4} / {:.4}    conf p5/p50/p95 = {:.3} / {:.3} / {:.3}",
        m.lift.amplitude_p5,
        m.lift.amplitude_p50,
        m.lift.amplitude_p95,
        m.lift.confidence_p5,
        m.lift.confidence_p50,
        m.lift.confidence_p95,
    );
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
    println!("SNR margin: {snr:.0}× ({snr_label})");
    if snr < SNR_WARN_THRESHOLD {
        println!(
            "  \u{26A0} warning: SNR below {:.0}× — clubs / loud venues may \
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
