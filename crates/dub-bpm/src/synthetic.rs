//! Synthetic audio generators for testing the BPM estimator.
//!
//! These are intentionally minimal — a click track is just a periodic
//! impulse train, optionally with envelope decay. They're deterministic
//! and trivially carry exact ground-truth BPM, which makes them the right
//! first oracle for the algorithm.
//!
//! For real-music validation we'd use M3 format-coverage fixtures or
//! genre-specific test rips, but those are deferred to M8 when the
//! streaming driver needs them.
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
}
