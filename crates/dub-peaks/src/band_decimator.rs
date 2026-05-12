//! Per-band waveform decimator (M9.5b).
//!
//! Thin shell over [`dub_spectral::SpectralFrameStream`]: every FFT
//! hop fires a callback with the compressed per-bin magnitudes; this
//! type aggregates those bins into one RMS-over-band value per
//! [`NUM_BANDS`] band and emits a [`BandPeakChunk`].
//!
//! Counterpart of the broadband [`Decimator`][crate::Decimator]:
//! same online "feed samples, get chunks via emit closure" idiom,
//! but at the FFT-hop cadence ([`BAND_SAMPLES_PER_CHUNK`] = 512
//! samples) rather than 64 samples. The two are designed to be
//! driven in parallel from the same mono audio stream — that's how
//! [`crate::PeakStream`] uses them.
//!
//! ## Why "RMS of compressed magnitudes" rather than physical RMS
//!
//! The renderer wants *perceptual loudness per band* for colour
//! shading. Physical RMS over band bins would weight loud transients
//! disproportionately and give muddy colour during a sustained kick.
//! `dub-spectral`'s `ln(1 + λ · |X|)` compression is already
//! μ-law-ish — RMS over those values gives a stable perceptual
//! energy that's the right input for a per-band colour mix.
//!
//! See [`crate::BandPeakChunk`] for the wire format and indexing
//! contract.

use crate::{BandPeakChunk, BAND_SAMPLES_PER_CHUNK};
use dub_spectral::SpectralFrameStream;

/// Online per-band loudness aggregator.
///
/// Construct with [`BandDecimator::new`], feed mono samples with
/// [`BandDecimator::feed`], reset with [`BandDecimator::reset`].
/// The emit closure fires exactly once per FFT hop.
pub struct BandDecimator {
    spectral: SpectralFrameStream,
}

impl BandDecimator {
    /// Construct a new band decimator targeting `sample_rate` Hz.
    ///
    /// Internally instantiates one [`SpectralFrameStream`]; the FFT
    /// plan is built up front and reused for every frame.
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        Self {
            spectral: SpectralFrameStream::new(sample_rate),
        }
    }

    /// Samples per [`BandPeakChunk`] this decimator emits.
    ///
    /// Always equals [`BAND_SAMPLES_PER_CHUNK`].
    #[must_use]
    #[allow(clippy::unused_self)]
    pub fn samples_per_chunk(&self) -> usize {
        BAND_SAMPLES_PER_CHUNK
    }

    /// Feed mono audio. Closure fires once per completed FFT hop with
    /// a [`BandPeakChunk`] carrying the per-band aggregated loudness.
    ///
    /// Alloc-free after construction (the underlying
    /// `SpectralFrameStream::process` is alloc-free, and the
    /// per-frame work here is a fixed-size sum over band bins).
    pub fn feed<F: FnMut(BandPeakChunk)>(&mut self, samples: &[f32], mut emit: F) {
        self.spectral.process(samples, |compressed, bands| {
            let mut chunk = BandPeakChunk::ZERO;
            for (band_idx, &(lo, hi)) in bands.iter().enumerate() {
                // RMS over the band's compressed per-bin magnitudes.
                // `f64` accumulation for numerical stability on
                // wide bands (the top band has ~ 200 bins at
                // 48 kHz; an f32 sum can lose ~ 1 % accuracy on
                // moderately loud content, which is visible as
                // colour jitter in the renderer).
                let mut sumsq = 0.0f64;
                for &c in compressed.iter().take(hi).skip(lo) {
                    let m = f64::from(c);
                    sumsq += m * m;
                }
                #[allow(clippy::cast_precision_loss)]
                let mean_sq = sumsq / (hi - lo) as f64;
                #[allow(clippy::cast_possible_truncation)]
                {
                    chunk.rms_per_band[band_idx] = mean_sq.sqrt() as f32;
                }
            }
            emit(chunk);
        });
    }

    /// Drop all per-stream state (overlap buffer, previous-frame
    /// markers). The next [`feed`] call starts a fresh stream.
    ///
    /// [`feed`]: Self::feed
    pub fn reset(&mut self) {
        self.spectral.reset();
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::NUM_BANDS;
    use dub_spectral::{FRAME_SIZE, HOP_SIZE};

    const SR: u32 = 48_000;

    fn collect_chunks(samples: &[f32]) -> Vec<BandPeakChunk> {
        let mut d = BandDecimator::new(SR);
        let mut out = Vec::new();
        d.feed(samples, |c| out.push(c));
        out
    }

    // -------- Cadence & alignment ----------------------------------

    #[test]
    fn fresh_decimator_emits_nothing_on_short_input() {
        let chunks = collect_chunks(&[0.0f32; FRAME_SIZE - 1]);
        assert_eq!(chunks.len(), 0, "need a full frame before emit");
    }

    #[test]
    fn one_frame_then_one_hop_emits_two_chunks() {
        let chunks = collect_chunks(&vec![0.5f32; FRAME_SIZE + HOP_SIZE]);
        // FRAME_SIZE = 1024, HOP_SIZE = 512: first 1024 samples →
        // 1 frame; +512 more → 1 more frame. Two band chunks total.
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn samples_per_chunk_equals_hop_size() {
        let d = BandDecimator::new(SR);
        assert_eq!(d.samples_per_chunk(), BAND_SAMPLES_PER_CHUNK);
        assert_eq!(BAND_SAMPLES_PER_CHUNK, HOP_SIZE);
    }

    // -------- Value semantics --------------------------------------

    #[test]
    fn silence_produces_zero_band_chunks() {
        let chunks = collect_chunks(&vec![0.0f32; FRAME_SIZE * 4]);
        assert!(!chunks.is_empty());
        for c in &chunks {
            for (k, &v) in c.rms_per_band.iter().enumerate() {
                assert!(v < 1e-3, "band {k} non-zero on silence: {v}");
            }
        }
    }

    #[test]
    fn pure_low_tone_excites_low_bands_more_than_high_bands() {
        // 60 Hz sine: well inside the bottom band at 48 kHz
        // (lo edge ~30 Hz, hi edge ~60 Hz).
        let n = FRAME_SIZE * 16;
        #[allow(clippy::cast_precision_loss)]
        let audio: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / SR as f32;
                (std::f32::consts::TAU * 60.0 * t).sin() * 0.5
            })
            .collect();
        let chunks = collect_chunks(&audio);
        assert!(!chunks.is_empty());

        // Average across all chunks for stability.
        let mut sums = [0.0f32; NUM_BANDS];
        for c in &chunks {
            for (k, v) in c.rms_per_band.iter().enumerate() {
                sums[k] += v;
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let mean: [f32; NUM_BANDS] = std::array::from_fn(|k| sums[k] / chunks.len() as f32);

        // Band 0 (30-60 Hz region) should be the loudest; the
        // top band (~10-16 kHz) should be near silent.
        assert!(
            mean[0] > mean[NUM_BANDS - 1] * 5.0,
            "low tone should excite low band: {mean:?}"
        );
    }

    #[test]
    fn pure_high_tone_excites_high_bands_more_than_low_bands() {
        // 10 kHz sine: lands inside the top band.
        let n = FRAME_SIZE * 16;
        #[allow(clippy::cast_precision_loss)]
        let audio: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / SR as f32;
                (std::f32::consts::TAU * 10_000.0 * t).sin() * 0.5
            })
            .collect();
        let chunks = collect_chunks(&audio);
        assert!(!chunks.is_empty());

        let mut sums = [0.0f32; NUM_BANDS];
        for c in &chunks {
            for (k, v) in c.rms_per_band.iter().enumerate() {
                sums[k] += v;
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let mean: [f32; NUM_BANDS] = std::array::from_fn(|k| sums[k] / chunks.len() as f32);

        // Top band should dominate; bottom band should be quiet.
        assert!(
            mean[NUM_BANDS - 1] > mean[0] * 5.0,
            "high tone should excite top band: {mean:?}"
        );
    }

    // -------- Streaming invariance ---------------------------------

    #[test]
    fn block_size_does_not_affect_band_chunks() {
        // Same audio, fed in one shot vs many small blocks, must
        // produce identical band chunk sequences.
        #[allow(clippy::cast_precision_loss)]
        let audio: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * 0.001).sin() * 0.3)
            .collect();

        let one_shot = collect_chunks(&audio);

        let mut d = BandDecimator::new(SR);
        let mut streamed = Vec::new();
        for chunk in audio.chunks(123) {
            d.feed(chunk, |c| streamed.push(c));
        }

        assert_eq!(one_shot.len(), streamed.len());
        for (i, (a, b)) in one_shot.iter().zip(streamed.iter()).enumerate() {
            for (k, (av, bv)) in a.rms_per_band.iter().zip(b.rms_per_band.iter()).enumerate() {
                assert!(
                    (av - bv).abs() < 1e-4,
                    "chunk {i} band {k}: one-shot={av} streamed={bv}"
                );
            }
        }
    }

    // -------- Reset ------------------------------------------------

    #[test]
    fn reset_clears_overlap_buffer() {
        let mut d = BandDecimator::new(SR);
        let mut chunks = 0usize;
        // Half a frame — no emit.
        d.feed(&vec![0.5f32; FRAME_SIZE / 2], |_| chunks += 1);
        assert_eq!(chunks, 0);
        d.reset();
        // Now feed another half — reset should have dropped the
        // accumulated 512 samples, so this is again "half a frame".
        d.feed(&vec![0.5f32; FRAME_SIZE / 2], |_| chunks += 1);
        assert_eq!(chunks, 0, "reset did not clear overlap buffer");
        // Full frame now triggers exactly one emit.
        d.feed(&vec![0.5f32; FRAME_SIZE / 2], |_| chunks += 1);
        assert_eq!(chunks, 1);
    }

    // -------- BandPeakChunk default --------------------------------

    #[test]
    fn band_peak_chunk_zero_default() {
        let c = BandPeakChunk::default();
        for v in c.rms_per_band {
            assert_eq!(v, 0.0);
        }
        assert_eq!(c, BandPeakChunk::ZERO);
    }
}
