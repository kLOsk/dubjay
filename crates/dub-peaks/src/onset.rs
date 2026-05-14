//! Log-band-weighted spectral-flux onset capture for the renderer
//! (M10.5l).
//!
//! Sibling of [`BandDecimator`] — same FFT-hop cadence
//! ([`BAND_SAMPLES_PER_CHUNK`] = 512 samples) on the same shared
//! [`SpectralFrameStream`] primitive, but emitting a single "did
//! something just hit" flux value per hop instead of a per-band
//! loudness vector.
//!
//! The renderer pairs the resulting `OnsetChunk` stream with the
//! existing broadband [`PeakChunk`] + per-band [`BandPeakChunk`]
//! streams to drive **transient-emphasising bloom + saturation**
//! in the M10.5l waveform shader: confirmed onsets (kicks, snares,
//! perc hits) bloom hot and saturate vividly, while sustained
//! content (pads, vocals held over multiple beats) desaturates
//! toward grey. The result is a Serato / Traktor-style visual
//! hierarchy where the eye instinctively lands on "where's the
//! kick / snare" rather than scanning a uniform field of pastel
//! colour.
//!
//! ## Algorithm
//!
//! Identical to [`dub_bpm::onset`]'s ODF computation — they share
//! the [`dub_spectral::SpectralFrameStream`] primitive and the same
//! Klapuri-style per-band weighted-flux summation:
//!
//! 1. Receive `(compressed_mags, bands)` from `SpectralFrameStream`.
//!    `compressed[b] = ln(1 + λ · |X[b]|)`.
//! 2. For each log-spaced band, compute
//!    `Σ_b max(0, compressed[b] - prev[b]) / band_bin_count`.
//! 3. Emit ODF sample = equal-weighted sum of the per-band averages.
//! 4. Store `compressed` as `prev` for the next frame.
//!
//! Why duplicate the math from `dub-bpm` rather than re-exporting
//! it: the rendering pipeline must continue to expose onset values
//! to the GPU even when no `BpmStream` is running (e.g. File-mode
//! playback in M10.5+, or future single-deck Prep configurations).
//! Tying the renderer to the BPM crate's analysis thread would
//! couple two independent off-RT pipelines and re-introduce the
//! kind of lifecycle headache the M9 split was specifically
//! designed to avoid. The two implementations are tiny (~30 lines
//! of FFT-frame work each) and live in the same workspace, so a
//! divergence would surface immediately in the workspace tests.
//!
//! ## What "onset flux" means in the renderer
//!
//! The shader treats each per-hop flux value as a "confirmed
//! transient" indicator and applies a sigmoid mapping `conf =
//! 1 - exp(-flux × k)` to turn it into a [0, 1] confidence. Flux
//! ≈ 0 on sustained / silent content; flux ≈ 5–15 on a typical
//! drum hit (very stretchy because Klapuri compression is in
//! `ln` units). The sigmoid saturates cleanly so the shader
//! doesn't need to track per-track normalisation state.

use crate::OnsetChunk;
use dub_spectral::SpectralFrameStream;

/// Online onset-flux aggregator. Mirrors [`crate::BandDecimator`]'s
/// surface (`new` / `feed` / `reset`); the emit closure fires
/// exactly once per FFT hop with a single-`f32` [`OnsetChunk`].
///
/// Constructing one [`SpectralFrameStream`] per decimator costs a
/// second FFT plan + hop-overlap buffer per deck (≈ 20 KiB scratch
/// total). The marginal CPU cost is one forward FFT per hop —
/// well under the analysis thread's 20 ms drain budget at 48 kHz
/// (the FFT runs in ~ 30 µs on M-class silicon, the band kernel
/// adds ~ 10 µs more, and we get one hop every ~ 10.6 ms).
pub struct OnsetDecimator {
    spectral: SpectralFrameStream,
    /// Per-bin compressed magnitude (`ln(1 + λ · |X|)`) from the
    /// previous frame. Size = `spectral.half_spectrum_size()`. Bins
    /// outside the active bands stay at zero so `reset()` has a
    /// simple invariant (zero the whole vec).
    prev_log_mag: Vec<f32>,
    /// `false` until the first frame is processed. The first frame
    /// has nothing to diff against and emits a flux of 0.
    have_prev: bool,
}

impl OnsetDecimator {
    /// Construct an onset decimator targeting `sample_rate` Hz. One
    /// [`SpectralFrameStream`] is built up front and reused for
    /// every frame. The half-spectrum-sized `prev_log_mag` Vec is
    /// also allocated here so `feed` is alloc-free in steady state.
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        let spectral = SpectralFrameStream::new(sample_rate);
        let half_spectrum = spectral.half_spectrum_size();
        Self {
            spectral,
            prev_log_mag: vec![0.0; half_spectrum],
            have_prev: false,
        }
    }

    /// Samples per [`OnsetChunk`] this decimator emits. Always
    /// equals [`crate::BAND_SAMPLES_PER_CHUNK`] (= one FFT hop).
    /// Provided for parity with [`crate::BandDecimator::samples_per_chunk`]
    /// so the streaming driver can pass an identical value through
    /// to the FFI surface without branching on which decimator
    /// produced it.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub fn samples_per_chunk(&self) -> usize {
        crate::BAND_SAMPLES_PER_CHUNK
    }

    /// Feed mono audio. The emit closure fires once per completed
    /// FFT hop with an [`OnsetChunk`] carrying the weighted-flux
    /// value for that hop. Block size is arbitrary; the underlying
    /// `SpectralFrameStream` buffers internally.
    pub fn feed<F: FnMut(OnsetChunk)>(&mut self, samples: &[f32], mut emit: F) {
        let prev = &mut self.prev_log_mag;
        let have_prev = &mut self.have_prev;

        self.spectral.process(samples, |compressed, bands| {
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
                emit(OnsetChunk { flux });
            } else {
                emit(OnsetChunk { flux: 0.0 });
                *have_prev = true;
            }
        });
    }

    /// Drop all per-stream state. Mirrors
    /// [`crate::BandDecimator::reset`] — same use case (a new audio
    /// source attaches to the same decimator instance and wants a
    /// clean slate without re-planning the FFT).
    pub fn reset(&mut self) {
        self.spectral.reset();
        for m in &mut self.prev_log_mag {
            *m = 0.0;
        }
        self.have_prev = false;
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use dub_spectral::{FRAME_SIZE, HOP_SIZE};

    const SR: u32 = 48_000;

    fn collect_chunks(samples: &[f32]) -> Vec<OnsetChunk> {
        let mut d = OnsetDecimator::new(SR);
        let mut out = Vec::new();
        d.feed(samples, |c| out.push(c));
        out
    }

    #[test]
    fn fresh_decimator_emits_nothing_on_short_input() {
        let chunks = collect_chunks(&[0.0f32; FRAME_SIZE - 1]);
        assert_eq!(chunks.len(), 0);
    }

    #[test]
    fn one_frame_then_one_hop_emits_two_chunks() {
        let chunks = collect_chunks(&vec![0.5f32; FRAME_SIZE + HOP_SIZE]);
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn samples_per_chunk_equals_hop_size() {
        let d = OnsetDecimator::new(SR);
        assert_eq!(d.samples_per_chunk(), crate::BAND_SAMPLES_PER_CHUNK);
        assert_eq!(crate::BAND_SAMPLES_PER_CHUNK, HOP_SIZE);
    }

    #[test]
    fn silence_produces_zero_flux() {
        // First-frame zero is the "have_prev == false" placeholder,
        // not the algorithm computing flux. Every subsequent frame
        // must also emit ~0 — silence has nothing to diff against
        // silence, by definition.
        let chunks = collect_chunks(&vec![0.0f32; FRAME_SIZE * 8]);
        assert!(!chunks.is_empty());
        for (i, c) in chunks.iter().enumerate() {
            assert!(c.flux < 1e-3, "silence chunk {i} non-zero: {}", c.flux);
        }
    }

    #[test]
    fn click_track_produces_periodic_spikes() {
        // Synthesise a 120 BPM click as a few-sample-wide impulse
        // train. The flux time series should be near-zero everywhere
        // except at the impulses where it spikes hard.
        let beat_period_samples: usize = (u64::from(SR) * 60 / 120) as usize;
        let mut audio = vec![0.0f32; SR as usize * 5];
        let mut t = 0;
        while t < audio.len() {
            for s in t..(t + 16).min(audio.len()) {
                audio[s] = 1.0;
            }
            t += beat_period_samples;
        }
        let chunks = collect_chunks(&audio);
        let max_flux = chunks.iter().fold(0.0f32, |a, c| a.max(c.flux));
        #[allow(clippy::cast_precision_loss)]
        let mean_flux = chunks.iter().map(|c| c.flux).sum::<f32>() / chunks.len() as f32;
        assert!(
            max_flux > 5.0 * mean_flux,
            "click ODF should be spiky; max={max_flux} mean={mean_flux}"
        );
    }

    #[test]
    fn block_size_invariance() {
        // Same audio fed in one shot vs many small blocks must
        // produce identical flux sequences — the streaming
        // contract the M9-style worker thread depends on.
        let audio: Vec<f32> = (0..48_000_i32)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32;
                (t * 0.01).sin() * 0.3
            })
            .collect();

        let one_shot = collect_chunks(&audio);

        let mut d = OnsetDecimator::new(SR);
        let mut streamed = Vec::new();
        for chunk in audio.chunks(123) {
            d.feed(chunk, |c| streamed.push(c));
        }

        assert_eq!(one_shot.len(), streamed.len());
        for (i, (a, b)) in one_shot.iter().zip(streamed.iter()).enumerate() {
            assert!(
                (a.flux - b.flux).abs() < 1e-4,
                "chunk {i}: one-shot={} streamed={}",
                a.flux,
                b.flux
            );
        }
    }

    #[test]
    fn reset_clears_have_prev() {
        // After reset, the *next* frame should again emit a
        // "first-frame" zero rather than diffing against the
        // pre-reset content.
        let mut d = OnsetDecimator::new(SR);
        let mut chunks = Vec::new();
        d.feed(&vec![0.5f32; FRAME_SIZE * 4], |c| chunks.push(c));
        let pre_reset_len = chunks.len();
        assert!(pre_reset_len > 0);
        d.reset();
        chunks.clear();
        // Re-feed enough to trigger one full frame post-reset.
        d.feed(&vec![1.0f32; FRAME_SIZE], |c| chunks.push(c));
        assert_eq!(chunks.len(), 1);
        // First chunk after reset must be the zero placeholder.
        assert_eq!(chunks[0].flux, 0.0);
    }
}
