//! # `dub-peaks` — live waveform-peak capture (M9)
//!
//! This crate is to **waveform rendering** what [`dub_bpm`] is to BPM:
//! a pure-Rust off-RT data layer that the audio thread feeds via a
//! `ringbuf` tap, with a thread-safe reader API the UI consumes.
//!
//! ## What it does
//!
//! Given a stream of mono audio samples (the same mono-downmix the
//! [`ThruSource`][thru] BPM tee receives — we reuse the buffer to
//! avoid recomputing it), [`PeakStream`] produces a growing sequence
//! of [`PeakChunk`]s:
//!
//! ```text
//!   sample 0..N      → PeakChunk { min, max, rms }   ← "chunk 0"
//!   sample N..2N     → PeakChunk { min, max, rms }   ← "chunk 1"
//!   sample 2N..3N    → PeakChunk { min, max, rms }   ← "chunk 2"
//!   …
//! ```
//!
//! where `N` is [`DEFAULT_SAMPLES_PER_CHUNK`] (64 samples ≈ 1.33 ms at
//! 48 kHz). 60 fps rendering needs roughly one chunk per 0.8 screen
//! pixels at typical scratch zoom (5 s on a 4K display), so 64-sample
//! decimation gives plenty of resolution for both scratch and
//! over-the-record overview rendering.
//!
//! Each chunk stores three values rather than just peak: `min`,
//! `max`, `rms`. This is the standard envelope-display format used by
//! Audacity, Mixxx and Serato — properly mastered drums are
//! asymmetric (a snare's positive peak is meaningfully different from
//! its negative one) and RMS gives perceived-loudness shading for
//! free.
//!
//! ## What it isn't
//!
//! * **Not a beatgrid.** Peaks are dumb envelopes; meter detection
//!   and barline marks happen elsewhere (post-M10).
//! * **Not multi-resolution by itself.** We emit one mip-level; the
//!   M10 renderer downsamples further for the overview view (an
//!   8-bin average of `min/max/rms` is the same operation a `Decimator`
//!   would do, so layering mips is straightforward when we need it).
//! * **Not persistent.** The buffer is in-memory only; full-track
//!   peak caching (so you re-load a 60 min set and see its waveform
//!   instantly) is M11 library scope.
//!
//! ## Architecture
//!
//! ```text
//!   Audio thread (ThruSource::render_into)
//!     ├─ stereo input → routed output (deck mixer / external mixer)
//!     ├─ mono-downmix → bpm_tee ringbuf  (M8 — pre-existing)
//!     └─ mono-downmix → peaks_tap ringbuf (M9 — this crate)
//!
//!   peaks_tap ringbuf
//!     │
//!     ▼
//!   PeakStream-spawned thread (off-RT)
//!     loop:
//!       drain peaks_tap → block buffer
//!       Decimator::feed(block) → emit PeakChunks
//!       PeakBuffer::push(chunks)  (briefly takes write-lock)
//!       if shutdown: exit
//!       else sleep(POLL_INTERVAL)
//!
//!   PeakBuffer (shared, Arc<RwLock<Vec<PeakChunk>>>)
//!     │
//!     ▼
//!   Renderer / CLI (60 fps poll)
//!     PeakStream::len()              → AtomicUsize load (lock-free)
//!     PeakStream::extend_chunks(...) → briefly read-lock, copy new
//! ```
//!
//! [thru]: ../dub_engine/struct.ThruSource.html
//!
//! ## M10 consumer contract
//!
//! For the upcoming Metal-backed waveform UI, the contract is:
//!
//! 1. Cache the last-seen `start_idx` (initially 0) per stream.
//! 2. Every render frame, call [`PeakStream::extend_chunks`] with
//!    that index into a local `Vec<PeakChunk>` — appends only new
//!    chunks, returns the new `start_idx`. This is the fast 60 fps
//!    path.
//! 3. For overview rendering, downsample your local mirror further
//!    in the UI (peak-of-peaks / rms-of-rms). The crate intentionally
//!    does **not** maintain mip pyramids — they're cheap to derive
//!    and the UI knows how many pixels it has.
//! 4. Treat [`PeakChunk`] as wire-format: `#[repr(C)]`, 12 bytes,
//!    f32 fields. It can be shoveled into a vertex buffer directly.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

mod band_decimator;
mod buffer;
mod decimator;
mod stream;
pub mod synthetic;

pub use band_decimator::BandDecimator;
pub use buffer::{BandPeakSnapshot, PeakBuffer, PeakSnapshot};
pub use decimator::Decimator;
pub use stream::{PeakStream, PeakStreamConfig, PeakStreamError};

/// Number of frequency bands carried by each [`BandPeakChunk`].
///
/// Re-export of [`dub_spectral::NUM_BANDS`] so consumers (the M10
/// renderer, FFI surface, CLI dump) can size their structures from
/// this crate alone without an extra `dub-spectral` dependency.
pub const NUM_BANDS: usize = dub_spectral::NUM_BANDS;

/// One decimated chunk of the input waveform.
///
/// `min` is the most-negative sample in the chunk's source range,
/// `max` is the most-positive, and `rms` is `sqrt(mean(sample²))`.
/// For an all-silence chunk, all three are exactly `0.0`.
///
/// `#[repr(C)]` so the M10 renderer (and any future FFI consumer)
/// can treat a `&[PeakChunk]` as a packed 12-byte stride for direct
/// upload to a vertex buffer.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeakChunk {
    /// Most-negative sample in the source range (≤ 0 for well-formed
    /// audio; can be positive only if every sample in the range was
    /// positive, which is rare in real signals and benign).
    pub min: f32,
    /// Most-positive sample in the source range.
    pub max: f32,
    /// `sqrt(mean(sample²))` over the source range. Always ≥ 0.
    pub rms: f32,
}

impl PeakChunk {
    /// Chunk with all fields zero. Used to represent silence and as
    /// the `Decimator` reset state.
    pub const ZERO: Self = Self {
        min: 0.0,
        max: 0.0,
        rms: 0.0,
    };
}

/// Default samples-per-chunk at the base mip level.
///
/// 64 samples ≈ 1.33 ms at 48 kHz — fine enough for scratch zoom
/// (5 s on a 4K display ≈ 0.8 chunks per pixel), coarse enough that
/// 90 minutes of audio costs ~50 MB at 12 bytes/chunk.
pub const DEFAULT_SAMPLES_PER_CHUNK: usize = 64;

/// One band-energy slice across [`NUM_BANDS`] log-spaced frequency
/// bands.
///
/// Emitted by [`BandDecimator`] once per FFT hop on the same mono
/// tap as the broadband [`PeakChunk`]s. The renderer pairs broadband
/// peaks (amplitude shape) with band peaks (perceptual colour) to
/// produce a Serato-style multi-colour waveform.
///
/// `#[repr(C)]` so the FFI surface (M10.1) can hand a
/// `&[BandPeakChunk]` to Metal as a packed 32-byte stride for upload
/// to a per-vertex attribute buffer.
///
/// ## Wire format
///
/// * Size: 8 × `f32` = **32 bytes**.
/// * Indexing: `rms_per_band[k]` matches
///   `dub_spectral::SpectralFrameStream::bands()[k]` (band 0 =
///   lowest log-band, band 7 = highest).
/// * Values: square root of the mean of squared compressed
///   magnitudes (`ln(1 + λ · |X|)`) within the band's FFT bins, over
///   one FFT hop window. *Not* a physical RMS — it's an RMS over the
///   per-bin perceptual loudness, which is what colour rendering
///   actually wants.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandPeakChunk {
    /// One non-negative energy value per band. See struct-level doc.
    pub rms_per_band: [f32; NUM_BANDS],
}

impl BandPeakChunk {
    /// All-zeros chunk. Used as the `Default`, the [`BandDecimator`]
    /// reset state, and the renderer's silence placeholder.
    pub const ZERO: Self = Self {
        rms_per_band: [0.0; NUM_BANDS],
    };
}

impl Default for BandPeakChunk {
    fn default() -> Self {
        Self::ZERO
    }
}

/// Samples per [`BandPeakChunk`] — the FFT hop size.
///
/// At 48 kHz this is 512 samples = ~10.6 ms = ~94 Hz cadence. The
/// renderer maps a broadband peak chunk `j` to its band chunk via
/// `k = j × DEFAULT_SAMPLES_PER_CHUNK / BAND_SAMPLES_PER_CHUNK`,
/// which for the default `samples_per_chunk = 64` is `k = j / 8`.
pub const BAND_SAMPLES_PER_CHUNK: usize = dub_spectral::HOP_SIZE;

/// Default initial Vec capacity for the peak buffer, expressed as a
/// duration in seconds at the engine sample rate. Sized for a typical
/// 10-minute long mix track; the buffer grows transparently beyond
/// this if a longer record is played.
///
/// Pre-allocating up front avoids reallocation jitter in the
/// decimator thread for the common case, without committing to a
/// hard upper bound.
pub const DEFAULT_BUFFER_CAPACITY_SECS: u64 = 600;

/// Capacity of the audio-side tap ring, in seconds of mono audio.
/// Mirrors `BPM_TEE_RING_CAPACITY_SECS` in `dub-engine`: 1 s of slack
/// is plenty since the decimator thread polls every 20 ms.
pub const PEAKS_TAP_RING_CAPACITY_SECS: usize = 1;
