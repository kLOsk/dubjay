//! Log-band-weighted spectral-flux onset detection.
//!
//! Given a mono audio stream, emit one "onset detection function" (ODF)
//! sample every `HOP_SIZE` input samples. The ODF spikes wherever the
//! spectral content changes abruptly — i.e., at note onsets, drum hits,
//! and other percussive events. This is the raw material the tempo
//! estimator autocorrelates over.
//!
//! ## Architecture (M9.5a)
//!
//! As of M9.5a, the FFT pipeline + Hann window + log-band layout +
//! Klapuri 2006-style magnitude compression all live in the shared
//! [`dub_spectral`] crate. `OnsetDetector` is a thin shell over
//! `SpectralFrameStream` that owns one extra piece of state — the
//! previous frame's per-bin compressed magnitudes — and turns the
//! per-frame callback into a single ODF sample.
//!
//! Algorithm per frame:
//!
//! 1. Receive `(compressed_mags, bands)` from `SpectralFrameStream`
//!    (`compressed[b] = ln(1 + λ · |X[b]|)`).
//! 2. For each log-spaced band, compute
//!    `Σ_b max(0, compressed[b] - prev[b]) / band_bin_count`.
//! 3. ODF sample = equal-weighted sum of the per-band averages.
//! 4. Store `compressed` as `prev` for next frame.
//!
//! ## Why log-band rather than single-bin (M8.1)
//!
//! The original M7.5 single-bin flux (`sum_b max(0, mag - prev_mag)`)
//! systematically mis-detected hip-hop at `2 × BPM` because hi-hats
//! (hundreds of bright onsets per beat across the upper spectrum)
//! dominated the flux sum over kicks (a handful of low-bin onsets).
//! The autocorrelation then saw a periodic spike at the hi-hat period
//! and reported half the beat period.
//!
//! The fix is three ingredients (all preserved post-M9.5a, just moved
//! to where they're shared between consumers):
//!
//! * **Compressed magnitude per bin** — `ln(1 + λ · |X|)` (see
//!   [`dub_spectral::LAMBDA`]). Compresses loud bins so a hi-hat noise
//!   bin (0.1 → 0.5) and a kick bin (0 → 30) end up with comparable
//!   per-bin contributions. Linear near silence, so FFT spectral
//!   leakage in the silence tail doesn't accumulate into phantom
//!   onsets.
//! * **Per-band averaging then equal-weighted sum** — wide high-band
//!   regions (~ 200 bins) no longer outvote narrow low-band regions
//!   (~ 1–2 bins). Each band contributes `1 / NUM_BANDS` of the ODF.
//! * **Choice of `λ`** — pinned in `dub-spectral`.
//!
//! See `tests/genre_octave.rs` for the regression suite that pins this
//! behaviour against single-bin regression.

use dub_spectral::SpectralFrameStream;

/// Streaming onset detector. Construct once with a sample rate (used
/// to lay out the log-spaced frequency bands), feed audio with
/// [`process`], read the cumulative ODF via [`odf`]. State is cleared
/// with [`reset`].
///
/// [`process`]: Self::process
/// [`odf`]: Self::odf
/// [`reset`]: Self::reset
pub(crate) struct OnsetDetector {
    spectral: SpectralFrameStream,
    /// Per-bin compressed magnitude (`ln(1 + λ · |X|)`) of the
    /// previous frame. Size = half spectrum
    /// (`SpectralFrameStream::half_spectrum_size`). Bins outside the
    /// active bands are also tracked so `reset()` has a simple
    /// invariant.
    prev_log_mag: Vec<f32>,
    have_prev: bool,
    odf: Vec<f32>,
}

impl OnsetDetector {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let spectral = SpectralFrameStream::new(sample_rate);
        let half_spectrum = spectral.half_spectrum_size();
        Self {
            spectral,
            prev_log_mag: vec![0.0; half_spectrum],
            have_prev: false,
            odf: Vec::new(),
        }
    }

    /// Feed mono samples. Any number per call; the detector buffers
    /// internally until it has enough for a frame, then emits ODF
    /// samples at the hop rate.
    pub(crate) fn process(&mut self, block: &[f32]) {
        // Split-borrow self so the closure can mutate the per-bin
        // state while `spectral.process` mutates its own buffers.
        let prev = &mut self.prev_log_mag;
        let have_prev = &mut self.have_prev;
        let odf = &mut self.odf;

        self.spectral.process(block, |compressed, bands| {
            // For each band: `Σ_b max(0, compressed[b] - prev[b])`,
            // averaged over the band's bin count. Equal-weighted sum
            // across bands gives the ODF sample.
            let mut flux = 0.0f32;
            for &(lo, hi) in bands {
                let mut band_sum = 0.0f32;
                for b in lo..hi {
                    if *have_prev {
                        let diff = compressed[b] - prev[b];
                        if diff > 0.0 {
                            band_sum += diff;
                        }
                    }
                    prev[b] = compressed[b];
                }
                if *have_prev {
                    #[allow(clippy::cast_precision_loss)]
                    let bin_count = (hi - lo) as f32;
                    flux += band_sum / bin_count;
                }
            }

            if *have_prev {
                odf.push(flux);
            } else {
                // First frame has nothing to diff against → emit 0 so
                // the ODF index lines up with hop boundaries.
                odf.push(0.0);
                *have_prev = true;
            }
        });
    }

    /// Cumulative ODF computed so far.
    pub(crate) fn odf(&self) -> &[f32] {
        &self.odf
    }

    /// Consume the detector and return its accumulated ODF by
    /// move. Used by the offline beat-grid path
    /// (`analyze_bpm_with_range_and_odf`) to avoid cloning what
    /// can be ~50 k f32 samples on a 5-minute track.
    pub(crate) fn into_odf(self) -> Vec<f32> {
        self.odf
    }

    /// Clear all state. Same `OnsetDetector` instance can then analyze a
    /// new audio stream — avoids re-planning the FFT and re-computing
    /// the band-bin map.
    pub(crate) fn reset(&mut self) {
        self.spectral.reset();
        for m in &mut self.prev_log_mag {
            *m = 0.0;
        }
        self.have_prev = false;
        self.odf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic;
    use crate::FRAME_SIZE;

    const SR: u32 = 48_000;

    #[test]
    fn fresh_detector_has_empty_odf() {
        let d = OnsetDetector::new(SR);
        assert!(d.odf().is_empty());
    }

    #[test]
    fn processes_silence_with_near_zero_flux() {
        let mut d = OnsetDetector::new(SR);
        d.process(&vec![0.0f32; 48_000]);
        // (48000 - 1024) / 512 + 1 = ~92 frames
        assert!(d.odf().len() > 80);
        let max_flux = d.odf().iter().fold(0.0f32, |a, &b| a.max(b));
        assert!(
            max_flux < 1e-3,
            "silence should produce ~0 flux; got max {max_flux}"
        );
    }

    #[test]
    fn click_track_produces_periodic_odf_spikes() {
        // Sanity: feed a 120-BPM click track and verify the ODF has
        // strong spikes spaced at ≈ 47 ODF samples (60 / 120 = 0.5 s,
        // odf_sr ≈ 93.75, period ≈ 47).
        let mut d = OnsetDetector::new(SR);
        let audio = synthetic::click_track(120.0, 5.0, 48_000);
        d.process(&audio);
        let odf = d.odf();

        let max_flux = odf.iter().fold(0.0f32, |a, &b| a.max(b));
        #[allow(clippy::cast_precision_loss)]
        let mean_flux = odf.iter().sum::<f32>() / (odf.len() as f32);
        assert!(
            max_flux > 5.0 * mean_flux,
            "click ODF should be spiky; max={max_flux} mean={mean_flux}"
        );
    }

    #[test]
    fn reset_clears_odf_and_carry() {
        let mut d = OnsetDetector::new(SR);
        d.process(&vec![0.5f32; 4096]);
        assert!(!d.odf().is_empty());
        d.reset();
        assert!(d.odf().is_empty());
        // After reset, the first frame should again emit a 0 (no prev).
        d.process(&vec![1.0f32; FRAME_SIZE]);
        // Exactly one ODF sample, value 0 (first frame after reset).
        assert_eq!(d.odf().len(), 1);
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(d.odf()[0], 0.0);
        }
    }

    #[test]
    fn block_size_does_not_affect_odf() {
        // Same audio fed in one big block vs many small blocks must
        // produce identical ODF — this is the "block-size invariance"
        // contract the streaming driver depends on.
        let audio = synthetic::click_track(140.0, 3.0, 48_000);

        let mut a = OnsetDetector::new(SR);
        a.process(&audio);
        let odf_one_shot = a.odf().to_vec();

        let mut b = OnsetDetector::new(SR);
        for chunk in audio.chunks(123) {
            b.process(chunk);
        }
        let odf_streamed = b.odf().to_vec();

        assert_eq!(odf_one_shot.len(), odf_streamed.len());
        for (i, (a, b)) in odf_one_shot.iter().zip(odf_streamed.iter()).enumerate() {
            assert!(
                (a - b).abs() < 1e-4,
                "ODF mismatch at {i}: one-shot={a}, streamed={b}"
            );
        }
    }
}
