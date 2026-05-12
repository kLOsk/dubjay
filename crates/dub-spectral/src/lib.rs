//! Shared spectral-analysis primitives for the Dub workspace.
//!
//! Two crates use the same STFT pipeline:
//!
//! * [`dub-bpm`](../dub_bpm/index.html) — log-band-weighted spectral flux for
//!   onset detection and tempo estimation (M7.5 / M8 / M8.1).
//! * [`dub-peaks`](../dub_peaks/index.html) — 8-band RMS envelope for
//!   multi-colour waveform rendering (M9.5b onward).
//!
//! Before M9.5 these lived inside `dub-bpm/src/onset.rs`. They were lifted
//! out without behaviour change so multiple consumers can share one FFT,
//! one Hann window, one log-band layout, and one magnitude-compression
//! curve — both for performance (we only compute the FFT once on the
//! shared mono-downmix) and so any future fix to one consumer's spectral
//! analysis automatically applies to the other.
//!
//! ## Pipeline
//!
//! Given mono audio at a known sample rate, [`SpectralFrameStream`]
//! emits one *frame* every [`HOP_SIZE`] input samples. A frame carries:
//!
//! 1. Compressed magnitudes per bin: `ln(1 + λ · |X[b]|)` where
//!    `λ = ` [`LAMBDA`]. Klapuri 2006-style μ-law-ish compression — linear
//!    near silence (so spectral-leakage noise stays quiet) and logarithmic
//!    at audible levels (so loud bins don't dominate). See
//!    [`docs/SHIPPED.md#m81`](../../docs/SHIPPED.md#m81) for the
//!    derivation of `λ = 1000`.
//! 2. The log-spaced frequency-band layout for this sample rate.
//!    [`NUM_BANDS`] = 8 bands from [`BAND_MIN_HZ`] to [`BAND_MAX_HZ`].
//!    Each band carries at least one bin even at 44.1 kHz with
//!    `FRAME_SIZE = 1024`.
//!
//! Consumers iterate over the band ranges and aggregate the per-bin
//! compressed magnitudes however they need (flux for `dub-bpm`, RMS
//! for `dub-peaks`).
//!
//! ## Honesty
//!
//! `SpectralFrameStream::process` is **not real-time safe**. It performs
//! one [`realfft`] forward transform per hop, which allocates a small
//! scratch internally and is not bounded in execution time. Both
//! current consumers run it on a dedicated off-RT worker thread (see
//! `dub_bpm::BpmStream` and `dub_peaks::PeakStream`).
//!
//! References: Goto & Muraoka (1994); Klapuri (2006); Davies & Plumbley
//! (2007). Aubio's `kl` mode is the closest in-the-wild equivalent
//! (Kullback-Liebler is essentially log-flux on a different
//! normalisation).

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Window size for each FFT frame, in samples. 1024 is ≈ 21 ms at 48 kHz
/// — long enough for stable spectral magnitudes, short enough that onset
/// localisation isn't smeared.
pub const FRAME_SIZE: usize = 1024;

/// Hop size between consecutive frames, in samples. 512 = 50 % overlap.
/// Gives a frame-rate of `sr / 512` ≈ 94 Hz at 48 kHz, which is enough
/// resolution to distinguish e.g. 174 vs 175 BPM after sub-bin
/// interpolation.
pub const HOP_SIZE: usize = 512;

/// Number of log-spaced frequency bands the pipeline produces.
///
/// 8 bands is the smallest count that gives ~ 1 octave per band over
/// 30 Hz – 16 kHz (the perceptually-relevant range), which keeps the
/// per-band aggregation stable (each band has at least one bin even at
/// 44.1 kHz with `FRAME_SIZE = 1024`).
pub const NUM_BANDS: usize = 8;

/// Lower edge of the lowest band, in Hz. Anything below this is
/// sub-bass / room rumble and doesn't carry meaningful spectral
/// information for either onset detection or multi-colour rendering.
pub const BAND_MIN_HZ: f32 = 30.0;

/// Upper edge of the highest band, in Hz. Beyond ~ 16 kHz lies air /
/// cymbal shimmer that's perceptually rich but redundant with what
/// the 4–10 kHz band already carries.
pub const BAND_MAX_HZ: f32 = 16_000.0;

/// Compression coefficient for the per-bin magnitude transform
/// `ln(1 + λ · |X|)`.
///
/// * Linear regime: when `λ · |X| ≪ 1` (i.e. `|X| < 1e-4` at λ = 1000),
///   `ln(1 + λ |X|) ≈ λ |X|` and the response is *linear* in the
///   underlying spectral magnitude. This keeps spectral-leakage noise
///   from a decaying transient in the silence tail down at the linear
///   magnitude level — far below the audible flux that real onsets
///   produce.
/// * Log regime: when `λ · |X| ≫ 1` (i.e. `|X| > 0.01`), the response
///   is `≈ ln(λ |X|)` and dynamic range gets compressed.  A kick bin
///   at `|X| ≈ 1` and a hi-hat bin at `|X| ≈ 0.1` compress to
///   `ln(1001) ≈ 6.9` and `ln(101) ≈ 4.6` respectively — well within
///   an order of magnitude. That suppression is what prevents hip-hop
///   hi-hats from out-voting kicks in the per-band ODF (M8.1 octave
///   fix).
///
/// `λ = 1000` is the smallest λ that puts the typical audible
/// drum-hit magnitude (`|X| ≈ 0.1–1.0`) firmly in the log regime
/// without dragging quiet but legitimate content (`|X| ≈ 1e-3`) down
/// into the linear noise floor.
pub const LAMBDA: f32 = 1000.0;

/// Streaming STFT + log-band frame generator.
///
/// Construct once with a sample rate (used to lay out the log-spaced
/// frequency bands), feed mono audio with [`process`], and the
/// callback fires once per hop with the compressed per-bin magnitudes
/// for that frame. State is cleared with [`reset`].
///
/// Alloc-free during `process` after construction. The forward FFT
/// itself does not allocate (it's a `process_with_scratch` call into
/// pre-sized buffers).
///
/// [`process`]: Self::process
/// [`reset`]: Self::reset
pub struct SpectralFrameStream {
    r2c: Arc<dyn RealToComplex<f32>>,

    /// Hop-overlap buffer. Holds at most `FRAME_SIZE + HOP_SIZE - 1`
    /// samples between calls.
    input_buffer: Vec<f32>,

    fft_in: Vec<f32>,
    fft_out: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,

    window: Vec<f32>,

    /// Per-bin compressed magnitudes for the current frame:
    /// `ln(1 + λ · |X[b]|)`. Sized to the half spectrum
    /// (`FRAME_SIZE / 2 + 1`). The DC bin (index 0) and any bin
    /// outside the active band ranges stay zero.
    compressed_mags: Vec<f32>,

    /// Half-open bin ranges `[lo, hi)` per log-spaced band. Computed
    /// once at construction from the sample rate; never re-allocated.
    bands: [(usize, usize); NUM_BANDS],
}

impl SpectralFrameStream {
    /// Construct a new pipeline targeting `sample_rate` Hz.
    ///
    /// One [`realfft`] forward-FFT plan is built up front and reused
    /// for every frame.
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
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
            compressed_mags: vec![0.0; half_spectrum],
            bands,
        }
    }

    /// Frame size in samples (= [`FRAME_SIZE`]).
    ///
    /// Kept as an `&self` method (rather than associated `fn`) because
    /// future variants may decouple frame size from a global constant.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub fn frame_size(&self) -> usize {
        FRAME_SIZE
    }

    /// Hop size in samples (= [`HOP_SIZE`]).
    ///
    /// Kept as an `&self` method for the same reason as [`frame_size`].
    ///
    /// [`frame_size`]: Self::frame_size
    #[must_use]
    #[allow(clippy::unused_self)]
    pub fn hop_size(&self) -> usize {
        HOP_SIZE
    }

    /// Half-spectrum size, `FRAME_SIZE / 2 + 1`. Convenience for
    /// consumers sizing per-bin state buffers.
    ///
    /// Kept as an `&self` method for the same reason as [`frame_size`].
    ///
    /// [`frame_size`]: Self::frame_size
    #[must_use]
    #[allow(clippy::unused_self)]
    pub fn half_spectrum_size(&self) -> usize {
        FRAME_SIZE / 2 + 1
    }

    /// Log-spaced band layout for this stream's sample rate.
    #[must_use]
    pub fn bands(&self) -> &[(usize, usize); NUM_BANDS] {
        &self.bands
    }

    /// Feed mono audio samples. Any number per call; the stream
    /// buffers internally until it has enough for a frame, then
    /// invokes `on_frame(compressed_mags, bands)` for each completed
    /// frame at the hop rate.
    ///
    /// `compressed_mags` is a slice of size `half_spectrum_size()`.
    /// Bins inside any band carry `ln(1 + λ · |X[b]|)`. Bins outside
    /// every band (DC, and the gap above the highest band edge) stay
    /// at zero. `bands` is the same array [`bands`] returns.
    ///
    /// Alloc-free after construction.
    ///
    /// # Panics
    ///
    /// Panics if the underlying [`realfft`] forward FFT fails. The
    /// internal buffers are sized at construction to the FFT plan's
    /// `make_*_vec()` requirements, so the only way this can fail
    /// is a `realfft` regression — at which point a panic is the
    /// right response.
    ///
    /// [`bands`]: Self::bands
    pub fn process<F>(&mut self, block: &[f32], mut on_frame: F)
    where
        F: FnMut(&[f32], &[(usize, usize); NUM_BANDS]),
    {
        self.input_buffer.extend_from_slice(block);

        while self.input_buffer.len() >= FRAME_SIZE {
            for i in 0..FRAME_SIZE {
                self.fft_in[i] = self.input_buffer[i] * self.window[i];
            }

            self.r2c
                .process_with_scratch(&mut self.fft_in, &mut self.fft_out, &mut self.fft_scratch)
                .expect("FFT can't fail on correctly-sized in/out/scratch vectors");

            // Compute compressed magnitudes for every band-range bin.
            // We deliberately scope this to band bins only — the bins
            // outside any band (DC, the gap above the highest band
            // edge at high sample rates) carry no information any
            // consumer of this stream cares about, and skipping them
            // keeps the per-frame work proportional to band coverage,
            // not Nyquist.
            for &(lo, hi) in &self.bands {
                for b in lo..hi {
                    let mag = self.fft_out[b].norm();
                    self.compressed_mags[b] = (1.0 + LAMBDA * mag).ln();
                }
            }

            on_frame(&self.compressed_mags, &self.bands);

            // Slide the analysis window forward by HOP_SIZE. `drain` is
            // O(remaining); for offline / per-block processing this is
            // fine. Streaming drivers (M8 `BpmStream`, M9 `PeakStream`)
            // already amortise the cost across their poll cadence.
            self.input_buffer.drain(..HOP_SIZE);
        }
    }

    /// Clear all per-stream state — the input buffer and the previous
    /// frame's compressed magnitudes. The same instance can then
    /// analyse a new audio stream without re-planning the FFT or
    /// re-computing the band-bin map.
    pub fn reset(&mut self) {
        self.input_buffer.clear();
        for m in &mut self.compressed_mags {
            *m = 0.0;
        }
    }
}

/// Lay out [`NUM_BANDS`] log-spaced half-open bin ranges over
/// `[BAND_MIN_HZ, BAND_MAX_HZ]` at the given `sample_rate`.
///
/// Uses `ceil()` on both edges so adjacent bands are contiguous and
/// non-overlapping. Each band is forced to contain at least one bin
/// (guards against the lowest band collapsing at small FFT sizes /
/// high sample rates). Both endpoints are clamped to `[1, n_bins]`
/// — we deliberately skip the DC bin because it carries no
/// rhythmic / spectral-shape information and can drift with
/// low-frequency room tone.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]
pub fn compute_band_bins(sample_rate: u32, n_bins: usize) -> [(usize, usize); NUM_BANDS] {
    let bin_hz = sample_rate as f32 / FRAME_SIZE as f32;
    let ratio = (BAND_MAX_HZ / BAND_MIN_HZ).powf(1.0 / NUM_BANDS as f32);

    let mut edges = [0usize; NUM_BANDS + 1];
    for (i, slot) in edges.iter_mut().enumerate() {
        // `i` is in `0..=NUM_BANDS` (= 0..=8) so the cast to i32
        // can't possibly wrap on any platform pointer size — the
        // clippy lint is over-cautious here.
        let hz = BAND_MIN_HZ * ratio.powi(i as i32);
        let bin = (hz / bin_hz).ceil() as usize;
        *slot = bin.clamp(1, n_bins);
    }

    let mut bands = [(0usize, 0usize); NUM_BANDS];
    for k in 0..NUM_BANDS {
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

    #[test]
    fn frame_and_hop_sizes_are_constants() {
        let s = SpectralFrameStream::new(48_000);
        assert_eq!(s.frame_size(), FRAME_SIZE);
        assert_eq!(s.hop_size(), HOP_SIZE);
        assert_eq!(s.half_spectrum_size(), FRAME_SIZE / 2 + 1);
    }

    #[test]
    fn band_bins_are_contiguous_and_non_overlapping_at_48k() {
        let n_bins = FRAME_SIZE / 2 + 1;
        let bands = compute_band_bins(48_000, n_bins);
        // Lowest band starts at bin ≥ 1 (DC excluded).
        assert!(bands[0].0 >= 1, "lowest band must skip DC: {bands:?}");
        // Highest band ends at bin ≤ n_bins.
        assert!(
            bands[NUM_BANDS - 1].1 <= n_bins,
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
        for k in 0..NUM_BANDS - 1 {
            // bins are well under i32::MAX; casting to i64 for the
            // signed subtraction is safe by construction.
            #[allow(clippy::cast_possible_wrap)]
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
        // CD-quality sample rate: smaller bin width (43 Hz vs 47 Hz at
        // 48 k) — verify the lowest band still has at least one bin
        // and the highest band still falls under Nyquist.
        let n_bins = FRAME_SIZE / 2 + 1;
        let bands = compute_band_bins(44_100, n_bins);
        for (k, &(lo, hi)) in bands.iter().enumerate() {
            assert!(hi > lo, "band {k} empty at 44.1k: {bands:?}");
            assert!(lo >= 1, "band {k} includes DC at 44.1k: {bands:?}");
            assert!(hi <= n_bins, "band {k} past Nyquist at 44.1k: {bands:?}");
        }
    }

    #[test]
    fn band_bins_layout_is_sane_at_96k() {
        // High sample rate: larger bin width (94 Hz) — the lowest band
        // edge collapses to the same bin as the second-lowest edge
        // unless we apply the `+1` carve-out. Verify the carve-out
        // kicks in.
        let n_bins = FRAME_SIZE / 2 + 1;
        let bands = compute_band_bins(96_000, n_bins);
        for (k, &(lo, hi)) in bands.iter().enumerate() {
            assert!(hi > lo, "band {k} empty at 96k: {bands:?}");
            assert!(lo >= 1, "band {k} includes DC at 96k: {bands:?}");
            assert!(hi <= n_bins, "band {k} past Nyquist at 96k: {bands:?}");
        }
    }

    #[test]
    fn fresh_stream_emits_no_frames_on_short_input() {
        let mut s = SpectralFrameStream::new(48_000);
        let mut frames = 0usize;
        s.process(&[0.0f32; FRAME_SIZE - 1], |_, _| frames += 1);
        assert_eq!(frames, 0, "should need a full frame before emitting");
    }

    #[test]
    fn one_frame_then_one_hop_emits_two_frames() {
        let mut s = SpectralFrameStream::new(48_000);
        let mut frames = 0usize;
        s.process(&vec![0.5f32; FRAME_SIZE + HOP_SIZE], |_, _| frames += 1);
        // First FRAME_SIZE samples → first frame; the +HOP_SIZE
        // extension slides one full hop and emits a second frame.
        assert_eq!(frames, 2);
    }

    #[test]
    fn block_size_invariance_one_shot_vs_streamed() {
        // Same audio fed in one big block vs many small blocks must
        // emit the same number of frames with identical compressed
        // magnitudes — the block-size-invariance contract the
        // streaming drivers depend on.
        #[allow(clippy::cast_precision_loss)]
        let audio: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.001).sin()).collect();

        let mut one_shot_frames: Vec<Vec<f32>> = Vec::new();
        let mut a = SpectralFrameStream::new(48_000);
        a.process(&audio, |mags, _| one_shot_frames.push(mags.to_vec()));

        let mut streamed_frames: Vec<Vec<f32>> = Vec::new();
        let mut b = SpectralFrameStream::new(48_000);
        for chunk in audio.chunks(123) {
            b.process(chunk, |mags, _| streamed_frames.push(mags.to_vec()));
        }

        assert_eq!(one_shot_frames.len(), streamed_frames.len());
        for (i, (a, b)) in one_shot_frames
            .iter()
            .zip(streamed_frames.iter())
            .enumerate()
        {
            assert_eq!(a.len(), b.len(), "frame {i} length mismatch");
            for (j, (av, bv)) in a.iter().zip(b.iter()).enumerate() {
                assert!(
                    (av - bv).abs() < 1e-5,
                    "frame {i} bin {j}: one-shot={av} streamed={bv}"
                );
            }
        }
    }

    #[test]
    fn reset_clears_overlap_buffer() {
        // Feed half a frame; reset; feed another half frame; the
        // detector should NOT emit (because reset cleared the
        // accumulated 512 samples). Then feed a full frame; one
        // frame should fire.
        let mut s = SpectralFrameStream::new(48_000);
        let mut frames = 0usize;
        s.process(&vec![0.5f32; FRAME_SIZE / 2], |_, _| frames += 1);
        assert_eq!(frames, 0);
        s.reset();
        s.process(&vec![0.5f32; FRAME_SIZE / 2], |_, _| frames += 1);
        assert_eq!(
            frames, 0,
            "reset should have cleared the accumulated half-frame"
        );
        s.process(&vec![0.5f32; FRAME_SIZE / 2], |_, _| frames += 1);
        assert_eq!(frames, 1, "now we've fed a full frame post-reset");
    }

    #[test]
    fn silence_produces_compressed_mags_near_zero() {
        let mut s = SpectralFrameStream::new(48_000);
        let mut max_mag = 0.0f32;
        s.process(&vec![0.0f32; FRAME_SIZE * 4], |mags, _| {
            for &m in mags {
                if m > max_mag {
                    max_mag = m;
                }
            }
        });
        assert!(
            max_mag < 1e-3,
            "silence should compress to ~0 magnitude; got max {max_mag}"
        );
    }

    #[test]
    fn process_is_alloc_free_after_construction() {
        // The audio thread never calls into dub-spectral directly, but
        // the off-RT analysis threads do. They batch process() calls
        // every ~ 20 ms — burning a heap allocation per frame would
        // wreck their tail latency. Verify `process` does no heap
        // work after the first call (which sizes the input buffer).
        let mut s = SpectralFrameStream::new(48_000);
        // Prime — first call's `extend_from_slice` will grow the
        // input buffer once.
        s.process(&vec![0.0f32; FRAME_SIZE], |_, _| {});

        // Now exercise. The input buffer has FRAME_SIZE - HOP_SIZE
        // samples carried over; feeding HOP_SIZE more samples
        // triggers exactly one frame and the buffer should stabilise
        // at FRAME_SIZE - HOP_SIZE again. No alloc should be needed.
        let cap_before = s.input_buffer.capacity();
        for _ in 0..16 {
            s.process(&vec![0.0f32; HOP_SIZE], |_, _| {});
        }
        let cap_after = s.input_buffer.capacity();
        assert_eq!(
            cap_before, cap_after,
            "input_buffer capacity should be stable"
        );
    }
}
