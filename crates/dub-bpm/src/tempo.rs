//! Tempo estimation from an onset detection function.
//!
//! Given the ODF emitted by [`crate::onset::OnsetDetector`], find the
//! tempo by autocorrelating in the lag range corresponding to
//! `[MIN_BPM, MAX_BPM]` and picking the lag whose **harmonic-summed**
//! autocorrelation is largest.
//!
//! ## Why harmonic summation?
//!
//! Pure autocorrelation has equal-magnitude peaks at every integer
//! multiple of the true period (the "octave ambiguity"). For a 128 BPM
//! pulse train, `acf[P]`, `acf[2P]`, `acf[3P]`… are all ≈ equal, and
//! numerical noise picks one almost at random — often 2P, reporting
//! half-tempo. This is the dominant failure mode of naïve
//! autocorrelation tempo trackers.
//!
//! Fix: for each candidate period P, sum `acf[k·P]` for `k = 1, 2, 3,
//! …` up to the end of the ACF. The true period accumulates evidence
//! from *all* its harmonics; 2P only sees the even subset; 3P only the
//! every-third subset. So the true period reliably scores highest.
//! This is the same family of techniques aubio and Mixxx use.
//!
//! Confidence is the *normalized cross-correlation* at the fundamental
//! peak — `acf[P] / acf[0]`. For a perfectly periodic signal this
//! approaches 1.0; for noise it tends toward 0. Below
//! [`DETECTION_THRESHOLD`] we refuse the estimate entirely (returning
//! `None`) so the caller can't confuse "no detection" with "very low
//! confidence detection".

use crate::{BpmEstimate, MAX_BPM, MIN_BPM};

/// Below this normalized-cross-correlation ratio we declare "no
/// detection" rather than reporting a low-confidence estimate. Tuned to
/// pass the single-click / silence honesty tests while not rejecting
/// genuinely weak-but-real beats. Re-evaluate when the streaming driver
/// arrives in M8 and we have real-music data to calibrate against.
const DETECTION_THRESHOLD: f64 = 0.05;

/// Internal helper: unbiased autocorrelation at a single lag.
///
/// Returns `sum(x[i] * x[i + lag]) / (N - lag)` so longer lags aren't
/// penalised by having fewer terms to sum.
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

/// How many harmonics deep to look when summing. `acf` is computed up
/// to `HARMONIC_DEPTH * lag_max`; longer means more octave-error
/// suppression at proportional cost. 4 is enough to clearly separate
/// e.g. 64 vs 128 BPM (8 harmonics for the fast one vs 4 for the slow
/// one across the same ODF length).
const HARMONIC_DEPTH: usize = 4;

/// Estimate tempo from an ODF.
///
/// `odf_sample_rate` is the ODF's own sample rate in Hz — typically
/// `audio_sr / HOP_SIZE`. The function does no audio-rate scaling
/// itself; it operates purely in ODF time.
///
/// Returns `None` when the ODF is too short, contains no energy, or has
/// no peak above [`DETECTION_THRESHOLD`].
pub(crate) fn estimate_tempo(odf: &[f32], odf_sample_rate: f64) -> Option<BpmEstimate> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lag_min = ((60.0 * odf_sample_rate) / MAX_BPM).floor() as usize;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lag_max = ((60.0 * odf_sample_rate) / MIN_BPM).ceil() as usize;
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

    let lag_zero = acf_raw[0];
    if lag_zero < 1e-12 {
        // No energy → no detection. Covers silence and post-detrending
        // flat signals.
        return None;
    }

    // Smooth the ACF with a 3-tap moving average. Rationale: when the
    // true beat period isn't an integer number of ODF samples (e.g.
    // 90 BPM @ 48 kHz has period 62.5 lag), the underlying peak
    // straddles two integer-lag bins. Smoothing makes adjacent bins
    // share energy so parabolic interpolation lands cleanly at the
    // fractional peak, and so the harmonic sum at the fundamental
    // catches the "off-by-1" peaks at the true-period multiples (e.g.
    // 125, 187, 250 for 90 BPM @ 48 kHz). Without this, those tracks
    // either return zero confidence or report the half-tempo lying
    // outside [MIN_BPM, MAX_BPM]'s search range.
    let mut acf = vec![0.0f64; max_lag + 1];
    for (lag, slot) in acf.iter_mut().enumerate() {
        let a = if lag == 0 { 0.0 } else { acf_raw[lag - 1] };
        let b = acf_raw[lag];
        let c = if lag == max_lag {
            0.0
        } else {
            acf_raw[lag + 1]
        };
        *slot = (a + b + c) / 3.0;
    }
    // ACF[0] for confidence stays on the un-smoothed value — smoothing
    // dilutes the at-zero peak by ⅓ and would systematically
    // under-report confidence. The smoothing's purpose is *picker*
    // stability, not energy estimation.
    let acf_zero = lag_zero;

    // Fractional-step harmonic search.
    //
    // Real beat periods rarely land on an integer ODF lag (e.g. 128
    // BPM @ 44.1 kHz has period 40.37 lag), so each harmonic k·P
    // drifts by k·frac from any integer-stepped candidate. At k ≥ 4
    // the drift exceeds the 3-tap smoothing window and harmonics stop
    // contributing — the picker then prefers the half-tempo whose 4
    // harmonics don't drift as far before exiting the search.
    //
    // Fix: search candidate periods at fractional resolution (step
    // 0.25) and linearly interpolate the smoothed ACF at the
    // fractional harmonic positions. This way k·P always hits the
    // true ACF peak for the true period, the harmonic sum at the true
    // period wins cleanly, and we don't need a separate parabolic
    // refinement step.
    //
    // Tie-break still applies: a pure pulse train at integer period P
    // scores identically at L = P, P/2, P/3, … if all sub-periods
    // are in range, so we prefer the lag with the highest fundamental
    // ACF to break the tie.
    const SEARCH_STEP: f64 = 0.25;

    let interp = |x: f64| -> f64 {
        if x < 0.0 {
            return 0.0;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let lo = x.floor() as usize;
        if lo >= max_lag {
            return acf[max_lag];
        }
        #[allow(clippy::cast_precision_loss)]
        let frac = x - lo as f64;
        acf[lo] * (1.0 - frac) + acf[lo + 1] * frac
    };

    #[allow(clippy::cast_precision_loss)]
    let lag_min_f = lag_min as f64;
    #[allow(clippy::cast_precision_loss)]
    let lag_max_f = lag_max as f64;
    #[allow(clippy::cast_precision_loss)]
    let max_lag_f = max_lag as f64;

    let mut best_lag_f = lag_min_f;
    let mut best_score = f64::NEG_INFINITY;
    let mut best_fundamental = f64::NEG_INFINITY;
    let mut lag_f = lag_min_f;
    while lag_f <= lag_max_f {
        let mut score = 0.0f64;
        let mut k = 1usize;
        loop {
            #[allow(clippy::cast_precision_loss)]
            let probe = (k as f64) * lag_f;
            if probe > max_lag_f {
                break;
            }
            score += interp(probe);
            k += 1;
        }
        let fundamental = interp(lag_f);
        let better = score > best_score || (score == best_score && fundamental > best_fundamental);
        if better {
            best_score = score;
            best_fundamental = fundamental;
            best_lag_f = lag_f;
        }
        lag_f += SEARCH_STEP;
    }

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
        assert!(estimate_tempo(&[], 100.0).is_none());
    }

    #[test]
    fn flat_odf_returns_none() {
        let odf = vec![0.0f32; 500];
        assert!(estimate_tempo(&odf, 100.0).is_none());
    }

    #[test]
    fn perfectly_periodic_odf_recovers_period() {
        // Synthetic ODF: a pulse train every 50 samples at odf_sr = 100
        // → period 0.5 s → 120 BPM exactly.
        let mut odf = vec![0.0f32; 1000];
        for i in (0..odf.len()).step_by(50) {
            odf[i] = 1.0;
        }
        let est = estimate_tempo(&odf, 100.0).expect("should detect");
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
        let est = estimate_tempo(&odf, 100.0).expect("should detect *some* tempo");
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
        assert!(estimate_tempo(&odf, 100.0).is_none());
    }
}
