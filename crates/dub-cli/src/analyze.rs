//! `dub analyze <input.wav>` — sample-discontinuity & quality auditor.
//!
//! Why this exists: subjective listening is a terrible debug loop for
//! audio glitches. Clicks are sub-millisecond events, easy to miss on
//! laptop speakers, and impossible to A/B reliably without rendering
//! to a file and looking at the math. This subcommand reads a WAV
//! produced by `dub play <input> -o <out.wav>` (or any other PCM/float
//! WAV) and reports:
//!
//! - peak / RMS / DC offset per channel,
//! - clipping count (samples where |s| ≥ 1.0 — float WAVs can exceed
//!   that, but anything in real audio that does is suspect),
//! - **maximum per-sample first-difference** per channel: `|s[i] - s[i-1]|`.
//!   For a smooth signal, even at full amplitude, this is bounded by
//!   the slope of the underlying waveform; for a sample-step "click",
//!   it's of order the sample value itself.
//! - top-K *suspicious* discontinuities (delta above a threshold),
//!   with their timestamps so you can locate the issue precisely.
//!
//! Threshold heuristics:
//!
//! - The M3.5 declick uses a 2 ms `sin²` envelope; the worst per-sample
//!   delta produced by that fade against a 1.0 source is bounded by
//!   `π/(2N)` ≈ 0.016 at 48 kHz. We default `--threshold 0.05` so
//!   ordinary fades are ignored.
//! - For real music with transients (kicks, snares), per-sample deltas
//!   of 0.1–0.3 are normal; raise the threshold to e.g. 0.5 if the
//!   default reports too many false positives.
//! - A single-sample step from `+1.0` to `-1.0` (a worst-case click)
//!   produces a delta of 2.0; it's always flagged.

use std::path::Path;

use anyhow::{Context, Result};

/// Default `|s[i] − s[i-1]|` flag threshold. Set to roughly 3× the
/// largest per-sample delta a 2 ms `sin²` declick can produce on a
/// 1.0 amplitude source, so the M3.5 fade itself never trips it.
pub const DEFAULT_DELTA_THRESHOLD: f32 = 0.05;

/// Maximum number of suspicious-delta locations to print per channel.
/// Keeps reports digestible even on pathological inputs.
const MAX_FLAGGED_PER_CHANNEL: usize = 20;

/// Per-channel audit results.
#[derive(Debug, Default, Clone)]
struct ChannelStats {
    peak: f32,
    rms_acc: f64,
    dc_acc: f64,
    n: u64,
    clipping: u64,
    max_delta: f32,
    /// `(frame_index, prev_value, curr_value, delta)` for samples that
    /// crossed the suspicious-delta threshold. Capped at
    /// [`MAX_FLAGGED_PER_CHANNEL`] entries by retain-largest selection.
    flagged: Vec<(u64, f32, f32, f32)>,
}

impl ChannelStats {
    fn ingest(&mut self, frame: u64, prev: Option<f32>, value: f32, threshold: f32) {
        self.n += 1;
        let abs = value.abs();
        if abs > self.peak {
            self.peak = abs;
        }
        if abs >= 1.0 {
            self.clipping += 1;
        }
        self.rms_acc += f64::from(value) * f64::from(value);
        self.dc_acc += f64::from(value);
        if let Some(p) = prev {
            let delta = (value - p).abs();
            if delta > self.max_delta {
                self.max_delta = delta;
            }
            if delta > threshold {
                self.push_flag(frame, p, value, delta);
            }
        }
    }

    fn push_flag(&mut self, frame: u64, prev: f32, curr: f32, delta: f32) {
        if self.flagged.len() < MAX_FLAGGED_PER_CHANNEL {
            self.flagged.push((frame, prev, curr, delta));
            return;
        }
        // Keep the largest-delta entries: replace the smallest stored
        // delta if this one is larger.
        let (idx, smallest) = self
            .flagged
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, e)| (i, e.3))
            .expect("non-empty by length check above");
        if delta > smallest {
            self.flagged[idx] = (frame, prev, curr, delta);
        }
    }

    fn rms(&self) -> f32 {
        if self.n == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let r = (self.rms_acc / self.n as f64).sqrt() as f32;
        r
    }

    fn dc(&self) -> f32 {
        if self.n == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let d = (self.dc_acc / self.n as f64) as f32;
        d
    }
}

/// Format an amplitude as dBFS (returns `"-inf"` for zero).
fn dbfs(x: f32) -> String {
    if x <= 0.0 {
        "-inf".to_string()
    } else {
        format!("{:.2}", 20.0 * x.log10())
    }
}

/// Run the analyzer on a WAV file.
pub fn run(input: &Path, threshold: f32) -> Result<()> {
    let mut reader =
        hound::WavReader::open(input).with_context(|| format!("opening {}", input.display()))?;
    let spec = reader.spec();
    let channels = spec.channels;
    let sr = f64::from(spec.sample_rate);
    let n_frames = u64::from(reader.duration());

    println!("analyze: {}", input.display());
    println!("  sample rate: {} Hz", spec.sample_rate);
    println!("  channels:    {channels}");
    println!("  frames:      {n_frames}");
    println!(
        "  duration:    {:.3} s",
        f64::from(spec.sample_rate)
            .recip()
            .mul_add(n_frames as f64, 0.0)
    );
    println!(
        "  format:      {} ({} bps, {})",
        match spec.sample_format {
            hound::SampleFormat::Float => "float",
            hound::SampleFormat::Int => "int",
        },
        spec.bits_per_sample,
        if matches!(spec.sample_format, hound::SampleFormat::Float) {
            "expected for `dub play -o`"
        } else {
            "fixed-point"
        }
    );
    println!("  threshold:   {threshold} (|s[i] - s[i-1]| flag)");

    // We accept both float and int-PCM WAVs; coerce everything to f32
    // in the [-1, 1] range. The analyzer doesn't care about the source
    // format beyond that normalization.
    let mut stats: Vec<ChannelStats> = vec![ChannelStats::default(); channels as usize];
    let mut prev: Vec<Option<f32>> = vec![None; channels as usize];
    let mut frame_idx: u64 = 0;
    let mut ch: usize = 0;

    let read_samples: Box<dyn Iterator<Item = Result<f32, hound::Error>>> = match spec.sample_format
    {
        hound::SampleFormat::Float => Box::new(reader.samples::<f32>()),
        hound::SampleFormat::Int => {
            #[allow(clippy::cast_precision_loss)]
            let scale = 1.0 / f32::from(i16::MAX);
            Box::new(
                reader
                    .samples::<i16>()
                    .map(move |r| r.map(|s| f32::from(s) * scale)),
            )
        }
    };

    for sample in read_samples {
        let value = sample.context("decoding sample")?;
        stats[ch].ingest(frame_idx, prev[ch], value, threshold);
        prev[ch] = Some(value);
        ch += 1;
        if ch == channels as usize {
            ch = 0;
            frame_idx += 1;
        }
    }

    println!();
    println!("per-channel stats:");
    for (i, s) in stats.iter().enumerate() {
        println!("  ch{i}:");
        println!("    peak       {:.4} ({} dBFS)", s.peak, dbfs(s.peak));
        println!("    rms        {:.4} ({} dBFS)", s.rms(), dbfs(s.rms()));
        println!("    dc         {:.6}", s.dc());
        println!("    clipping   {} samples", s.clipping);
        println!(
            "    max delta  {:.4} (per-sample first-difference)",
            s.max_delta
        );
        if !s.flagged.is_empty() {
            // Sort flagged largest-delta-first for inspection.
            let mut flagged = s.flagged.clone();
            flagged.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
            println!("    suspicious deltas (>{threshold}):");
            for (frame, prev, curr, delta) in flagged.iter().take(MAX_FLAGGED_PER_CHANNEL) {
                #[allow(clippy::cast_precision_loss)]
                let t = (*frame as f64) / sr;
                println!(
                    "      t={t:>9.5}s  frame={frame:>9}  {prev:>+8.4} -> {curr:>+8.4}  Δ={delta:.4}"
                );
            }
        } else {
            println!("    suspicious deltas: none above threshold");
        }
    }

    let any_clipping = stats.iter().any(|s| s.clipping > 0);
    let any_flagged = stats.iter().any(|s| !s.flagged.is_empty());
    println!();
    println!(
        "verdict: {}",
        match (any_clipping, any_flagged) {
            (false, false) => "CLEAN — no clipping, no suspicious discontinuities".to_string(),
            (true, false) => format!(
                "CLIPPING — {} samples ≥ 1.0",
                stats.iter().map(|s| s.clipping).sum::<u64>()
            ),
            (false, true) => format!(
                "DISCONTINUITIES — {} samples above Δ={threshold}",
                stats.iter().map(|s| s.flagged.len()).sum::<usize>()
            ),
            (true, true) => "CLIPPING + DISCONTINUITIES".to_string(),
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dbfs_zero_is_minus_inf() {
        assert_eq!(dbfs(0.0), "-inf");
    }

    #[test]
    fn dbfs_full_scale_is_zero() {
        assert!(dbfs(1.0).starts_with("0.00"));
    }

    #[test]
    fn channel_stats_track_max_delta() {
        let mut s = ChannelStats::default();
        s.ingest(0, None, 0.0, 0.05);
        s.ingest(1, Some(0.0), 0.1, 0.05);
        s.ingest(2, Some(0.1), 0.5, 0.05);
        s.ingest(3, Some(0.5), 0.2, 0.05);
        assert!(
            (s.max_delta - 0.4).abs() < 1e-6,
            "max_delta = {}",
            s.max_delta
        );
    }

    #[test]
    fn channel_stats_flag_only_above_threshold() {
        let mut s = ChannelStats::default();
        s.ingest(0, None, 0.0, 0.05);
        s.ingest(1, Some(0.0), 0.01, 0.05); // delta = 0.01, below
        s.ingest(2, Some(0.01), 0.10, 0.05); // delta = 0.09, above
        assert_eq!(s.flagged.len(), 1);
        assert_eq!(s.flagged[0].0, 2);
    }

    #[test]
    fn channel_stats_keep_largest_when_overflowing() {
        let mut s = ChannelStats::default();
        // Push MAX_FLAGGED_PER_CHANNEL + 5 large deltas; the top
        // MAX_FLAGGED_PER_CHANNEL by magnitude must survive.
        for i in 0..MAX_FLAGGED_PER_CHANNEL + 5 {
            #[allow(clippy::cast_precision_loss)]
            let v = i as f32 * 0.01 + 0.1; // distinct, ascending
            s.ingest(i as u64, Some(0.0), v, 0.05);
        }
        assert_eq!(s.flagged.len(), MAX_FLAGGED_PER_CHANNEL);
        // Smallest stored delta should be larger than the smallest
        // pushed (i.e., the smallest-5 were kicked out).
        let smallest_kept = s
            .flagged
            .iter()
            .map(|f| f.3)
            .min_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap();
        assert!(smallest_kept > 0.10);
    }
}
