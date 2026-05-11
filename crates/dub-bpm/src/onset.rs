//! Spectral-flux onset detection.
//!
//! Given a mono audio stream, emit one "onset detection function" (ODF)
//! sample every `HOP_SIZE` input samples. The ODF spikes wherever the
//! spectral content changes abruptly — i.e., at note onsets, drum hits,
//! and other percussive events. This is the raw material the tempo
//! estimator autocorrelates over.
//!
//! Implementation: standard STFT pipeline.
//!
//! 1. Hann-window an `FRAME_SIZE`-sample frame.
//! 2. Real-input FFT → half-spectrum magnitudes.
//! 3. Spectral flux = sum over bins of `max(0, mag[t] - mag[t-1])`.
//! 4. Slide by `HOP_SIZE` and repeat.
//!
//! References: Dixon (2006) "Onset Detection Revisited"; the same family
//! aubio's "specflux" mode uses. We don't claim sophistication — just
//! "honestly detects clicks at known tempi", which is exactly what M7.5
//! needs.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::{FRAME_SIZE, HOP_SIZE};

/// Streaming onset detector. Construct once, feed audio with [`process`],
/// read the cumulative ODF via [`odf`]. State is cleared with [`reset`].
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

    prev_mag: Vec<f32>,
    have_prev: bool,

    odf: Vec<f32>,
}

impl OnsetDetector {
    pub(crate) fn new() -> Self {
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
        Self {
            r2c,
            input_buffer: Vec::with_capacity(FRAME_SIZE * 4),
            fft_in,
            fft_out,
            fft_scratch,
            window,
            prev_mag: vec![0.0; half_spectrum],
            have_prev: false,
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

            let mut flux = 0.0f32;
            for (i, c) in self.fft_out.iter().enumerate() {
                let mag = c.norm();
                if self.have_prev {
                    let diff = mag - self.prev_mag[i];
                    if diff > 0.0 {
                        flux += diff;
                    }
                }
                self.prev_mag[i] = mag;
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
    /// new audio stream — avoids re-planning the FFT.
    pub(crate) fn reset(&mut self) {
        self.input_buffer.clear();
        self.prev_mag.iter_mut().for_each(|m| *m = 0.0);
        self.have_prev = false;
        self.odf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic;

    #[test]
    fn fresh_detector_has_empty_odf() {
        let d = OnsetDetector::new();
        assert!(d.odf().is_empty());
    }

    #[test]
    fn processes_silence_with_near_zero_flux() {
        let mut d = OnsetDetector::new();
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
        let mut d = OnsetDetector::new();
        let audio = synthetic::click_track(120.0, 5.0, 48_000);
        d.process(&audio);
        let odf = d.odf();

        let max_flux = odf.iter().fold(0.0f32, |a, &b| a.max(b));
        let mean_flux = odf.iter().sum::<f32>() / (odf.len() as f32);
        assert!(
            max_flux > 10.0 * mean_flux,
            "click ODF should be very spiky; max={max_flux} mean={mean_flux}"
        );
    }

    #[test]
    fn reset_clears_odf_and_carry() {
        let mut d = OnsetDetector::new();
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

        let mut a = OnsetDetector::new();
        a.process(&audio);
        let odf_one_shot = a.odf().to_vec();

        let mut b = OnsetDetector::new();
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
