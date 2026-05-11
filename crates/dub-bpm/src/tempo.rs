//! Tempo estimation from an onset detection function.
//!
//! Given the ODF emitted by [`crate::onset::OnsetDetector`], find the
//! tempo by autocorrelating in the lag range corresponding to
//! `[MIN_BPM, MAX_BPM]` and picking the integer lag whose
//! harmonic-mean **windowed local energy** is largest. A centroid
//! refinement on the picked lag's neighbourhood then yields the
//! fractional sub-bin BPM that the confidence tracker reports.
//!
//! ## Why windowed local-energy (M8.1+)
//!
//! Pure autocorrelation has equal-magnitude peaks at every integer
//! multiple of the true period (the classic "octave ambiguity"). For
//! a 128 BPM pulse train, `acf[P]`, `acf[2P]`, `acf[3P]`… are all
//! ≈ equal, and a naïve picker chooses one almost at random.
//!
//! M7.5 used a *harmonic sum* score: for each candidate `L`, sum
//! `acf[k·L]` for `k = 1, 2, …` up to `max_lag`. That correctly
//! suppresses the "picked 2P instead of P" failure mode on pure
//! pulse trains — at `2P`, the odd harmonics `3P, 5P, …` land at
//! non-peak lags and contribute zero, so `SUM(2P) < SUM(P)`. The
//! hidden cost: smaller `L` gets summed `floor(max_lag / L)` terms,
//! so faster tempos systematically score higher when their own odd
//! harmonics happen to fall on real peaks. That happens whenever
//! the ODF has sub-beat ostinato — hi-hats on every 8th note in
//! hip-hop, ride patterns in house, etc. The M8 production bug was
//! exactly this: a Diamond D track at 100 BPM was detected as 200
//! BPM because the hi-hat ODF energy made every `k · P/2` lag a
//! real acf peak, and the sum at `L = P/2` had `2×` the harmonics
//! of `L = P`.
//!
//! M8.1 fix part 1: score by the harmonic *mean*, not sum. Mean
//! removes the "more terms = bigger score" bias completely.
//!
//! M8.1 fix part 2 — *windowed local energy*. Real beat periods are
//! almost never integer multiples of the ODF sample interval. For
//! 140 BPM @ 48 kHz the true period is 40.18 ODF samples, so the
//! discrete spike pattern lands most consecutive-beat pairs in bin
//! 40 with a few in bin 41 — and analogously, skip-1 pairs land
//! mostly in bin 80 with a few in 81. The *total* energy under each
//! periodic peak is the same (as it must be for a periodic signal),
//! but the distribution across bins differs at each harmonic.
//! Parabolic-vertex height of either smoothed or raw ACF depends on
//! this distribution asymmetry — a wider shoulder pulls the vertex
//! up — so it consistently overshoots at `2P` versus `P`. That
//! structural bias was the regression that broke
//! `click_track_works_at_44100_hz` and the streaming tests during
//! M8.1 development.
//!
//! Window sum (`local(lag) = sum acf_raw[lag-W..=lag+W]`, with
//! `W = 2`) is *invariant* to where the energy sits within the
//! window. The total integrates the same way regardless of bin
//! split, so on clean periodic signals `score(P) ≡ score(2P)` to
//! within float epsilon, and the smaller-lag tiebreak fires only on
//! those genuine octave ties without ever swallowing real signal.
//!
//! M8.1 fix part 3 — *centroid refinement*. The same invariance
//! that makes the score robust erases sub-integer position info, so
//! the reported BPM is finally a centroid (energy-weighted mean
//! position) over the picked lag's window. Centroid evaluates to
//! the true continuous lag for any bin-split distribution of
//! periodic-peak energy, which is what gets the 128 and 174 BPM
//! synthetic tracks to land within `±1 BPM` of ground truth.
//!
//! ### Real-music robustness note
//!
//! The windowed-energy + centroid design is calibrated against the
//! synthetic fixtures in `tests/genre_octave.rs`. Real music may
//! introduce complications we don't currently address:
//!
//! 1. *Missed beats* (a kick dropping out for one bar) lower the
//!    mean of `L = P` slightly and could flip a marginal call.
//! 2. *K-S backbeat half-tempo* — patterns like real drum-n-bass
//!    (kick on 1+3, snare on 2+4 at 174 BPM) are structurally
//!    ambiguous between 174 and 87 BPM because the autocorrelation
//!    peaks at the K-K / S-S period (lag 64) just as strongly as at
//!    the K-S alternation period (lag 32). Resolving this requires
//!    a tempo / genre prior, M9+ scope. The user explicitly
//!    accepted this same limitation for dubstep at 140 → 70 BPM.
//!
//! M9+ real-music validation may motivate switching to a more
//! robust aggregator (trimmed mean, Hodges-Lehmann) and/or a richer
//! prior. The M8.1 acceptance gate is "the user's stated genre mix —
//! reggae 65, hip-hop 90/100, rolling dnb 174 — locks at the
//! correct octave"; we hit that with the windowed local energy +
//! harmonic mean + centroid refinement combination.
//!
//! Confidence is still the *normalized cross-correlation* at the
//! fundamental peak — `acf[P] / acf[0]`. For a perfectly periodic
//! signal this approaches 1.0; for noise it tends toward 0. Below
//! [`DETECTION_THRESHOLD`] we refuse the estimate entirely
//! (returning `None`) so the caller can't confuse "no detection"
//! with "very low confidence detection".

use crate::{BpmEstimate, BpmRange};

/// Below this normalized-cross-correlation ratio we declare "no
/// detection" rather than reporting a low-confidence estimate. Tuned to
/// pass the single-click / silence honesty tests while not rejecting
/// genuinely weak-but-real beats. Re-evaluate when the streaming driver
/// arrives in M8 and we have real-music data to calibrate against.
const DETECTION_THRESHOLD: f64 = 0.05;

/// Internal helper: unbiased autocorrelation at a single lag.
///
/// Returns `sum(x[i] * x[i + lag]) / (N - lag)` so longer lags
/// aren't penalised by having fewer terms to sum. This gives every
/// lag a "fair" per-pair magnitude, which matters for the
/// harmonic-mean score: biased ACF (with a `1 - lag/N` taper)
/// would shift the picker toward shorter lags structurally and
/// re-introduce the M8 hip-hop 2× failure mode that the log-band
/// ODF was designed to eliminate.
fn autocorr_at(detrended: &[f32], lag: usize) -> f64 {
    let n = detrended.len() - lag;
    let mut sum = 0.0f64;
    for i in 0..n {
        sum += f64::from(detrended[i]) * f64::from(detrended[i + lag]);
    }
    #[allow(clippy::cast_precision_loss)]
    {
        sum / (n as f64)
    }
}

/// How many harmonics deep to look. `acf` is computed up to
/// `HARMONIC_DEPTH * lag_max`; deeper means more octave-error
/// suppression at proportional cost.
const HARMONIC_DEPTH: usize = 4;

/// Cap on the number of harmonics scored per candidate. The harmonic
/// mean is *not* invariant under additional harmonics that drift past
/// the 3-tap smoothing window — if we include them, fast tempos (with
/// many fittable harmonics) get dragged down by drift while slow
/// tempos (with few harmonics) keep clean ones. That bias flips the
/// 140 BPM case to 70 BPM. Fixing the count at 4 puts every candidate
/// on equal footing in the same drift regime, which is exactly what
/// the harmonic-mean score requires for fair comparison across L.
///
/// 4 is chosen because:
///
/// * `4 × T_lag_max = max_lag` at our parameters, so the slowest
///   candidate naturally sees 4 harmonics without needing extension.
/// * For dnb at 174 BPM and shorter, 4 harmonics is `4 / (174 / 60)`
///   ≈ 1.4 seconds of acf evidence — enough to reject sub-multiples.
/// * For hip-hop at 100 BPM, 4 harmonics is plenty to distinguish
///   `L = P` (4 full-tempo peaks) from `L = P/2` (alternating
///   full/half peaks). The genre_octave acceptance tests confirm.
const MAX_HARMONICS: usize = 4;

/// Below this many harmonics-in-range, the mean isn't statistically
/// meaningful; we fall back to the fundamental alone. Hit only at
/// the very slowest candidates where `2 · lag_max > max_lag`.
const MIN_HARMONICS_FOR_MEAN: usize = 2;

/// Relative tolerance for the "true tie → faster-tempo" preference.
/// When two candidates' harmonic-mean scores agree within this
/// fraction of the larger score, we prefer the *smaller* refined
/// lag (faster BPM).
///
/// 1 % is appropriate here because the biased autocorrelation
/// already gives every octave its structural attenuation
/// (`(N-2P)/(N-P) ≈ 5–10 %` at typical 8–10 s ODFs). The remaining
/// noise to absorb is just float-epsilon (~1e-12) plus tiny
/// streaming-mode residuals from ODF length changing between
/// recomputes. 1 % gives margin without re-introducing the bias
/// the parabolic refinement was meant to remove.
///
/// Pure pulse trains where `P` and `2P` are *exactly* equal in
/// biased ACF (mathematically impossible — they can't tie unless
/// `N = ∞`) would fall into this window; that's the
/// pulse-train-octave case we explicitly want to default to the
/// faster tempo on.
const SCORE_TIE_REL_TOL: f64 = 0.01;

/// Estimate tempo from an ODF, restricting the search to `range`.
///
/// `odf_sample_rate` is the ODF's own sample rate in Hz — typically
/// `audio_sr / HOP_SIZE`. The function does no audio-rate scaling
/// itself; it operates purely in ODF time.
///
/// Returns `None` when the ODF is too short, contains no energy, or has
/// no peak above [`DETECTION_THRESHOLD`].
pub(crate) fn estimate_tempo(
    odf: &[f32],
    odf_sample_rate: f64,
    range: BpmRange,
) -> Option<BpmEstimate> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lag_min = ((60.0 * odf_sample_rate) / range.max).floor() as usize;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lag_max = ((60.0 * odf_sample_rate) / range.min).ceil() as usize;
    if lag_min < 2 || lag_max <= lag_min {
        return None;
    }

    if odf.len() < lag_max * 2 {
        return None;
    }

    // Detrend (subtract mean) + half-wave rectify. This removes the DC
    // bias that would otherwise dominate autocorrelation at every lag.
    #[allow(clippy::cast_precision_loss)]
    let n_f = odf.len() as f32;
    let mean = odf.iter().sum::<f32>() / n_f;
    let detrended: Vec<f32> = odf.iter().map(|&v| (v - mean).max(0.0)).collect();

    // Pre-compute autocorrelation up to HARMONIC_DEPTH × lag_max (or
    // the end of the ODF, whichever comes first) so the harmonic-sum
    // step is just an array lookup per candidate.
    let max_lag = (lag_max * HARMONIC_DEPTH).min(detrended.len().saturating_sub(1));
    let mut acf_raw = vec![0.0f64; max_lag + 1];
    for (lag, slot) in acf_raw.iter_mut().enumerate() {
        *slot = autocorr_at(&detrended, lag);
    }

    let acf_zero = acf_raw[0];
    if acf_zero < 1e-12 {
        // No energy → no detection. Covers silence and post-detrending
        // flat signals.
        return None;
    }

    // Per-candidate windowed local-energy scoring.
    //
    // For each integer lag `lo` in [lag_min, lag_max], compute the
    // peak's *total local energy*:
    //
    //   local(lo) = sum of acf_raw over [lo - W, lo + W]
    //
    // and score by the harmonic mean of `local(k · lo)` for
    // `k = 1..=MAX_HARMONICS`. The picker chooses the best integer
    // lag; a final parabolic refinement (3-pt vertex of `local`
    // around the best lag) gives sub-sample BPM precision.
    //
    // Why this and not the smoothed-ACF parabolic-vertex height that
    // earlier M8.1 iterations used: when the true beat period is
    // fractional (e.g. 140 BPM @ 48 kHz → P = 40.18 lag), the
    // discrete ODF spike pattern lands most consecutive-beat pairs
    // in bin 40 with a few in bin 41, and analogously bin 80 vs 81
    // for the skip-1 pairs. The *total* energy under each periodic
    // peak is the same (as it should be for a periodic signal), but
    // the distribution across bins differs: bin 40 has a sharp
    // left shoulder (one tall bin, one shorter bin) while bin 80 is
    // more even (two near-equal bins). Parabolic-vertex height of
    // either smoothed or raw ACF depends on this distribution
    // asymmetry — a wider shoulder pulls the vertex up — so it
    // consistently overshoots at 2P versus P. That's the structural
    // bias that earlier iterations papered over with broad
    // tie-tolerance / biased-raw tiebreaks. The 140 BPM @ 48 kHz
    // 10-second click test exposed that paper as too thin: the gap
    // grew with ODF length until it exceeded any reasonable
    // tolerance.
    //
    // Window sum is *invariant* to where the energy sits within the
    // window — it just integrates. With `W = 2` (5-bin window) the
    // worst-case fractional period gives both bins of the bin-split
    // energy plus quiet bins on each side. Adjacent harmonic windows
    // don't overlap as long as the harmonic spacing exceeds `2W +
    // 1 = 5`; at `MAX_HARMONICS = 4` and `lag_min ≈ 29` (200 BPM at
    // our typical ODF rates), the 4th-harmonic windows around `4·lo`
    // and `4·(lo+1)` are 4 lag apart and 5 wide — touching but not
    // overlapping. (For the slowest tempos near `lag_max ≈ 94` only
    // 1–2 harmonics fit anyway.)
    //
    // On clean periodic signals `score(P) = score(2P)` within float
    // epsilon, so the smaller-lag tiebreak fires only on those
    // genuine octave ties without ever swallowing real signal.
    //
    // Cost: 5 ACF lookups per harmonic instead of 1. With
    // `MAX_HARMONICS = 4` and ~67 integer lag candidates in the
    // 60–200 BPM range at our ODF rates, that's `67 · 4 · 5 = 1340`
    // lookups per `recompute()` — well under the M8 1 ms budget.
    const WINDOW: usize = 2;

    let local = |lag: usize| -> f64 {
        if lag > max_lag {
            return 0.0;
        }
        let lo = lag.saturating_sub(WINDOW);
        let hi = (lag + WINDOW).min(max_lag);
        let mut sum = 0.0f64;
        for &v in &acf_raw[lo..=hi] {
            sum += v;
        }
        sum
    };

    let mut best_lo: Option<usize> = None;
    let mut best_score = f64::NEG_INFINITY;

    for lo in lag_min..=lag_max {
        let mut score_sum = 0.0f64;
        let mut k_count = 0usize;
        for k in 1..=MAX_HARMONICS {
            let probe = k * lo;
            if probe > max_lag {
                break;
            }
            score_sum += local(probe);
            k_count += 1;
        }
        if k_count == 0 {
            continue;
        }
        let score = if k_count < MIN_HARMONICS_FOR_MEAN {
            local(lo)
        } else {
            #[allow(clippy::cast_precision_loss)]
            let count_f = k_count as f64;
            score_sum / count_f
        };

        let better = if !best_score.is_finite() {
            true
        } else {
            let tie_window = best_score.abs() * SCORE_TIE_REL_TOL;
            if score > best_score + tie_window {
                true
            } else if (score - best_score).abs() <= tie_window {
                best_lo.is_none_or(|prev| lo < prev)
            } else {
                false
            }
        };
        if better {
            best_score = score;
            best_lo = Some(lo);
        }
    }

    let best_lo = best_lo?;

    // Sub-integer refinement: centroid of the local-energy window
    // on the *raw* ACF around the picked integer lag.
    //
    // The window-sum scoring above is by design invariant to where
    // the energy sits within the window — that's what made it
    // robust against bin-split asymmetry. But that same invariance
    // erases the sub-integer position information we need for
    // continuous BPM output. Without sub-integer refinement, the
    // reported BPM lands on the ODF integer-lag grid, which has
    // ~1.5–3 BPM steps in the 60–200 BPM range — coarse enough to
    // jitter the confidence-tracker hysteresis and to fail the
    // 128 / 174 BPM ± 1 acceptance gates.
    //
    // Centroid recovers the underlying fractional position because
    // it's the energy-weighted mean of bin indices. For a periodic
    // signal at fractional period P that lands `c` consecutive
    // pairs in bin `floor(P)` and `(1-c)` in bin `ceil(P)`, the
    // centroid evaluates to `floor(P) · c + ceil(P) · (1 - c) =
    // P` — the true continuous lag. The window radius `W = 2`
    // captures both flanking bins plus one quiet bin on each side
    // (which contribute nothing to the weighted sum, so the
    // centroid stays valid).
    #[allow(clippy::cast_precision_loss)]
    let best_lag_f = {
        let lo_f = best_lo as f64;
        let lo = best_lo.saturating_sub(WINDOW);
        let hi = (best_lo + WINDOW).min(max_lag);
        let mut weight_sum = 0.0f64;
        let mut moment = 0.0f64;
        for (offset, &w) in acf_raw[lo..=hi].iter().enumerate() {
            if w > 0.0 {
                weight_sum += w;
                moment += (lo + offset) as f64 * w;
            }
        }
        if weight_sum > 0.0 {
            moment / weight_sum
        } else {
            lo_f
        }
    };

    // Confidence uses the raw (unsmoothed) ACF at the picked lag's
    // local maximum, so a clean periodic signal yields confidence
    // near 1.0 regardless of how the smoothing distributed the peak
    // across adjacent bins. Sample ±1 around the picked lag to
    // capture the underlying peak height even when split across bins.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let near = best_lag_f.round() as usize;
    let raw_peak = {
        let lower = if near > 0 { acf_raw[near - 1] } else { 0.0 };
        let mid = acf_raw[near.min(max_lag)];
        let upper = if near < max_lag {
            acf_raw[near + 1]
        } else {
            0.0
        };
        lower.max(mid).max(upper)
    };
    let ratio = raw_peak / acf_zero;
    if ratio < DETECTION_THRESHOLD {
        return None;
    }

    let bpm = 60.0 * odf_sample_rate / best_lag_f;
    #[allow(clippy::cast_possible_truncation)]
    let confidence = ratio.clamp(0.0, 1.0) as f32;

    Some(BpmEstimate { bpm, confidence })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_odf_returns_none() {
        assert!(estimate_tempo(&[], 100.0, BpmRange::DEFAULT).is_none());
    }

    #[test]
    fn flat_odf_returns_none() {
        let odf = vec![0.0f32; 500];
        assert!(estimate_tempo(&odf, 100.0, BpmRange::DEFAULT).is_none());
    }

    #[test]
    fn perfectly_periodic_odf_recovers_period() {
        // Synthetic ODF: a pulse train every 50 samples at odf_sr = 100
        // → period 0.5 s → 120 BPM exactly.
        let mut odf = vec![0.0f32; 1000];
        for i in (0..odf.len()).step_by(50) {
            odf[i] = 1.0;
        }
        let est = estimate_tempo(&odf, 100.0, BpmRange::DEFAULT).expect("should detect");
        assert!(
            (est.bpm - 120.0).abs() < 0.5,
            "expected ~120 BPM, got {}",
            est.bpm
        );
        assert!(est.confidence > 0.5);
    }

    #[test]
    fn period_at_lag_min_boundary_doesnt_panic() {
        // A periodic ODF whose period sits at the search boundary —
        // make sure we handle the "can't take y₋₁ at lag_min" branch
        // without panic. We don't assert an exact BPM here: a pure
        // pulse train at the boundary lag has equally-strong
        // autocorrelation at every multiple of the period (the classic
        // octave ambiguity), and choosing which one is "correct" is
        // an M8+ concern that needs musical context priors.
        let mut odf = vec![0.0f32; 1000];
        for i in (0..odf.len()).step_by(30) {
            odf[i] = 1.0;
        }
        let est =
            estimate_tempo(&odf, 100.0, BpmRange::DEFAULT).expect("should detect *some* tempo");
        assert!(
            est.bpm >= crate::MIN_BPM && est.bpm <= crate::MAX_BPM,
            "tempo out of search range: {}",
            est.bpm
        );
    }

    #[test]
    fn one_spike_no_periodicity_returns_none() {
        // Single spike at the start, then flat — exactly the single-
        // click case in the integration tests. Must return None
        // (confidence 0), not a phantom tempo.
        let mut odf = vec![0.0f32; 1000];
        odf[100] = 1.0;
        assert!(estimate_tempo(&odf, 100.0, BpmRange::DEFAULT).is_none());
    }

    #[test]
    fn narrow_range_constrains_search() {
        // 120 BPM pulse train, but the search range only covers
        // [60, 90]. The estimator must report the half-tempo at 60
        // BPM (the only candidate inside the range), not the true
        // 120 BPM that lies outside it.
        let mut odf = vec![0.0f32; 2000];
        for i in (0..odf.len()).step_by(50) {
            odf[i] = 1.0;
        }
        let narrow = BpmRange::new(60.0, 90.0).unwrap();
        let est = estimate_tempo(&odf, 100.0, narrow).expect("should detect");
        assert!(
            est.bpm >= 60.0 && est.bpm <= 90.0,
            "narrow-range BPM should stay inside [60, 90]; got {}",
            est.bpm
        );
    }
}
