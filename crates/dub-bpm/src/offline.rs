//! Offline `analyze_bpm` driver.
//!
//! Whole-buffer-in, single-estimate-out. The shape callers use when a
//! file finishes loading and we want to stamp `Track::bpm` (PRD §5.3).
//!
//! This is also the "ground-truth oracle" the M8 streaming driver
//! cross-checks itself against in tests — same algorithm internals,
//! different driving pattern.

use crate::onset::OnsetDetector;
use crate::tempo::estimate_tempo;
use crate::{BpmEstimate, BpmRange, FRAME_SIZE, HOP_SIZE};

/// Errors that prevent `analyze_bpm` from producing an estimate.
///
/// "No periodic structure detected" is *not* an error — it returns
/// `Ok(BpmEstimate::NONE)` with `confidence == 0`. Errors are reserved
/// for inputs the algorithm fundamentally cannot reason about
/// (malformed layout, sub-threshold duration).
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    /// Input is shorter than two beat periods at [`MIN_BPM`], so no
    /// autocorrelation peak in our search range can be confirmed.
    #[error(
        "input too short: {got_frames} frames, need at least {need_frames} \
         for {bpm_floor} BPM detection (≈{need_secs:.2}s at {sample_rate} Hz)"
    )]
    TooShort {
        /// Frames actually given.
        got_frames: usize,
        /// Frames required.
        need_frames: usize,
        /// Lower BPM bound driving the requirement.
        bpm_floor: f64,
        /// Equivalent duration in seconds.
        need_secs: f64,
        /// The sample rate the requirement was computed against.
        sample_rate: u32,
    },

    /// Channel count is not 1 or 2.
    #[error("invalid channels: {0}; must be 1 (mono) or 2 (stereo)")]
    InvalidChannels(u8),

    /// Sample rate is zero.
    #[error("invalid sample rate: 0")]
    ZeroSampleRate,

    /// Sample count is not a whole number of frames at the given
    /// channel count (i.e. `samples.len() % channels != 0`).
    ///
    /// In practice this trips on malformed interleaved stereo with an
    /// odd sample count. Without this check the downstream stereo
    /// downmix (`chunks_exact(2)`) and the `frames` count
    /// (`len / channels`) would both silently truncate the trailing
    /// unpaired sample and return a `BpmEstimate` derived from
    /// invisibly-shortened input — no panic, but no diagnostic either.
    /// Surfacing it as a typed error keeps the honesty contract honest.
    #[error(
        "samples.len() ({sample_count}) is not a multiple of channels ({channels}); \
         interleaved input must contain a whole number of frames"
    )]
    NonInterleavedFrames {
        /// Number of samples actually provided.
        sample_count: usize,
        /// Channel count in effect.
        channels: u8,
    },
}

/// Analyse a complete audio buffer and return a single tempo estimate
/// using the default 60–200 BPM search range.
///
/// `samples` is interleaved (`L R L R …` for stereo, `M M …` for mono).
/// Stereo input is downmixed to mono internally (mean of L+R).
///
/// # Errors
///
/// See [`AnalysisError`]. Notably this returns `Err(TooShort)` rather
/// than a zero-confidence estimate when the input cannot support the
/// algorithm — that's a static-property error, not an
/// algorithm-couldn't-find-anything outcome.
pub fn analyze_bpm(
    samples: &[f32],
    sample_rate: u32,
    channels: u8,
) -> Result<BpmEstimate, AnalysisError> {
    analyze_bpm_with_range(samples, sample_rate, channels, BpmRange::DEFAULT)
}

/// Like [`analyze_bpm`] but restricts the tempo search to `range`.
///
/// Useful when the caller has a strong genre / style prior that the
/// algorithm's defaults can't disambiguate on its own — e.g. forcing
/// `BpmRange::new(120.0, 180.0)` on dubstep tracks that the bare
/// algorithm would (correctly per its math but incorrectly per the
/// genre convention) report as half-tempo.
///
/// # Errors
///
/// See [`analyze_bpm`].
pub fn analyze_bpm_with_range(
    samples: &[f32],
    sample_rate: u32,
    channels: u8,
    range: BpmRange,
) -> Result<BpmEstimate, AnalysisError> {
    if sample_rate == 0 {
        return Err(AnalysisError::ZeroSampleRate);
    }
    if !(1..=2).contains(&channels) {
        return Err(AnalysisError::InvalidChannels(channels));
    }
    // Validate the interleaved-layout contract before computing
    // `frames` via integer division. Without this the trailing
    // unpaired sample of a malformed stereo buffer would be silently
    // dropped twice (once by `len / channels`, once by
    // `chunks_exact(2)`) and the caller would never know.
    if !samples.len().is_multiple_of(usize::from(channels)) {
        return Err(AnalysisError::NonInterleavedFrames {
            sample_count: samples.len(),
            channels,
        });
    }

    let frames = samples.len() / usize::from(channels);

    let odf_sr = f64::from(sample_rate) / HOP_SIZE as f64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lag_max = ((60.0 * odf_sr) / range.min).ceil() as usize;
    let need_odf = lag_max * 2;
    let need_frames = need_odf * HOP_SIZE + FRAME_SIZE;
    if frames < need_frames {
        #[allow(clippy::cast_precision_loss)]
        let need_secs = need_frames as f64 / f64::from(sample_rate);
        return Err(AnalysisError::TooShort {
            got_frames: frames,
            need_frames,
            bpm_floor: range.min,
            need_secs,
            sample_rate,
        });
    }

    let mut detector = OnsetDetector::new(sample_rate);
    if channels == 1 {
        detector.process(samples);
    } else {
        // Stereo: downmix on the fly. Allocating a mono Vec is fine —
        // we're already in an off-RT context (file just loaded; the
        // streaming path uses BpmEstimator, not this function).
        let mono: Vec<f32> = samples
            .chunks_exact(2)
            .map(|c| 0.5 * (c[0] + c[1]))
            .collect();
        detector.process(&mono);
    }

    Ok(estimate_tempo(detector.odf(), odf_sr, range).unwrap_or(BpmEstimate::NONE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_sample_rate_rejected() {
        let samples = vec![0.0f32; 1000];
        assert!(matches!(
            analyze_bpm(&samples, 0, 1),
            Err(AnalysisError::ZeroSampleRate)
        ));
    }

    #[test]
    fn three_channels_rejected() {
        let samples = vec![0.0f32; 1000];
        assert!(matches!(
            analyze_bpm(&samples, 48_000, 3),
            Err(AnalysisError::InvalidChannels(3))
        ));
    }

    #[test]
    fn zero_channels_rejected() {
        let samples = vec![0.0f32; 1000];
        assert!(matches!(
            analyze_bpm(&samples, 48_000, 0),
            Err(AnalysisError::InvalidChannels(0))
        ));
    }

    /// Short malformed buffer. Pre-fix this still erred (with
    /// `TooShort`, because `frames = 1` after truncation), but the
    /// error message named the wrong cause. The interleave layout
    /// check now fires first and reports the actual problem.
    #[test]
    fn odd_stereo_sample_count_rejected_short() {
        let samples = vec![0.0f32; 3];
        match analyze_bpm(&samples, 48_000, 2) {
            Err(AnalysisError::NonInterleavedFrames {
                sample_count,
                channels,
            }) => {
                assert_eq!(sample_count, 3);
                assert_eq!(channels, 2);
            }
            other => panic!("expected NonInterleavedFrames, got {other:?}"),
        }
    }

    /// Long malformed buffer. This is the case the pre-fix code
    /// would have silently accepted: `frames = len / 2` clears the
    /// `TooShort` gate, then `chunks_exact(2)` silently drops the
    /// trailing unpaired sample and `analyze_bpm` returns `Ok` with
    /// no diagnostic. Now it must err loudly.
    #[test]
    fn odd_stereo_sample_count_rejected_long() {
        // 5 s of zeros at 48 kHz, stereo, plus one extra mono sample
        // to make the layout odd. Comfortably longer than the
        // TooShort threshold so the bug would otherwise be hidden.
        let samples = vec![0.0f32; 48_000 * 5 * 2 + 1];
        assert!(matches!(
            analyze_bpm(&samples, 48_000, 2),
            Err(AnalysisError::NonInterleavedFrames { .. })
        ));
    }

    /// Mono is exempt from the multiple-of-channels check (anything
    /// is a multiple of 1) — verify the new gate doesn't reject
    /// legal mono input.
    #[test]
    fn odd_sample_count_allowed_for_mono() {
        let samples = vec![0.0f32; 1001];
        // Will return TooShort because the buffer is too small, not
        // NonInterleavedFrames — that's what we're checking.
        assert!(matches!(
            analyze_bpm(&samples, 48_000, 1),
            Err(AnalysisError::TooShort { .. })
        ));
    }
}
