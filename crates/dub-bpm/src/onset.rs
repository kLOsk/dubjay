//! Log-band-weighted spectral-flux onset detection.
//!
//! Given a mono audio stream, emit one "onset detection function" (ODF)
//! sample every `HOP_SIZE` input samples. The ODF spikes wherever the
//! spectral content changes abruptly — i.e., at note onsets, drum hits,
//! and other percussive events. This is the raw material the tempo
//! estimator autocorrelates over.
//!
//! Implementation: standard STFT pipeline with **per-band log-magnitude
//! flux**, summed equally across `NUM_ODF_BANDS` log-spaced frequency
//! bands from `ODF_BAND_MIN_HZ` to `ODF_BAND_MAX_HZ`.
//!
//! 1. Hann-window an `FRAME_SIZE`-sample frame.
//! 2. Real-input FFT → half-spectrum magnitudes.
//! 3. For each bin: `compressed = ln(1 + λ · |X[b]|)`
//!    (Klapuri 2006-style μ-law-ish compression — linear near silence
//!    so noise stays quiet, logarithmic at audible levels so loud bins
//!    don't dominate).
//! 4. For each log-spaced band `k`: average over its bins of
//!    `max(0, compressed[t,b] - compressed[t-1,b])`.
//! 5. ODF sample = equal-weighted sum of the `NUM_ODF_BANDS` band fluxes.
//! 6. Slide by `HOP_SIZE` and repeat.
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
//! The fix is three ingredients:
//!
//! * **Compressed magnitude per bin** — `ln(1 + λ · |X|)` compresses
//!   loud bins, so a hi-hat noise bin (0.1 → 0.5) and a kick bin
//!   (0 → 30) end up with comparable per-bin contributions. Unlike
//!   the naive `ln(ε + |X|)` formulation, this one is *linear near
//!   silence* — `ln(1 + λ · 1e-7) ≈ λ · 1e-7` for tiny `|X|`. That
//!   matters because FFT spectral leakage from a single decaying
//!   transient leaves tiny per-bin magnitudes for hundreds of frames
//!   in the tail; the naive log formulation amplified that into ODF
//!   noise that correlated with itself and produced a phantom
//!   tempo on the `single_click` honesty test.
//! * **Per-band averaging then equal-weighted sum** — wide high-band
//!   regions (~ 200 bins) no longer outvote narrow low-band regions
//!   (~ 1–2 bins). The kick band, the snare band and each of the
//!   hi-hat bands each contribute `1 / NUM_ODF_BANDS` of the ODF.
//! * **Choice of `λ`** — pinned at `LAMBDA` = 1000. At that value an
//!   audible bin (`|X| ≈ 0.01`) compresses to `ln(11) ≈ 2.4`, a loud
//!   bin (`|X| ≈ 1`) to `ln(1001) ≈ 6.9`. The 3× separation that
//!   matters for the octave fix is preserved; silence still
//!   diffs to ≈ 0.
//!
//! See `tests/genre_octave.rs` for the regression suite that pins this
//! behaviour against single-bin regression.
//!
//! References: Goto & Muraoka (1994); Klapuri (2006); Davies & Plumbley
//! (2007). Aubio's `kl` mode is the closest in-the-wild equivalent
//! (Kullback-Liebler is essentially log-flux on a different
//! normalisation).

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::{FRAME_SIZE, HOP_SIZE, NUM_ODF_BANDS, ODF_BAND_MAX_HZ, ODF_BAND_MIN_HZ};

/// Compression coefficient for the per-bin magnitude transform
/// `ln(1 + λ · |X|)`.
///
/// * Linear regime: when `λ · |X| ≪ 1` (i.e. `|X| < 1e-4` at λ = 1000),
///   `ln(1 + λ |X|) ≈ λ |X|` and the response is *linear* in the
///   underlying spectral magnitude. This keeps spectral-leakage noise
///   from a decaying transient in the silence tail down at the linear
///   magnitude level — far below the audible flux that real onsets
///   produce. The `single_click_has_zero_confidence` test depends on
///   this regime being effectively noise-free.
/// * Log regime: when `λ · |X| ≫ 1` (i.e. `|X| > 0.01`), the
///   response is `≈ ln(λ |X|)` and dynamic range gets compressed.
///   A kick bin at `|X| ≈ 1` and a hi-hat bin at `|X| ≈ 0.1`
///   compress to `ln(1001) ≈ 6.9` and `ln(101) ≈ 4.6` respectively
///   — well within an order of magnitude. That suppression is what
///   prevents the hip-hop hi-hats from out-voting kicks in the
///   per-band ODF, which is the M8.1 octave-error fix.
///
/// `λ = 1000` is the smallest λ that puts the typical audible
/// drum-hit magnitude (`|X| ≈ 0.1–1.0`) firmly in the log regime
/// without dragging quiet but legitimate content (`|X| ≈ 1e-3`)
/// down into the linear noise floor.
const LAMBDA: f32 = 1000.0;

/// Streaming onset detector. Construct once with a sample rate (used
/// to lay out the log-spaced frequency bands), feed audio with
/// [`process`], read the cumulative ODF via [`odf`]. State is cleared
/// with [`reset`].
///
/// [`process`]: Self::process
/// [`odf`]: Self::odf
/// [`reset`]: Self::reset
pub(crate) struct OnsetDetector {
    r2c: Arc<dyn RealToComplex<f32>>,

    input_buffer: Vec<f32>,

    fft_in: Vec<f32>,
    fft_out: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,

    window: Vec<f32>,

    /// Per-bin compressed magnitude (`ln(1 + λ · |X|)`) of the
    /// previous frame. Size = half spectrum (`FRAME_SIZE / 2 + 1`).
    /// Bins outside the active bands are still tracked so `reset()`
    /// has a simple invariant.
    prev_log_mag: Vec<f32>,
    have_prev: bool,

    /// Half-open bin ranges `[lo, hi)` per log-spaced band. Computed
    /// once at construction from the sample rate; never re-allocated.
    bands: [(usize, usize); NUM_ODF_BANDS],

    odf: Vec<f32>,
}

impl OnsetDetector {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(FRAME_SIZE);
        let fft_in = r2c.make_input_vec();
        let fft_out = r2c.make_output_vec();
        let fft_scratch = r2c.make_scratch_vec();

        let n = FRAME_SIZE;
        #[allow(clippy::cast_precision_loss)]
        let nf = n as f32;
        let window: Vec<f32> = (0..n)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let phase = std::f32::consts::TAU * (i as f32) / nf;
                0.5 * (1.0 - phase.cos())
            })
            .collect();

        let half_spectrum = FRAME_SIZE / 2 + 1;
        let bands = compute_band_bins(sample_rate, half_spectrum);

        Self {
            r2c,
            input_buffer: Vec::with_capacity(FRAME_SIZE * 4),
            fft_in,
            fft_out,
            fft_scratch,
            window,
            prev_log_mag: vec![0.0; half_spectrum],
            have_prev: false,
            bands,
            odf: Vec::new(),
        }
    }

    /// Feed mono samples. Any number per call; the detector buffers
    /// internally until it has enough for a frame, then emits ODF
    /// samples at the hop rate.
    pub(crate) fn process(&mut self, block: &[f32]) {
        self.input_buffer.extend_from_slice(block);

        while self.input_buffer.len() >= FRAME_SIZE {
            for i in 0..FRAME_SIZE {
                self.fft_in[i] = self.input_buffer[i] * self.window[i];
            }

            self.r2c
                .process_with_scratch(&mut self.fft_in, &mut self.fft_out, &mut self.fft_scratch)
                .expect("FFT can't fail on correctly-sized in/out/scratch vectors");

            // M8.1: log-band-weighted spectral flux on compressed
            // magnitudes (see module docs for the `ln(1 + λ |X|)`
            // rationale).
            //
            // For each band we accumulate `Σ max(0, c[t,b] -
            // c[t-1,b])` over the band's bins (`c = ln(1 + λ |X|)`),
            // then divide by the band's bin count and sum across all
            // bands.
            //
            // We update `prev_log_mag` only for bins we touch (those
            // inside band ranges). Bins outside any band keep their
            // previous value, which is fine because they don't
            // contribute to the ODF.
            let mut flux = 0.0f32;
            for &(lo, hi) in &self.bands {
                let mut band_sum = 0.0f32;
                for b in lo..hi {
                    let mag = self.fft_out[b].norm();
                    let compressed = (1.0 + LAMBDA * mag).ln();
                    if self.have_prev {
                        let diff = compressed - self.prev_log_mag[b];
                        if diff > 0.0 {
                            band_sum += diff;
                        }
                    }
                    self.prev_log_mag[b] = compressed;
                }
                if self.have_prev {
                    let bin_count = (hi - lo) as f32;
                    flux += band_sum / bin_count;
                }
            }

            if self.have_prev {
                self.odf.push(flux);
            } else {
                // First frame has nothing to diff against → emit 0 so
                // the ODF index lines up with hop boundaries.
                self.odf.push(0.0);
                self.have_prev = true;
            }

            // Slide the analysis window forward by HOP_SIZE. drain() is
            // O(remaining); for offline / per-block processing this is
            // fine. The M8 streaming driver will replace this with a
            // ring buffer when we care about per-block latency.
            self.input_buffer.drain(..HOP_SIZE);
        }
    }

    /// Cumulative ODF computed so far.
    pub(crate) fn odf(&self) -> &[f32] {
        &self.odf
    }

    /// Clear all state. Same `OnsetDetector` instance can then analyze a
    /// new audio stream — avoids re-planning the FFT and re-computing
    /// the band-bin map.
    pub(crate) fn reset(&mut self) {
        self.input_buffer.clear();
        self.prev_log_mag.iter_mut().for_each(|m| *m = 0.0);
        self.have_prev = false;
        self.odf.clear();
    }
}

/// Lay out `NUM_ODF_BANDS` log-spaced half-open bin ranges over
/// `[ODF_BAND_MIN_HZ, ODF_BAND_MAX_HZ]` at the given `sample_rate`.
///
/// Uses `ceil()` on both edges so adjacent bands are contiguous and
/// non-overlapping. Each band is forced to contain at least one bin
/// (guards against the lowest band collapsing at small FFT sizes /
/// high sample rates). Both endpoints are clamped to `[1, n_bins]`
/// — we deliberately skip the DC bin because it carries no rhythmic
/// information and can drift with low-frequency room tone.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn compute_band_bins(sample_rate: u32, n_bins: usize) -> [(usize, usize); NUM_ODF_BANDS] {
    let bin_hz = sample_rate as f32 / FRAME_SIZE as f32;
    let ratio = (ODF_BAND_MAX_HZ / ODF_BAND_MIN_HZ).powf(1.0 / NUM_ODF_BANDS as f32);

    let mut edges = [0usize; NUM_ODF_BANDS + 1];
    for (i, slot) in edges.iter_mut().enumerate() {
        let hz = ODF_BAND_MIN_HZ * ratio.powi(i as i32);
        let bin = (hz / bin_hz).ceil() as usize;
        *slot = bin.clamp(1, n_bins);
    }

    let mut bands = [(0usize, 0usize); NUM_ODF_BANDS];
    for k in 0..NUM_ODF_BANDS {
        let lo = edges[k];
        // Ensure each band carries at least one bin, even when the
        // log-spaced edge collapses two adjacent boundaries onto the
        // same bin (lowest band at high sample rates).
        let hi = edges[k + 1].max(lo + 1).min(n_bins);
        bands[k] = (lo, hi);
    }
    bands
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic;

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

    // -------- M8.1: log-band layout ---------------------------

    #[test]
    fn band_bins_are_contiguous_and_non_overlapping_at_48k() {
        let n_bins = FRAME_SIZE / 2 + 1;
        let bands = compute_band_bins(48_000, n_bins);
        // Lowest band starts at bin ≥ 1 (DC excluded).
        assert!(bands[0].0 >= 1, "lowest band must skip DC: {bands:?}");
        // Highest band ends at bin ≤ n_bins.
        assert!(
            bands[NUM_ODF_BANDS - 1].1 <= n_bins,
            "highest band must stay within Nyquist: {bands:?}"
        );
        // Each band has at least one bin.
        for (k, &(lo, hi)) in bands.iter().enumerate() {
            assert!(hi > lo, "band {k} has zero width: {bands:?}");
        }
        // Adjacent bands are contiguous: `bands[k].1 == bands[k+1].0`
        // (with the same `+1` carve-out the implementation uses to
        // guarantee non-empty bands — which can shift the next
        // band's lower edge by 1 in the degenerate case).
        for k in 0..NUM_ODF_BANDS - 1 {
            let gap = bands[k + 1].0 as i64 - bands[k].1 as i64;
            assert!(
                gap.abs() <= 1,
                "bands {k} and {} not contiguous: {bands:?}",
                k + 1
            );
        }
    }

    #[test]
    fn band_bins_layout_is_sane_at_44_1k() {
        // CD-quality sample rate: smaller bin width (43 Hz vs 47 Hz
        // at 48 k) — verify the lowest band still has at least one
        // bin and the highest band still falls under Nyquist.
        let n_bins = FRAME_SIZE / 2 + 1;
        let bands = compute_band_bins(44_100, n_bins);
        for (k, &(lo, hi)) in bands.iter().enumerate() {
            assert!(hi > lo, "band {k} empty at 44.1k: {bands:?}");
            assert!(lo >= 1, "band {k} includes DC at 44.1k: {bands:?}");
            assert!(hi <= n_bins, "band {k} past Nyquist at 44.1k: {bands:?}");
        }
    }
}
