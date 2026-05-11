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

mod confidence;
mod estimator;
mod offline;
mod onset;
mod stream;
pub mod synthetic;
mod tempo;
mod tracker;

pub use confidence::{
    ConfidenceTracker, TrackerEvent, TrackerState, LOCK_CONSECUTIVE, LOCK_THRESHOLD,
    LOCK_TOLERANCE_BPM, LOST_LOCKED_CONSECUTIVE, LOST_TENTATIVE_CONSECUTIVE, REJECT_TOLERANCE_BPM,
    TENTATIVE_THRESHOLD,
};
pub use estimator::{BpmEstimator, BpmEstimatorError};
pub use offline::{analyze_bpm, AnalysisError};
pub use stream::BpmStream;
pub use tracker::{BpmTracker, TrackerConfig, TrackerError};

/// Window size for each FFT frame, in samples. 1024 is ≈ 21 ms at 48 kHz
/// — long enough for stable spectral magnitudes, short enough that onset
/// localisation isn't smeared.
pub(crate) const FRAME_SIZE: usize = 1024;

/// Hop size between consecutive frames, in samples. 512 = 50 % overlap.
/// Gives an ODF sample rate of `sr / 512` ≈ 94 Hz at 48 kHz, which is
/// enough resolution to distinguish e.g. 174 vs 175 BPM after parabolic
/// interpolation.
pub(crate) const HOP_SIZE: usize = 512;

/// Minimum tempo we attempt to detect. Below this lies the realm of "is
/// it a beat or a tape-stop?" and the autocorrelation peak picker isn't
/// reliable.
pub(crate) const MIN_BPM: f64 = 60.0;

/// Maximum tempo we attempt to detect. Above ~200 BPM most DJs feel half
/// time anyway; for dnb/jungle (170–180) and gabber (>200) we'd want
/// genre-specific priors, which are deferred to M8+.
pub(crate) const MAX_BPM: f64 = 200.0;

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
