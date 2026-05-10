//! Stereo timecode decoder via coherent block phase-difference.
//!
//! ## Algorithm
//!
//! The two stereo channels of Serato Control CV02 vinyl carry the
//! same carrier sinusoid offset by 90°. Per the empirical convention
//! observed on real CV02 cartridges going through the SL3, channel 0
//! leads channel 1 by 90° at forward play — i.e. `ch0 ≈ A·sin(φ)`,
//! `ch1 ≈ A·cos(φ)`. We treat the sample pair as a single complex
//! sample `s = ch1 + j·ch0`, which is `A·exp(j·2π·f·t)` rotating
//! counter-clockwise (positive frequency) for forward stylus motion
//! and clockwise (negative) for reverse.
//!
//! Per-sample phase advance is therefore:
//!
//! ```text
//!   Δφ = arg(s_n · conj(s_{n-1}))  ≈  2π · f_inst · Δt
//! ```
//!
//! Summing `s_n · conj(s_{n-1})` over a block before taking `arg` is a
//! coherent average: noise (uncorrelated across samples) suppresses
//! by `√N`, signal adds linearly. With a 64-sample block at 48 kHz
//! that's a ~9 dB noise gain — enough to make the decoder work
//! happily on tape-quality timecode rips.
//!
//! Direction falls out for free: `f_inst < 0` ⇔ reverse motion. No
//! separate "forward/reverse" flag flip needed.
//!
//! Position is the integral of `f_inst` over time, normalized by the
//! nominal carrier frequency. We accumulate it in seconds-of-record
//! at unity speed so the engine can map deck position 1:1 in M5.3.
//!
//! ## What this *doesn't* do (yet)
//!
//! - **Absolute position** (M6). The Serato/Traktor bitstream rides
//!   on top of the carrier as AM modulation; we'd need to demodulate
//!   the envelope, sample one bit per carrier cycle, and look it up
//!   in the format's position table. Not needed for v1 relative mode.
//! - **Stickiness on lift** (M5.4). The decoder reports `confidence`
//!   today; the *policy* of "stop the deck and remember position"
//!   when confidence drops belongs in the integration layer, not here.
//! - **Calibration / amplitude AGC** (M6). We assume the input is
//!   nominally `±0.3..±0.7` after gain-staging. Real cartridges plus
//!   real preamps need an AGC; deferred.
//!
//! ## RT-safety
//!
//! [`Decoder::process`] is allocation-free and lock-free, so the
//! decoder is safe to run on the audio thread once the live wiring
//! lands in M5.3. Floating-point only — no transcendentals other than
//! `atan2` once per block. At 48 kHz / 64-frame blocks that's 750
//! atan2 calls/sec/deck — trivial.

use crate::Format;

/// Output of one [`Decoder::process`] call. Caller drives deck
/// transport from these.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecodeOutput {
    /// Estimated playback rate over this block, normalized to nominal
    /// carrier frequency. `1.0` = forward unity, `-1.0` = reverse unity,
    /// `0.0` = stylus stationary on groove.
    ///
    /// At very high speeds (`|rate| > 0.5 · sample_rate / carrier_hz`,
    /// e.g. 24× at 48 kHz / 1 kHz) the per-sample phase advance
    /// approaches ±π and the estimate ambiguates with its alias —
    /// the decoder will return *some* number but it may be wrong by
    /// 2× the true rate. Real DJs scratch up to ~8×, well clear of
    /// the alias band.
    pub rate: f64,

    /// Cumulative position offset since [`Decoder::new`] (or the last
    /// [`Decoder::reset`]), measured in seconds at unity speed. This
    /// is the relative-mode position; absolute-mode position requires
    /// the bitstream decode (M6).
    pub position_secs: f64,

    /// RMS amplitude of the input signal, useful for stylus-lift
    /// detection. Falls below ~0.01 when the stylus is off the groove
    /// (assuming reasonable cartridge gain). Use this in the
    /// integration layer to drive the "stickiness" policy.
    pub amplitude: f32,

    /// Heuristic confidence in `[0, 1]`. `1.0` means the input is a
    /// pure complex exponential at some frequency (forward, reverse,
    /// or zero); `0.0` means uncorrelated noise. Below ~0.5 indicates
    /// noise/transients/crosstalk and the rate estimate should not
    /// drive deck transport.
    pub confidence: f32,
}

/// Stateful timecode decoder. One instance per deck.
///
/// `Decoder` is `!Send` only by convention — there is nothing
/// non-`Send` inside; we just don't want to encourage handing the
/// same decoder to multiple threads. Wrap in an `Arc<Mutex>` if you
/// need cross-thread access (you almost certainly don't).
pub struct Decoder {
    sample_rate: f32,
    /// Nominal carrier frequency in Hz (cached from [`Format`]).
    carrier_hz: f32,
    /// Previous complex sample's real part (= file ch1, the cos
    /// component). Retained across `process` calls so the
    /// phase-difference formula has continuity at block boundaries.
    prev_re: f64,
    /// Previous complex sample's imag part (= file ch0, the sin component).
    prev_im: f64,
    /// Whether `prev_*` have been seeded with at least one sample.
    primed: bool,
    /// Cumulative position in seconds-at-unity-speed.
    position_secs: f64,
}

impl Decoder {
    /// Create a decoder for the given timecode format and sample rate.
    ///
    /// As of M6 all three relative-mode formats are supported:
    /// [`Format::SeratoCv02`] (1 kHz carrier),
    /// [`Format::TraktorMk1`] (2 kHz, AM modulation), and
    /// [`Format::TraktorMk2`] (2.5 kHz, offset modulation). The
    /// algorithm is format-agnostic — the only per-format parameter
    /// the decoder uses today is the nominal carrier frequency,
    /// pulled from [`Format::carrier_hz`]. All three encode their
    /// stereo carrier in the same quadrature convention (`ch0 = sin`,
    /// `ch1 = cos`), validated empirically against real cartridges
    /// on the SL3 in M5.3 (Serato) and M6 (both Traktor generations).
    /// MK2's offset modulation rides as a vertical DC shift; the
    /// cartridge/preamp AC-couples it out before it reaches us, so
    /// the relative-mode math sees a clean 2.5 kHz carrier.
    ///
    /// Absolute-position decoding (the bitstream riding on top of
    /// the carrier) still isn't done — relative mode covers v1's
    /// scratch-DJ workflow.
    ///
    /// # Panics
    /// `sample_rate` must be positive.
    #[must_use]
    pub fn new(format: Format, sample_rate: f32) -> Self {
        assert!(sample_rate > 0.0, "sample rate must be > 0");
        Self {
            sample_rate,
            carrier_hz: format.carrier_hz(),
            prev_re: 0.0,
            prev_im: 0.0,
            primed: false,
            position_secs: 0.0,
        }
    }

    /// Reset accumulated position and the prev-sample register. Useful
    /// when re-cueing the deck or recovering from a stylus lift.
    pub fn reset(&mut self) {
        self.prev_re = 0.0;
        self.prev_im = 0.0;
        self.primed = false;
        self.position_secs = 0.0;
    }

    /// Cumulative position in seconds-at-unity-speed.
    #[must_use]
    pub fn position_secs(&self) -> f64 {
        self.position_secs
    }

    /// Decode one stereo block (interleaved). The block can be any
    /// length ≥ 1 frame; longer blocks give better noise rejection
    /// but the decoder also tolerates per-sample calls.
    ///
    /// # Panics
    /// `stereo.len()` must be even.
    pub fn process(&mut self, stereo: &[f32]) -> DecodeOutput {
        assert_eq!(stereo.len() % 2, 0, "interleaved stereo buffer required");
        let n_frames = stereo.len() / 2;
        if n_frames == 0 {
            return DecodeOutput {
                rate: 0.0,
                position_secs: self.position_secs,
                amplitude: 0.0,
                confidence: 0.0,
            };
        }

        // Accumulators: `acc` is the coherent sum of consecutive-sample
        // phase-difference vectors; `mag_acc` is the sum of |s|² used
        // both for amplitude RMS and confidence normalization.
        let mut acc_re = 0.0_f64;
        let mut acc_im = 0.0_f64;
        let mut mag_acc = 0.0_f64;
        let mut samples_consumed = 0_usize;

        for frame in stereo.chunks_exact(2) {
            // Serato CV02 convention (verified against a real cartridge
            // on an SL3): file ch0 ≈ A·sin(φ), file ch1 ≈ A·cos(φ).
            // Map ch1 → real, ch0 → imag so `s = re + j·im = A·e^(jφ)`
            // rotates the *positive* direction at forward play.
            let im = f64::from(frame[0]);
            let re = f64::from(frame[1]);
            // |s|² = re² + im²; Pythagorean amplitude regardless of phase.
            mag_acc += re * re + im * im;

            if self.primed {
                // s_curr · conj(s_prev) = (re + j·im)·(prev_re − j·prev_im)
                //                       = (re·prev_re + im·prev_im)
                //                       + j·(im·prev_re − re·prev_im)
                acc_re += re * self.prev_re + im * self.prev_im;
                acc_im += im * self.prev_re - re * self.prev_im;
                samples_consumed += 1;
            }
            self.prev_re = re;
            self.prev_im = im;
            self.primed = true;
        }

        // Block-level instantaneous frequency from the coherent sum's
        // argument. `samples_consumed` is `n_frames` on a primed
        // decoder; on the very first call it's `n_frames − 1` (we lose
        // one sample of phase-diff to bootstrap `prev_*`).
        let dt = 1.0 / f64::from(self.sample_rate);
        let phase_diff = if samples_consumed > 0 {
            acc_im.atan2(acc_re)
        } else {
            0.0
        };
        let inst_freq_hz = phase_diff / (std::f64::consts::TAU * dt);
        let nominal = f64::from(self.carrier_hz);
        let rate = inst_freq_hz / nominal;

        // Amplitude is RMS of |s| over the block. Note: |s|² = L²+R²
        // is *constant* (= A²) for a perfect quadrature signal, so RMS
        // here ≈ A, not A/√2 — which is what we want as the "carrier
        // amplitude" reading.
        #[allow(clippy::cast_precision_loss)]
        let mean_sq = mag_acc / (n_frames as f64);
        #[allow(clippy::cast_possible_truncation)]
        let amplitude = (mean_sq.sqrt()) as f32;

        // Confidence: |coherent sum| / Σ |s_curr·conj(s_prev)|.
        // For pure quadrature, |s_curr·conj(s_prev)| = |s|², so this
        // reduces to |sum|/Σ|s|² ≈ 1.0. Noise drives it toward 0.
        let coherent_mag = (acc_re * acc_re + acc_im * acc_im).sqrt();
        let confidence = if mag_acc > 1e-12 {
            #[allow(clippy::cast_possible_truncation)]
            ((coherent_mag / mag_acc).clamp(0.0, 1.0) as f32)
        } else {
            0.0
        };

        // Integrate position. Block duration in seconds at the engine
        // SR (NOT scaled by rate — `rate` already encodes how fast the
        // record is moving relative to nominal).
        #[allow(clippy::cast_precision_loss)]
        let block_secs_real = (n_frames as f64) * dt;
        // `rate` is normalized vs nominal carrier; multiplying by real
        // seconds gives "seconds of record advanced" which is what we
        // want for relative position.
        self.position_secs += rate * block_secs_real;

        DecodeOutput {
            rate,
            position_secs: self.position_secs,
            amplitude,
            confidence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::Generator;

    /// Generate `n_frames` of timecode at `rate`, decode it, return
    /// the final [`DecodeOutput`].
    fn roundtrip(rate: f64, n_frames: usize) -> DecodeOutput {
        let sr = 48_000.0_f32;
        let mut gen = Generator::new(Format::SeratoCv02, sr);
        let mut dec = Decoder::new(Format::SeratoCv02, sr);
        let mut buf = vec![0.0f32; n_frames * 2];
        gen.render(&mut buf, rate, 0.5);
        dec.process(&buf)
    }

    /// Tolerance on the rate estimate: at unity, our coherent sum's
    /// argument resolves down to `~ 1/√(N·SNR)` rad ≈ a few mrad for
    /// noiseless synthetic input over thousands of samples. Tighten
    /// this once we have a noise model (M5.4).
    const RATE_TOL: f64 = 0.005;

    #[test]
    fn unity_rate_decodes_to_unity() {
        let out = roundtrip(1.0, 4_800);
        assert!(
            (out.rate - 1.0).abs() < RATE_TOL,
            "rate = {} (want ≈1.0)",
            out.rate
        );
        assert!(out.confidence > 0.99, "confidence = {}", out.confidence);
        assert!((out.amplitude - 0.5).abs() < 0.01);
    }

    #[test]
    fn half_rate_decodes_to_half() {
        let out = roundtrip(0.5, 4_800);
        assert!(
            (out.rate - 0.5).abs() < RATE_TOL,
            "rate = {} (want ≈0.5)",
            out.rate
        );
        assert!(out.confidence > 0.99);
    }

    #[test]
    fn double_rate_decodes_to_double() {
        let out = roundtrip(2.0, 4_800);
        assert!(
            (out.rate - 2.0).abs() < RATE_TOL,
            "rate = {} (want ≈2.0)",
            out.rate
        );
    }

    #[test]
    fn reverse_unity_decodes_to_negative_unity() {
        let out = roundtrip(-1.0, 4_800);
        assert!(
            (out.rate - (-1.0)).abs() < RATE_TOL,
            "rate = {} (want ≈-1.0)",
            out.rate
        );
        assert!(out.confidence > 0.99);
    }

    #[test]
    fn stopped_decodes_to_zero_rate() {
        let out = roundtrip(0.0, 4_800);
        // At rate=0 the signal is DC (constant ch0=0, ch1=A). The
        // phase-difference is zero, so rate = 0. Confidence stays
        // high because the signal is still perfectly coherent — just
        // at zero frequency.
        assert!(out.rate.abs() < 1e-9, "rate = {}", out.rate);
        assert!(out.confidence > 0.99);
    }

    #[test]
    fn silence_yields_low_confidence() {
        // No signal at all → low amplitude, undefined frequency.
        // We accept any rate (it's nonsense by definition) but
        // confidence MUST be near zero so the integration layer
        // can ignore the output.
        let mut dec = Decoder::new(Format::SeratoCv02, 48_000.0);
        let buf = vec![0.0f32; 4_800 * 2];
        let out = dec.process(&buf);
        assert!(out.amplitude < 1e-6, "amplitude = {}", out.amplitude);
        assert!(out.confidence < 0.01, "confidence = {}", out.confidence);
    }

    #[test]
    fn position_integrates_at_unity() {
        // 1 second at unity rate should advance position by 1 second.
        let sr = 48_000.0_f32;
        let mut gen = Generator::new(Format::SeratoCv02, sr);
        let mut dec = Decoder::new(Format::SeratoCv02, sr);
        let mut buf = vec![0.0f32; 48_000 * 2];
        gen.render(&mut buf, 1.0, 0.5);
        let out = dec.process(&buf);
        assert!(
            (out.position_secs - 1.0).abs() < 0.01,
            "position = {} (want ≈1.0s)",
            out.position_secs
        );
    }

    #[test]
    fn position_is_signed_under_reverse() {
        // 0.5 s forward + 0.5 s reverse → final position ≈ 0.
        let sr = 48_000.0_f32;
        let mut gen = Generator::new(Format::SeratoCv02, sr);
        let mut dec = Decoder::new(Format::SeratoCv02, sr);
        let mut buf = vec![0.0f32; 24_000 * 2];

        gen.render(&mut buf, 1.0, 0.5);
        dec.process(&buf);
        gen.render(&mut buf, -1.0, 0.5);
        let out = dec.process(&buf);

        assert!(
            out.position_secs.abs() < 0.02,
            "net position = {} (want ≈0)",
            out.position_secs
        );
    }

    #[test]
    fn block_size_independent_at_unity() {
        // Decoding the same input as one big block vs many small
        // blocks should give the same final position (within a few
        // mrad of phase, i.e. ~1 µs of record time).
        let sr = 48_000.0_f32;
        let mut gen_big = Generator::new(Format::SeratoCv02, sr);
        let mut dec_big = Decoder::new(Format::SeratoCv02, sr);
        let mut big = vec![0.0f32; 9_600 * 2];
        gen_big.render(&mut big, 1.0, 0.5);
        let big_out = dec_big.process(&big);

        let mut gen_small = Generator::new(Format::SeratoCv02, sr);
        let mut dec_small = Decoder::new(Format::SeratoCv02, sr);
        let mut small = vec![0.0f32; 64 * 2];
        let mut last = DecodeOutput {
            rate: 0.0,
            position_secs: 0.0,
            amplitude: 0.0,
            confidence: 0.0,
        };
        for _ in 0..(9_600 / 64) {
            gen_small.render(&mut small, 1.0, 0.5);
            last = dec_small.process(&small);
        }
        assert!(
            (big_out.position_secs - last.position_secs).abs() < 1e-3,
            "big={} small={}",
            big_out.position_secs,
            last.position_secs
        );
    }

    #[test]
    fn process_is_alloc_free() {
        // Steady-state RT use: process() called over and over on the
        // audio thread. Must not allocate.
        let sr = 48_000.0_f32;
        let mut gen = Generator::new(Format::SeratoCv02, sr);
        let mut dec = Decoder::new(Format::SeratoCv02, sr);
        let mut buf = vec![0.0f32; 64 * 2];
        gen.render(&mut buf, 1.0, 0.5);
        // Prime the decoder once outside the assertion.
        dec.process(&buf);

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..100 {
                gen.render(&mut buf, 1.0, 0.5);
                let _ = dec.process(&buf);
            }
        });
    }

    /// M6: Traktor round-trip parity. The decoder math is format-
    /// agnostic — the only per-format parameter today is `carrier_hz`
    /// — so the same property tests should pass at 2 kHz (MK1) and
    /// 2.5 kHz (MK2) as at 1 kHz (Serato). Both are covered because
    /// their carriers differ by 25%; decoding MK2 vinyl with an MK1
    /// nominal would silently play back at +25% speed — exactly the
    /// bug class these property tests catch.
    fn roundtrip_format(format: Format, rate: f64, n_frames: usize) -> DecodeOutput {
        let sr = 48_000.0_f32;
        let mut gen = Generator::new(format, sr);
        let mut dec = Decoder::new(format, sr);
        let mut buf = vec![0.0f32; n_frames * 2];
        gen.render(&mut buf, rate, 0.5);
        dec.process(&buf)
    }

    #[test]
    fn traktor_mk1_unity_decodes_to_unity() {
        let out = roundtrip_format(Format::TraktorMk1, 1.0, 4_800);
        assert!(
            (out.rate - 1.0).abs() < RATE_TOL,
            "MK1 rate = {} (want ≈1.0)",
            out.rate
        );
        assert!(out.confidence > 0.99);
        assert!((out.amplitude - 0.5).abs() < 0.01);
    }

    #[test]
    fn traktor_mk2_unity_decodes_to_unity() {
        let out = roundtrip_format(Format::TraktorMk2, 1.0, 4_800);
        assert!(
            (out.rate - 1.0).abs() < RATE_TOL,
            "MK2 rate = {} (want ≈1.0)",
            out.rate
        );
        assert!(out.confidence > 0.99);
    }

    #[test]
    fn traktor_mk1_reverse_decodes_negative() {
        let out = roundtrip_format(Format::TraktorMk1, -1.0, 4_800);
        assert!(
            (out.rate - (-1.0)).abs() < RATE_TOL,
            "MK1 reverse = {}",
            out.rate
        );
    }

    #[test]
    fn traktor_mk2_reverse_decodes_negative() {
        let out = roundtrip_format(Format::TraktorMk2, -1.0, 4_800);
        assert!(
            (out.rate - (-1.0)).abs() < RATE_TOL,
            "MK2 reverse = {}",
            out.rate
        );
    }

    #[test]
    fn traktor_mk2_4x_rate_clears_alias_band() {
        // At 2.5 kHz carrier, alias band starts at 0.5·SR/carrier
        // = 9.6× rate. Real DJ scratching tops out ~8× so we have
        // headroom — but it's the tightest of our three formats.
        // Pin a 4× rate test so any future regression that drops
        // alias-safety below 5× shows up as a test failure.
        let out = roundtrip_format(Format::TraktorMk2, 4.0, 4_800);
        assert!(
            (out.rate - 4.0).abs() < RATE_TOL * 4.0,
            "MK2 4× rate = {} (want ≈4.0, well clear of alias band)",
            out.rate
        );
    }

    #[test]
    fn traktor_position_integrates_at_unity_for_both_generations() {
        // 1 second at unity rate should advance position by 1 second
        // for both MK1 (2 kHz) and MK2 (2.5 kHz). If the
        // generator-decoder loop ever desynchronises on carrier, this
        // test moves first.
        for format in [Format::TraktorMk1, Format::TraktorMk2] {
            let sr = 48_000.0_f32;
            let mut gen = Generator::new(format, sr);
            let mut dec = Decoder::new(format, sr);
            let mut buf = vec![0.0f32; 48_000 * 2];
            gen.render(&mut buf, 1.0, 0.5);
            let out = dec.process(&buf);
            assert!(
                (out.position_secs - 1.0).abs() < 0.01,
                "{format:?} position = {} (want ≈1.0s)",
                out.position_secs
            );
        }
    }

    #[test]
    fn mk2_vinyl_decoded_as_mk1_plays_back_too_fast_by_25_percent() {
        // Critical regression test. M6 was originally shipped with
        // MK2 set to 2 kHz instead of 2.5 kHz — silent mis-routing,
        // playback would have been at 80% speed on MK2 vinyl. To
        // catch any future refactor that accidentally collapses MK1
        // and MK2 to the same carrier, we *deliberately* feed an
        // MK2-generated signal to an MK1-configured decoder and
        // assert the rate comes back at +25% (= 2500/2000), not
        // +0%. If MK1 and MK2 ever share a carrier, this test will
        // break — which is the right time to revisit format
        // proliferation.
        let sr = 48_000.0_f32;
        let mut gen_mk2 = Generator::new(Format::TraktorMk2, sr);
        let mut dec_mk1 = Decoder::new(Format::TraktorMk1, sr);
        let mut buf = vec![0.0f32; 4_800 * 2];
        gen_mk2.render(&mut buf, 1.0, 0.5);
        let out = dec_mk1.process(&buf);
        let expected = 2500.0 / 2000.0;
        assert!(
            (out.rate - expected).abs() < 0.01,
            "MK2-vinyl-as-MK1-decoder rate = {} (want ≈{} = wrong-by-25%)",
            out.rate,
            expected
        );
    }

    #[test]
    fn traktor_silence_yields_low_confidence() {
        for format in [Format::TraktorMk1, Format::TraktorMk2] {
            let mut dec = Decoder::new(format, 48_000.0);
            let buf = vec![0.0f32; 4_800 * 2];
            let out = dec.process(&buf);
            assert!(
                out.confidence < 0.01,
                "{format:?} confidence on silence = {}",
                out.confidence
            );
        }
    }

    #[test]
    fn varying_rate_tracks_continuously() {
        // Slew the rate from 1.0 down to 0.0 over 1 second in 100
        // steps. Decoded rate at each step should be within tolerance
        // of the requested value. This is the closest unit-test
        // approximation of a real scratch.
        let sr = 48_000.0_f32;
        let block = 480_usize; // 10 ms blocks
        let mut gen = Generator::new(Format::SeratoCv02, sr);
        let mut dec = Decoder::new(Format::SeratoCv02, sr);
        let mut buf = vec![0.0f32; block * 2];
        let mut max_err = 0.0_f64;
        for step in 0..100i32 {
            let want = 1.0 - f64::from(step) * 0.01;
            gen.render(&mut buf, want, 0.5);
            let out = dec.process(&buf);
            let err = (out.rate - want).abs();
            if err > max_err {
                max_err = err;
            }
        }
        assert!(max_err < 0.02, "max rate err = {max_err}");
    }
}
