//! Streaming BPM tracker — the M8 building block.
//!
//! Composes the M7.5 [`BpmEstimator`] with the [`ConfidenceTracker`]
//! hysteresis state machine and adds streaming-specific concerns:
//!
//! * **Stereo input.** Tracks built with `channels = 2` downmix
//!   interleaved L/R to mono inside [`process`]. The estimator
//!   itself is mono-only (PRD §5.2.3).
//! * **Throttled tempo search.** The expensive part of the algorithm
//!   (autocorrelation over the whole ODF) runs once per
//!   [`TrackerConfig::analysis_period_samples`] — every ~1 s of
//!   audio by default, not every audio block. Onset detection still
//!   runs every block so the ODF stays up to date.
//! * **State-machine cadence matches search cadence.** The
//!   `ConfidenceTracker` is driven once per recompute, so its
//!   `LOCK_CONSECUTIVE` etc. counters are measured in *analysis
//!   updates* (~1 per second), not audio blocks. This is what lets
//!   `LOCK_CONSECUTIVE = 3` translate to "≈ 3 s of agreement before
//!   lock" rather than the meaningless "≈ 15 ms of agreement" we'd
//!   get if we drove the state machine on every audio block.
//!
//! ## Threading model
//!
//! `BpmTracker` is pure and single-threaded. It owns no audio source
//! and spawns no threads. The M8 streaming driver
//! ([`crate::stream`]) wraps it in an OS thread that reads audio
//! from a ring buffer, calls [`process`] in a loop, and forwards any
//! emitted [`TrackerEvent`]s to the UI.
//!
//! ## Honesty contract (inherited)
//!
//! Silence, isolated transients, and very-low-confidence input all
//! emit `BpmEstimate::NONE` from the estimator, which keeps the
//! tracker in `TrackerState::Searching`. Drive `process` on every
//! block regardless — the state machine needs to see signal absence
//! to decay out of `Locked` correctly.
//!
//! [`process`]: BpmTracker::process

use crate::confidence::{ConfidenceTracker, TrackerEvent, TrackerState};
use crate::estimator::{BpmEstimator, BpmEstimatorError};
use crate::{BpmEstimate, BpmRange};

/// Construction-time configuration for [`BpmTracker`].
///
/// All defaults are tuned for the typical "DJ Thru deck on a 48 kHz
/// interface" case; deviations should be deliberate.
#[derive(Debug, Clone, Copy)]
pub struct TrackerConfig {
    /// Audio sample rate. Must match the source the tracker is being
    /// fed from. Used to size the underlying [`BpmEstimator`].
    pub sample_rate: u32,
    /// Number of channels in the input blocks passed to [`process`].
    /// `1` (mono) or `2` (interleaved stereo).
    ///
    /// [`process`]: BpmTracker::process
    pub channels: u8,
    /// How often to run the expensive tempo-search step + step the
    /// confidence state machine, measured in audio samples consumed.
    /// Defaults to `sample_rate / 1` (= once per second).
    ///
    /// Smaller values lock faster but burn more CPU and over-drive
    /// the hysteresis counters (which are tuned in *recompute
    /// units*). Larger values save CPU at the cost of slower lock.
    pub analysis_period_samples: u32,
    /// Inclusive BPM range to search within. Defaults to
    /// [`BpmRange::DEFAULT`] (60–200 BPM).
    pub bpm_range: BpmRange,
}

impl TrackerConfig {
    /// Sensible default config at the given sample rate: stereo
    /// input, recompute once per second, full 60–200 BPM range.
    #[must_use]
    pub fn at(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            channels: 2,
            analysis_period_samples: sample_rate.max(1),
            bpm_range: BpmRange::DEFAULT,
        }
    }
}

/// Errors from [`BpmTracker::new`]. Forwards
/// [`BpmEstimatorError`] for sample-rate validation plus
/// tracker-specific checks.
#[derive(Debug, thiserror::Error)]
pub enum TrackerError {
    /// Underlying estimator rejected its configuration.
    #[error(transparent)]
    Estimator(#[from] BpmEstimatorError),
    /// Channel count out of range. Must be 1 or 2.
    #[error("invalid channels: {0}; must be 1 (mono) or 2 (stereo)")]
    InvalidChannels(u8),
    /// `analysis_period_samples == 0` would mean "recompute every
    /// sample" — pointless, and would degenerate the hysteresis
    /// thresholds to no-ops.
    #[error("analysis_period_samples must be > 0")]
    ZeroAnalysisPeriod,
}

/// Streaming BPM tracker. Construct once per audio source, feed
/// audio with [`process`], read state with [`state`], handle
/// transitions via the [`TrackerEvent`] return value.
///
/// Not `Debug` because the underlying [`BpmEstimator`] holds an FFT
/// plan trait object that does not implement `Debug`. Diagnostic
/// output should go through [`state`] and [`last_estimate`]
/// directly, which are both pretty-printable in their own right.
///
/// [`process`]: Self::process
/// [`state`]: Self::state
/// [`last_estimate`]: Self::last_estimate
pub struct BpmTracker {
    estimator: BpmEstimator,
    confidence: ConfidenceTracker,
    channels: u8,
    analysis_period_samples: u32,
    samples_since_recompute: u32,
    mono_scratch: Vec<f32>,
}

impl BpmTracker {
    /// Construct a tracker from the given config.
    ///
    /// # Errors
    ///
    /// See [`TrackerError`] for the failure cases.
    pub fn new(cfg: TrackerConfig) -> Result<Self, TrackerError> {
        if !(1..=2).contains(&cfg.channels) {
            return Err(TrackerError::InvalidChannels(cfg.channels));
        }
        if cfg.analysis_period_samples == 0 {
            return Err(TrackerError::ZeroAnalysisPeriod);
        }
        let estimator = BpmEstimator::with_range(cfg.sample_rate, cfg.bpm_range)?;
        Ok(Self {
            estimator,
            confidence: ConfidenceTracker::new(),
            channels: cfg.channels,
            analysis_period_samples: cfg.analysis_period_samples,
            samples_since_recompute: 0,
            mono_scratch: Vec::new(),
        })
    }

    /// Current externally-visible state. Always present; never panics.
    #[must_use]
    pub fn state(&self) -> TrackerState {
        self.confidence.state()
    }

    /// Most recent raw estimator output (post-honesty-threshold) if
    /// one has been produced, else `None`. Mostly diagnostic — UI
    /// surfaces should consume [`state`] and the [`TrackerEvent`]
    /// stream from [`process`], not poll this.
    ///
    /// [`state`]: Self::state
    /// [`process`]: Self::process
    #[must_use]
    pub fn last_estimate(&self) -> Option<BpmEstimate> {
        self.estimator.current()
    }

    /// Wipe to fresh state. Equivalent to constructing a new tracker
    /// with the same config but cheaper (no FFT re-plan).
    pub fn reset(&mut self) {
        self.estimator.reset();
        self.confidence.reset();
        self.samples_since_recompute = 0;
        // Keep `mono_scratch` capacity — it'll be reused on the next
        // process call. Just shrink-to-empty so stale data doesn't
        // leak across resets.
        self.mono_scratch.clear();
    }

    /// Feed a block of audio. Returns `Some(TrackerEvent)` when the
    /// hysteresis state machine transitioned this block; `None`
    /// otherwise.
    ///
    /// Block size is arbitrary. For mono tracks the block is
    /// `&[mono samples]`; for stereo it's interleaved `&[L, R, L,
    /// R, …]` and the length must be even.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `self.channels == 2` and
    /// `block.len()` is odd (a malformed interleaved layout). Release
    /// builds tolerate the misalignment by trimming the last sample;
    /// the panic is for catching wiring bugs early.
    pub fn process(&mut self, block: &[f32]) -> Option<TrackerEvent> {
        if block.is_empty() {
            return None;
        }

        let mono_slice: &[f32] = if self.channels == 1 {
            block
        } else {
            debug_assert!(
                block.len().is_multiple_of(2),
                "stereo block length must be even, got {}",
                block.len()
            );
            let frames = block.len() / 2;
            // Grow without reallocating once the steady-state block
            // size is reached. Pre-allocation inside the streaming
            // thread is fine — this is off-RT — but resizing inside a
            // hot loop is still wasteful.
            if self.mono_scratch.capacity() < frames {
                self.mono_scratch
                    .reserve(frames - self.mono_scratch.capacity());
            }
            self.mono_scratch.clear();
            for frame in block.chunks_exact(2) {
                self.mono_scratch.push(0.5 * (frame[0] + frame[1]));
            }
            &self.mono_scratch
        };

        self.estimator.feed(mono_slice);

        // Sample-accurate cadence counter. We measure "samples
        // consumed" in mono samples, which means a stereo tracker
        // sees half the cadence rate per byte-wise audio block —
        // but the *mono samples / second* rate equals the source
        // sample rate either way, so the period in real time stays
        // identical. This is the user-facing invariant: cadence is
        // measured in seconds, not in stereo frames or stereo
        // samples.
        let consumed = u32::try_from(mono_slice.len()).unwrap_or(u32::MAX);
        self.samples_since_recompute = self.samples_since_recompute.saturating_add(consumed);

        if self.samples_since_recompute < self.analysis_period_samples {
            return None;
        }
        self.samples_since_recompute = 0;

        self.estimator.recompute();
        let estimate = self.estimator.current().unwrap_or(BpmEstimate::NONE);
        self.confidence.update(estimate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic;

    fn cfg_mono(sr: u32) -> TrackerConfig {
        TrackerConfig {
            sample_rate: sr,
            channels: 1,
            analysis_period_samples: sr,
            bpm_range: BpmRange::DEFAULT,
        }
    }

    #[test]
    fn new_rejects_invalid_channels() {
        let mut bad = TrackerConfig::at(48_000);
        bad.channels = 3;
        assert!(matches!(
            BpmTracker::new(bad),
            Err(TrackerError::InvalidChannels(3))
        ));
    }

    #[test]
    fn new_rejects_zero_analysis_period() {
        let mut bad = TrackerConfig::at(48_000);
        bad.analysis_period_samples = 0;
        assert!(matches!(
            BpmTracker::new(bad),
            Err(TrackerError::ZeroAnalysisPeriod)
        ));
    }

    #[test]
    fn new_propagates_estimator_errors() {
        let bad = TrackerConfig {
            sample_rate: 0,
            channels: 1,
            analysis_period_samples: 1,
            bpm_range: BpmRange::DEFAULT,
        };
        assert!(matches!(
            BpmTracker::new(bad),
            Err(TrackerError::Estimator(BpmEstimatorError::ZeroSampleRate))
        ));
    }

    #[test]
    fn fresh_tracker_in_searching() {
        let t = BpmTracker::new(cfg_mono(48_000)).unwrap();
        assert_eq!(t.state(), TrackerState::Searching);
        assert!(t.last_estimate().is_none());
    }

    #[test]
    fn empty_block_is_noop() {
        let mut t = BpmTracker::new(cfg_mono(48_000)).unwrap();
        let ev = t.process(&[]);
        assert!(ev.is_none());
        assert_eq!(t.state(), TrackerState::Searching);
    }

    #[test]
    fn silence_stays_in_searching() {
        // 5 seconds of silence at 48 kHz with a 1 s recompute cadence
        // → 5 recompute opportunities, all should leave us in
        // Searching (the estimator returns NONE on silence; the
        // tracker's `lost_streak` increments but doesn't transition
        // out of Searching since we're already there).
        let mut t = BpmTracker::new(cfg_mono(48_000)).unwrap();
        let silence = vec![0.0f32; 48_000 * 5];
        for chunk in silence.chunks(2048) {
            let _ = t.process(chunk);
        }
        assert_eq!(t.state(), TrackerState::Searching);
    }

    #[test]
    fn click_track_eventually_locks() {
        // 8 seconds of 128 BPM click track at 48 kHz with a 1 s
        // recompute cadence:
        //   - First ~2 s: not enough ODF for any estimate at all.
        //   - Updates 2–5: estimates appear, state machine builds
        //     streak.
        //   - By 8 s we should be Locked at ~128 BPM.
        let sr = 48_000u32;
        let audio = synthetic::click_track(128.0, 8.0, sr);
        let mut t = BpmTracker::new(cfg_mono(sr)).unwrap();
        for chunk in audio.chunks(2048) {
            t.process(chunk);
        }
        match t.state() {
            TrackerState::Locked { bpm } => {
                assert!(
                    (bpm - 128.0).abs() <= 1.0,
                    "locked BPM should be 128±1, got {bpm}"
                );
            }
            other => panic!("expected Locked after 8 s of clicks, got {other:?}"),
        }
    }

    #[test]
    fn stereo_input_downmixed_and_locks() {
        // Same click track, duplicated to L+R. Tracker is constructed
        // for 2-channel input; downmix path must reach the same
        // conclusion as the mono path.
        let sr = 48_000u32;
        let mono = synthetic::click_track(140.0, 8.0, sr);
        let mut stereo = Vec::with_capacity(mono.len() * 2);
        for &s in &mono {
            stereo.push(s);
            stereo.push(s);
        }

        let cfg = TrackerConfig {
            sample_rate: sr,
            channels: 2,
            analysis_period_samples: sr,
            bpm_range: BpmRange::DEFAULT,
        };
        let mut t = BpmTracker::new(cfg).unwrap();
        for chunk in stereo.chunks(4096) {
            t.process(chunk);
        }
        match t.state() {
            TrackerState::Locked { bpm } => {
                assert!(
                    (bpm - 140.0).abs() <= 1.0,
                    "locked BPM should be 140±1 on stereo input, got {bpm}"
                );
            }
            other => panic!("expected Locked, got {other:?}"),
        }
    }

    #[test]
    fn reset_returns_to_searching() {
        let sr = 48_000u32;
        let audio = synthetic::click_track(120.0, 8.0, sr);
        let mut t = BpmTracker::new(cfg_mono(sr)).unwrap();
        for chunk in audio.chunks(2048) {
            t.process(chunk);
        }
        assert!(t.state().is_locked() || matches!(t.state(), TrackerState::Tentative { .. }));
        t.reset();
        assert_eq!(t.state(), TrackerState::Searching);
        assert!(t.last_estimate().is_none());
    }

    #[test]
    fn transitions_yield_an_event() {
        // The first time the state goes Searching → Tentative we
        // should see a StateChanged event. We can't guarantee which
        // process() call emits it (depends on block size + cadence)
        // but we can collect events and assert at least one
        // transition occurred.
        let sr = 48_000u32;
        let audio = synthetic::click_track(120.0, 8.0, sr);
        let mut t = BpmTracker::new(cfg_mono(sr)).unwrap();

        let mut transitions = Vec::new();
        for chunk in audio.chunks(2048) {
            if let Some(ev) = t.process(chunk) {
                transitions.push(ev);
            }
        }
        // Should be at least Searching → Tentative; usually also
        // Tentative → Locked.
        assert!(
            !transitions.is_empty(),
            "expected at least 1 transition, got {transitions:?}"
        );
        // First emitted event must be a state change *out of*
        // Searching — that's the user-visible "we found something"
        // moment.
        match transitions[0] {
            TrackerEvent::StateChanged(s) => {
                assert_ne!(
                    s,
                    TrackerState::Searching,
                    "first transition must leave Searching"
                );
            }
        }
    }

    #[test]
    fn analysis_period_zero_rejected_at_construction() {
        let cfg = TrackerConfig {
            sample_rate: 48_000,
            channels: 1,
            analysis_period_samples: 0,
            bpm_range: BpmRange::DEFAULT,
        };
        assert!(matches!(
            BpmTracker::new(cfg),
            Err(TrackerError::ZeroAnalysisPeriod)
        ));
    }

    #[test]
    fn faster_analysis_period_does_not_break_correctness() {
        // 250 ms recompute cadence — 4× more frequent than default.
        // Should still converge on a clean click track.
        let sr = 48_000u32;
        let audio = synthetic::click_track(128.0, 8.0, sr);
        let cfg = TrackerConfig {
            sample_rate: sr,
            channels: 1,
            analysis_period_samples: sr / 4,
            bpm_range: BpmRange::DEFAULT,
        };
        let mut t = BpmTracker::new(cfg).unwrap();
        for chunk in audio.chunks(2048) {
            t.process(chunk);
        }
        match t.state() {
            TrackerState::Locked { bpm } => {
                assert!((bpm - 128.0).abs() <= 1.0, "got {bpm}");
            }
            // If the faster cadence over-drives the hysteresis we
            // may still be Tentative at the end of 8 s; that's a
            // tolerable outcome to flag here (and a separate
            // milestone-level concern).
            TrackerState::Tentative { bpm } => {
                assert!(
                    (bpm - 128.0).abs() <= 1.0,
                    "Tentative at end, got bpm={bpm}"
                );
            }
            TrackerState::Searching => panic!("should not still be Searching after 8 s"),
        }
    }
}
