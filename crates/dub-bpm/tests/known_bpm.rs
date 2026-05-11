//! Ground-truth tests: synthetic click tracks at known tempi must be
//! detected within tight tolerance.
//!
//! These are the load-bearing M7.5 acceptance tests. If the algorithm
//! ever regresses on synthetic clicks — the cleanest possible BPM signal
//! — it's broken, full stop. Real-music robustness is a separate concern
//! (deferred to M8 with the streaming driver and richer fixtures).

use dub_bpm::{analyze_bpm, synthetic};

/// Tolerance for synthetic clicks: ±1 BPM.
///
/// At our ODF sample rate (≈ 94 Hz at 48 kHz) integer-lag resolution
/// near 174 BPM is ~5.3 BPM per step; parabolic interpolation reduces
/// this an order of magnitude but cannot perfectly fit a peak whose
/// underlying shape isn't truly parabolic. ±1 BPM is the honest
/// tolerance for the M7.5 algorithm and matches the M8 acceptance
/// target for real music (PRD §5.2.3, "median ±1 BPM").
const SYNTHETIC_TOLERANCE_BPM: f64 = 1.0;

/// Duration we synthesize: 10 s gives ≈ 940 ODF samples, more than enough
/// for stable autocorrelation peaks down to 60 BPM (1 s period → 94 ODF
/// samples).
const TEST_DURATION_SECS: f64 = 10.0;

fn assert_close(expected: f64, est: dub_bpm::BpmEstimate, tol: f64) {
    let diff = (est.bpm - expected).abs();
    assert!(
        diff <= tol,
        "expected {expected:.2} BPM ± {tol:.2}, got {:.3} (confidence = {:.3})",
        est.bpm,
        est.confidence
    );
    assert!(
        est.confidence > 0.0,
        "confidence should be > 0 for clean click track; got {}",
        est.confidence
    );
}

#[test]
fn click_track_120_bpm() {
    let sr = 48_000u32;
    let audio = synthetic::click_track(120.0, TEST_DURATION_SECS, sr);
    let est = analyze_bpm(&audio, sr, 1).expect("analysis should succeed");
    assert_close(120.0, est, SYNTHETIC_TOLERANCE_BPM);
}

#[test]
fn click_track_60_bpm() {
    let sr = 48_000u32;
    let audio = synthetic::click_track(60.0, TEST_DURATION_SECS, sr);
    let est = analyze_bpm(&audio, sr, 1).expect("analysis should succeed");
    assert_close(60.0, est, SYNTHETIC_TOLERANCE_BPM);
}

#[test]
fn click_track_90_bpm() {
    let sr = 48_000u32;
    let audio = synthetic::click_track(90.0, TEST_DURATION_SECS, sr);
    let est = analyze_bpm(&audio, sr, 1).expect("analysis should succeed");
    assert_close(90.0, est, SYNTHETIC_TOLERANCE_BPM);
}

#[test]
fn click_track_140_bpm() {
    let sr = 48_000u32;
    let audio = synthetic::click_track(140.0, TEST_DURATION_SECS, sr);
    let est = analyze_bpm(&audio, sr, 1).expect("analysis should succeed");
    assert_close(140.0, est, SYNTHETIC_TOLERANCE_BPM);
}

#[test]
fn click_track_174_bpm_dnb() {
    let sr = 48_000u32;
    let audio = synthetic::click_track(174.0, TEST_DURATION_SECS, sr);
    let est = analyze_bpm(&audio, sr, 1).expect("analysis should succeed");
    assert_close(174.0, est, SYNTHETIC_TOLERANCE_BPM);
}

#[test]
fn click_track_works_at_44100_hz() {
    // CD-quality sample rate path. Important: tracks loaded via dub-io
    // keep their native sample rate, and a lot of dance music is mastered
    // at 44.1 kHz.
    let sr = 44_100u32;
    let audio = synthetic::click_track(128.0, TEST_DURATION_SECS, sr);
    let est = analyze_bpm(&audio, sr, 1).expect("analysis should succeed");
    assert_close(128.0, est, SYNTHETIC_TOLERANCE_BPM);
}

#[test]
fn silence_has_zero_confidence() {
    let sr = 48_000u32;
    let audio = synthetic::silence(TEST_DURATION_SECS, sr);
    let est = analyze_bpm(&audio, sr, 1).expect("silence analysis should not error");
    assert_eq!(
        est.confidence, 0.0,
        "silence must have confidence == 0; got {est:?}"
    );
}

#[test]
fn single_click_has_zero_confidence() {
    // A single transient is not periodic; estimator must honestly say
    // "I don't know" rather than picking the gap-between-click-and-end
    // as a phantom tempo.
    let sr = 48_000u32;
    let audio = synthetic::single_click(TEST_DURATION_SECS, sr);
    let est = analyze_bpm(&audio, sr, 1).expect("single-click analysis should not error");
    assert_eq!(
        est.confidence, 0.0,
        "single click must have confidence == 0; got {est:?}"
    );
}

#[test]
fn too_short_input_is_an_error() {
    // 100 ms at 48 kHz = 4800 samples = ~9 ODF frames. Not enough for
    // any meaningful autocorrelation in the 60-200 BPM range. Estimator
    // should refuse rather than fabricate.
    let sr = 48_000u32;
    let audio = vec![0.0f32; 4_800];
    let result = analyze_bpm(&audio, sr, 1);
    assert!(
        matches!(result, Err(dub_bpm::AnalysisError::TooShort { .. })),
        "expected TooShort error, got {result:?}"
    );
}

#[test]
fn stereo_input_is_downmixed_and_still_detected() {
    // Same click track on both channels; downmix must produce equivalent
    // mono and detect the same tempo.
    let sr = 48_000u32;
    let mono = synthetic::click_track(125.0, TEST_DURATION_SECS, sr);
    let mut stereo = Vec::with_capacity(mono.len() * 2);
    for &s in &mono {
        stereo.push(s);
        stereo.push(s);
    }
    let est = analyze_bpm(&stereo, sr, 2).expect("stereo analysis should succeed");
    assert_close(125.0, est, SYNTHETIC_TOLERANCE_BPM);
}

#[test]
fn streaming_estimator_converges_to_offline_result() {
    // Feed the same click track block-by-block. After enough audio, the
    // streaming estimator's current() should agree with analyze_bpm
    // within tolerance. This is the cross-check that validates the M8
    // streaming driver against the M7.5 offline oracle — they share
    // internals, so any divergence is a bug.
    let sr = 48_000u32;
    let audio = synthetic::click_track(128.0, TEST_DURATION_SECS, sr);

    let mut estimator = dub_bpm::BpmEstimator::new(sr).expect("estimator should construct");
    for chunk in audio.chunks(1024) {
        estimator.process(chunk);
    }

    let est = estimator
        .current()
        .expect("estimator should have produced an estimate");
    assert_close(128.0, est, SYNTHETIC_TOLERANCE_BPM);
}

#[test]
fn streaming_reset_clears_state() {
    let sr = 48_000u32;
    let audio = synthetic::click_track(120.0, TEST_DURATION_SECS, sr);

    let mut estimator = dub_bpm::BpmEstimator::new(sr).expect("estimator should construct");
    for chunk in audio.chunks(1024) {
        estimator.process(chunk);
    }
    assert!(estimator.current().is_some());

    estimator.reset();
    assert!(
        estimator.current().is_none(),
        "after reset, current() must be None"
    );
}
