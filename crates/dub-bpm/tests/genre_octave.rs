//! Multi-genre octave-error acceptance tests (M8.1).
//!
//! Pinned in M8.1 to validate the *log-band-weighted spectral flux*
//! ODF. The pre-M8.1 single-bin ODF reliably reports `2 × BPM` on
//! the hip-hop fixture (because hi-hats outnumber kicks 8-to-2 in
//! the linear-bin flux sum) — this file is the regression gate that
//! proves the multi-band fix and prevents future ODF changes from
//! silently re-introducing the octave error.
//!
//! Fixtures live in [`dub_bpm::synthetic`] (see `drum_pattern_*`).
//! They are layered kick + snare + hi-hat patterns at different
//! spectral bands so the multi-band ODF has something representative
//! to weight.
//!
//! # Tolerance
//!
//! ±2 BPM, looser than the `click_track` tests' ±1 BPM. Rationale:
//! the drum-pattern fixtures have richer spectral content, so the
//! ODF peak at the true period has a slightly wider top than the
//! click-train case — parabolic interpolation gets less precision.
//! ±2 still rejects every octave error (½×, 2×, ⅔×, ¾×) for the
//! tempi we test, which is the actual goal.

use dub_bpm::{analyze_bpm, synthetic};

/// Tolerance for drum-pattern fixtures. ±2 BPM is tight enough to
/// reject every octave/triplet error (½×, 2×, ⅔×, ¾×, etc.) in
/// the tempo range we care about, loose enough to accept the
/// natural shoulder-width of the autocorrelation peak on real-ish
/// percussion.
const TOLERANCE_BPM: f64 = 2.0;

/// Long enough to give the autocorrelation function several full
/// beat periods at the slowest tempo we test (65 BPM → 0.92 s/beat
/// → 10 beats in 10 s). Per PRD §5.2.3 the streaming driver locks
/// within ~3–5 s, so 10 s is also a faithful proxy for "the moment
/// the UI displays a locked BPM".
const TEST_DURATION_SECS: f64 = 10.0;

fn assert_within(expected: f64, est: dub_bpm::BpmEstimate, tol: f64, label: &str) {
    let diff = (est.bpm - expected).abs();
    assert!(
        diff <= tol,
        "[{label}] expected {expected:.1} BPM ± {tol:.1}, got {:.3} \
         (confidence = {:.3})",
        est.bpm,
        est.confidence
    );
    assert!(
        est.confidence > 0.0,
        "[{label}] confidence should be > 0 on a clean drum pattern; got {}",
        est.confidence
    );
}

// ============================================================
// The headline test: hip-hop at 100 BPM
// ============================================================
//
// This is the test the user observed failing in production: a
// Diamond D rap track at 100 BPM detected as 200 BPM. Pre-M8.1
// this assertion fails — the single-band ODF picks the hi-hat
// period (off-beats every 0.3 s ≈ 200 BPM) over the kick period
// (0.6 s ≈ 100 BPM). After M8.1's log-band-weighted ODF the kick's
// sub-200 Hz energy gets equal weight as the hi-hat's 4–16 kHz
// energy, and the autocorrelation peak at the true beat period
// wins.

#[test]
fn hip_hop_100_bpm_locks_at_100_not_200() {
    let sr = 48_000u32;
    let audio = synthetic::drum_pattern_hip_hop(100.0, TEST_DURATION_SECS, sr);
    let est = analyze_bpm(&audio, sr, 1).expect("analysis should succeed");
    assert_within(100.0, est, TOLERANCE_BPM, "hip-hop 100 BPM");
}

#[test]
fn hip_hop_90_bpm_locks_at_90_not_180() {
    let sr = 48_000u32;
    let audio = synthetic::drum_pattern_hip_hop(90.0, TEST_DURATION_SECS, sr);
    let est = analyze_bpm(&audio, sr, 1).expect("analysis should succeed");
    assert_within(90.0, est, TOLERANCE_BPM, "hip-hop 90 BPM");
}

// ============================================================
// Inverse-failure check: rolling drum-n-bass at 174 BPM
// ============================================================
//
// The risk with rebalancing the ODF toward low frequencies is over-
// correction: a multi-band ODF that *over*-weights the low end could
// detect bass-heavy dnb at half tempo (87 BPM) because the kick now
// dominates the autocorrelation peak strength at lag 64 (= 1 bar
// boundary).
//
// The fixture here is "rolling dnb" — kick on every beat, hi-hat
// on every 8th, no snare backbeat. It is structurally unambiguous
// at 174 BPM (every beat carries identical content). Real-music dnb
// with a snare-on-2-and-4 backbeat is **not** structurally
// unambiguous — its autocorrelation legitimately peaks higher at
// the 2-beat (87 BPM) period than at the 1-beat (174 BPM) period,
// because every harmonic of lag 64 lands on a same-instrument
// alignment (K-K, S-S) while every other harmonic of lag 32 lands
// on the cross K-S correlation (weaker).
//
// That K-S-backbeat half-tempo problem is the same class as
// dubstep at 140 → 70 BPM detection. The user explicitly
// acknowledged this is an unavoidable property of beat-tracking
// without genre / tempo priors (M9+ scope). Rolling dnb is a real
// sub-genre (jump-up, neuro, some liquid) and is what we use here
// to validate the M8.1 multi-band ODF doesn't introduce *new*
// errors for bass-heavy content at fast tempi.

#[test]
fn drum_n_bass_174_bpm_locks_at_174_not_87() {
    let sr = 48_000u32;
    let audio = synthetic::drum_pattern_drum_n_bass(174.0, TEST_DURATION_SECS, sr);
    let est = analyze_bpm(&audio, sr, 1).expect("analysis should succeed");
    assert_within(174.0, est, TOLERANCE_BPM, "rolling dnb 174 BPM");
}

// ============================================================
// Sparse / slow / low-end-heavy: reggae one-drop at 65 BPM
// ============================================================
//
// The reggae one-drop is the most pathological case we expect to
// see in the wild: a single kick per bar (0.92-s period at 65 BPM,
// effective tempo ≈ 16 BPM if you counted only kicks) with off-beat
// hi-hat skanks providing the periodicity. The ODF has to weight
// the hi-hat skanks at the half-beat period to give the autocorre-
// lation a strong peak — *and* not overweight the kick to the point
// of locking at the bar-period (16 BPM, far below `MIN_BPM`).
//
// The user explicitly mentioned reggae as part of their genre mix
// (reggae often gets played alongside hip-hop and dnb in sets), so
// this is on the acceptance gate, not an aspirational test.

#[test]
fn reggae_one_drop_65_bpm_locks_at_65() {
    let sr = 48_000u32;
    let audio = synthetic::drum_pattern_reggae_one_drop(65.0, TEST_DURATION_SECS, sr);
    let est = analyze_bpm(&audio, sr, 1).expect("analysis should succeed");
    // Reggae's sparseness gives the autocorrelation peak a wider
    // shoulder; allow ±3 here (still rejects ½× = 32.5 and 2× = 130).
    assert_within(65.0, est, 3.0, "reggae one-drop 65 BPM");
}
