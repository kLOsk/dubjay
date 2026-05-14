//! Tempo (BPM) estimation for Dub.
//!
//! Two surfaces, one engine:
//!
//! * [`analyze_bpm`] — offline driver. Takes an entire audio buffer, returns
//!   a single [`BpmEstimate`]. Used on track load (PRD §5.3) and by tests as
//!   the ground-truth oracle for the streaming driver.
//! * [`BpmEstimator`] — streaming. Fed blocks of audio over time; emits the
//!   current best estimate at any moment. M8 wraps this in a non-RT analysis
//!   thread per Thru deck.
//!
//! ## Algorithm (M7.5 baseline)
//!
//! Pure-Rust spectral-flux onset detection + autocorrelation tempo
//! estimation. Textbook approach — Dixon (2006), Davies & Plumbley (2005)
//! style, simplified for our offline-first scope:
//!
//! 1. Hann-window the input in `FRAME_SIZE`-sample frames with `HOP_SIZE`
//!    overlap.
//! 2. FFT each frame; take magnitude spectrum.
//! 3. Spectral flux: sum of positive magnitude differences between
//!    consecutive frames. Output is the *onset detection function* (ODF),
//!    sampled at `sr / HOP_SIZE` Hz (≈ 94 Hz at 48 kHz).
//! 4. Local-mean subtract + half-wave rectify the ODF.
//! 5. Autocorrelate the ODF over lags corresponding to 60–200 BPM. The
//!    largest peak in that range gives the period; parabolic interpolation
//!    around the peak yields sub-bin precision.
//! 6. Confidence = peak / autocorr-at-zero, clamped to `[0, 1]`.
//!
//! Aubio (or any future backend) can replace the internals later without
//! changing this public surface — that's the whole point of the M7.5/M8
//! split documented in `docs/PRD.md` §12.
//!
//! ## Channel handling
//!
//! [`analyze_bpm`] takes interleaved input + a channel count. Stereo input
//! is downmixed to mono internally (mean of L+R). This matches how track
//! loaders in [`dub-io`](../dub_io/index.html) hand us audio.
//!
//! ## Honesty contract
//!
//! When the algorithm cannot find a periodic structure (silence, a single
//! transient, very short input, white noise), the returned estimate has
//! `confidence == 0.0` and a `bpm` value that is well-defined but not
//! meaningful. Callers MUST check `confidence` before trusting `bpm`. This
//! is deliberately not modelled as `Option<BpmEstimate>` because the
//! streaming driver always has *some* current value once it has begun
//! processing — it just may be junk.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod beats;
mod confidence;
mod estimator;
mod offline;
mod onset;
mod stream;
pub mod synthetic;
mod tempo;
mod tracker;

pub use beats::{analyze_beat_grid, BeatGrid};
pub use confidence::{
    ConfidenceTracker, TrackerEvent, TrackerState, LOCK_CONSECUTIVE, LOCK_THRESHOLD,
    LOCK_TOLERANCE_BPM, LOST_LOCKED_CONSECUTIVE, LOST_TENTATIVE_CONSECUTIVE, REJECT_TOLERANCE_BPM,
    TENTATIVE_THRESHOLD,
};
pub use estimator::{BpmEstimator, BpmEstimatorError};
pub use offline::{analyze_bpm, analyze_bpm_with_range, AnalysisError};
pub use stream::BpmStream;
pub use tracker::{BpmTracker, TrackerConfig, TrackerError};

// M9.5a: the FFT pipeline + log-band layout + magnitude compression that
// `OnsetDetector` builds on are now in `dub-spectral`, shared with
// `dub-peaks`. Re-export the constants we lifted out under the same
// internal name they used to have so the rest of this crate
// (`offline.rs`, `estimator.rs`, `onset.rs`) keeps reading
// `crate::FRAME_SIZE` etc. without churn.
pub(crate) use dub_spectral::{FRAME_SIZE, HOP_SIZE};

/// Minimum tempo of the default [`BpmRange`]. Below this lies the realm
/// of "is it a beat or a tape-stop?" and the autocorrelation peak picker
/// isn't reliable.
pub const MIN_BPM: f64 = 60.0;

/// Maximum tempo of the default [`BpmRange`]. Above ~200 BPM most DJs
/// feel half time anyway; for dnb/jungle (170–180) and gabber (>200) we'd
/// want genre-specific priors, which are deferred to M9+.
pub const MAX_BPM: f64 = 200.0;

/// A single tempo estimate.
///
/// `bpm` is meaningful only when `confidence > 0`. See the module-level
/// "Honesty contract" note.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BpmEstimate {
    /// Tempo in beats per minute. Range `[MIN_BPM, MAX_BPM]` when
    /// confidence is non-zero; arbitrary otherwise.
    pub bpm: f64,
    /// Algorithm confidence in `[0.0, 1.0]`. `0.0` means "no periodic
    /// structure detected; ignore `bpm`."
    pub confidence: f32,
}

impl BpmEstimate {
    /// A null estimate. Returned when no periodic structure was detected.
    pub(crate) const NONE: Self = Self {
        bpm: 0.0,
        confidence: 0.0,
    };
}

/// Inclusive BPM search window for the tempo estimator.
///
/// The estimator searches autocorrelation lags corresponding to the
/// inclusive `[min, max]` BPM range and ignores periodic structure
/// outside it. The default ([`BpmRange::DEFAULT`]) is the full
/// `[MIN_BPM, MAX_BPM]` = 60–200 BPM that the M8.1 algorithm is
/// calibrated for.
///
/// Constraining the range is the recommended escape hatch for the
/// inherent half-tempo ambiguity in beat-tracking (dubstep at
/// 140 BPM masquerading as 70 BPM, drum-n-bass with strong K-S
/// backbeat masquerading as half tempo, …). The M8.1 algorithm
/// resolves the common cases — reggae 65, hip-hop 90/100, rolling
/// dnb 174 — out of the box without any range hint, so the range
/// only needs to be tightened for the edge cases.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BpmRange {
    /// Inclusive lower BPM bound. Must be `>= MIN_BPM`.
    pub min: f64,
    /// Inclusive upper BPM bound. Must be `<= MAX_BPM` and `> min`.
    pub max: f64,
}

/// Errors that prevent construction of a [`BpmRange`].
#[derive(Debug, thiserror::Error)]
pub enum BpmRangeError {
    /// `min` is below [`MIN_BPM`] or `max` is above [`MAX_BPM`].
    /// The autocorrelation algorithm isn't reliable outside that
    /// window; widening it would silently degrade accuracy.
    #[error(
        "BPM range [{min}, {max}] must fit within the algorithm-supported \
         window [{MIN_BPM}, {MAX_BPM}]"
    )]
    OutsideSupported {
        /// Provided lower bound.
        min: f64,
        /// Provided upper bound.
        max: f64,
    },

    /// `max <= min` or non-finite values.
    #[error("BPM range bounds must satisfy 0 < min < max (got min={min}, max={max})")]
    Empty {
        /// Provided lower bound.
        min: f64,
        /// Provided upper bound.
        max: f64,
    },
}

impl BpmRange {
    /// Default range: 60–200 BPM. The widest the M8.1 algorithm is
    /// calibrated for.
    pub const DEFAULT: Self = Self {
        min: MIN_BPM,
        max: MAX_BPM,
    };

    /// Build a [`BpmRange`] from explicit bounds.
    ///
    /// # Errors
    ///
    /// * [`BpmRangeError::Empty`] if `!(0 < min < max)` or either
    ///   bound is non-finite.
    /// * [`BpmRangeError::OutsideSupported`] if the bounds fall
    ///   outside `[MIN_BPM, MAX_BPM]`.
    pub fn new(min: f64, max: f64) -> Result<Self, BpmRangeError> {
        if !min.is_finite() || !max.is_finite() || min <= 0.0 || max <= min {
            return Err(BpmRangeError::Empty { min, max });
        }
        if min < MIN_BPM || max > MAX_BPM {
            return Err(BpmRangeError::OutsideSupported { min, max });
        }
        Ok(Self { min, max })
    }
}

impl Default for BpmRange {
    fn default() -> Self {
        Self::DEFAULT
    }
}
