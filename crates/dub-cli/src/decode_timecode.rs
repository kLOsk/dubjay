//! `dub decode-timecode <wav>` — offline timecode-vinyl decoder.
//!
//! Reads a stereo WAV containing recorded timecode (Serato CV02 in v1)
//! and reports decoded rate / position / amplitude / confidence in
//! discrete time slices. Use this to validate the decoder against
//! real-world recordings before plugging in a turntable in M5.3.
//!
//! With `--synthetic` and no input path the CLI generates a known
//! signal and decodes it — a sanity check for the decoder math
//! independent of any audio interface.
//!
//! Output is a TSV-ish report (one slice per line) followed by a
//! summary verdict — "LOCKED" if confidence and amplitude stayed in
//! plausible ranges, "POOR" otherwise, with a short reason.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use dub_timecode::{DecodeOutput, Decoder, Format};

/// Default analysis-window size in milliseconds. Smaller = more
/// rate-tracking detail in the report; larger = better noise rejection.
/// 25 ms (= 1200 samples @ 48 kHz, ~25 carrier cycles) is a good
/// balance for human-readable output.
pub const DEFAULT_WINDOW_MS: f32 = 25.0;

/// Run `dub decode-timecode` with the parsed CLI options.
///
/// # Errors
/// Returns any error from WAV decoding, sample-format mismatches,
/// or filesystem I/O.
pub fn run(input: Option<&Path>, synthetic: bool, window_ms: f32, max_lines: usize) -> Result<()> {
    if synthetic {
        return run_synthetic(window_ms, max_lines);
    }
    let path = input
        .ok_or_else(|| anyhow!("usage: dub decode-timecode <wav> [--window MS] [--head N]"))?;
    run_file(path, window_ms, max_lines)
}

fn run_file(path: &Path, window_ms: f32, max_lines: usize) -> Result<()> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 2 {
        return Err(anyhow!(
            "decode-timecode requires a stereo WAV; got {} channels",
            spec.channels
        ));
    }
    let sample_rate = spec.sample_rate as f32;
    if !(32_000.0..=192_000.0).contains(&sample_rate) {
        eprintln!(
            "warning: unusual sample rate {} Hz — decoder is tuned for 44.1–96 kHz",
            spec.sample_rate
        );
    }

    println!(
        "decode-timecode: {}\n  sr={} Hz, ch={}, bps={}, fmt={:?}",
        path.display(),
        spec.sample_rate,
        spec.channels,
        spec.bits_per_sample,
        spec.sample_format
    );

    // Read everything into memory. Real-world timecode WAVs are short
    // (< 2 min for a captured groove); we don't need streaming for the
    // offline tool.
    let interleaved = read_stereo_f32(&mut reader)
        .with_context(|| format!("reading samples from {}", path.display()))?;

    decode_and_report(
        Format::SeratoCv02,
        sample_rate,
        &interleaved,
        window_ms,
        max_lines,
    )
}

fn run_synthetic(window_ms: f32, max_lines: usize) -> Result<()> {
    use dub_timecode::signal::Generator;
    let sample_rate = 48_000.0_f32;
    let format = Format::SeratoCv02;
    println!(
        "decode-timecode: SYNTHETIC ({:?}, sr={} Hz, no input file)",
        format, sample_rate
    );
    println!("  scenario: 1 s @ 1.0× → 1 s @ 0.5× → 1 s @ -1.0× → 1 s silence");

    let mut g = Generator::new(format, sample_rate);
    let one_sec = 48_000_usize;
    let mut buf = vec![0.0f32; one_sec * 2 * 4];
    let (a, rest) = buf.split_at_mut(one_sec * 2);
    let (b, rest) = rest.split_at_mut(one_sec * 2);
    let (c, d) = rest.split_at_mut(one_sec * 2);
    g.render(a, 1.0, 0.5);
    g.render(b, 0.5, 0.5);
    g.render(c, -1.0, 0.5);
    for s in d.iter_mut() {
        *s = 0.0;
    }
    decode_and_report(format, sample_rate, &buf, window_ms, max_lines)
}

fn decode_and_report(
    format: Format,
    sample_rate: f32,
    interleaved: &[f32],
    window_ms: f32,
    max_lines: usize,
) -> Result<()> {
    let mut decoder = Decoder::new(format, sample_rate);

    let window_frames = ((window_ms / 1000.0) * sample_rate).round().max(64.0) as usize;
    let total_frames = interleaved.len() / 2;
    let total_secs = total_frames as f64 / f64::from(sample_rate);

    println!(
        "  format: {format:?} (carrier {} Hz)\n  window: {window_ms:.1} ms ({window_frames} frames)\n  total:  {total_secs:.3} s ({total_frames} frames)\n",
        format.carrier_hz()
    );
    println!("  t(s)\trate\tposition(s)\tamp\tconfidence");

    let mut printed = 0_usize;
    let mut hidden = 0_usize;
    let mut summary = SummaryStats::default();
    let mut t_frames = 0_usize;

    while t_frames + window_frames <= total_frames {
        let start = t_frames * 2;
        let end = (t_frames + window_frames) * 2;
        let block = &interleaved[start..end];
        let out = decoder.process(block);
        summary.update(&out);

        let t_secs = t_frames as f64 / f64::from(sample_rate);
        if printed < max_lines {
            println!(
                "  {t_secs:6.3}\t{:+.4}\t{:+.4}\t{:.3}\t{:.3}",
                out.rate, out.position_secs, out.amplitude, out.confidence
            );
            printed += 1;
        } else {
            hidden += 1;
        }
        t_frames += window_frames;
    }
    if hidden > 0 {
        println!(
            "  ... ({hidden} more windows omitted; pass --head {} to see all)",
            printed + hidden
        );
    }

    summary.report();
    Ok(())
}

#[derive(Default)]
struct SummaryStats {
    n: u64,
    rate_min: f64,
    rate_max: f64,
    amp_min: f32,
    amp_max: f32,
    conf_min: f32,
    conf_max: f32,
    conf_sum: f64,
    locked_windows: u64,
}

impl SummaryStats {
    fn update(&mut self, o: &DecodeOutput) {
        if self.n == 0 {
            self.rate_min = o.rate;
            self.rate_max = o.rate;
            self.amp_min = o.amplitude;
            self.amp_max = o.amplitude;
            self.conf_min = o.confidence;
            self.conf_max = o.confidence;
        } else {
            self.rate_min = self.rate_min.min(o.rate);
            self.rate_max = self.rate_max.max(o.rate);
            self.amp_min = self.amp_min.min(o.amplitude);
            self.amp_max = self.amp_max.max(o.amplitude);
            self.conf_min = self.conf_min.min(o.confidence);
            self.conf_max = self.conf_max.max(o.confidence);
        }
        self.n += 1;
        self.conf_sum += f64::from(o.confidence);
        // "Locked" = confidence > 0.5 AND amplitude > 0.01 — the
        // decoder thinks it's tracking a real carrier. Sub-threshold
        // windows are stylus-lifted or noise.
        if o.confidence > 0.5 && o.amplitude > 0.01 {
            self.locked_windows += 1;
        }
    }

    fn report(&self) {
        if self.n == 0 {
            println!("\nverdict: NO WINDOWS — input shorter than analysis window");
            return;
        }
        #[allow(clippy::cast_precision_loss)]
        let conf_avg = self.conf_sum / self.n as f64;
        #[allow(clippy::cast_precision_loss)]
        let lock_pct = 100.0 * self.locked_windows as f64 / self.n as f64;
        println!(
            "\nsummary across {} windows:\n  rate range:  {:+.4} .. {:+.4}\n  amp range:   {:.4} .. {:.4}\n  confidence:  {:.3} .. {:.3} (avg {:.3})\n  locked:      {}/{} ({:.1}%)",
            self.n,
            self.rate_min,
            self.rate_max,
            self.amp_min,
            self.amp_max,
            self.conf_min,
            self.conf_max,
            conf_avg,
            self.locked_windows,
            self.n,
            lock_pct,
        );
        // Verdict heuristic — calibrated to the synthetic test scenarios.
        // Real-world tolerance lands in M5.3 once we have actual cartridge captures.
        let verdict = if conf_avg > 0.9 && lock_pct > 80.0 {
            "LOCKED — decoder tracked the carrier across most of the input"
        } else if lock_pct > 50.0 {
            "PARTIAL — significant locked sections; check unlocked windows for transients/silence"
        } else {
            "POOR — decoder did not lock onto a carrier; likely wrong format, wrong channel, or no signal"
        };
        println!("verdict: {verdict}");
    }
}

/// Read a (potentially integer-PCM) WAV into normalized stereo f32.
fn read_stereo_f32(
    reader: &mut hound::WavReader<std::io::BufReader<std::fs::File>>,
) -> Result<Vec<f32>> {
    let spec = reader.spec();
    let mut out: Vec<f32> = Vec::with_capacity(reader.len() as usize);
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.samples::<f32>() {
                out.push(s.context("reading float sample")?);
            }
        }
        hound::SampleFormat::Int => {
            // Normalize to ±1.0 based on the bit depth.
            let scale = 1.0_f32 / ((1_i64 << (spec.bits_per_sample - 1)) as f32);
            for s in reader.samples::<i32>() {
                let v = s.context("reading int sample")?;
                #[allow(clippy::cast_precision_loss)]
                out.push(v as f32 * scale);
            }
        }
    }
    Ok(out)
}

/// Argument parser used by `main`.
pub fn parse_args(args: &[String]) -> Result<(Option<PathBuf>, bool, f32, usize)> {
    let mut input: Option<PathBuf> = None;
    let mut synthetic = false;
    let mut window_ms = DEFAULT_WINDOW_MS;
    let mut max_lines = 40_usize;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--synthetic" | "--self-test" => {
                synthetic = true;
                i += 1;
            }
            "--window" | "--window-ms" => {
                window_ms = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--window expects a value in ms"))?
                    .parse()
                    .context("--window not a number")?;
                i += 2;
            }
            "--head" => {
                max_lines = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--head expects an integer"))?
                    .parse()
                    .context("--head not an integer")?;
                i += 2;
            }
            s if s.starts_with('-') => {
                return Err(anyhow!("unknown flag: {s}"));
            }
            _ => {
                if input.is_some() {
                    return Err(anyhow!("unexpected positional arg: {}", args[i]));
                }
                input = Some(PathBuf::from(&args[i]));
                i += 1;
            }
        }
    }
    Ok((input, synthetic, window_ms, max_lines))
}
