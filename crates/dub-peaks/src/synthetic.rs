//! Synthetic audio generators for testing peak capture.
//!
//! Counterpart to `dub_bpm::synthetic`: short helpers that produce
//! deterministic signals whose expected peak envelope can be
//! computed in closed form, so tests assert exact numerical
//! correctness rather than approximate "did anything happen".

/// Constant-amplitude signal: `n_samples` of `amp`. Useful for
/// verifying that a fully-symmetric chunk produces
/// `min == max == rms == amp`.
#[must_use]
pub fn constant(amp: f32, n_samples: usize) -> Vec<f32> {
    vec![amp; n_samples]
}

/// Saw ramp from `0.0` up to `peak` over `n_samples`. Sample `i` is
/// `peak * i / n_samples`. The peak waveform of a long ramp split
/// into equal chunks is itself a ramp, which is a useful invariant
/// for the M10 renderer to validate against.
#[must_use]
pub fn saw_ramp(peak: f32, n_samples: usize) -> Vec<f32> {
    if n_samples == 0 {
        return Vec::new();
    }
    let mut out = vec![0.0f32; n_samples];
    #[allow(clippy::cast_precision_loss)]
    let denom = n_samples as f32;
    for (i, s) in out.iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        {
            *s = peak * (i as f32) / denom;
        }
    }
    out
}

/// Bursts of constant amplitude `amp` of length `burst_samples`,
/// alternating with silence of length `silence_samples`. Repeats
/// `n_bursts` times. Useful for verifying that the peak buffer's
/// `rms` correctly drops to zero in silent regions, with the right
/// chunk-level boundaries.
#[must_use]
pub fn bursts(amp: f32, burst_samples: usize, silence_samples: usize, n_bursts: usize) -> Vec<f32> {
    let total = (burst_samples + silence_samples) * n_bursts;
    let mut out = vec![0.0f32; total];
    for b in 0..n_bursts {
        let off = b * (burst_samples + silence_samples);
        for s in &mut out[off..off + burst_samples] {
            *s = amp;
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn constant_is_constant() {
        let s = constant(0.5, 100);
        assert_eq!(s.len(), 100);
        for v in s {
            assert!((v - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn saw_ramp_endpoints_and_monotonic() {
        let s = saw_ramp(1.0, 100);
        assert_eq!(s.len(), 100);
        assert_eq!(s[0], 0.0);
        // i=99 → 1.0 * 99 / 100 = 0.99
        assert!((s[99] - 0.99).abs() < 1e-6);
        for w in s.windows(2) {
            assert!(w[1] >= w[0]);
        }
    }

    #[test]
    fn bursts_have_correct_layout() {
        // 3 bursts of 2 samples at 0.5, with 4 samples of silence
        // between → total 18 samples: BBSSSSBBSSSSBBSSSS
        let s = bursts(0.5, 2, 4, 3);
        assert_eq!(s.len(), 18);
        assert_eq!(s[0], 0.5);
        assert_eq!(s[1], 0.5);
        assert_eq!(s[2], 0.0);
        assert_eq!(s[5], 0.0);
        assert_eq!(s[6], 0.5);
    }

    #[test]
    fn bursts_empty_for_zero_bursts() {
        assert!(bursts(0.5, 4, 4, 0).is_empty());
    }
}
