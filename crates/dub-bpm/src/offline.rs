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
use crate::{BpmEstimate, FRAME_SIZE, HOP_SIZE, MIN_BPM};

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
}

/// Analyse a complete audio buffer and return a single tempo estimate.
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
    if sample_rate == 0 {
        return Err(AnalysisError::ZeroSampleRate);
    }
    if !(1..=2).contains(&channels) {
        return Err(AnalysisError::InvalidChannels(channels));
    }

    let frames = samples.len() / usize::from(channels);

    let odf_sr = f64::from(sample_rate) / HOP_SIZE as f64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lag_max = ((60.0 * odf_sr) / MIN_BPM).ceil() as usize;
    let need_odf = lag_max * 2;
    let need_frames = need_odf * HOP_SIZE + FRAME_SIZE;
    if frames < need_frames {
        #[allow(clippy::cast_precision_loss)]
        let need_secs = need_frames as f64 / f64::from(sample_rate);
        return Err(AnalysisError::TooShort {
            got_frames: frames,
            need_frames,
            bpm_floor: MIN_BPM,
            need_secs,
            sample_rate,
        });
    }

    let mut detector = OnsetDetector::new();
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

    Ok(estimate_tempo(detector.odf(), odf_sr).unwrap_or(BpmEstimate::NONE))
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
}
