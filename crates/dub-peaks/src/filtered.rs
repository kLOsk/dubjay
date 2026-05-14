//! Time-domain band-filtered peak decimator (Mixxx-style RGB
//! waveform foundation).
//!
//! ## Why this exists (M10.5p Stage 3)
//!
//! The Stage 1–2 djLandmarks calibration tried to gate "this chunk
//! is a kick" off `bandLow.y × onsetConf` — both of which come from
//! the **FFT path** in `dub-spectral`. That approach fails on real
//! music because:
//!
//! 1. `bandLow.y` is a **μ-law-compressed STFT magnitude**. Compres-
//!    sion (the `ln(1 + λ·|X|)` curve) by construction *pulls dis-
//!    tinct amplitudes toward the same range*. A sustained bassline
//!    at raw FFT magnitude 0.4 and a kick at raw magnitude 0.9 both
//!    land around 0.6–0.8 after compression. The "kick is taller in
//!    the bass" signal is throttled.
//! 2. The kick's **temporal attack-decay shape** is smeared across
//!    the STFT window (~46 ms Hann at 48 kHz / 2048 frame). A kick
//!    transient looks just as wide-in-magnitude as a sustained tone
//!    of similar in-band energy. The transient *shape* — the thing
//!    that distinguishes a kick from a bass roll in the time domain
//!    — is gone by the time the magnitudes reach the shader.
//!
//! Mixxx's "Filtered" / "RGB" waveform types (see
//! <https://mixxx.org/news/2024-02-23-improved-waveforms/>) sidestep
//! both issues by running the audio through **time-domain band-pass
//! filters at load time** and storing per-pixel min/max of the
//! *filtered* signal. A kick's filtered LF envelope is a sharp
//! attack-decay spike (~1.0 at attack, decaying over 50–200 ms); a
//! sustained 80 Hz bassline's filtered LF envelope is a clean ~0.4
//! sinusoid. The transient shape *survives*, and the kick stands
//! out by a clean ~2–3× ratio over sustained content with no
//! cleverness needed at render time.
//!
//! This module is our equivalent. Where `BandDecimator` gives the
//! shader **frequency-domain colour information** (which is what
//! the Serato-faithful palette wants), `FilteredDecimator` gives it
//! **time-domain band envelopes** (which is what the DJ-landmarks
//! palette wants for kick prominence, and what a future Mixxx-style
//! RGB palette would want for honest per-band amplitude display).
//!
//! ## Bands
//!
//! v1 implements **the LF band only** (≤ 250 Hz, 2-pole Butterworth
//! LP at Q ≈ 0.707) and leaves the MF / HF fields of
//! [`FilteredPeakChunk`] zero-valued. The wire format reserves space
//! for MF (~250–4000 Hz, kick-snare-vocal body) and HF (≥ 4 kHz,
//! cymbal / vocal-sibilance) channels so a follow-up can populate
//! them without breaking the FFI / shader layout.
//!
//! ## Filter choice (LF band)
//!
//! * **Topology**: 2-pole Butterworth low-pass biquad (RBJ cookbook
//!   formulas, Direct-Form-I implementation).
//! * **Cutoff**: 180 Hz (Stage 3.1). Kick fundamentals sit at
//!   50–80 Hz, well inside the flat passband. The kick body's
//!   200-Hz harmonic falls 1 octave above cutoff at -12 dB, but
//!   the kick fundamental dominates per-chunk peak amplitude
//!   anyway. Snare fundamentals at 180–200 Hz now sit *at* the
//!   cutoff (~-3 dB) so the snare LF residual drops noticeably
//!   below the kick LF — empirically about a third to a fifth at
//!   the same broadband RMS, enough separation when multiplied
//!   with `onsetConf`.
//! * **Order**: 2 (12 dB/octave). At 500 Hz: -12 dB. At 1 kHz:
//!   -24 dB. At 4 kHz: -36 dB. Sufficient rejection for the
//!   kick-vs-everything-else discrimination, simpler / lower group
//!   delay than 4-pole. Group delay at DC is ~3 ms / 144 samples at
//!   48 kHz — visually that means the filtered LF spike of a kick
//!   lags the broadband peak by ~3 ms / ~2 chunks at the default
//!   64-sample cadence, below the audio-visual synchronisation
//!   threshold.
//!
//! ## Real-time safety
//!
//! Like every other decimator in this crate, [`FilteredDecimator`]
//! is **alloc-free** on the hot path. State is two `f32`s of biquad
//! memory plus a `(min, max, count)` accumulator. The caller streams
//! samples in (any block size) and supplies a closure invoked once
//! per completed chunk. Same contract as [`crate::Decimator`].

use crate::FilteredPeakChunk;

/// Direct-Form-I 2-pole biquad. State = two input + two output
/// memory cells.
///
/// Public so tests can construct one independently, but the typical
/// caller goes through [`FilteredDecimator`] which manages the cells
/// alongside its min/max accumulator.
#[derive(Debug, Clone, Copy)]
struct Biquad {
    /// Normalised numerator coefficients.
    b0: f32,
    b1: f32,
    b2: f32,
    /// Normalised denominator coefficients (a0 normalised to 1).
    a1: f32,
    a2: f32,
    /// Input memory `x[n-1], x[n-2]`.
    x1: f32,
    x2: f32,
    /// Output memory `y[n-1], y[n-2]`.
    y1: f32,
    y2: f32,
}

impl Biquad {
    /// Construct a low-pass biquad at the given cutoff frequency
    /// and Q factor for the given sample rate.
    ///
    /// RBJ cookbook formulas:
    /// ```text
    /// ω0 = 2π · fc / fs
    /// α  = sin(ω0) / (2Q)
    /// b0 = (1 − cos(ω0)) / 2
    /// b1 =  1 − cos(ω0)
    /// b2 = (1 − cos(ω0)) / 2
    /// a0 =  1 + α
    /// a1 = -2 · cos(ω0)
    /// a2 =  1 − α
    /// ```
    /// then normalise all by `a0`.
    fn lowpass(fc_hz: f32, q: f32, sample_rate_hz: f32) -> Self {
        // Cookbook biquad — see Audio EQ Cookbook (Bristow-Johnson).
        // All math in f32 because the audio path is f32 and we want
        // bit-stable behaviour against future SIMD vectorisation.
        let omega = 2.0 * core::f32::consts::PI * fc_hz / sample_rate_hz;
        let (sin_w, cos_w) = (omega.sin(), omega.cos());
        let alpha = sin_w / (2.0 * q);

        let b0 = (1.0 - cos_w) * 0.5;
        let b1 = 1.0 - cos_w;
        let b2 = (1.0 - cos_w) * 0.5;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w;
        let a2 = 1.0 - alpha;

        let inv_a0 = 1.0 / a0;
        Self {
            b0: b0 * inv_a0,
            b1: b1 * inv_a0,
            b2: b2 * inv_a0,
            a1: a1 * inv_a0,
            a2: a2 * inv_a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// Step one sample through the filter. DF-I:
    /// `y[n] = b0·x[n] + b1·x[n-1] + b2·x[n-2] − a1·y[n-1] − a2·y[n-2]`.
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        // Shift memory cells.
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    /// Reset memory cells to zero. Coefficients preserved.
    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

/// LF band cutoff. See module docs for rationale.
///
/// Stage 3.0 shipped at 250 Hz (kick body fully in band, but snare
/// fundamentals at ~180 Hz also passed near-unity → kick gate
/// polluted by snare leakage). Stage 3.1 lowered to 180 Hz: a
/// snare's 180-Hz fundamental now sits *at* the cutoff (-3 dB),
/// while a kick's 60-Hz fundamental is still in the flat passband.
/// The 200-Hz harmonic of a kick body gets -3 dB attenuation —
/// acceptable, since the kick fundamental dominates per-chunk peak
/// amplitude anyway.
const LF_CUTOFF_HZ: f32 = 180.0;

/// Butterworth Q for a 2nd-order low/high-pass: 1/√2 ≈ 0.7071.
const BUTTERWORTH_Q_2POLE: f32 = core::f32::consts::FRAC_1_SQRT_2;

/// Online time-domain band-filtered peak decimator.
///
/// Maintains one biquad per band (currently LF only; MF / HF
/// reserved). For each fed sample, the LF biquad output's signed
/// value is accumulated into `(lf_min, lf_max)`. When
/// `samples_per_chunk` samples have been consumed, the chunk is
/// emitted via the caller's closure and the accumulator resets.
///
/// MF / HF fields of the emitted chunk are always zero in v1.
#[derive(Debug, Clone)]
pub struct FilteredDecimator {
    /// Chunk size in samples. Set at construction, matched to the
    /// broadband [`crate::Decimator`] so the renderer can index both
    /// streams 1:1.
    samples_per_chunk: usize,

    /// LF band biquad. 2-pole Butterworth LP at [`LF_CUTOFF_HZ`].
    lf: Biquad,

    /// Min / max accumulator for the current partial chunk.
    lf_min: f32,
    lf_max: f32,

    /// Samples consumed into the current partial chunk. Wraps to 0
    /// on each emit.
    accum_count: usize,
}

impl FilteredDecimator {
    /// Construct at the given sample rate, with broadband-matched
    /// chunk size (typically [`crate::DEFAULT_SAMPLES_PER_CHUNK`]).
    ///
    /// # Panics
    /// Panics if `samples_per_chunk == 0` or `sample_rate_hz == 0`
    /// — both would produce nonsense (infinite emits / NaN coeffs).
    #[must_use]
    pub fn new(sample_rate_hz: u32, samples_per_chunk: usize) -> Self {
        assert!(
            samples_per_chunk > 0,
            "samples_per_chunk must be > 0; got 0"
        );
        assert!(sample_rate_hz > 0, "sample_rate_hz must be > 0; got 0");

        #[allow(clippy::cast_precision_loss)]
        let sr = sample_rate_hz as f32;

        Self {
            samples_per_chunk,
            lf: Biquad::lowpass(LF_CUTOFF_HZ, BUTTERWORTH_Q_2POLE, sr),
            lf_min: f32::INFINITY,
            lf_max: f32::NEG_INFINITY,
            accum_count: 0,
        }
    }

    /// Configured chunk size in samples.
    #[must_use]
    pub fn samples_per_chunk(&self) -> usize {
        self.samples_per_chunk
    }

    /// How many samples of the current partial chunk have been
    /// consumed.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.accum_count
    }

    /// Feed a block of samples. Closure is invoked once per
    /// completed chunk. Alloc-free.
    pub fn feed<F: FnMut(FilteredPeakChunk)>(&mut self, samples: &[f32], mut emit: F) {
        for &s in samples {
            let lf = self.lf.process(s);

            if lf < self.lf_min {
                self.lf_min = lf;
            }
            if lf > self.lf_max {
                self.lf_max = lf;
            }
            self.accum_count += 1;

            if self.accum_count == self.samples_per_chunk {
                emit(FilteredPeakChunk {
                    lf_min: self.lf_min,
                    lf_max: self.lf_max,
                    mf_min: 0.0,
                    mf_max: 0.0,
                    hf_min: 0.0,
                    hf_max: 0.0,
                });
                self.lf_min = f32::INFINITY;
                self.lf_max = f32::NEG_INFINITY;
                self.accum_count = 0;
            }
        }
    }

    /// Drop the partial-chunk state. Does not emit. Filter memory
    /// cells are also cleared (so the next feed starts from a clean
    /// transient response — important for offline use where each
    /// `compute_offline_peaks` call should be independent).
    pub fn reset(&mut self) {
        self.lf_min = f32::INFINITY;
        self.lf_max = f32::NEG_INFINITY;
        self.accum_count = 0;
        self.lf.reset();
    }

    /// Emit a partial chunk now if one is in progress, then reset.
    /// Mirrors [`crate::Decimator::flush`] for end-of-stream parity.
    pub fn flush<F: FnMut(FilteredPeakChunk)>(&mut self, mut emit: F) {
        if self.accum_count > 0 {
            emit(FilteredPeakChunk {
                lf_min: self.lf_min,
                lf_max: self.lf_max,
                mf_min: 0.0,
                mf_max: 0.0,
                hf_min: 0.0,
                hf_max: 0.0,
            });
        }
        self.reset();
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss)]
mod tests {
    use super::*;
    use crate::DEFAULT_SAMPLES_PER_CHUNK;

    /// Construct decimator, feed samples, return all emitted chunks.
    fn run(sr: u32, spc: usize, samples: &[f32]) -> Vec<FilteredPeakChunk> {
        let mut d = FilteredDecimator::new(sr, spc);
        let mut out = Vec::new();
        d.feed(samples, |c| out.push(c));
        out
    }

    // -------- Boundary / construction --------

    #[test]
    #[should_panic(expected = "samples_per_chunk must be > 0")]
    fn zero_chunk_panics() {
        let _ = FilteredDecimator::new(48_000, 0);
    }

    #[test]
    #[should_panic(expected = "sample_rate_hz must be > 0")]
    fn zero_sample_rate_panics() {
        let _ = FilteredDecimator::new(0, 64);
    }

    #[test]
    fn no_emit_until_full_chunk() {
        let out = run(48_000, 64, &[0.0f32; 63]);
        assert!(out.is_empty(), "partial chunk must not emit");
    }

    #[test]
    fn silence_chunk_is_zero() {
        let out = run(48_000, 64, &[0.0f32; 64]);
        assert_eq!(out.len(), 1);
        let c = out[0];
        assert_eq!(c.lf_min, 0.0);
        assert_eq!(c.lf_max, 0.0);
        // MF / HF are zero-by-construction in v1.
        assert_eq!(c.mf_min, 0.0);
        assert_eq!(c.mf_max, 0.0);
        assert_eq!(c.hf_min, 0.0);
        assert_eq!(c.hf_max, 0.0);
    }

    #[test]
    fn partial_carries_across_feeds() {
        let mut d = FilteredDecimator::new(48_000, 64);
        let mut out = Vec::new();
        d.feed(&[0.5f32; 32], |c| out.push(c));
        assert!(out.is_empty());
        d.feed(&[0.5f32; 32], |c| out.push(c));
        assert_eq!(out.len(), 1);
    }

    // -------- Filter correctness --------

    /// DC input passes through a lowpass unattenuated (gain at 0 Hz
    /// = 1). After the filter warms up, the LF envelope should
    /// match the input level.
    #[test]
    fn dc_passes_through_at_unity_gain() {
        // Feed enough samples to fully warm the biquad memory.
        let warmup = vec![0.5f32; 4096];
        let mut d = FilteredDecimator::new(48_000, 64);
        let mut last = FilteredPeakChunk::ZERO;
        d.feed(&warmup, |c| last = c);
        // Steady-state DC should have lf_min ≈ lf_max ≈ 0.5.
        assert!(
            (last.lf_max - 0.5).abs() < 1e-3,
            "DC steady-state max should be ~0.5, got {}",
            last.lf_max
        );
        assert!(
            (last.lf_min - 0.5).abs() < 1e-3,
            "DC steady-state min should be ~0.5, got {}",
            last.lf_min
        );
    }

    /// A pure 80 Hz sine (well below the 250 Hz cutoff) should pass
    /// with near-unity amplitude.
    #[test]
    fn in_band_sine_passes_through() {
        let sr = 48_000;
        let freq = 80.0;
        let n = sr as usize; // 1 second
        let samples: Vec<f32> = (0..n)
            .map(|i| (2.0 * core::f32::consts::PI * freq * i as f32 / sr as f32).sin())
            .collect();
        let chunks = run(sr, 64, &samples);
        // Last chunk (after steady-state) should have peak amplitude
        // close to 1.0 — give it 10 % headroom for biquad rounding.
        let last = chunks.last().expect("got chunks");
        let peak = last.lf_max.max(-last.lf_min);
        assert!(
            peak > 0.85,
            "80 Hz sine should pass through at near-unity, peak = {peak}"
        );
    }

    /// A pure 2 kHz sine (well above the cutoff) should be rejected.
    /// 2 kHz is 3 octaves above 250 Hz; 2-pole roll-off is 12 dB /
    /// octave = ~36 dB rejection. 10^(-36/20) ≈ 0.0158, so peak
    /// should be < 0.05 with headroom.
    #[test]
    fn out_of_band_sine_is_rejected() {
        let sr = 48_000;
        let freq = 2_000.0;
        let n = sr as usize;
        let samples: Vec<f32> = (0..n)
            .map(|i| (2.0 * core::f32::consts::PI * freq * i as f32 / sr as f32).sin())
            .collect();
        let chunks = run(sr, 64, &samples);
        let last = chunks.last().expect("got chunks");
        let peak = last.lf_max.max(-last.lf_min);
        assert!(
            peak < 0.05,
            "2 kHz sine should be rejected (>30 dB), peak = {peak}"
        );
    }

    /// The big one: a synthetic kick (short LF-heavy transient
    /// against a sustained sub-bass background) must produce a
    /// **visible LF peak ratio** between the kick chunk and the
    /// sustained-bass chunks. This is the core architectural
    /// promise of the time-domain approach.
    ///
    /// Setup:
    /// * 1 second of sustained 80 Hz sine at amplitude 0.4 (a
    ///   typical sub-bass bassline level).
    /// * At t = 0.5 s, additively layer a 30 ms decaying-sine kick
    ///   at 60 Hz with peak amplitude 0.9 (a typical kick).
    ///
    /// Expectation: the chunks covering the kick window have an LF
    /// peak meaningfully larger than the sustained-bass chunks
    /// before / after. We assert a ratio ≥ 1.8× — this is the
    /// separation the shader relies on.
    #[test]
    fn synthetic_kick_pops_against_sustained_bass() {
        let sr: u32 = 48_000;
        let n = sr as usize;
        let mut samples = vec![0.0f32; n];

        let bass_freq = 80.0;
        let kick_freq = 60.0;
        let kick_start_sample = n / 2;
        // 30 ms at 48 kHz = 1440 samples, well within usize range.
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let kick_len_samples = (sr as f32 * 0.030) as usize;
        let kick_decay_tau = sr as f32 * 0.020;

        for (i, s) in samples.iter_mut().enumerate() {
            let t = i as f32 / sr as f32;
            let bass = 0.4 * (2.0 * core::f32::consts::PI * bass_freq * t).sin();
            let kick = if i >= kick_start_sample && i < kick_start_sample + kick_len_samples {
                let kt = (i - kick_start_sample) as f32;
                let envelope = (-kt / kick_decay_tau).exp();
                let phase = 2.0 * core::f32::consts::PI * kick_freq * (kt / sr as f32);
                0.9 * envelope * phase.sin()
            } else {
                0.0
            };
            *s = bass + kick;
        }

        let chunks = run(sr, 64, &samples);

        // Identify pre-kick, kick, and post-kick chunk ranges.
        let kick_start_chunk = kick_start_sample / 64;
        // Kick decays in ~60 ms (3τ) = ~2880 samples = ~45 chunks.
        let kick_end_chunk = kick_start_chunk + 45;

        // Skip filter warm-up (first 0.2 s ≈ 150 chunks).
        let warmup_chunks = 150;
        let pre_kick_chunks = &chunks[warmup_chunks..(kick_start_chunk - 5)];
        let kick_chunks = &chunks[kick_start_chunk..kick_end_chunk];

        let pre_kick_peak = pre_kick_chunks
            .iter()
            .map(|c| c.lf_max.max(-c.lf_min))
            .fold(0.0f32, f32::max);
        let kick_peak = kick_chunks
            .iter()
            .map(|c| c.lf_max.max(-c.lf_min))
            .fold(0.0f32, f32::max);

        let ratio = kick_peak / pre_kick_peak.max(1e-6);
        assert!(
            ratio >= 1.8,
            "kick LF peak ({kick_peak}) should be ≥ 1.8× sustained-bass peak \
             ({pre_kick_peak}); got ratio {ratio}"
        );
    }

    /// Companion to `synthetic_kick_pops_against_sustained_bass`:
    /// a synthetic *snare* (a 180-Hz fundamental burst with a
    /// 400-Hz body) must leak through the LF filter at a
    /// **substantially lower** amplitude than a same-peak-energy
    /// kick. Stage 3.1 lowered the LP cutoff from 250 → 180 Hz to
    /// push the snare fundamental to the -3-dB shoulder. We assert
    /// the snare's LF peak is ≤ 65 % of a kick's LF peak at the
    /// same broadband peak amplitude.
    ///
    /// (At Stage 3.0's 250 Hz cutoff the snare fundamental sat in
    /// the flat passband, so the LF residual was ~ 90 % of a
    /// kick's — not enough separation to gate cleanly. With the
    /// kick gate multiplying by `onsetConf` we still need the
    /// raw LF amplitude separation to give the gate something
    /// to multiply against.)
    #[test]
    fn snare_lf_residual_stays_below_kick() {
        let sr: u32 = 48_000;
        let n = sr as usize / 2; // 0.5 s each

        // Synthesise a one-shot kick: 30 ms decaying sine at 60 Hz,
        // peak amplitude 0.9.
        let mut kick_buf = vec![0.0f32; n];
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let kick_len = (sr as f32 * 0.030) as usize;
        let kick_tau = sr as f32 * 0.020;
        for i in 0..kick_len {
            let kt = i as f32;
            let env = (-kt / kick_tau).exp();
            let phase = 2.0 * core::f32::consts::PI * 60.0 * (kt / sr as f32);
            kick_buf[i + 1000] = 0.9 * env * phase.sin();
        }

        // Synthesise a one-shot snare: 30 ms decaying sine at 180 Hz
        // fundamental + 30 ms decaying sine at 400 Hz body, both at
        // peak 0.45 so total broadband peak ≈ 0.9 (matched to kick).
        let mut snare_buf = vec![0.0f32; n];
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let snare_len = (sr as f32 * 0.030) as usize;
        let snare_tau = sr as f32 * 0.020;
        for i in 0..snare_len {
            let kt = i as f32;
            let env = (-kt / snare_tau).exp();
            let fund = 0.45 * env * (2.0 * core::f32::consts::PI * 180.0 * (kt / sr as f32)).sin();
            let body = 0.45 * env * (2.0 * core::f32::consts::PI * 400.0 * (kt / sr as f32)).sin();
            snare_buf[i + 1000] = fund + body;
        }

        let kick_chunks = run(sr, 64, &kick_buf);
        let snare_chunks = run(sr, 64, &snare_buf);

        let kick_lf = kick_chunks
            .iter()
            .map(|c| c.lf_max.max(-c.lf_min))
            .fold(0.0f32, f32::max);
        let snare_lf = snare_chunks
            .iter()
            .map(|c| c.lf_max.max(-c.lf_min))
            .fold(0.0f32, f32::max);

        let ratio = snare_lf / kick_lf.max(1e-6);
        assert!(
            ratio < 0.65,
            "snare LF peak ({snare_lf}) should be < 65 % of kick LF peak \
             ({kick_lf}) at matched broadband amplitude; got ratio {ratio}"
        );
    }

    // -------- Reset / flush --------

    #[test]
    fn reset_clears_filter_state() {
        let mut d = FilteredDecimator::new(48_000, 64);
        let mut out = Vec::new();
        d.feed(&[1.0f32; 100], |c| out.push(c));
        d.reset();
        // After reset, the biquad memory is zero. Feeding silence
        // should produce a chunk of all zeros (no residual filter
        // tail).
        out.clear();
        d.feed(&[0.0f32; 64], |c| out.push(c));
        assert_eq!(out.len(), 1);
        assert!(
            out[0].lf_min.abs() < 1e-6 && out[0].lf_max.abs() < 1e-6,
            "post-reset silence chunk should be exactly zero, got ({}, {})",
            out[0].lf_min,
            out[0].lf_max
        );
    }

    #[test]
    fn flush_emits_partial() {
        let mut d = FilteredDecimator::new(48_000, DEFAULT_SAMPLES_PER_CHUNK);
        let mut out = Vec::new();
        d.feed(&[0.5f32; 32], |c| out.push(c));
        assert!(out.is_empty(), "partial chunk must not auto-emit");
        d.flush(|c| out.push(c));
        assert_eq!(out.len(), 1, "flush must emit the partial chunk");
        assert_eq!(d.pending(), 0);
    }

    // -------- Block-size invariance --------

    #[test]
    fn block_size_does_not_change_output() {
        let sr = 48_000;
        let n = 1024;
        let samples: Vec<f32> = (0..n).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();

        let by_one: Vec<_> = {
            let mut d = FilteredDecimator::new(sr, 64);
            let mut out = Vec::new();
            for s in &samples {
                d.feed(std::slice::from_ref(s), |c| out.push(c));
            }
            out
        };
        let by_all: Vec<_> = {
            let mut d = FilteredDecimator::new(sr, 64);
            let mut out = Vec::new();
            d.feed(&samples, |c| out.push(c));
            out
        };

        assert_eq!(by_one.len(), by_all.len());
        for (a, b) in by_one.iter().zip(by_all.iter()) {
            assert!(
                (a.lf_min - b.lf_min).abs() < 1e-6,
                "lf_min divergence: {a:?} vs {b:?}"
            );
            assert!(
                (a.lf_max - b.lf_max).abs() < 1e-6,
                "lf_max divergence: {a:?} vs {b:?}"
            );
        }
    }
}
