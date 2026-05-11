//! Per-deck timecode input — wires a real-time audio input ringbuf
//! through the [`dub_timecode::Decoder`] and drives a deck's transport
//! from the decoded rate.
//!
//! M5.3 lives here. The integration is one decoder per deck, with a
//! small state machine that combines:
//!
//! 1. An **amplitude gate** — RMS of the input must be above
//!    [`DEFAULT_AMPLITUDE_THRESHOLD`] for the deck to track at all.
//!    On stylus lift the cartridge picks up handling/rumble noise
//!    that the decoder can find *some* coherent rotation in (moderate
//!    confidence), but the RMS is near-zero. Amplitude is the
//!    truthful "is the cartridge on the groove?" signal; confidence
//!    alone is not.
//! 2. A **two-edge confidence hysteresis** — engage at
//!    [`DEFAULT_CONFIDENCE_THRESHOLD`], stay engaged through the
//!    lukewarm `[disengage_threshold, engage_threshold)` band.
//! 3. A **sticky-block window** —
//!    [`DEFAULT_STICKY_BLOCKS_TO_DISENGAGE`] consecutive below-floor
//!    blocks before the deck mutes, so single-block dust ticks /
//!    dropouts never reach the user.
//!
//! The first two SL3 validations of M5.3 each surfaced a missing
//! piece: single-threshold confidence chatters around the engage
//! edge during lift; two-edge confidence-only treats lift as a
//! "lukewarm scratch transient" and keeps the deck playing burstily
//! while the needle is up. The amplitude gate closes that hole.
//! M5.4 owns the *calibration* UX and the scope; the policy here is
//! the minimum needed for clean lifts on real hardware.
//!
//! ## RT-safety
//!
//! Everything in [`TimecodeInput::drive`] is allocation-free, lock-free,
//! and finite-time:
//!
//! - `pop_slice` on the SPSC consumer is a load + memcpy.
//! - [`dub_timecode::Decoder::process`] is `assert_no_alloc`-clean
//!   (verified in M5.1).
//! - The scratch buffer is pre-allocated at attach time to
//!   `max_block_frames * 2` interleaved samples; we only ever index
//!   into it, never resize it.
//! - The decoder's confidence/rate are `Copy`, no [`Box`] / [`Vec`] in
//!   the hot path.
//!
//! ## Why a separate module instead of folding into [`crate::Engine`]
//!
//! Two tests want to drive the timecode path *without* an engine
//! (synthetic carrier → decoder → deck-rate assertion in isolation),
//! and the rt-audit binary wants to construct one without the rest of
//! the engine. Keeping the type public and standalone avoids a maze of
//! `#[cfg(test)]` shims and lets each layer be tested independently.

use ringbuf::traits::{Consumer, Observer};
use ringbuf::HeapCons;

use dub_timecode::{DecodeOutput, Decoder, Format};

// -------------------------------------------------------------------
// Re-exports from this module's public API:
//   - LiftPolicy / LiftIntent: the state machine, decoupled from the
//     ringbuf/decoder. The scope and calibration tools use these
//     without touching `TimecodeInput`.
//   - TimecodeInput / TimecodeInputConfig / AttachError /
//     DEFAULT_*: the engine-side wrapper that owns a `LiftPolicy`
//     plus a ringbuf consumer + decoder.
//
// `TimecodeInput` is the only shape the audio thread sees;
// `LiftPolicy` is the only shape diagnostic tools see. Both ship
// the same lift logic — so M5.4.1's `dub scope` and M5.4.2's
// `dub calibrate` cannot drift from M5.3's playback policy.
// -------------------------------------------------------------------

/// Default confidence threshold to *engage* the timecode lock.
///
/// Picked from the M5.1 + M5.2 empirical data: real cartridges through
/// an SL3 sit at 0.99–1.00 confidence on a clean signal, drop into
/// 0.5–0.9 during transients (scratches, dust ticks), and sit near
/// zero on silence (stylus lift). 0.8 is comfortably above the
/// transient floor — so genuine scratching keeps the deck rate-driven
/// — while still cleanly rejecting silence.
///
/// This is the *upper* hysteresis edge: confidence must reach this
/// value to (re-)engage. See [`DEFAULT_DISENGAGE_THRESHOLD`] for the
/// lower edge.
pub const DEFAULT_CONFIDENCE_THRESHOLD: f32 = 0.8;

/// Default confidence threshold below which the deck *disengages*
/// (after [`DEFAULT_STICKY_BLOCKS_TO_DISENGAGE`] consecutive blocks).
///
/// Set well below [`DEFAULT_CONFIDENCE_THRESHOLD`] so genuine scratch
/// transients (which dip to ~0.5–0.7 mid-stroke) keep the deck driven
/// at the last good rate instead of muting. Lift events drop confidence
/// far below this almost immediately.
pub const DEFAULT_DISENGAGE_THRESHOLD: f32 = 0.5;

/// Default number of consecutive blocks below
/// [`DEFAULT_DISENGAGE_THRESHOLD`] required to actually disengage.
///
/// At 256-frame blocks @ 48 kHz that's 4 × 5.33 ms ≈ 21 ms — long
/// enough to ride out dust ticks and the brief 0-confidence dips
/// between scratch direction reversals, short enough that a real lift
/// mutes well within a typical fader-in window.
pub const DEFAULT_STICKY_BLOCKS_TO_DISENGAGE: u32 = 4;

/// Default RMS amplitude floor below which the input is treated as
/// "carrier dead" regardless of confidence.
///
/// CV02 carriers through an SL3 typically sit at RMS 0.1–0.5; a
/// lifted needle picks up handling/rumble at < 0.005 RMS. 0.01 is
/// well below any usable carrier and well above the noise floor.
/// The decoder's own comment ([`dub_timecode::DecodeOutput::amplitude`])
/// flags 0.01 as the empirical lift threshold.
pub const DEFAULT_AMPLITUDE_THRESHOLD: f32 = 0.01;

/// Off-RT configuration for [`TimecodeInput`]. All fields are validated
/// at attach time, so the audio thread sees only checked values.
#[derive(Debug, Clone, Copy)]
pub struct TimecodeInputConfig {
    /// Timecode format on the input vinyl.
    pub format: Format,

    /// Sample rate of the input audio stream (Hz). Must match the
    /// engine's render sample rate to within 0.5 Hz; SR conversion
    /// between input and engine is not in v1 scope.
    pub input_sample_rate: f32,

    /// Maximum number of stereo frames the engine might be asked to
    /// render in a single `render()` call. Sizes the per-decoder scratch
    /// buffer; one allocation, never resized at runtime. CoreAudio
    /// typically delivers 256–1024 frame blocks; pick a comfortable
    /// upper bound (4096 is the v1 default everywhere we've measured).
    pub max_block_frames: usize,

    /// Upper hysteresis edge: the decoder confidence the audio thread
    /// requires to (re-)engage the deck after a dropout / lift /
    /// startup. Default [`DEFAULT_CONFIDENCE_THRESHOLD`].
    pub confidence_threshold: f32,

    /// Lower hysteresis edge: while engaged, confidence must stay at
    /// or above this value or the deck enters a "sticky countdown"
    /// before disengaging. Single-block dust ticks below this don't
    /// disengage; sustained drops do (see
    /// `sticky_blocks_to_disengage`). Must be ≤ `confidence_threshold`.
    /// Default [`DEFAULT_DISENGAGE_THRESHOLD`].
    pub disengage_threshold: f32,

    /// Number of consecutive blocks with confidence below
    /// `disengage_threshold` required to actually disengage (mute the
    /// deck via the 2 ms declick fade). Default
    /// [`DEFAULT_STICKY_BLOCKS_TO_DISENGAGE`]. `0` means "disengage on
    /// the first below-threshold block" (no stickiness).
    pub sticky_blocks_to_disengage: u32,

    /// RMS amplitude floor below which the carrier is considered
    /// dead regardless of decoder confidence. See
    /// [`DEFAULT_AMPLITUDE_THRESHOLD`] for the rationale and default.
    /// Set to 0.0 to disable the amplitude gate (confidence-only
    /// policy, equivalent to the M5.3 first-cut behavior — useful
    /// only for diagnostics).
    pub amplitude_threshold: f32,
}

impl Default for TimecodeInputConfig {
    fn default() -> Self {
        Self {
            format: Format::SeratoCv02,
            input_sample_rate: 48_000.0,
            max_block_frames: 4096,
            confidence_threshold: DEFAULT_CONFIDENCE_THRESHOLD,
            disengage_threshold: DEFAULT_DISENGAGE_THRESHOLD,
            sticky_blocks_to_disengage: DEFAULT_STICKY_BLOCKS_TO_DISENGAGE,
            amplitude_threshold: DEFAULT_AMPLITUDE_THRESHOLD,
        }
    }
}

/// Errors from [`crate::Engine::attach_timecode_input`].
#[allow(missing_docs)]
#[derive(Debug, thiserror::Error)]
pub enum AttachError {
    /// Deck index is out of range.
    #[error("deck index {idx} out of range (have {count})")]
    InvalidDeck { idx: usize, count: usize },

    /// A timecode input is already attached to the deck. Call
    /// `detach_timecode_input` first if you really mean to swap it.
    #[error("deck {idx} already has a timecode input attached")]
    AlreadyAttached { idx: usize },

    /// The configured input sample rate doesn't match the engine. v1
    /// rejects this rather than silently SRC-ing.
    #[error(
        "timecode input SR {input_sr} Hz != engine SR {engine_sr} Hz \
         (sample-rate conversion is not in v1 scope)"
    )]
    SampleRateMismatch { input_sr: f32, engine_sr: f32 },

    /// `max_block_frames` was zero. Indicates a programming error.
    #[error("max_block_frames must be > 0")]
    BadBlockSize,

    /// `disengage_threshold` was greater than `confidence_threshold`.
    /// That inverts the hysteresis (we'd disengage *before* engaging),
    /// always indicating a config bug.
    #[error("disengage_threshold ({disengage}) must be ≤ confidence_threshold ({engage})")]
    InvalidHysteresis { engage: f32, disengage: f32 },

    /// `amplitude_threshold` was negative or non-finite. 0 is allowed
    /// (it disables the gate); any other invalid value is a bug.
    #[error("amplitude_threshold ({value}) must be ≥ 0 and finite")]
    InvalidAmplitudeThreshold { value: f32 },

    /// SPSC command channel between [`crate::EngineHandle`] and the
    /// audio thread is full (M5.4.5: only emitted by the handle's
    /// command-channel attach path; never by the synchronous
    /// `Engine::attach_timecode_input`). Recoverable — retry on the
    /// next render block.
    #[error("audio engine command channel is full")]
    ChannelFull,
}

/// Lift / engage policy state machine, decoupled from the
/// ringbuf and decoder. Owns all state needed to decide, for any
/// given [`DecodeOutput`], whether the deck should be driven
/// (and at what rate). Pure data with one method ([`Self::step`]);
/// allocation-free; the only non-`Copy` field is the `f64` rate,
/// so the whole struct is `Copy` and there are no heap
/// allocations at all.
///
/// This is the shared truth between three callers:
///
/// 1. [`TimecodeInput`] — the audio-thread wrapper that calls
///    `step` once per render block from inside `Engine::render`.
/// 2. `dub scope` (M5.4.1) — the standalone TUI inspector. Owns
///    its own [`LiftPolicy`] + [`Decoder`] + input buffer so it
///    can run without an engine attached.
/// 3. `dub calibrate` (M5.4.2) — replays recorded carrier samples
///    through a `LiftPolicy` to evaluate candidate thresholds
///    against historical data.
///
/// All three see *exactly the same* lift behavior because they
/// share this code path. If the policy changes, every diagnostic
/// tool follows.
#[derive(Debug, Clone, Copy)]
pub struct LiftPolicy {
    /// Upper hysteresis edge — see [`TimecodeInputConfig::confidence_threshold`].
    engage_threshold: f32,

    /// Lower hysteresis edge — see
    /// [`TimecodeInputConfig::disengage_threshold`].
    disengage_threshold: f32,

    /// Sticky-disengage window — see
    /// [`TimecodeInputConfig::sticky_blocks_to_disengage`].
    sticky_blocks_to_disengage: u32,

    /// RMS floor — see [`TimecodeInputConfig::amplitude_threshold`].
    amplitude_threshold: f32,

    /// State-machine flag: are we currently driving the deck from
    /// timecode? `false` while waiting for the carrier to climb above
    /// `engage_threshold`, `true` once it has (until sticky disengage
    /// fires).
    engaged: bool,

    /// Consecutive blocks with confidence below `disengage_threshold`.
    /// Counts up while engaged; resets when confidence climbs back into
    /// the active band. When this reaches `sticky_blocks_to_disengage`
    /// we disengage.
    consecutive_below: u32,

    /// Last rate the decoder reported with sufficient confidence. Used
    /// to keep the deck moving at the previous rate when a block drops
    /// below threshold (single-block dropouts shouldn't freeze a
    /// scratch).
    last_locked_rate: f64,
}

impl LiftPolicy {
    /// Build a policy from the same threshold fields as a
    /// [`TimecodeInputConfig`]. Starts disengaged with
    /// `last_locked_rate = 0`. Allocation-free.
    #[must_use]
    pub fn new(config: &TimecodeInputConfig) -> Self {
        Self {
            engage_threshold: config.confidence_threshold,
            disengage_threshold: config.disengage_threshold,
            sticky_blocks_to_disengage: config.sticky_blocks_to_disengage,
            amplitude_threshold: config.amplitude_threshold,
            engaged: false,
            consecutive_below: 0,
            last_locked_rate: 0.0,
        }
    }

    /// Whether the policy is currently driving the deck (i.e. the
    /// last call to [`Self::step`] returned [`LiftIntent::Locked`]
    /// from the engaged branch). Diagnostic UIs read this to color
    /// the engaged/disengaged indicator.
    #[must_use]
    pub fn is_engaged(&self) -> bool {
        self.engaged
    }

    /// Number of consecutive blocks the policy has spent below the
    /// disengage threshold while engaged. Reaches
    /// `sticky_blocks_to_disengage` exactly when the policy
    /// disengages. Diagnostic UIs render this as a count-down bar
    /// during lift.
    #[must_use]
    pub fn consecutive_below(&self) -> u32 {
        self.consecutive_below
    }

    /// Last rate the policy locked to. Held during dropouts so the
    /// deck (or the diagnostic) doesn't snap to zero on a single
    /// dust tick.
    #[must_use]
    pub fn last_locked_rate(&self) -> f64 {
        self.last_locked_rate
    }

    /// Advance the state machine by one decoder block.
    ///
    /// **Amplitude gate.** If `out.amplitude < amplitude_threshold`
    /// the carrier is considered dead — *whatever* the confidence
    /// reads — and the block is treated as "below floor" (counts
    /// toward the sticky disengage window; cannot re-engage). This
    /// closes the lift hole where the cartridge picks up
    /// handling/rumble noise that the decoder finds *some* coherent
    /// rotation in (moderate confidence) but the RMS is near-zero.
    /// Ignoring amplitude here is what made M5.3's second iteration
    /// leak track-audio bursts during stylus lift on the SL3.
    ///
    /// **Confidence bands** (only checked when the carrier is alive):
    ///
    /// - `conf ≥ engage_threshold` → "fully locked" — track current
    ///   block's rate.
    /// - `disengage_threshold ≤ conf < engage_threshold` → "lukewarm"
    ///   while engaged — keep last good rate, stay engaged (no
    ///   countdown). When *disengaged*, this band does *not*
    ///   re-engage (avoids letting noise sneak the deck back on).
    /// - `conf < disengage_threshold` → if engaged, increment the
    ///   countdown; disengage when it hits
    ///   `sticky_blocks_to_disengage`.
    pub fn step(&mut self, out: DecodeOutput) -> LiftIntent {
        let carrier_alive = out.amplitude >= self.amplitude_threshold;

        if self.engaged {
            if carrier_alive && out.confidence >= self.engage_threshold {
                self.consecutive_below = 0;
                self.last_locked_rate = out.rate;
                LiftIntent::Locked { rate: out.rate }
            } else if carrier_alive && out.confidence >= self.disengage_threshold {
                // Lukewarm scratch transient — hold the last good
                // rate (don't trust the noisy current-block estimate)
                // but keep the deck engaged. Don't count toward the
                // disengage window: scratches can sit in this band
                // for tens of ms while the cartridge is firmly on the
                // groove. This branch is *only* reached when the
                // carrier is alive, so handling-noise during lift no
                // longer counts as "scratch transient".
                self.consecutive_below = 0;
                LiftIntent::Locked {
                    rate: self.last_locked_rate,
                }
            } else {
                // Below floor: either confidence collapsed
                // (carrier-alive but coherence gone — e.g. dust tick
                // mid-scratch), or amplitude collapsed (carrier dead
                // — e.g. stylus lift). Either way, count toward the
                // sticky disengage window.
                self.consecutive_below = self.consecutive_below.saturating_add(1);
                if self.consecutive_below >= self.sticky_blocks_to_disengage {
                    self.engaged = false;
                    LiftIntent::DropoutHoldRate {
                        rate: self.last_locked_rate,
                    }
                } else {
                    // Inside the sticky window — keep the deck running
                    // at the last good rate. Single-block dust ticks
                    // and brief carrier dips never reach the user.
                    LiftIntent::Locked {
                        rate: self.last_locked_rate,
                    }
                }
            }
        } else if carrier_alive && out.confidence >= self.engage_threshold {
            self.engaged = true;
            self.consecutive_below = 0;
            self.last_locked_rate = out.rate;
            LiftIntent::Locked { rate: out.rate }
        } else {
            // Disengaged and not confident *and* alive enough to
            // re-engage. Amplitude check matters here too: a quiet
            // burst of structured noise must not re-engage.
            LiftIntent::DropoutHoldRate {
                rate: self.last_locked_rate,
            }
        }
    }
}

/// One deck's-worth of timecode input. Owned by the engine after
/// [`crate::Engine::attach_timecode_input`] succeeds.
pub struct TimecodeInput {
    /// Single-producer/single-consumer ring; producer is the CoreAudio
    /// input IOProc inside `dub-audio`'s `AudioInput`.
    rx: HeapCons<f32>,

    /// Per-deck phase tracker. Holds prev-sample state across blocks.
    decoder: Decoder,

    /// Pre-allocated workspace for one block of input samples. Sized
    /// `max_block_frames * 2` (interleaved stereo) at attach time.
    scratch: Vec<f32>,

    /// Latest decoded block result. Cached so the engine can surface
    /// `(rate, position, confidence)` to the UI without reaching into
    /// the decoder.
    last_output: Option<DecodeOutput>,

    /// Lift / engage state machine. Owns the rate hold + sticky
    /// countdown + amplitude-gated hysteresis. See [`LiftPolicy`]
    /// for the algorithm.
    policy: LiftPolicy,
}

impl TimecodeInput {
    /// Off-RT constructor. Allocates the scratch buffer and the decoder
    /// state; both pre-sized so the audio-thread side touches no heap.
    #[must_use]
    pub fn new(rx: HeapCons<f32>, config: TimecodeInputConfig) -> Self {
        let scratch = vec![0.0_f32; config.max_block_frames.saturating_mul(2).max(2)];
        let decoder = Decoder::new(config.format, config.input_sample_rate);
        Self {
            rx,
            decoder,
            scratch,
            last_output: None,
            policy: LiftPolicy::new(&config),
        }
    }

    /// Number of input samples currently buffered between the IOProc
    /// and the engine. UI-side observability for "is the input alive?".
    #[must_use]
    pub fn available(&self) -> usize {
        self.rx.occupied_len()
    }

    /// Most recent decode result, if at least one block has been
    /// processed since attach. `None` before the first render with new
    /// input data.
    #[must_use]
    pub fn last_output(&self) -> Option<DecodeOutput> {
        self.last_output
    }

    /// Drain whatever input audio has arrived since the last call,
    /// process it, and return the resulting transport intent for the
    /// caller (the engine) to apply to the deck.
    ///
    /// This is the audio-thread entrypoint. Allocation-free as long as
    /// the configured `max_block_frames` is large enough that one
    /// pop_slice of the ring suffices — we don't loop, we cap to
    /// `scratch.len()`. Any input beyond that simply waits for the next
    /// render block.
    ///
    /// Returns `None` if nothing was processed this block (no input
    /// available, or fewer than 1 stereo frame). Callers should hold
    /// the deck's previous transport state in that case.
    pub(crate) fn drive(&mut self) -> Option<LiftIntent> {
        let cap = self.scratch.len();
        let popped = self.rx.pop_slice(&mut self.scratch[..cap]);
        // Decoder requires interleaved stereo (even-length input).
        // A single odd sample at the tail just stays in the ring for
        // next block — which is the right thing because losing it
        // would walk the L/R alignment by one sample forever.
        let popped_even = popped & !1;
        if popped_even == 0 {
            return None;
        }
        // Push the dangling odd sample (if any) back into the ring is
        // not possible with HeapCons — but pop_slice doesn't take half
        // a frame anyway: the IOProc only ever pushes whole frames
        // (channels × N). So `popped` is even in practice; the masking
        // is defensive belt-and-braces.
        let out = self.decoder.process(&self.scratch[..popped_even]);
        self.last_output = Some(out);
        Some(self.policy.step(out))
    }
}

/// What [`LiftPolicy::step`] tells the caller to do with the deck
/// after observing one decoder block.
///
/// Public so diagnostic tools (`dub scope`, `dub calibrate`) can
/// match on it. The engine's [`crate::Engine::render`] is the
/// primary consumer — it translates the intent into deck transport
/// state (rate + paused/playing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LiftIntent {
    /// Confidence is above threshold — engage transport at this rate.
    Locked {
        /// Playback rate for the deck this block: `1.0` for unity
        /// forward, `-1.0` for reverse, etc. While inside the
        /// "lukewarm" hysteresis band, this is the *last* good rate
        /// (the policy doesn't trust mid-scratch confidence-dipped
        /// estimates); inside the engage band it's the current
        /// block's decoded rate.
        rate: f64,
    },
    /// Confidence dropped — the deck is paused; rate is held in case
    /// confidence comes back this very next block (chatter immunity).
    DropoutHoldRate {
        /// Last rate the policy locked to before this dropout.
        /// Diagnostic UIs render this dimmed; the engine sets it
        /// on the deck so the rate is already correct when the
        /// carrier returns.
        rate: f64,
    },
}

impl TimecodeInputConfig {
    /// Validate against the engine's render SR. Run off-RT during
    /// `attach_timecode_input` so the audio thread never sees a bad
    /// config.
    pub(crate) fn validate(&self, engine_sr: f32) -> Result<(), AttachError> {
        if (self.input_sample_rate - engine_sr).abs() > 0.5 {
            return Err(AttachError::SampleRateMismatch {
                input_sr: self.input_sample_rate,
                engine_sr,
            });
        }
        if self.max_block_frames == 0 {
            return Err(AttachError::BadBlockSize);
        }
        if self.disengage_threshold > self.confidence_threshold {
            return Err(AttachError::InvalidHysteresis {
                engage: self.confidence_threshold,
                disengage: self.disengage_threshold,
            });
        }
        if self.amplitude_threshold < 0.0 || !self.amplitude_threshold.is_finite() {
            return Err(AttachError::InvalidAmplitudeThreshold {
                value: self.amplitude_threshold,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod policy_tests {
    //! Tests the hysteresis state machine in isolation. We bypass the
    //! decoder by feeding synthetic [`DecodeOutput`]s straight into
    //! [`LiftPolicy::step`] — that's the *whole point* of factoring
    //! the policy out of `drive`: data sourcing and policy are
    //! independent. Tests target [`LiftPolicy`] directly; the
    //! [`TimecodeInput`] wrapper is exercised separately by the
    //! crate's integration tests.
    use super::*;

    /// Build a [`LiftPolicy`] with the given thresholds and the
    /// canonical SL3 amplitude gate (0.01).
    fn make(engage: f32, disengage: f32, sticky: u32) -> LiftPolicy {
        LiftPolicy::new(&TimecodeInputConfig {
            format: Format::SeratoCv02,
            input_sample_rate: 48_000.0,
            max_block_frames: 64,
            confidence_threshold: engage,
            disengage_threshold: disengage,
            sticky_blocks_to_disengage: sticky,
            amplitude_threshold: 0.01,
        })
    }

    /// Most policy tests want a *live* carrier (amplitude well above
    /// the gate). Use 0.5 — typical CV02-through-SL3 RMS — so the
    /// confidence-band logic is exercised in isolation.
    fn out(rate: f64, confidence: f32) -> DecodeOutput {
        out_with_amp(rate, confidence, 0.5)
    }

    fn out_with_amp(rate: f64, confidence: f32, amplitude: f32) -> DecodeOutput {
        DecodeOutput {
            rate,
            position_secs: 0.0,
            amplitude,
            confidence,
        }
    }

    #[test]
    fn engages_only_at_or_above_engage_threshold() {
        // Engage threshold 0.8 → 0.79 below it must not engage even
        // though it's well above the disengage threshold (0.5).
        let mut p = make(0.8, 0.5, 4);
        let i = p.step(out(1.0, 0.79));
        assert!(matches!(i, LiftIntent::DropoutHoldRate { .. }));
        let i = p.step(out(1.0, 0.80));
        assert!(matches!(i, LiftIntent::Locked { rate } if (rate - 1.0).abs() < 1e-9));
    }

    #[test]
    fn sticky_window_holds_deck_during_brief_dropouts() {
        // Engage, then suffer 3 consecutive sub-disengage blocks with
        // sticky=4. Deck must stay engaged and emit Locked at the last
        // locked rate the whole time.
        let mut p = make(0.8, 0.5, 4);
        assert!(matches!(p.step(out(1.0, 0.95)), LiftIntent::Locked { .. }));
        for _ in 0..3 {
            match p.step(out(0.0, 0.0)) {
                LiftIntent::Locked { rate } => assert!((rate - 1.0).abs() < 1e-9),
                LiftIntent::DropoutHoldRate { .. } => panic!("disengaged inside sticky window"),
            }
        }
    }

    #[test]
    fn disengages_exactly_after_sticky_window() {
        // sticky=4 → 4th consecutive sub-disengage block flips engaged
        // off; subsequent blocks emit DropoutHoldRate.
        let mut p = make(0.8, 0.5, 4);
        p.step(out(1.0, 0.95));
        for _ in 0..3 {
            assert!(matches!(p.step(out(0.0, 0.0)), LiftIntent::Locked { .. }));
        }
        // 4th below-threshold block: disengage fires this block.
        let i = p.step(out(0.0, 0.0));
        assert!(matches!(i, LiftIntent::DropoutHoldRate { rate } if (rate - 1.0).abs() < 1e-9));
        // And stays disengaged.
        let i = p.step(out(0.0, 0.0));
        assert!(matches!(i, LiftIntent::DropoutHoldRate { .. }));
    }

    #[test]
    fn lukewarm_band_holds_rate_but_keeps_engaged() {
        // While engaged, a confidence in [disengage, engage) should
        // hold the last rate (don't trust noisy mid-scratch estimates)
        // but stay engaged and reset the disengage countdown.
        let mut p = make(0.8, 0.5, 4);
        p.step(out(1.0, 0.95));
        // Burn 3 sub-disengage blocks toward the sticky window edge.
        for _ in 0..3 {
            p.step(out(0.0, 0.0));
        }
        // Lukewarm block: should not disengage, should reset countdown,
        // and should hold the last good rate (1.0) — *not* trust the
        // current 0.5 rate estimate.
        match p.step(out(0.5, 0.6)) {
            LiftIntent::Locked { rate } => assert!((rate - 1.0).abs() < 1e-9),
            LiftIntent::DropoutHoldRate { .. } => panic!("disengaged in lukewarm band"),
        }
        // Now we have a fresh sticky window; need 4 more sub-disengage
        // blocks to actually disengage.
        for _ in 0..3 {
            assert!(matches!(p.step(out(0.0, 0.0)), LiftIntent::Locked { .. }));
        }
        assert!(matches!(
            p.step(out(0.0, 0.0)),
            LiftIntent::DropoutHoldRate { .. }
        ));
    }

    #[test]
    fn lukewarm_does_not_re_engage_from_disengaged() {
        // Once disengaged, only confidence ≥ engage_threshold (0.8)
        // re-engages. A lukewarm 0.7 must stay disengaged.
        let mut p = make(0.8, 0.5, 1);
        p.step(out(1.0, 0.95));
        p.step(out(0.0, 0.0)); // sticky=1 → disengages now
        let i = p.step(out(1.0, 0.7));
        assert!(
            matches!(i, LiftIntent::DropoutHoldRate { .. }),
            "lukewarm shouldn't re-engage; got {i:?}"
        );
        // Crossing the engage threshold finally re-engages.
        let i = p.step(out(1.0, 0.85));
        assert!(matches!(i, LiftIntent::Locked { .. }));
    }

    #[test]
    fn engaged_full_lock_resets_sticky_countdown() {
        // Three sub-disengage blocks should burn ¾ of the sticky=4
        // window; then a fully-locked block should reset it, not just
        // emit Locked once.
        let mut p = make(0.8, 0.5, 4);
        p.step(out(1.0, 0.95));
        for _ in 0..3 {
            p.step(out(0.0, 0.0));
        }
        p.step(out(1.2, 0.95)); // full re-lock — countdown reset
                                // Now a fresh full window of 4 below-threshold blocks needed.
        for _ in 0..3 {
            assert!(matches!(p.step(out(0.0, 0.0)), LiftIntent::Locked { .. }));
        }
        assert!(matches!(
            p.step(out(0.0, 0.0)),
            LiftIntent::DropoutHoldRate { rate } if (rate - 1.2).abs() < 1e-9
        ));
    }

    #[test]
    fn config_validate_rejects_inverted_hysteresis() {
        let cfg = TimecodeInputConfig {
            confidence_threshold: 0.5,
            disengage_threshold: 0.8,
            ..TimecodeInputConfig::default()
        };
        let err = cfg.validate(48_000.0).unwrap_err();
        assert!(matches!(err, AttachError::InvalidHysteresis { .. }));
    }

    #[test]
    fn lift_with_lukewarm_noise_disengages_via_amplitude_gate() {
        // Reproduces the second-iteration M5.3 SL3 lift bug: while
        // engaged, the cartridge picks up handling/rumble whose
        // confidence sits in the lukewarm band but whose amplitude is
        // near zero. Without an amplitude gate the deck stays
        // engaged at last_locked_rate and burst-plays the track. With
        // the gate, lukewarm-but-quiet counts as below-floor and the
        // sticky window disengages within sticky_blocks_to_disengage.
        let mut p = make(0.8, 0.5, 4);
        p.step(out_with_amp(1.0, 0.95, 0.5));
        // Three quiet "lukewarm-by-confidence" blocks: amplitude
        // override should classify them as below-floor; deck stays
        // engaged at last rate (sticky window not yet expired).
        for _ in 0..3 {
            match p.step(out_with_amp(0.0, 0.7, 0.001)) {
                LiftIntent::Locked { rate } => assert!((rate - 1.0).abs() < 1e-9),
                LiftIntent::DropoutHoldRate { .. } => panic!("disengaged inside sticky window"),
            }
        }
        // 4th quiet block: actually disengage.
        let i = p.step(out_with_amp(0.0, 0.7, 0.001));
        assert!(
            matches!(i, LiftIntent::DropoutHoldRate { .. }),
            "amplitude gate should disengage even with lukewarm confidence; got {i:?}"
        );
    }

    #[test]
    fn quiet_high_confidence_does_not_re_engage() {
        // Once disengaged, a block with engage-level confidence but
        // amplitude below the gate must NOT re-engage. (Decoder can
        // report high confidence on quiet structured noise — e.g.
        // 60 Hz hum from a lifted cartridge near a transformer — and
        // we don't want that to grab the deck back on.)
        let mut p = make(0.8, 0.5, 1);
        p.step(out_with_amp(1.0, 0.95, 0.5));
        // sticky=1 → first below-floor block disengages.
        p.step(out_with_amp(0.0, 0.0, 0.001));
        let i = p.step(out_with_amp(1.0, 0.95, 0.001));
        assert!(
            matches!(i, LiftIntent::DropoutHoldRate { .. }),
            "quiet block must not re-engage even with high confidence; got {i:?}"
        );
        // Carrier returns (amplitude back up): re-engage.
        let i = p.step(out_with_amp(1.0, 0.95, 0.5));
        assert!(matches!(i, LiftIntent::Locked { .. }));
    }

    #[test]
    fn amplitude_gate_disabled_at_zero_threshold() {
        // amplitude_threshold = 0 disables the gate; behavior should
        // collapse to confidence-only (the M5.3 first-cut policy).
        // Useful as a diagnostic mode and worth pinning so we don't
        // accidentally regress it.
        let mut p = LiftPolicy::new(&TimecodeInputConfig {
            format: Format::SeratoCv02,
            input_sample_rate: 48_000.0,
            max_block_frames: 64,
            confidence_threshold: 0.8,
            disengage_threshold: 0.5,
            sticky_blocks_to_disengage: 1,
            amplitude_threshold: 0.0,
        });
        // High confidence + zero amplitude must engage when the gate
        // is off.
        let i = p.step(out_with_amp(1.0, 0.95, 0.0));
        assert!(matches!(i, LiftIntent::Locked { .. }));
    }

    #[test]
    fn accessors_track_state_machine() {
        // Diagnostic UIs read these — pin the contract.
        let mut p = make(0.8, 0.5, 3);
        assert!(!p.is_engaged());
        assert_eq!(p.consecutive_below(), 0);
        assert!((p.last_locked_rate() - 0.0).abs() < 1e-12);

        p.step(out(1.5, 0.95));
        assert!(p.is_engaged());
        assert_eq!(p.consecutive_below(), 0);
        assert!((p.last_locked_rate() - 1.5).abs() < 1e-9);

        p.step(out(0.0, 0.0));
        assert!(p.is_engaged());
        assert_eq!(p.consecutive_below(), 1);

        p.step(out(0.0, 0.0));
        assert_eq!(p.consecutive_below(), 2);

        p.step(out(0.0, 0.0));
        assert!(!p.is_engaged(), "sticky=3 should disengage on 3rd block");
    }

    #[test]
    fn config_validate_rejects_negative_amplitude_threshold() {
        let cfg = TimecodeInputConfig {
            amplitude_threshold: -0.001,
            ..TimecodeInputConfig::default()
        };
        let err = cfg.validate(48_000.0).unwrap_err();
        assert!(matches!(err, AttachError::InvalidAmplitudeThreshold { .. }));
    }
}
