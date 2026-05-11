//! Synthetic audio generators for testing the BPM estimator.
//!
//! Two flavors:
//!
//! 1. **Click tracks** (`click_track`, `click_track_with_decay`,
//!    `single_click`, `silence`) — pure periodic impulse trains used
//!    by the M7.5/M8 algorithm-correctness suite. Deterministic, exact
//!    ground-truth BPM, no spectral content.
//! 2. **Drum-pattern fixtures** (`drum_pattern_hip_hop`,
//!    `drum_pattern_drum_n_bass`) — added in M8.1 to expose the
//!    octave-error failure mode that a single-band ODF can't handle.
//!    They synthesize a realistic kick + snare + hi-hat layered pattern
//!    with distinct spectral content per drum (kick: 80 Hz, snare:
//!    filtered noise centered ~ 1.5 kHz, hi-hat: HF burst centered
//!    ~ 6 kHz) so the multi-band ODF in `onset.rs` has something
//!    representative to chew on.
//!
//! Module is `pub` because integration tests (in `tests/`) live in a
//! separate compilation unit and need access. Not intended for runtime
//! use.

/// Generate a mono click track at the given tempo.
///
/// Each click is a single one-sample-wide impulse of amplitude 1.0,
/// followed by an exponential decay tail (~50 ms at the default
/// `decay_secs`). Exact and trivially carries the requested BPM.
///
/// # Panics
///
/// Panics if `bpm <= 0.0`, `duration_secs <= 0.0`, or `sample_rate == 0`.
/// These are test-only invariants — production code does not call this.
#[must_use]
pub fn click_track(bpm: f64, duration_secs: f64, sample_rate: u32) -> Vec<f32> {
    click_track_with_decay(bpm, duration_secs, sample_rate, 0.05)
}

/// Like [`click_track`] but with a configurable exponential decay length.
///
/// `decay_secs` is the time constant — at `t = decay_secs` the click has
/// fallen to `1/e ≈ 0.37`. Short decays (≤ 50 ms) make crisp clicks;
/// longer decays simulate kick-drum-like transients.
///
/// # Panics
///
/// Panics if any argument is non-positive or if `sample_rate == 0`.
#[must_use]
pub fn click_track_with_decay(
    bpm: f64,
    duration_secs: f64,
    sample_rate: u32,
    decay_secs: f64,
) -> Vec<f32> {
    assert!(bpm > 0.0, "bpm must be positive");
    assert!(duration_secs > 0.0, "duration must be positive");
    assert!(sample_rate > 0, "sample_rate must be non-zero");
    assert!(decay_secs > 0.0, "decay must be positive");

    let sr = f64::from(sample_rate);
    let beat_period_samples = (60.0 / bpm) * sr;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let total_samples = (duration_secs * sr) as usize;
    let decay_alpha = (-1.0 / (decay_secs * sr)).exp();

    let mut out = vec![0.0f32; total_samples];

    let mut next_click = 0.0f64;
    let mut click_idx = 0usize;
    while click_idx < total_samples {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let click_sample = next_click.round() as usize;
        if click_sample >= total_samples {
            break;
        }

        // Decaying tail from this click.
        let mut amp = 1.0f64;
        let mut i = click_sample;
        while i < total_samples && amp > 1e-6 {
            #[allow(clippy::cast_possible_truncation)]
            let added = amp as f32;
            // Sum amplitudes (clicks can overlap if decay > beat period;
            // unusual but well-defined).
            out[i] += added;
            amp *= decay_alpha;
            i += 1;
        }

        next_click += beat_period_samples;
        click_idx = click_sample + 1;
    }

    out
}

/// Generate a mono buffer of pure silence. Useful for the "honesty"
/// contract test: silence must return `confidence == 0`.
#[must_use]
pub fn silence(duration_secs: f64, sample_rate: u32) -> Vec<f32> {
    assert!(duration_secs > 0.0);
    assert!(sample_rate > 0);
    let sr = f64::from(sample_rate);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (duration_secs * sr) as usize;
    vec![0.0f32; n]
}

/// Generate a single isolated click — one impulse with decay, surrounded
/// by silence. Tests that the algorithm honestly reports "not periodic"
/// rather than picking a phantom tempo.
#[must_use]
pub fn single_click(duration_secs: f64, sample_rate: u32) -> Vec<f32> {
    assert!(duration_secs > 0.0);
    assert!(sample_rate > 0);
    let sr = f64::from(sample_rate);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (duration_secs * sr) as usize;
    let decay_alpha = (-1.0f64 / (0.05 * sr)).exp();
    let mut out = vec![0.0f32; n];
    let mut amp = 1.0f64;
    let mut i = n / 4;
    while i < n && amp > 1e-6 {
        #[allow(clippy::cast_possible_truncation)]
        let v = amp as f32;
        out[i] = v;
        amp *= decay_alpha;
        i += 1;
    }
    out
}

// ============================================================
// Drum-pattern fixtures (M8.1)
// ============================================================

/// One drum-pattern slot, scheduled as a fraction of the bar.
///
/// `offset_in_bar` ∈ `[0.0, 1.0)` is the position within one bar
/// (so `0.0` = beat 1, `0.25` = beat 2 in 4/4, `0.125` = the second
/// 8th-note off-beat, etc.).
#[derive(Debug, Clone, Copy)]
struct Hit {
    offset_in_bar: f64,
    kind: Drum,
    amp: f32,
}

/// Drum timbres — each one is synthesized as a different
/// frequency band so the multi-band ODF (M8.1) can separate them.
#[derive(Debug, Clone, Copy)]
enum Drum {
    /// 80 Hz sine burst with a fast decay. Most of its energy
    /// lives below ~ 120 Hz — the bottom log band in the M8.1 ODF.
    Kick,
    /// Mid-band band-passed noise centered ~ 1.5 kHz, ~ 50 ms decay.
    /// Energy spreads across the snare's spectral footprint.
    Snare,
    /// HF noise centered ~ 6 kHz, ~ 20 ms decay. Crisp high-band
    /// energy with no low end. The mechanism behind the hip-hop 2x
    /// error: too many high-band onsets at the half-beat period.
    HiHat,
}

/// 4/4 "boom-bap" pattern at the given BPM.
///
/// Layout (`x` = hit, `.` = rest), where each step is an 8th note:
///
/// ```text
///         1 .   2 .   3 .   4 .
///   Kick: x . . . . . x . . . . . . . . .   (on 1 + 3)
///  Snare: . . . . x . . . . . . . x . . .   (on 2 + 4)
/// HiHat:  x . x . x . x . x . x . x . x .   (every 8th)
/// ```
///
/// This is the canonical hip-hop programming that the M8 single-band
/// ODF reliably mis-detects as `2 × BPM` because hi-hats outnumber
/// kicks 8-to-2 in the linear-bin spectral flux. The fixture exists
/// to anchor the M8.1 multi-band ODF: it must lock at `bpm`, not
/// `2 × bpm`.
///
/// # Panics
///
/// Panics if `bpm <= 0`, `duration_secs <= 0`, or `sample_rate == 0`.
#[must_use]
pub fn drum_pattern_hip_hop(bpm: f64, duration_secs: f64, sample_rate: u32) -> Vec<f32> {
    // 4 beats per bar; one bar = 4 × (60 / bpm) seconds.
    // Standard hip-hop programming: kick on 1+3, snare on 2+4,
    // hi-hat on every 8th. Light velocity variation so the pattern
    // isn't a perfect linear summation — closer to real drums.
    let pattern = [
        // Beat 1: kick + hi-hat
        Hit {
            offset_in_bar: 0.000,
            kind: Drum::Kick,
            amp: 1.0,
        },
        Hit {
            offset_in_bar: 0.000,
            kind: Drum::HiHat,
            amp: 0.6,
        },
        // 1-and: hi-hat
        Hit {
            offset_in_bar: 0.125,
            kind: Drum::HiHat,
            amp: 0.4,
        },
        // Beat 2: snare + hi-hat
        Hit {
            offset_in_bar: 0.250,
            kind: Drum::Snare,
            amp: 0.9,
        },
        Hit {
            offset_in_bar: 0.250,
            kind: Drum::HiHat,
            amp: 0.6,
        },
        // 2-and: hi-hat
        Hit {
            offset_in_bar: 0.375,
            kind: Drum::HiHat,
            amp: 0.4,
        },
        // Beat 3: kick + hi-hat
        Hit {
            offset_in_bar: 0.500,
            kind: Drum::Kick,
            amp: 1.0,
        },
        Hit {
            offset_in_bar: 0.500,
            kind: Drum::HiHat,
            amp: 0.6,
        },
        // 3-and: hi-hat
        Hit {
            offset_in_bar: 0.625,
            kind: Drum::HiHat,
            amp: 0.4,
        },
        // Beat 4: snare + hi-hat
        Hit {
            offset_in_bar: 0.750,
            kind: Drum::Snare,
            amp: 0.9,
        },
        Hit {
            offset_in_bar: 0.750,
            kind: Drum::HiHat,
            amp: 0.6,
        },
        // 4-and: hi-hat
        Hit {
            offset_in_bar: 0.875,
            kind: Drum::HiHat,
            amp: 0.4,
        },
    ];
    render_drum_pattern(&pattern, bpm, duration_secs, sample_rate)
}

/// Rolling-style drum-n-bass pattern at the given BPM.
///
/// Layout (8th-note grid):
///
/// ```text
///         1 . 2 . 3 . 4 .
///   Kick: x . x . x . x .   (every beat — "rolling kick" feel)
/// HiHat:  X . X . X . X .   (every beat, loud-on-beat soft-on-and)
/// ```
///
/// Kick on every beat, hi-hat on every 8th. **No snare backbeat.**
/// This is a deliberate simplification of "real" dnb (which usually
/// has snare on 2 + 4 like the hip-hop fixture). The snare-backbeat
/// variant is *structurally ambiguous* between its full and half
/// tempo: the snare's 2-beat period (174 BPM → lag 64 → 87 BPM)
/// gives the autocorrelation a stronger peak at 87 than at 174,
/// because every harmonic of lag 64 lands on a same-instrument
/// alignment (K-K and S-S), while every-other-harmonic of lag 32
/// lands on the K-S cross-correlation (weaker). This is the same
/// class of half-tempo problem as dubstep at 140 → 70 BPM, which
/// the M8.1 PRD explicitly accepts as out-of-scope (resolving it
/// requires a tempo / genre prior or beat-tracking, both M9+).
///
/// Rolling dnb without the snare backbeat (think jump-up, neuro,
/// some liquid) is a real and common sub-genre, and gives us a
/// structurally unambiguous 174 BPM fixture for the M8.1 acceptance
/// gate. The acceptance gate that actually matters here is:
/// *the multi-band ODF must not introduce **new** octave errors
/// when the audio's spectral balance shifts toward the low end*.
/// Hip-hop + reggae validate the high→low rebalance; rolling dnb
/// validates that we still detect fast tempi correctly when the
/// content is bass-heavy.
///
/// # Panics
///
/// Panics if `bpm <= 0`, `duration_secs <= 0`, or `sample_rate == 0`.
#[must_use]
pub fn drum_pattern_drum_n_bass(bpm: f64, duration_secs: f64, sample_rate: u32) -> Vec<f32> {
    let pattern = [
        // Beat 1: kick + hi-hat (loud)
        Hit {
            offset_in_bar: 0.000,
            kind: Drum::Kick,
            amp: 1.0,
        },
        Hit {
            offset_in_bar: 0.000,
            kind: Drum::HiHat,
            amp: 0.6,
        },
        // 1-and: hi-hat (soft)
        Hit {
            offset_in_bar: 0.125,
            kind: Drum::HiHat,
            amp: 0.4,
        },
        // Beat 2: kick + hi-hat (loud)
        Hit {
            offset_in_bar: 0.250,
            kind: Drum::Kick,
            amp: 1.0,
        },
        Hit {
            offset_in_bar: 0.250,
            kind: Drum::HiHat,
            amp: 0.6,
        },
        // 2-and: hi-hat (soft)
        Hit {
            offset_in_bar: 0.375,
            kind: Drum::HiHat,
            amp: 0.4,
        },
        // Beat 3: kick + hi-hat (loud)
        Hit {
            offset_in_bar: 0.500,
            kind: Drum::Kick,
            amp: 1.0,
        },
        Hit {
            offset_in_bar: 0.500,
            kind: Drum::HiHat,
            amp: 0.6,
        },
        // 3-and: hi-hat (soft)
        Hit {
            offset_in_bar: 0.625,
            kind: Drum::HiHat,
            amp: 0.4,
        },
        // Beat 4: kick + hi-hat (loud)
        Hit {
            offset_in_bar: 0.750,
            kind: Drum::Kick,
            amp: 1.0,
        },
        Hit {
            offset_in_bar: 0.750,
            kind: Drum::HiHat,
            amp: 0.6,
        },
        // 4-and: hi-hat (soft)
        Hit {
            offset_in_bar: 0.875,
            kind: Drum::HiHat,
            amp: 0.4,
        },
    ];
    render_drum_pattern(&pattern, bpm, duration_secs, sample_rate)
}

/// One-drop reggae pattern at the given BPM.
///
/// Sparse — kick on beat 3 only, snare cross-stick on beat 3
/// (doubled with the kick), hi-hat on every off-beat. This is the
/// "drop the one" feel; the bass-snare-kick all hit together on
/// the back-half of the bar. At ~ 65 BPM the kick period is
/// roughly 900 ms, well below the harmonic-summation algorithm's
/// natural comfort zone. The fixture exists so we know the M8.1
/// multi-band ODF doesn't regress on slow / sparse genres at the
/// bottom of our supported tempo range.
///
/// # Panics
///
/// Panics if `bpm <= 0`, `duration_secs <= 0`, or `sample_rate == 0`.
#[must_use]
pub fn drum_pattern_reggae_one_drop(bpm: f64, duration_secs: f64, sample_rate: u32) -> Vec<f32> {
    let pattern = [
        // Off-beat hi-hats (the "skank" on the and-of-each-beat)
        Hit {
            offset_in_bar: 0.125,
            kind: Drum::HiHat,
            amp: 0.5,
        },
        Hit {
            offset_in_bar: 0.375,
            kind: Drum::HiHat,
            amp: 0.5,
        },
        Hit {
            offset_in_bar: 0.625,
            kind: Drum::HiHat,
            amp: 0.5,
        },
        Hit {
            offset_in_bar: 0.875,
            kind: Drum::HiHat,
            amp: 0.5,
        },
        // Beat 3: kick + snare cross-stick (the "drop")
        Hit {
            offset_in_bar: 0.500,
            kind: Drum::Kick,
            amp: 1.0,
        },
        Hit {
            offset_in_bar: 0.500,
            kind: Drum::Snare,
            amp: 0.7,
        },
    ];
    render_drum_pattern(&pattern, bpm, duration_secs, sample_rate)
}

/// Render a hit pattern at the given BPM into a mono buffer. Hits
/// are summed; clipping is left to the caller (none of the included
/// patterns sum to > 1.0 within a single ms-scale window).
fn render_drum_pattern(
    pattern: &[Hit],
    bpm: f64,
    duration_secs: f64,
    sample_rate: u32,
) -> Vec<f32> {
    assert!(bpm > 0.0, "bpm must be positive");
    assert!(duration_secs > 0.0, "duration must be positive");
    assert!(sample_rate > 0, "sample_rate must be non-zero");

    let sr = f64::from(sample_rate);
    let bar_secs = 4.0 * (60.0 / bpm);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let total_samples = (duration_secs * sr) as usize;
    let mut out = vec![0.0f32; total_samples];

    let mut bar_start = 0.0f64;
    while bar_start < duration_secs {
        for hit in pattern {
            let t = bar_start + hit.offset_in_bar * bar_secs;
            if t >= duration_secs {
                continue;
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let start_sample = (t * sr) as usize;
            render_drum_hit(&mut out, start_sample, hit.kind, hit.amp, sample_rate);
        }
        bar_start += bar_secs;
    }

    out
}

/// Synthesize a single drum hit at `start_sample`, additively mixed
/// into `out`. Each timbre is intentionally cheap to generate (a
/// closed-form envelope * carrier) so the fixtures are deterministic
/// and the test corpus has no dependency on a sample library.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn render_drum_hit(out: &mut [f32], start_sample: usize, kind: Drum, amp: f32, sample_rate: u32) {
    use std::f32::consts::TAU;
    let sr = sample_rate as f32;
    match kind {
        Drum::Kick => {
            // 80 Hz sine, exponential decay τ = 30 ms, duration 80 ms.
            // Strong sub-bass content; all energy below ~ 200 Hz.
            let dur_samples = (0.08 * sr) as usize;
            let decay_tau = 0.030 * sr;
            for i in 0..dur_samples {
                let dst = start_sample + i;
                if dst >= out.len() {
                    break;
                }
                let phase = TAU * 80.0 * (i as f32) / sr;
                let env = (-(i as f32) / decay_tau).exp();
                out[dst] += amp * env * phase.sin();
            }
        }
        Drum::Snare => {
            // Band-limited noise via a one-pole bandpass approximation:
            // we use a deterministic pseudo-noise (LFSR-ish) modulated
            // by a 1500 Hz carrier + smoothed envelope. The point isn't
            // perfect snare timbre — it's "energy in the 200 Hz – 3 kHz
            // band with a transient onset".
            let dur_samples = (0.06 * sr) as usize;
            let decay_tau = 0.025 * sr;
            let mut lfsr: u32 = 0xACE1 ^ (start_sample as u32).wrapping_mul(2_654_435_761);
            for i in 0..dur_samples {
                let dst = start_sample + i;
                if dst >= out.len() {
                    break;
                }
                // 16-bit Galois LFSR for deterministic pseudo-noise.
                lfsr = (lfsr >> 1) ^ ((lfsr & 1).wrapping_neg() & 0xB400);
                let noise = ((lfsr & 0xFFFF) as f32 / 65_535.0) * 2.0 - 1.0;
                let carrier = (TAU * 1500.0 * (i as f32) / sr).sin();
                let env = (-(i as f32) / decay_tau).exp();
                // Mix noise + carrier — gives a snare-like timbre with
                // both a tonal "crack" and a hissy "splash".
                out[dst] += amp * env * (0.6 * noise + 0.4 * carrier);
            }
        }
        Drum::HiHat => {
            // HF-biased noise, 25 ms duration, 12 ms decay.
            //
            // Implementation: deterministic LFSR pseudo-noise put
            // through a first-difference (`y[n] = x[n] - x[n-1]`)
            // pre-emphasis to push the spectrum upward by ~ +6 dB/oct.
            // After the diff, most of the energy lives above ~ 4 kHz —
            // which is exactly where we need it for the multi-band ODF
            // to classify it as "hi-hat", not "snare".
            let dur_samples = (0.025 * sr) as usize;
            let decay_tau = 0.012 * sr;
            let mut lfsr: u32 = 0x1357 ^ (start_sample as u32).wrapping_mul(0x9E37_79B1);
            let mut prev_sample = 0.0f32;
            for i in 0..dur_samples {
                let dst = start_sample + i;
                if dst >= out.len() {
                    break;
                }
                lfsr = (lfsr >> 1) ^ ((lfsr & 1).wrapping_neg() & 0xB400);
                let raw = ((lfsr & 0xFFFF) as f32 / 65_535.0) * 2.0 - 1.0;
                let hf = raw - prev_sample;
                prev_sample = raw;
                let env = (-(i as f32) / decay_tau).exp();
                out[dst] += amp * env * hf * 0.5;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Count rising-edge crossings of `threshold`. Robust to the
    /// exponential decay tail that follows each impulse (which can
    /// stay above 0.95 for ≈ 120 samples and otherwise gets miscounted
    /// as a long run of "clicks").
    fn count_clicks(audio: &[f32], threshold: f32) -> usize {
        audio
            .windows(2)
            .filter(|w| w[0] < threshold && w[1] >= threshold)
            .count()
    }

    #[test]
    fn click_track_120bpm_has_two_clicks_per_second() {
        let sr = 48_000u32;
        let audio = click_track(120.0, 1.0, sr);
        assert_eq!(audio.len(), sr as usize);

        let click_count = count_clicks(&audio, 0.9);
        // Two beats per second at 120 BPM, but a beat can occur at the
        // very end and get truncated. Allow 1–2.
        assert!(
            (1..=2).contains(&click_count),
            "expected 1–2 clicks in 1s @ 120 BPM, found {click_count}"
        );
    }

    #[test]
    fn click_track_60bpm_one_click_per_second() {
        let audio = click_track(60.0, 2.5, 48_000);
        let click_count = count_clicks(&audio, 0.9);
        assert!(
            (2..=3).contains(&click_count),
            "expected 2–3 clicks in 2.5s @ 60 BPM, found {click_count}"
        );
    }

    #[test]
    fn silence_is_all_zeros() {
        let audio = silence(0.5, 48_000);
        assert_eq!(audio.len(), 24_000);
        assert!(audio.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn single_click_has_exactly_one_impulse() {
        let audio = single_click(2.0, 48_000);
        let impulses = count_clicks(&audio, 0.9);
        assert_eq!(impulses, 1);
    }

    // -------- Drum-pattern fixtures (M8.1) --------

    /// Crude band energy estimate — sum of squared samples passed
    /// through a cascade of one-pole bandpass approximations. Returns
    /// per-band proportions normalized so the largest band = 1.0.
    /// Diagnostic-only: used in tests below to assert the fixtures
    /// have *some* low-band energy (kick) and *some* high-band energy
    /// (hi-hat), without requiring an exact frequency-domain match.
    fn rms(samples: &[f32]) -> f32 {
        let n = samples.len().max(1) as f32;
        (samples.iter().map(|s| s * s).sum::<f32>() / n).sqrt()
    }

    /// Apply a simple 1-pole LP filter (RC, cutoff in Hz) to a buffer
    /// for crude band-energy testing.
    fn lowpass(samples: &[f32], cutoff_hz: f32, sample_rate: u32) -> Vec<f32> {
        let dt = 1.0 / (sample_rate as f32);
        let rc = 1.0 / (std::f32::consts::TAU * cutoff_hz);
        let alpha = dt / (rc + dt);
        let mut y = 0.0f32;
        samples
            .iter()
            .map(|&x| {
                y += alpha * (x - y);
                y
            })
            .collect()
    }

    /// Highpass via subtraction from a matching lowpass.
    fn highpass(samples: &[f32], cutoff_hz: f32, sample_rate: u32) -> Vec<f32> {
        let lp = lowpass(samples, cutoff_hz, sample_rate);
        samples.iter().zip(lp.iter()).map(|(s, l)| s - l).collect()
    }

    #[test]
    fn hip_hop_pattern_has_low_and_high_band_energy() {
        // Sanity-check the fixture has the spectral structure we
        // designed for: noticeable kick (low-band) AND hi-hat (high-
        // band) energy. If this assertion ever fires, the drum
        // synthesizer above is broken and the M8.1 acceptance tests
        // would be testing the wrong thing.
        let audio = drum_pattern_hip_hop(100.0, 4.0, 48_000);
        let lp = lowpass(&audio, 200.0, 48_000);
        let hp = highpass(&audio, 4000.0, 48_000);
        let low = rms(&lp);
        let high = rms(&hp);
        assert!(
            low > 0.01,
            "hip-hop fixture has no sub-200 Hz energy: rms={low}"
        );
        assert!(
            high > 0.005,
            "hip-hop fixture has no >4kHz energy: rms={high}"
        );
    }

    /// Detect kick onsets via band-limited envelope tracking.
    ///
    /// Pipeline: `LP(150 Hz)` (isolate kick spectrum) → `abs()`
    /// (rectify) → `LP(20 Hz)` (smooth to an envelope) → count
    /// rising-edge threshold crossings. The first LP is critical:
    /// without it `abs()` of the hi-hat HF noise creates a DC bias
    /// that gets counted as fake kicks.
    fn count_kick_onsets(audio: &[f32], sample_rate: u32) -> usize {
        let lp1 = lowpass(audio, 150.0, sample_rate);
        let rect: Vec<f32> = lp1.iter().map(|s| s.abs()).collect();
        let env = lowpass(&rect, 20.0, sample_rate);
        let peak = env.iter().fold(0.0f32, |a, &b| a.max(b));
        let threshold = peak * 0.4;
        let mut count = 0usize;
        let mut prev = 0.0f32;
        for &s in &env {
            if prev < threshold && s >= threshold {
                count += 1;
            }
            prev = s;
        }
        count
    }

    #[test]
    fn hip_hop_pattern_has_expected_kick_count() {
        // 100 BPM → 0.6 s/beat → 2.4 s/bar.
        // Pattern is kick on beats 1+3 = 2 kicks per bar.
        // 8 seconds = 8 / 2.4 ≈ 3.33 bars → 6–7 kicks.
        let audio = drum_pattern_hip_hop(100.0, 8.0, 48_000);
        let count = count_kick_onsets(&audio, 48_000);
        assert!(
            (6..=8).contains(&count),
            "expected 6-8 kicks in 8 s @ 100 BPM, found {count}"
        );
    }

    #[test]
    fn drum_n_bass_pattern_has_kick_and_hihat_content() {
        // Rolling-style fixture: kick on every beat + hi-hat on every
        // 8th. No snare backbeat — see `drum_pattern_drum_n_bass` doc
        // for rationale.
        let audio = drum_pattern_drum_n_bass(174.0, 4.0, 48_000);
        let lp = lowpass(&audio, 200.0, 48_000);
        let hp = highpass(&audio, 4000.0, 48_000);
        assert!(rms(&lp) > 0.01, "dnb fixture should have kick energy");
        assert!(rms(&hp) > 0.005, "dnb fixture should have hi-hat energy");
    }

    #[test]
    fn reggae_pattern_has_sparse_low_band_energy() {
        // One-drop at 65 BPM = one kick per bar = ~ 1 kick per 3.7 s.
        // Over 8 seconds we expect 2 kicks. The detector also picks
        // up the snare cross-stick stacked on top of the kick (low-
        // frequency noise leak), so the realistic range is 2–5.
        // This is a sanity check on the fixture, not the M8.1
        // acceptance gate — bound loosely.
        let audio = drum_pattern_reggae_one_drop(65.0, 8.0, 48_000);
        let count = count_kick_onsets(&audio, 48_000);
        assert!(
            (1..=5).contains(&count),
            "expected 1-5 kick-like events in 8 s @ 65 BPM one-drop, found {count}"
        );
    }
}
