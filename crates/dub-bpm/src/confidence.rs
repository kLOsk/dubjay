//! Confidence state machine for live BPM tracking.
//!
//! Wraps the raw [`BpmEstimate`] stream emitted by [`BpmEstimator`]
//! into a three-state UI-facing model:
//!
//! * `Searching` — no usable estimate yet. The default at construction
//!   and the state we return to after sustained signal loss.
//! * `Tentative { bpm }` — we have a candidate tempo but haven't seen
//!   it confirmed often enough to commit. Common during the first
//!   ~5 s of analysis or while a track is breaking down / quiet.
//! * `Locked { bpm }` — the candidate has been confirmed for
//!   `LOCK_CONSECUTIVE` successive updates within `LOCK_TOLERANCE_BPM`
//!   and confidence stayed at `LOCK_THRESHOLD` or above. The DJ-facing
//!   "this is the BPM" UI state.
//!
//! ## Why hysteresis?
//!
//! Raw `BpmEstimate` from the M7.5 algorithm wobbles bin-to-bin even
//! on perfect input (parabolic interpolation isn't a perfect fit; ODF
//! grows over time and shifts the autocorrelation peaks). Surfacing
//! that wobble unfiltered would mean the BPM number on screen flickers
//! at every analysis pass — visually distracting and useless for
//! beat-matching. The state machine smooths that out: the *value*
//! shown in `Locked` is the agreed-upon estimate, not the moment-to-
//! moment one, and the transition out of `Locked` only happens when
//! the underlying estimate has either degraded (confidence drop) or
//! genuinely changed (BPM jump > tolerance, e.g. the DJ swapped
//! records on the same Thru deck).
//!
//! The thresholds and tolerances below are an initial calibration
//! based on M7.5's algorithm characteristics. Expect to revisit once
//! real-music validation (M8 acceptance) starts surfacing genre
//! sensitivities.
//!
//! ## Where this lives in the pipeline
//!
//! ```text
//!  CoreAudio input ─► ThruSource ─► tee ring ─► [analysis thread]
//!                                                      │
//!                                                      ▼
//!                                          BpmEstimator (M7.5)
//!                                                      │
//!                                                      ▼ BpmEstimate
//!                                          ConfidenceTracker (this module)
//!                                                      │
//!                                                      ▼ TrackerEvent
//!                                          UI / CLI consumer
//! ```
//!
//! The state machine is intentionally pure (no IO, no threading, no
//! `BpmEstimator` field) so it can be unit-tested by hand-rolling
//! `BpmEstimate` sequences. The wiring to a real estimator lives in
//! [`crate::tracker::BpmTracker`].

use crate::BpmEstimate;

/// Confidence floor below which we treat the estimator's output as
/// noise. M7.5 already refuses estimates below `0.05` inside the
/// algorithm and returns `BpmEstimate::NONE` (confidence == 0), so
/// any input with confidence strictly greater than zero already
/// cleared the algorithm's internal honesty bar. This bound is
/// higher: it gates entry into `Tentative` so we don't latch onto a
/// borderline detection that won't survive the next update.
pub const TENTATIVE_THRESHOLD: f32 = 0.20;

/// Confidence required to allow a `Tentative → Locked` transition.
/// Higher than [`TENTATIVE_THRESHOLD`] so locking always reflects a
/// strong signal, not just a slightly-better-than-Tentative one.
pub const LOCK_THRESHOLD: f32 = 0.40;

/// Consecutive updates the candidate BPM must remain within
/// [`LOCK_TOLERANCE_BPM`] before we transition `Tentative → Locked`.
/// 3 means roughly 3 × analysis cadence — at the default ~250 ms
/// cadence that's about 750 ms of agreement before lock.
pub const LOCK_CONSECUTIVE: u32 = 3;

/// Max BPM drift allowed across [`LOCK_CONSECUTIVE`] consecutive
/// updates and still count as "the same tempo." 1.5 BPM is generous
/// enough for M7.5's parabolic-peak wobble but tight enough that a
/// genuine tempo change (mix, record swap) breaks the streak.
pub const LOCK_TOLERANCE_BPM: f64 = 1.5;

/// Once in `Locked`, a new estimate beyond this many BPM away from
/// the locked value drops us back to `Tentative` with the new value.
/// Larger than [`LOCK_TOLERANCE_BPM`] so a brief estimator wobble
/// doesn't unlock us, but small enough that a real tempo change
/// (typically ≥ 4 BPM in DJ practice) does.
pub const REJECT_TOLERANCE_BPM: f64 = 4.0;

/// Consecutive zero-confidence (`BpmEstimate::NONE`) updates that
/// drop us out of `Tentative` back to `Searching`. From `Locked`
/// the bar is higher (see [`LOST_LOCKED_CONSECUTIVE`]) because we
/// already had a confirmed tempo and a single bad block shouldn't
/// erase it.
pub const LOST_TENTATIVE_CONSECUTIVE: u32 = 5;

/// Consecutive zero-confidence updates that drop us out of `Locked`.
/// Higher than [`LOST_TENTATIVE_CONSECUTIVE`] because losing lock
/// is the bigger UI-visible event; we want it to mean "the input
/// genuinely went away," not "one analysis block had a bad ODF."
pub const LOST_LOCKED_CONSECUTIVE: u32 = 12;

/// Externally-visible tracker state. The `bpm` payload is the
/// algorithm's best estimate at the moment of transition; for
/// `Locked` it stays stable until the state changes again (we don't
/// keep mutating it on every subsequent confirming update — that
/// would defeat the purpose of locking).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackerState {
    /// No usable tempo estimate. Default at construction; returned
    /// to after sustained signal loss.
    Searching,
    /// Candidate tempo identified but not yet confirmed for long
    /// enough. UI may render as a flickering / italic number.
    Tentative {
        /// Most recent BPM estimate; will fluctuate update-to-update.
        bpm: f64,
    },
    /// Confirmed tempo. UI may render as a solid number. Held
    /// stable across confirming updates.
    Locked {
        /// The BPM value at the moment of locking. Not mutated by
        /// subsequent confirming updates.
        bpm: f64,
    },
}

impl TrackerState {
    /// BPM payload if the state carries one (`Tentative` or `Locked`).
    #[must_use]
    pub fn bpm(self) -> Option<f64> {
        match self {
            Self::Searching => None,
            Self::Tentative { bpm } | Self::Locked { bpm } => Some(bpm),
        }
    }

    /// Convenience: is the state `Locked`?
    #[must_use]
    pub fn is_locked(self) -> bool {
        matches!(self, Self::Locked { .. })
    }
}

/// State-transition event emitted by the tracker. The single variant
/// today (`StateChanged`) covers everything an external consumer
/// needs; we anticipate adding raw-estimate events in M9+ for
/// waveform-overlay decoration, hence the enum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackerEvent {
    /// The tracker entered a new state. Payload is the new state.
    StateChanged(TrackerState),
}

/// Hysteresis state machine over a stream of [`BpmEstimate`].
///
/// Constructed in [`TrackerState::Searching`]. Drive with [`update`]
/// for every estimator output. [`update`] returns `Some(TrackerEvent)`
/// when (and only when) the externally-visible state changes — drive
/// it on every update regardless; the tracker decides when to emit.
///
/// [`update`]: Self::update
#[derive(Debug)]
pub struct ConfidenceTracker {
    state: TrackerState,
    /// Number of consecutive updates whose `bpm` agreed with the
    /// current `Tentative` candidate within [`LOCK_TOLERANCE_BPM`].
    /// Reset to 0 on disagreement; reaches [`LOCK_CONSECUTIVE`]
    /// triggers `Tentative → Locked`.
    lock_streak: u32,
    /// Number of consecutive zero-confidence updates. Triggers
    /// `Tentative → Searching` at [`LOST_TENTATIVE_CONSECUTIVE`]
    /// and `Locked → Tentative` at [`LOST_LOCKED_CONSECUTIVE`].
    lost_streak: u32,
}

impl Default for ConfidenceTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfidenceTracker {
    /// Fresh tracker in [`TrackerState::Searching`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: TrackerState::Searching,
            lock_streak: 0,
            lost_streak: 0,
        }
    }

    /// Current state. Always present; never panics.
    #[must_use]
    pub fn state(&self) -> TrackerState {
        self.state
    }

    /// Wipe to fresh `Searching` state. Use when the source audio
    /// changes underneath us (deck reattached, track swapped).
    pub fn reset(&mut self) {
        self.state = TrackerState::Searching;
        self.lock_streak = 0;
        self.lost_streak = 0;
    }

    /// Consume one estimator output and step the state machine.
    ///
    /// Returns `Some(TrackerEvent::StateChanged(new_state))` when the
    /// state changed; `None` when the state was unchanged. Drive on
    /// *every* estimator output regardless of whether it carries
    /// confidence — the "signal lost" path needs to see the zeros.
    pub fn update(&mut self, estimate: BpmEstimate) -> Option<TrackerEvent> {
        let new_state = self.step(estimate);
        if new_state == self.state {
            return None;
        }
        self.state = new_state;
        Some(TrackerEvent::StateChanged(new_state))
    }

    /// Pure state-transition function. Returns the next state given
    /// the current state, the estimate, and the streak counters
    /// (which it also mutates as a side-effect of transitioning).
    /// Separated from [`update`] so the equality check + emission
    /// logic lives in one place above.
    fn step(&mut self, estimate: BpmEstimate) -> TrackerState {
        let is_silent = estimate.confidence == 0.0;

        if is_silent {
            self.lock_streak = 0;
            self.lost_streak = self.lost_streak.saturating_add(1);
        } else {
            self.lost_streak = 0;
        }

        match self.state {
            TrackerState::Searching => {
                if !is_silent && estimate.confidence >= TENTATIVE_THRESHOLD {
                    self.lock_streak = 1;
                    TrackerState::Tentative { bpm: estimate.bpm }
                } else {
                    TrackerState::Searching
                }
            }

            TrackerState::Tentative { bpm: current } => {
                if self.lost_streak >= LOST_TENTATIVE_CONSECUTIVE {
                    self.lock_streak = 0;
                    return TrackerState::Searching;
                }

                if is_silent {
                    return TrackerState::Tentative { bpm: current };
                }

                let agrees = (estimate.bpm - current).abs() <= LOCK_TOLERANCE_BPM;

                if agrees {
                    self.lock_streak = self.lock_streak.saturating_add(1);
                    if estimate.confidence >= LOCK_THRESHOLD && self.lock_streak >= LOCK_CONSECUTIVE
                    {
                        TrackerState::Locked { bpm: current }
                    } else {
                        // Update the tentative payload with the new
                        // estimate so the displayed Tentative number
                        // tracks the latest information. Locked
                        // semantics are different: once locked, the
                        // payload is frozen until we leave Locked.
                        TrackerState::Tentative { bpm: estimate.bpm }
                    }
                } else {
                    self.lock_streak = 1;
                    TrackerState::Tentative { bpm: estimate.bpm }
                }
            }

            TrackerState::Locked { bpm: current } => {
                if self.lost_streak >= LOST_LOCKED_CONSECUTIVE {
                    self.lock_streak = 0;
                    return TrackerState::Tentative { bpm: current };
                }
                if is_silent {
                    return TrackerState::Locked { bpm: current };
                }

                let drift = (estimate.bpm - current).abs();
                if drift > REJECT_TOLERANCE_BPM {
                    self.lock_streak = 1;
                    TrackerState::Tentative { bpm: estimate.bpm }
                } else {
                    // Stay locked. We do NOT update `current` here —
                    // see the doc on TrackerState::Locked.
                    TrackerState::Locked { bpm: current }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn est(bpm: f64, confidence: f32) -> BpmEstimate {
        BpmEstimate { bpm, confidence }
    }

    #[test]
    fn starts_in_searching() {
        let t = ConfidenceTracker::new();
        assert_eq!(t.state(), TrackerState::Searching);
    }

    #[test]
    fn searching_to_tentative_on_confident_estimate() {
        let mut t = ConfidenceTracker::new();
        let ev = t.update(est(128.0, 0.5)).expect("should transition");
        assert_eq!(
            ev,
            TrackerEvent::StateChanged(TrackerState::Tentative { bpm: 128.0 })
        );
        assert_eq!(t.state(), TrackerState::Tentative { bpm: 128.0 });
    }

    #[test]
    fn low_confidence_keeps_searching() {
        let mut t = ConfidenceTracker::new();
        assert!(t.update(est(128.0, 0.10)).is_none());
        assert!(t.update(est(128.0, TENTATIVE_THRESHOLD - 0.01)).is_none());
        assert_eq!(t.state(), TrackerState::Searching);
    }

    #[test]
    fn tentative_locks_after_consecutive_agreement() {
        let mut t = ConfidenceTracker::new();

        // Update 1: Searching → Tentative (streak = 1).
        let ev = t.update(est(120.0, 0.5));
        assert!(matches!(
            ev,
            Some(TrackerEvent::StateChanged(TrackerState::Tentative { .. }))
        ));

        // Update 2: still Tentative, streak = 2, payload may shift.
        t.update(est(120.2, 0.5));
        assert!(matches!(t.state(), TrackerState::Tentative { .. }));

        // Update 3: streak reaches LOCK_CONSECUTIVE = 3 → Locked.
        let ev = t.update(est(119.8, 0.5));
        match ev {
            Some(TrackerEvent::StateChanged(TrackerState::Locked { bpm })) => {
                // Locked BPM is the prior Tentative payload (frozen at
                // the moment of transition; the new estimate's value
                // is only used for the agrees-with-current check).
                assert!(
                    (bpm - 120.0).abs() < 1.0,
                    "locked bpm should be ~120, got {bpm}"
                );
            }
            other => panic!("expected Locked transition, got {other:?}"),
        }
        assert!(t.state().is_locked());
    }

    #[test]
    fn drift_within_tolerance_does_not_break_streak() {
        let mut t = ConfidenceTracker::new();
        // Three consecutive updates, each drifting by ≤ 0.5 BPM (all
        // within LOCK_TOLERANCE_BPM = 1.5) → lock on the third.
        t.update(est(128.0, 0.5));
        t.update(est(128.5, 0.5));
        let ev = t.update(est(127.5, 0.5));
        assert!(
            matches!(
                ev,
                Some(TrackerEvent::StateChanged(TrackerState::Locked { .. }))
            ),
            "expected Locked transition on 3rd agreeing update, got {ev:?}"
        );
    }

    #[test]
    fn drift_does_not_lock_in_fewer_than_lock_consecutive_updates() {
        // Defensive coverage of the LOCK_CONSECUTIVE constant: even
        // with perfect confidence and exact-BPM agreement, locking
        // shouldn't happen until LOCK_CONSECUTIVE updates have
        // accumulated.
        let mut t = ConfidenceTracker::new();
        for _ in 0..(LOCK_CONSECUTIVE - 1) {
            t.update(est(128.0, 0.9));
        }
        assert!(
            !t.state().is_locked(),
            "should not lock in fewer than {LOCK_CONSECUTIVE} updates"
        );
    }

    #[test]
    fn drift_outside_tolerance_resets_streak() {
        let mut t = ConfidenceTracker::new();
        t.update(est(120.0, 0.5));
        t.update(est(120.2, 0.5)); // ok
                                   // Jump outside tolerance: streak resets to 1
        t.update(est(140.0, 0.5));
        // Now we need LOCK_CONSECUTIVE more agreeing updates near 140.
        let ev = t.update(est(140.5, 0.5));
        // Still tentative (one or two short of lock).
        assert!(!matches!(
            ev,
            Some(TrackerEvent::StateChanged(TrackerState::Locked { .. }))
        ));
    }

    #[test]
    fn requires_lock_threshold_confidence_to_lock() {
        let mut t = ConfidenceTracker::new();
        // Above TENTATIVE_THRESHOLD but below LOCK_THRESHOLD.
        let mid = (TENTATIVE_THRESHOLD + LOCK_THRESHOLD) / 2.0;
        for _ in 0..10 {
            t.update(est(128.0, mid));
        }
        assert!(
            !t.state().is_locked(),
            "should not lock at sub-lock-threshold confidence"
        );
        assert!(matches!(t.state(), TrackerState::Tentative { .. }));
    }

    #[test]
    fn locked_stays_locked_on_brief_silence() {
        let mut t = ConfidenceTracker::new();
        for _ in 0..(LOCK_CONSECUTIVE + 2) {
            t.update(est(128.0, 0.5));
        }
        assert!(t.state().is_locked());

        // A handful of zero-confidence updates: still locked.
        for _ in 0..(LOST_LOCKED_CONSECUTIVE - 1) {
            t.update(BpmEstimate::NONE);
        }
        assert!(
            t.state().is_locked(),
            "should not drop lock on brief silence"
        );
    }

    #[test]
    fn locked_drops_to_tentative_on_sustained_silence() {
        let mut t = ConfidenceTracker::new();
        for _ in 0..(LOCK_CONSECUTIVE + 2) {
            t.update(est(128.0, 0.5));
        }
        assert!(t.state().is_locked());

        for _ in 0..LOST_LOCKED_CONSECUTIVE {
            t.update(BpmEstimate::NONE);
        }
        assert!(
            matches!(t.state(), TrackerState::Tentative { bpm } if (bpm - 128.0).abs() < 0.1),
            "should drop to Tentative with last-known BPM, got {:?}",
            t.state()
        );
    }

    #[test]
    fn locked_holds_its_bpm_even_when_estimate_drifts() {
        let mut t = ConfidenceTracker::new();
        for _ in 0..(LOCK_CONSECUTIVE + 2) {
            t.update(est(128.0, 0.5));
        }
        let locked_bpm = match t.state() {
            TrackerState::Locked { bpm } => bpm,
            _ => panic!("expected Locked"),
        };

        // Feed slightly drifting estimates within tolerance — locked
        // BPM must not change.
        t.update(est(128.4, 0.5));
        t.update(est(127.6, 0.5));
        t.update(est(128.7, 0.5));
        match t.state() {
            TrackerState::Locked { bpm } => {
                #[allow(clippy::float_cmp)]
                {
                    assert_eq!(bpm, locked_bpm, "locked BPM must not mutate");
                }
            }
            other => panic!("expected still-Locked, got {other:?}"),
        }
    }

    #[test]
    fn locked_unlocks_on_big_bpm_jump() {
        let mut t = ConfidenceTracker::new();
        for _ in 0..(LOCK_CONSECUTIVE + 2) {
            t.update(est(120.0, 0.5));
        }
        assert!(t.state().is_locked());

        // A 10 BPM jump (well beyond REJECT_TOLERANCE_BPM = 4) → drop
        // to Tentative carrying the new value.
        let ev = t.update(est(140.0, 0.5));
        assert!(matches!(
            ev,
            Some(TrackerEvent::StateChanged(TrackerState::Tentative { .. }))
        ));
        if let TrackerState::Tentative { bpm } = t.state() {
            assert!((bpm - 140.0).abs() < 0.1);
        }
    }

    #[test]
    fn tentative_drops_to_searching_on_sustained_silence() {
        let mut t = ConfidenceTracker::new();
        t.update(est(128.0, 0.5));
        assert!(matches!(t.state(), TrackerState::Tentative { .. }));
        for _ in 0..LOST_TENTATIVE_CONSECUTIVE {
            t.update(BpmEstimate::NONE);
        }
        assert_eq!(t.state(), TrackerState::Searching);
    }

    #[test]
    fn reset_returns_to_searching() {
        let mut t = ConfidenceTracker::new();
        for _ in 0..10 {
            t.update(est(128.0, 0.6));
        }
        assert!(t.state().is_locked());
        t.reset();
        assert_eq!(t.state(), TrackerState::Searching);
    }

    #[test]
    fn no_event_emitted_when_state_unchanged() {
        let mut t = ConfidenceTracker::new();
        t.update(est(128.0, 0.5));
        // Same exact estimate — payload is identical to the last
        // Tentative, no transition.
        let ev = t.update(est(128.0, 0.5));
        assert!(ev.is_none(), "no transition expected, got {ev:?}");
    }

    #[test]
    fn state_bpm_accessor() {
        assert_eq!(TrackerState::Searching.bpm(), None);
        assert_eq!(TrackerState::Tentative { bpm: 128.0 }.bpm(), Some(128.0));
        assert_eq!(TrackerState::Locked { bpm: 140.0 }.bpm(), Some(140.0));
    }
}
