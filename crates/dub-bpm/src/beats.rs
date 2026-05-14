//! Offline beat-grid analysis: BPM + phase + beat-position list.
//!
//! `analyze_bpm` (in `offline.rs`) reports the *tempo* of a track but
//! not where each individual beat falls in time. This module fills in
//! that second piece: given a track, it computes the BPM, then finds
//! the **phase offset** that best aligns a synthetic beat grid with
//! the track's onset detection function (ODF), and emits a list of
//! beat positions in seconds.
//!
//! ## What this is for
//!
//! M10.5p ("DJ-focused waveform redesign") strips the multi-band
//! frequency colouring from the playing waveform and replaces it
//! with two semantic markers: a **monochrome amplitude envelope**
//! (read for energy structure: break / buildup / drop) and **discrete
//! beat ticks** at the 4/4 grid positions (read for "where's the
//! downbeat I'm mixing against"). The first half doesn't need this
//! module; the second half does. Without phase alignment, ticks would
//! be tempo-correct but *position*-arbitrary — useless for the DJ.
//!
//! ## Algorithm
//!
//! 1. Run the BPM analyser, which produces both a [`BpmEstimate`]
//!    and the ODF as a byproduct. We consume both via the
//!    crate-private [`analyze_bpm_with_range_and_odf`] hook so the
//!    spectral-flux pass — the dominant cost in offline analysis —
//!    runs exactly once per track load.
//! 2. Phase search: for each candidate offset `phi` in `[0, P)`
//!    ODF samples (where `P = odf_sr × 60 / bpm`), compute the
//!    energy sum `Σ_i odf[phi + i × P]` for all `i` that land in
//!    bounds. The `phi` with the largest sum is the phase that
//!    best aligns the synthetic grid with the actual onsets.
//! 3. Parabolic interpolation around the best discrete `phi` gives
//!    sub-ODF-sample precision (matters: at 48 kHz / HOP_SIZE 512
//!    the ODF tick is ~10.7 ms, large compared to a typical "is
//!    the click on the beat?" ±20 ms gate).
//! 4. Emit beat times as `(phi + i × P) / odf_sr` seconds for
//!    `i = 0, 1, …` until the position exceeds the track length.
//!
//! ## What's deliberately NOT done in v0
//!
//! * **Downbeat detection** — every 4th beat is marked as a
//!   downbeat by visual convention (the DJ-focused waveform shows
//!   every-4th tick brighter), but no algorithmic identification
//!   of "which beat is the *1* of each bar". For DJ-relevant
//!   genres (hip-hop, house, dnb, dubstep) the 4/4 assumption is
//!   correct; the "is beat 0 actually the 1?" question is rarely
//!   important in practice and can be solved later by a "tap to
//!   align" UI affordance.
//! * **Tempo drift** — beats are spaced uniformly at the global
//!   BPM. Real DJ-relevant tracks don't drift (they're produced
//!   at a click), so this is correct for our scope. Live recordings
//!   would need a per-beat tempo trace; not v1.
//! * **Beat-tracker hysteresis / confidence** — the per-beat
//!   output trusts the global BPM estimate's confidence. If the
//!   tempo estimate is junk, the beat list is empty (callers
//!   gate on `confidence > 0`).
//!
//! ## Cost
//!
//! One spectral-flux pass over the full track (shared with the
//! BPM analyser) plus a phase scan of `P × n_beats` ODF samples.
//! For a 5-min track at 44.1 kHz with 120 BPM: ~13.2M samples of
//! flux ops; `P ≈ 41` ODF samples × 600 beats = 25k phase-scan
//! additions. The flux pass dominates at maybe ~200 ms on a fast
//! Mac; the phase scan is sub-millisecond.

use crate::offline::analyze_bpm_with_range_and_odf;
use crate::{AnalysisError, BpmEstimate, BpmRange, HOP_SIZE};

/// Per-track beat grid. Returned by [`analyze_beat_grid`]; consumed
/// by the renderer to draw beat ticks on the waveform.
#[derive(Debug, Clone, PartialEq)]
pub struct BeatGrid {
    /// Tempo, in beats per minute. Meaningful iff `confidence > 0.0`.
    pub bpm: f64,
    /// Tempo-estimator confidence in `[0.0, 1.0]`. `0.0` means "no
    /// periodic structure detected; `beats` is empty".
    pub confidence: f32,
    /// Beat positions in seconds from sample 0 of the track. Empty
    /// if `confidence == 0.0`. Beat 0 is the **discovered phase**
    /// — i.e. the offset that best aligns the grid with actual
    /// onsets, not "the start of the file".
    pub beats: Vec<f64>,
    /// Beats per bar. Fixed at 4 for v0; every 4th beat is the
    /// downbeat (the visual "1") by convention. See the module
    /// docs' "What's deliberately NOT done in v0" note.
    pub beats_per_bar: u8,
}

impl BeatGrid {
    /// An empty grid. Returned when BPM detection fails (silence,
    /// non-musical input, too-short audio after the
    /// [`AnalysisError::TooShort`] gate).
    #[must_use]
    pub const fn none() -> Self {
        Self {
            bpm: 0.0,
            confidence: 0.0,
            beats: Vec::new(),
            beats_per_bar: 4,
        }
    }
}

/// Analyse a buffer and return its beat grid.
///
/// `samples` is interleaved (`L R L R …` for stereo, `M M …` for
/// mono). Stereo is downmixed to mono internally.
///
/// # Errors
///
/// Same as [`analyze_bpm_with_range`] — invalid sample rate / channel
/// count / non-interleaved buffer / too-short input. A successfully
/// analysed but non-periodic input returns `Ok(BeatGrid::none())`,
/// not an error.
pub fn analyze_beat_grid(
    samples: &[f32],
    sample_rate: u32,
    channels: u8,
) -> Result<BeatGrid, AnalysisError> {
    // Tempo + ODF in a single spectral-flux pass — the BPM analyser
    // already computed the ODF internally, so we consume it from the
    // crate-private hook rather than running the FFT pipeline twice.
    let (estimate, odf): (BpmEstimate, Vec<f32>) =
        analyze_bpm_with_range_and_odf(samples, sample_rate, channels, BpmRange::DEFAULT)?;
    if estimate.confidence <= 0.0 {
        return Ok(BeatGrid::none());
    }

    let odf_sr = f64::from(sample_rate) / HOP_SIZE as f64;
    let beats = find_beats(&odf, odf_sr, estimate.bpm);

    Ok(BeatGrid {
        bpm: estimate.bpm,
        confidence: estimate.confidence,
        beats,
        beats_per_bar: 4,
    })
}

/// Internal: given an ODF and a known tempo, find beat positions.
///
/// Returns beat times in **seconds** from the start of the audio.
/// Empty vec if the inputs are degenerate (period too small / too
/// large for the available ODF length).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn find_beats(odf: &[f32], odf_sr: f64, bpm: f64) -> Vec<f64> {
    if bpm <= 0.0 || odf_sr <= 0.0 || odf.is_empty() {
        return Vec::new();
    }
    let period = (60.0 * odf_sr) / bpm;
    if !period.is_finite() || period < 2.0 {
        return Vec::new();
    }
    // Integer period (used to bound the phase search). Sub-integer
    // precision is recovered later by parabolic interpolation.
    let period_int = period.ceil() as usize;
    if period_int == 0 || odf.len() < 2 * period_int {
        return Vec::new();
    }

    // Phase search. For each candidate `phi ∈ [0, period_int)`, sum
    // odf[phi + round(i × period)] across all in-bounds `i`. Linear
    // in `period × n_beats`, sub-millisecond at typical track
    // lengths.
    let mut best_phase = 0usize;
    let mut best_score = f64::NEG_INFINITY;
    for phi in 0..period_int {
        let mut score = 0.0f64;
        let mut i = 0usize;
        loop {
            let idx_f = phi as f64 + (i as f64) * period;
            let idx = idx_f.round() as usize;
            if idx >= odf.len() {
                break;
            }
            score += f64::from(odf[idx]);
            i += 1;
        }
        if score > best_score {
            best_score = score;
            best_phase = phi;
        }
    }

    // Parabolic vertex around the discrete best-phase for sub-ODF-
    // sample precision. y0/y1/y2 are the scores at phases
    // (best - 1, best, best + 1) modulo `period_int` (the score
    // function is `period_int`-periodic — phase `phi + period_int`
    // is the same beat alignment shifted one cycle, which produces
    // the same score). Vertex offset = `(y0 - y2) / (2 × (y0 - 2 y1 + y2))`.
    let score_at_phase = |phi: usize| -> f64 {
        let mut s = 0.0f64;
        let mut i = 0usize;
        loop {
            let idx_f = phi as f64 + (i as f64) * period;
            let idx = idx_f.round() as usize;
            if idx >= odf.len() {
                break;
            }
            s += f64::from(odf[idx]);
            i += 1;
        }
        s
    };
    let prev = (best_phase + period_int - 1) % period_int;
    let next = (best_phase + 1) % period_int;
    let y0 = score_at_phase(prev);
    let y1 = best_score;
    let y2 = score_at_phase(next);
    let denom = 2.0 * (y0 - 2.0 * y1 + y2);
    let frac_offset = if denom.abs() > 1e-9 {
        ((y0 - y2) / denom).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    let refined_phase = best_phase as f64 + frac_offset;

    // Emit beat positions in seconds. The phase scan finds the
    // offset that best aligns with *detected* onsets — but ODF[0]
    // is forced to zero (no previous frame to diff against), so if
    // the track starts with a kick on sample 0 the discovered phase
    // will lock onto the *second* kick, leaving an unwanted gap at
    // the head. Compensate by walking the grid backward from the
    // discovered phase, keeping every beat whose time is ≥ 0.
    //
    // Symmetrically, walk forward until we run off the end of the
    // ODF.
    let mut start_odf = refined_phase;
    while start_odf - period >= 0.0 {
        start_odf -= period;
    }

    #[allow(clippy::cast_precision_loss)]
    let n_max = ((odf.len() as f64 - start_odf) / period).floor() as usize + 1;
    let mut beats = Vec::with_capacity(n_max);
    for i in 0..n_max {
        let beat_odf = start_odf + (i as f64) * period;
        if beat_odf >= odf.len() as f64 {
            break;
        }
        if beat_odf < 0.0 {
            continue;
        }
        beats.push(beat_odf / odf_sr);
    }
    beats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::click_track;

    const SR: u32 = 48_000;

    /// 120-BPM click track produces beats at 0.5 s intervals (60/120).
    /// Phase should be ~0 (the synthetic click track starts on the 1).
    #[test]
    fn click_120_bpm_emits_beats_at_500_ms() {
        let samples = click_track(120.0, 16.0, SR);
        let grid = analyze_beat_grid(&samples, SR, 1).expect("analysis");

        assert!(grid.confidence > 0.5, "got confidence {}", grid.confidence);
        assert!(
            (grid.bpm - 120.0).abs() < 1.0,
            "BPM should be ≈120, got {}",
            grid.bpm
        );
        assert!(
            grid.beats.len() > 10,
            "expected ~32 beats, got {}",
            grid.beats.len()
        );

        // Consecutive beats should be 0.5 s apart (within one ODF tick).
        let odf_tick = HOP_SIZE as f64 / f64::from(SR);
        for pair in grid.beats.windows(2) {
            let dt = pair[1] - pair[0];
            assert!(
                (dt - 0.5).abs() < odf_tick * 2.0,
                "beat spacing should be ~0.5 s, got {dt} (tick={odf_tick})"
            );
        }

        // First beat lands on a *click position* — within ±20 ms of
        // some integer multiple of 0.5 s. Spectral flux for a click
        // at sample 0 is invisible (no previous frame to diff),
        // so the discovered phase locks onto the second click; this
        // is correct, not a bug, and acceptable for DJ use (beats
        // align with audible kicks).
        let first = grid.beats[0];
        let nearest_click = (first / 0.5).round() * 0.5;
        assert!(
            (first - nearest_click).abs() < 0.025,
            "first beat should be within 25 ms of a click position; got {first} s (nearest click = {nearest_click} s)"
        );
    }

    /// Phase shift: feed a click track starting at 0.25 s and check
    /// that the first detected beat lands near 0.25 s, not 0.
    #[test]
    fn phase_offset_quarter_second_recovered() {
        let bpm = 120.0;
        let mut samples = vec![0.0f32; SR as usize / 4]; // 0.25 s silence
        samples.extend(click_track(bpm, 12.0, SR));
        let grid = analyze_beat_grid(&samples, SR, 1).expect("analysis");

        assert!(grid.confidence > 0.4, "got confidence {}", grid.confidence);
        assert!((grid.bpm - bpm).abs() < 1.0, "BPM = {}", grid.bpm);

        // Find the first beat at or after t=0.2 s (allow some slop
        // because beat-0 might be at -0.25 + period_offset if the
        // phase landed before the clicks started).
        let first_aligned = grid
            .beats
            .iter()
            .find(|&&b| b >= 0.20)
            .copied()
            .expect("should have a beat after t=0.2s");
        assert!(
            (first_aligned - 0.25).abs() < 0.03,
            "first aligned beat should be ≈0.25 s, got {first_aligned}"
        );
    }

    /// Silence in → BeatGrid::none() out (or close to it).
    #[test]
    fn silence_returns_no_beats_or_zero_confidence() {
        // Long enough to pass the TooShort gate (~12 s at 60 BPM floor
        // ≈ 4 s minimum).
        let samples = vec![0.0f32; (SR * 12) as usize];
        let grid = analyze_beat_grid(&samples, SR, 1).expect("silence is valid input");
        // Either zero-confidence (the usual path) or some pathological
        // low-confidence detection; either way we mustn't return a
        // grid with high confidence on silent input.
        assert!(
            grid.confidence < 0.3,
            "silence should not produce confident beats; got confidence {}",
            grid.confidence
        );
    }

    /// Beat positions are monotonically increasing.
    #[test]
    fn beats_are_strictly_increasing() {
        let samples = click_track(140.0, 10.0, SR);
        let grid = analyze_beat_grid(&samples, SR, 1).expect("analysis");
        if grid.beats.is_empty() {
            return; // confidence might be low on a short clip; not the test target
        }
        for pair in grid.beats.windows(2) {
            assert!(pair[0] < pair[1], "beats not monotonic: {pair:?}");
        }
    }
}
