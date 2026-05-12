//! Threaded streaming driver — the M9 entry point.
//!
//! Wraps a [`Decimator`][crate::Decimator] in an OS thread that
//! consumes mono audio from a `ringbuf` consumer and appends
//! [`PeakChunk`]s to a shared [`PeakBuffer`]. Exposes a single
//! thread-safe handle ([`PeakStream`]) that the engine integration
//! constructs at "attach Thru source" time and the UI/CLI reads
//! peak data from.
//!
//! Mirrors `dub_bpm::BpmStream` in structure so the two telemetry
//! drivers share the same operational model:
//!
//! ```text
//!   Audio thread                PeakStream-spawned thread
//!   ─────────────────           ─────────────────────────
//!   ThruSource::render_into     loop:
//!     pop input ring               pop audio_rx into block buffer
//!     mono-downmix             →   decimator.feed(block) → emit chunks
//!     peaks_tap.push_slice         buffer.push_chunks(chunks)
//!                                  if shutdown: exit
//!                                  sleep(POLL_INTERVAL)
//!                          ───────────────────────────────
//!                                            │
//!                                            ▼
//!                                       buffer (on UI thread)
//! ```
//!
//! ## Design notes
//!
//! * **Sleep-based polling**, same as `BpmStream`. 20 ms drain
//!   cadence; at 750 chunks/s a 20 ms slice is ~15 chunks worth of
//!   work, well within budget.
//! * **No event channel.** Unlike BPM (which has discrete state
//!   transitions to notify), peaks are a continuous append-only
//!   stream. The reader polls the buffer's lock-free `len()` and
//!   extends on change. Simpler API, less moving parts.
//! * **Graceful shutdown** via `AtomicBool`; identical idiom to
//!   `BpmStream`. `Drop` always sets the flag and joins.
//! * **Per-chunk reusable Vec.** The thread allocates one
//!   `Vec<PeakChunk>` at spawn and reuses it across drain loops
//!   (`clear()` before each iteration). No per-chunk allocation
//!   pressure.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ringbuf::traits::{Consumer, Observer};
use ringbuf::HeapCons;

use crate::band_decimator::BandDecimator;
use crate::buffer::PeakBuffer;
use crate::decimator::Decimator;
use crate::{
    BandPeakChunk, PeakChunk, BAND_SAMPLES_PER_CHUNK, DEFAULT_BUFFER_CAPACITY_SECS,
    DEFAULT_SAMPLES_PER_CHUNK,
};

/// Drain-loop poll interval. Same as `dub_bpm::BpmStream` —
/// 20 ms is well under the renderer's 16.7 ms frame budget, so a
/// chunk appended right after a render still appears on the
/// next-but-one frame at worst.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Maximum samples drained from the audio ring per loop iteration.
/// Caps memory of the analysis-side scratch buffer; larger values
/// reduce the number of decimator calls per second of audio without
/// affecting correctness.
const DRAIN_BLOCK_SAMPLES: usize = 4096;

/// Configuration for [`PeakStream::spawn`]. Use [`PeakStreamConfig::at`]
/// to construct with validated values.
#[derive(Debug, Clone, Copy)]
pub struct PeakStreamConfig {
    /// Engine sample rate, used to pre-size the buffer.
    pub sample_rate: u32,
    /// Samples per chunk; the base mip level. Default
    /// [`DEFAULT_SAMPLES_PER_CHUNK`] (64). Power-of-two values are
    /// recommended so a downstream renderer can do further mip
    /// reduction by averaging N adjacent chunks without resampling.
    pub samples_per_chunk: usize,
    /// Initial buffer capacity in seconds of audio. Pre-allocates
    /// `sample_rate × secs / samples_per_chunk` chunks at spawn time.
    /// Growth beyond this is allowed (transparent realloc on the
    /// decimator thread, never on the audio thread).
    pub buffer_capacity_secs: u64,
    /// Whether to run the [`BandDecimator`] alongside the broadband
    /// `Decimator`. Adds one FFT per [`BAND_SAMPLES_PER_CHUNK`]
    /// samples of audio on the worker thread (never on the audio
    /// thread); a few hundred microseconds of CPU per second of
    /// audio per deck. The M10.1 multi-colour renderer needs this
    /// data, so it's on by default — the opt-out is for CLI users
    /// who only want broadband peaks and want to keep the worker
    /// budget minimal.
    pub bands_enabled: bool,
}

impl PeakStreamConfig {
    /// Convenience constructor with the M9 + M9.5b defaults: 64
    /// samples-per-broadband-chunk, 10 min initial buffer, bands
    /// on.
    #[must_use]
    pub fn at(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            samples_per_chunk: DEFAULT_SAMPLES_PER_CHUNK,
            buffer_capacity_secs: DEFAULT_BUFFER_CAPACITY_SECS,
            bands_enabled: true,
        }
    }

    /// Pre-allocated broadband chunk capacity derived from
    /// `sample_rate × buffer_capacity_secs / samples_per_chunk`.
    /// Saturates on degenerate configurations rather than panicking.
    #[allow(clippy::cast_possible_truncation)]
    fn initial_chunk_capacity(&self) -> usize {
        // On 32-bit targets a multi-decade `buffer_capacity_secs`
        // could exceed `usize` range; saturating_cast via `as` is
        // the correct behaviour (we end up reserving "as much as
        // we can"). Allowed explicitly.
        let secs = self.buffer_capacity_secs as usize;
        let total = (self.sample_rate as usize).saturating_mul(secs);
        total / self.samples_per_chunk.max(1)
    }

    /// Pre-allocated per-band chunk capacity. Same arithmetic as
    /// [`Self::initial_chunk_capacity`] but with the FFT hop size
    /// as the denominator: there are `sample_rate ×
    /// buffer_capacity_secs / BAND_SAMPLES_PER_CHUNK` band chunks
    /// in the configured duration.
    #[allow(clippy::cast_possible_truncation)]
    fn initial_band_chunk_capacity(&self) -> usize {
        let secs = self.buffer_capacity_secs as usize;
        let total = (self.sample_rate as usize).saturating_mul(secs);
        total / BAND_SAMPLES_PER_CHUNK.max(1)
    }
}

/// Errors at [`PeakStream::spawn`].
#[allow(missing_docs)]
#[derive(Debug, thiserror::Error)]
pub enum PeakStreamError {
    #[error("sample_rate must be > 0; got 0")]
    InvalidSampleRate,
    #[error("samples_per_chunk must be > 0; got 0")]
    InvalidChunkSize,
}

/// Streaming peak-capture handle. Spawned via [`PeakStream::spawn`];
/// drop or call [`shutdown`] to stop the worker thread.
///
/// All methods are safe to call from any thread. The handle is
/// `Send`; cloning the inner [`PeakBuffer`] (via
/// [`PeakStream::buffer`]) is how multiple readers (CLI dump + UI
/// renderer) share the data.
///
/// [`shutdown`]: Self::shutdown
pub struct PeakStream {
    buffer: PeakBuffer,
    samples_per_chunk: usize,
    /// Samples per band chunk, if band capture is enabled at spawn
    /// time. `None` when `bands_enabled: false`. Renderers use this
    /// to map a broadband chunk index to its containing band chunk:
    /// `band_idx = peak_idx × samples_per_chunk / samples_per_band_chunk`.
    samples_per_band_chunk: Option<usize>,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for PeakStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeakStream")
            .field("len", &self.buffer.len())
            .field("band_len", &self.buffer.band_len())
            .field("samples_per_chunk", &self.samples_per_chunk)
            .field("samples_per_band_chunk", &self.samples_per_band_chunk)
            .field("running", &self.join.is_some())
            .field("shutdown_pending", &self.shutdown.load(Ordering::Relaxed))
            .finish()
    }
}

impl PeakStream {
    /// Spawn a decimator thread that reads mono audio from
    /// `audio_rx`, feeds it through a [`Decimator`][crate::Decimator]
    /// at `cfg.samples_per_chunk`, and appends the emitted
    /// [`PeakChunk`]s to an internal [`PeakBuffer`].
    ///
    /// The thread runs until either:
    /// * [`shutdown`] is called on this handle, or
    /// * the producer end of `audio_rx` is dropped (e.g. the engine
    ///   reclaimed the associated `ThruSource`).
    ///
    /// `cfg.sample_rate` and `cfg.buffer_capacity_secs` together
    /// determine the initial Vec capacity; growth beyond that is
    /// transparent.
    ///
    /// # Errors
    ///
    /// * [`PeakStreamError::InvalidSampleRate`] if `cfg.sample_rate == 0`.
    /// * [`PeakStreamError::InvalidChunkSize`] if
    ///   `cfg.samples_per_chunk == 0`.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to spawn the decimator thread. This
    /// is a fatal-environment condition (every other thread in the
    /// process would also be at risk); we surface it as a panic
    /// rather than burying it in `PeakStreamError` because there is
    /// no recovery path the caller could take.
    ///
    /// [`shutdown`]: Self::shutdown
    pub fn spawn(audio_rx: HeapCons<f32>, cfg: PeakStreamConfig) -> Result<Self, PeakStreamError> {
        if cfg.sample_rate == 0 {
            return Err(PeakStreamError::InvalidSampleRate);
        }
        if cfg.samples_per_chunk == 0 {
            return Err(PeakStreamError::InvalidChunkSize);
        }

        let buffer = if cfg.bands_enabled {
            PeakBuffer::with_capacity_with_bands(
                cfg.initial_chunk_capacity(),
                cfg.initial_band_chunk_capacity(),
            )
        } else {
            PeakBuffer::with_capacity(cfg.initial_chunk_capacity())
        };
        let buffer_for_thread = buffer.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_thread = Arc::clone(&shutdown);
        let spc = cfg.samples_per_chunk;
        let sample_rate = cfg.sample_rate;
        let bands_enabled = cfg.bands_enabled;

        let join = thread::Builder::new()
            .name("dub-peaks-decimator".to_string())
            .spawn(move || {
                analysis_loop(
                    audio_rx,
                    buffer_for_thread,
                    spc,
                    sample_rate,
                    bands_enabled,
                    shutdown_for_thread,
                );
            })
            .expect("OS refused to spawn decimator thread — fatal");

        Ok(Self {
            buffer,
            samples_per_chunk: spc,
            samples_per_band_chunk: bands_enabled.then_some(BAND_SAMPLES_PER_CHUNK),
            shutdown,
            join: Some(join),
        })
    }

    /// Samples per [`BandPeakChunk`] this stream emits, if band
    /// capture is enabled. `None` if `bands_enabled: false` at
    /// spawn time. Renderers map a broadband chunk index `j` to its
    /// containing band chunk index `k = j × samples_per_chunk /
    /// samples_per_band_chunk`.
    #[must_use]
    pub fn samples_per_band_chunk(&self) -> Option<usize> {
        self.samples_per_band_chunk
    }

    /// Number of band chunks captured so far. Returns 0 if band
    /// capture is disabled. Lock-free; equivalent to
    /// `self.buffer().band_len()`.
    #[must_use]
    pub fn band_len(&self) -> usize {
        self.buffer.band_len()
    }

    /// Clone-the-Arc handle to the shared buffer. Cheap; the renderer
    /// keeps one and the CLI dump path keeps another.
    #[must_use]
    pub fn buffer(&self) -> PeakBuffer {
        self.buffer.clone()
    }

    /// Number of chunks captured so far. Lock-free; equivalent to
    /// `self.buffer().len()`, just a convenience.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// True iff no chunks have been captured yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Samples per chunk this stream is configured with. Useful for
    /// converting a chunk index into a wall-clock offset (`idx *
    /// samples_per_chunk / sample_rate`).
    #[must_use]
    pub fn samples_per_chunk(&self) -> usize {
        self.samples_per_chunk
    }

    /// Signal the worker thread to exit and join it. Idempotent: a
    /// second call is a no-op. Consumes the handle.
    ///
    /// `Drop` calls this implicitly, so explicit calls are only
    /// needed when you want to surface a join panic (a decimator-
    /// thread invariant tripped) — `Drop`'s join is best-effort.
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for PeakStream {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

// `buffer` and `shutdown` are moved into the thread closure that
// owns this function call; passing them by reference would change
// the call-site to a borrow that can't outlive the spawning scope.
// `clippy::needless_pass_by_value` doesn't know about the move into
// a thread closure, hence the explicit allow.
#[allow(clippy::needless_pass_by_value)]
fn analysis_loop(
    mut audio_rx: HeapCons<f32>,
    buffer: PeakBuffer,
    samples_per_chunk: usize,
    sample_rate: u32,
    bands_enabled: bool,
    shutdown: Arc<AtomicBool>,
) {
    let mut block = vec![0.0f32; DRAIN_BLOCK_SAMPLES];
    let mut decimator = Decimator::new(samples_per_chunk);
    let mut band_decimator = bands_enabled.then(|| BandDecimator::new(sample_rate));
    // Reusable scratch for chunks emitted in one drain iteration.
    // Sized to "worst case one drain", which is DRAIN_BLOCK_SAMPLES /
    // samples_per_chunk chunks. We initialize empty and let `push`
    // grow it once if needed; in steady state it never grows again.
    let mut chunk_scratch: Vec<PeakChunk> =
        Vec::with_capacity(DRAIN_BLOCK_SAMPLES / samples_per_chunk.max(1) + 1);
    let mut band_scratch: Vec<BandPeakChunk> =
        Vec::with_capacity(DRAIN_BLOCK_SAMPLES / BAND_SAMPLES_PER_CHUNK + 1);

    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }

        let mut drained_any = false;
        while audio_rx.occupied_len() > 0 {
            let n = audio_rx.pop_slice(&mut block);
            if n == 0 {
                break;
            }
            drained_any = true;

            chunk_scratch.clear();
            decimator.feed(&block[..n], |c| chunk_scratch.push(c));
            if !chunk_scratch.is_empty() {
                buffer.push_chunks(&chunk_scratch);
            }

            if let Some(bd) = band_decimator.as_mut() {
                band_scratch.clear();
                bd.feed(&block[..n], |c| band_scratch.push(c));
                if !band_scratch.is_empty() {
                    buffer.push_band_chunks(&band_scratch);
                }
            }
        }

        if !drained_any {
            thread::sleep(POLL_INTERVAL);
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use ringbuf::traits::{Producer, Split};
    use ringbuf::{HeapProd, HeapRb};

    fn ring(capacity: usize) -> (HeapProd<f32>, HeapCons<f32>) {
        HeapRb::<f32>::new(capacity).split()
    }

    fn wait_until<F: FnMut() -> bool>(total_timeout: Duration, mut pred: F) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < total_timeout {
            if pred() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        pred()
    }

    // -------- Config validation --------

    #[test]
    fn zero_sample_rate_rejected() {
        let (_tx, rx) = ring(64);
        let cfg = PeakStreamConfig {
            sample_rate: 0,
            samples_per_chunk: 64,
            buffer_capacity_secs: 1,
            bands_enabled: false,
        };
        assert!(matches!(
            PeakStream::spawn(rx, cfg),
            Err(PeakStreamError::InvalidSampleRate)
        ));
    }

    #[test]
    fn zero_chunk_size_rejected() {
        let (_tx, rx) = ring(64);
        let cfg = PeakStreamConfig {
            sample_rate: 48_000,
            samples_per_chunk: 0,
            buffer_capacity_secs: 1,
            bands_enabled: false,
        };
        assert!(matches!(
            PeakStream::spawn(rx, cfg),
            Err(PeakStreamError::InvalidChunkSize)
        ));
    }

    #[test]
    fn config_at_uses_defaults() {
        let cfg = PeakStreamConfig::at(48_000);
        assert_eq!(cfg.samples_per_chunk, DEFAULT_SAMPLES_PER_CHUNK);
        assert_eq!(cfg.buffer_capacity_secs, DEFAULT_BUFFER_CAPACITY_SECS);
        assert!(cfg.bands_enabled, "M9.5b: bands on by default");
    }

    // -------- End-to-end decimation --------

    #[test]
    fn samples_pushed_become_chunks_in_buffer() {
        // Feed exactly 256 samples through a stream at spc=64;
        // expect 4 chunks in the buffer.
        let (mut tx, rx) = ring(1024);
        let cfg = PeakStreamConfig {
            sample_rate: 48_000,
            samples_per_chunk: 64,
            buffer_capacity_secs: 1,
            bands_enabled: false,
        };
        let stream = PeakStream::spawn(rx, cfg).expect("spawn");

        // Constant 0.3 amplitude — every chunk should be (0.3, 0.3, 0.3).
        let buf = vec![0.3f32; 256];
        let n = tx.push_slice(&buf);
        assert_eq!(n, 256);

        let ok = wait_until(Duration::from_secs(2), || stream.len() >= 4);
        assert!(ok, "expected ≥4 chunks within 2s, got {}", stream.len());

        let snap = stream.buffer().snapshot();
        for c in &snap.chunks[..4] {
            assert!((c.min - 0.3).abs() < 1e-6, "min = {}", c.min);
            assert!((c.max - 0.3).abs() < 1e-6, "max = {}", c.max);
            assert!((c.rms - 0.3).abs() < 1e-6, "rms = {}", c.rms);
        }
    }

    #[test]
    fn incremental_reader_streams_chunks() {
        // Producer trickles 8 chunks worth of samples; reader
        // polls extend_chunks and accumulates a local mirror.
        let (mut tx, rx) = ring(2048);
        let cfg = PeakStreamConfig {
            sample_rate: 48_000,
            samples_per_chunk: 64,
            buffer_capacity_secs: 1,
            bands_enabled: false,
        };
        let stream = PeakStream::spawn(rx, cfg).expect("spawn");

        let buf = vec![0.1f32; 64 * 8];
        let mut remaining = &buf[..];
        while !remaining.is_empty() {
            let n = tx.push_slice(remaining);
            remaining = &remaining[n..];
            if !remaining.is_empty() {
                thread::sleep(Duration::from_millis(2));
            }
        }

        let mut mirror = Vec::new();
        let mut start = 0usize;
        let ok = wait_until(Duration::from_secs(2), || {
            start = stream.buffer().extend_chunks(start, &mut mirror);
            mirror.len() >= 8
        });
        assert!(ok, "expected ≥8 chunks in mirror, got {}", mirror.len());
    }

    // -------- Lifecycle --------

    #[test]
    fn dropping_producer_terminates_thread() {
        let (tx, rx) = ring(64);
        let stream = PeakStream::spawn(rx, PeakStreamConfig::at(48_000)).expect("spawn");
        drop(tx);
        // If the thread never exits on shutdown, the drop would hang.
        drop(stream);
    }

    #[test]
    fn explicit_shutdown_joins_promptly() {
        let (_tx, rx) = ring(64);
        let stream = PeakStream::spawn(rx, PeakStreamConfig::at(48_000)).expect("spawn");
        let start = std::time::Instant::now();
        stream.shutdown();
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "shutdown took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn silence_pushes_zero_chunks_through() {
        let (mut tx, rx) = ring(1024);
        let stream = PeakStream::spawn(rx, PeakStreamConfig::at(48_000)).expect("spawn");
        let buf = vec![0.0f32; 256];
        let _ = tx.push_slice(&buf);
        let ok = wait_until(Duration::from_secs(2), || stream.len() >= 4);
        assert!(ok);
        let snap = stream.buffer().snapshot();
        for c in &snap.chunks[..4] {
            assert_eq!(c.min, 0.0);
            assert_eq!(c.max, 0.0);
            assert_eq!(c.rms, 0.0);
        }
    }

    // -------- Bands (M9.5b) ---------------------------------------

    #[test]
    fn bands_off_means_no_band_storage() {
        let (_tx, rx) = ring(64);
        let cfg = PeakStreamConfig {
            sample_rate: 48_000,
            samples_per_chunk: 64,
            buffer_capacity_secs: 1,
            bands_enabled: false,
        };
        let stream = PeakStream::spawn(rx, cfg).expect("spawn");
        assert!(stream.samples_per_band_chunk().is_none());
        assert_eq!(stream.band_len(), 0);
        assert!(!stream.buffer().has_bands());
    }

    #[test]
    fn bands_on_produces_band_chunks() {
        let (mut tx, rx) = ring(8192);
        let cfg = PeakStreamConfig::at(48_000);
        assert!(cfg.bands_enabled);
        let stream = PeakStream::spawn(rx, cfg).expect("spawn");
        assert_eq!(
            stream.samples_per_band_chunk(),
            Some(BAND_SAMPLES_PER_CHUNK)
        );
        assert!(stream.buffer().has_bands());

        // Feed enough samples to emit several FFT hops.
        // FRAME_SIZE = 1024, HOP_SIZE = 512 → 4 frames need
        // 1024 + 3*512 = 2560 samples.
        let buf = vec![0.5f32; 4096];
        let n = tx.push_slice(&buf);
        assert_eq!(n, 4096);

        let ok = wait_until(Duration::from_secs(2), || stream.band_len() >= 4);
        assert!(
            ok,
            "expected ≥4 band chunks within 2s, got {}",
            stream.band_len()
        );
    }

    #[test]
    fn bands_on_keeps_broadband_capture_intact() {
        // Regression: turning bands on must not affect the broadband
        // capture cadence or values.
        let (mut tx, rx) = ring(2048);
        let cfg = PeakStreamConfig::at(48_000);
        let stream = PeakStream::spawn(rx, cfg).expect("spawn");

        let buf = vec![0.3f32; 256];
        let n = tx.push_slice(&buf);
        assert_eq!(n, 256);

        let ok = wait_until(Duration::from_secs(2), || stream.len() >= 4);
        assert!(ok, "broadband: expected ≥4 chunks, got {}", stream.len());

        let snap = stream.buffer().snapshot();
        for c in &snap.chunks[..4] {
            assert!((c.min - 0.3).abs() < 1e-6);
            assert!((c.max - 0.3).abs() < 1e-6);
            assert!((c.rms - 0.3).abs() < 1e-6);
        }
    }

    #[test]
    fn buffer_handle_outlives_explicit_shutdown() {
        // After shutdown, snapshots taken before should still be
        // valid (they own their data). The buffer handle should also
        // still answer `len()` because it's just an Arc bump.
        let (mut tx, rx) = ring(1024);
        let stream = PeakStream::spawn(rx, PeakStreamConfig::at(48_000)).expect("spawn");
        let buf = vec![0.5f32; 256];
        let _ = tx.push_slice(&buf);
        let ok = wait_until(Duration::from_secs(2), || stream.len() >= 4);
        assert!(ok);
        let handle = stream.buffer();
        stream.shutdown();
        // Handle still valid.
        assert!(handle.len() >= 4);
        let snap = handle.snapshot();
        assert!(snap.len() >= 4);
    }
}
