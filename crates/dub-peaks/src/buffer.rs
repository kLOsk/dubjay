//! Shared, append-only buffer of [`PeakChunk`]s.
//!
//! Read-mostly: the decimator thread appends in bursts of a few
//! chunks per 20 ms poll; the renderer reads in bursts of `~16 ms`
//! at 60 fps. We pick `RwLock<Vec<PeakChunk>>` over a fully lock-free
//! design because:
//!
//! 1. The append rate is low (~750 chunks/s at 48 kHz / 64 spc), and
//!    the lock is held for `Vec::extend_from_slice` on a small slice
//!    — single-digit microseconds in the worst case.
//! 2. The renderer side benefits from a stable snapshot, so its
//!    extend-from-tail copy doesn't tear.
//! 3. `len()` is exposed as a separate `AtomicUsize` so the renderer
//!    can ask "anything new?" without touching the lock at all,
//!    which is the common case (most frames have 1–2 new chunks).
//!
//! Capacity is pre-allocated at construction so common-case appends
//! don't reallocate. Growth beyond capacity is allowed (we don't drop
//! data when a 90 min record overflows the 10 min default), at the
//! cost of one reallocation. Reallocation happens on the decimator
//! thread, never on the audio thread.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use crate::{BandPeakChunk, PeakChunk};

/// Shared peak buffer. Cloning is cheap (Arc bump); all clones see
/// the same underlying data.
///
/// The buffer grows monotonically — chunks are never removed or
/// overwritten. To start a fresh recording, drop the owning
/// [`crate::PeakStream`] and spawn a new one.
#[derive(Clone, Debug)]
pub struct PeakBuffer {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Lock-free count of chunks pushed so far. Updated *after*
    /// each successful push so a reader that observes `len = N`
    /// will see `chunks[..N]` populated under the read lock.
    len: AtomicUsize,
    /// The chunk vec. Read-write lock so the renderer's `extend`
    /// path can copy without worrying about a concurrent push
    /// reallocating mid-copy.
    chunks: RwLock<Vec<PeakChunk>>,
    /// Parallel storage for per-band loudness chunks (M9.5b).
    /// `None` if the buffer was constructed without band capture;
    /// `Some` if `with_capacity_with_bands` was used. The renderer
    /// can inspect `band_len()` to discover whether band data is
    /// available without poking at this Option directly.
    bands: Option<BandStorage>,
}

#[derive(Debug)]
struct BandStorage {
    len: AtomicUsize,
    chunks: RwLock<Vec<BandPeakChunk>>,
}

impl PeakBuffer {
    /// Construct an empty buffer with the given pre-allocated
    /// broadband capacity (in chunks). No band storage; band-related
    /// methods will return as if "off". Growth beyond `capacity` is
    /// allowed transparently.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                len: AtomicUsize::new(0),
                chunks: RwLock::new(Vec::with_capacity(capacity)),
                bands: None,
            }),
        }
    }

    /// Construct an empty buffer with both broadband and per-band
    /// storage. `capacity` sizes the broadband Vec; `band_capacity`
    /// sizes the band Vec. Either Vec grows transparently when
    /// exceeded.
    ///
    /// This is the M9.5b construction path; it's used when the
    /// caller (the `PeakStream` worker) wants to capture band
    /// energy alongside the broadband envelope for multi-colour
    /// rendering.
    #[must_use]
    pub fn with_capacity_with_bands(capacity: usize, band_capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                len: AtomicUsize::new(0),
                chunks: RwLock::new(Vec::with_capacity(capacity)),
                bands: Some(BandStorage {
                    len: AtomicUsize::new(0),
                    chunks: RwLock::new(Vec::with_capacity(band_capacity)),
                }),
            }),
        }
    }

    /// True iff this buffer was constructed with band storage. M10
    /// uses this to decide whether to upload `BandPeakChunk`s to
    /// Metal in addition to broadband peaks.
    #[must_use]
    pub fn has_bands(&self) -> bool {
        self.inner.bands.is_some()
    }

    /// Lock-free length query. Stable to call from any thread at any
    /// frequency; the renderer should use this as its "anything new?"
    /// check before calling [`Self::extend_chunks`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len.load(Ordering::Acquire)
    }

    /// True iff no chunks have been pushed yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Append a slice of chunks. Briefly takes the write-lock; the
    /// caller batches as much as it can per call (the decimator
    /// thread accumulates per drain loop and calls this once per
    /// loop iteration).
    ///
    /// # Panics
    ///
    /// Panics if the internal `RwLock` is poisoned — i.e. another
    /// thread panicked while holding the write lock. Poison should
    /// never occur in practice (the only writer is the decimator
    /// thread, which has no fallible operations under the lock),
    /// but if it does it indicates an unrecoverable invariant
    /// violation and surfacing the panic is correct.
    pub fn push_chunks(&self, chunks: &[PeakChunk]) {
        if chunks.is_empty() {
            return;
        }
        let mut guard = self
            .inner
            .chunks
            .write()
            .expect("PeakBuffer poisoned; a producer panicked mid-write");
        guard.extend_from_slice(chunks);
        // Release-store so any reader doing an acquire-load of `len`
        // is guaranteed to see the appended chunks under the read
        // lock.
        self.inner.len.store(guard.len(), Ordering::Release);
    }

    /// Append new chunks (those with index `>= start_idx`) into
    /// `dst`. Returns the *new* total chunk count, which the caller
    /// uses as `start_idx` on the next call.
    ///
    /// O(new chunks); the renderer fast path. Does NOT clear `dst`
    /// — the caller's mirror grows monotonically across frames.
    ///
    /// If `start_idx > len()`, returns `len()` without modifying
    /// `dst`. This handles the "renderer mirrored a stale stream
    /// and the buffer reset" case gracefully (in v1 the buffer
    /// never shrinks, so this codepath is unreachable; documenting
    /// the semantics now so M11+ reset can plug in).
    ///
    /// # Panics
    ///
    /// Panics if the internal `RwLock` is poisoned. See
    /// [`Self::push_chunks`] for why this is the correct
    /// surface for that condition.
    pub fn extend_chunks(&self, start_idx: usize, dst: &mut Vec<PeakChunk>) -> usize {
        let guard = self
            .inner
            .chunks
            .read()
            .expect("PeakBuffer poisoned; a producer panicked mid-write");
        let len = guard.len();
        if start_idx >= len {
            return len;
        }
        dst.extend_from_slice(&guard[start_idx..]);
        len
    }

    /// Snapshot the full broadband buffer (copies). Convenience for
    /// CLI tools, tests, and shutdown-time dumps; renderers should
    /// prefer [`Self::extend_chunks`] to keep per-frame cost
    /// O(new), not O(total).
    ///
    /// Per-band data, if present, can be captured separately via
    /// [`Self::band_snapshot`].
    ///
    /// # Panics
    ///
    /// Panics if the internal `RwLock` is poisoned. See
    /// [`Self::push_chunks`] for why this is the correct
    /// surface for that condition.
    #[must_use]
    pub fn snapshot(&self) -> PeakSnapshot {
        let guard = self
            .inner
            .chunks
            .read()
            .expect("PeakBuffer poisoned; a producer panicked mid-write");
        PeakSnapshot {
            chunks: guard.clone(),
        }
    }

    // ---- Per-band capture (M9.5b) ----------------------------------------

    /// Lock-free length query for the per-band side. Returns `0` if
    /// this buffer has no band storage. M10 callers use this as
    /// their "anything new in the colour channel?" check before
    /// calling [`Self::extend_band_chunks`].
    #[must_use]
    pub fn band_len(&self) -> usize {
        self.inner
            .bands
            .as_ref()
            .map_or(0, |b| b.len.load(Ordering::Acquire))
    }

    /// Append per-band chunks. No-op if band storage is disabled (a
    /// runtime cost the worker thread happily pays so the caller
    /// doesn't have to branch on `has_bands()` per drain loop).
    ///
    /// # Panics
    ///
    /// Panics if the band `RwLock` is poisoned. See
    /// [`Self::push_chunks`] for why this is the correct surface.
    pub fn push_band_chunks(&self, chunks: &[BandPeakChunk]) {
        if chunks.is_empty() {
            return;
        }
        let Some(storage) = self.inner.bands.as_ref() else {
            return;
        };
        let mut guard = storage
            .chunks
            .write()
            .expect("PeakBuffer band storage poisoned; a producer panicked mid-write");
        guard.extend_from_slice(chunks);
        storage.len.store(guard.len(), Ordering::Release);
    }

    /// Renderer fast path for per-band chunks. Same semantics as
    /// [`Self::extend_chunks`]: appends new band chunks (index
    /// `>= start_idx`) into `dst` and returns the new total band
    /// length. Returns 0 immediately if band storage is disabled
    /// (caller should treat that as "no colour data — fall back to
    /// monochrome").
    ///
    /// # Panics
    ///
    /// Panics if the band `RwLock` is poisoned. See
    /// [`Self::push_chunks`] for why this is the correct surface.
    pub fn extend_band_chunks(&self, start_idx: usize, dst: &mut Vec<BandPeakChunk>) -> usize {
        let Some(storage) = self.inner.bands.as_ref() else {
            return 0;
        };
        let guard = storage
            .chunks
            .read()
            .expect("PeakBuffer band storage poisoned; a producer panicked mid-write");
        let len = guard.len();
        if start_idx >= len {
            return len;
        }
        dst.extend_from_slice(&guard[start_idx..]);
        len
    }

    /// Snapshot the full per-band buffer. Empty if band storage is
    /// disabled.
    ///
    /// # Panics
    ///
    /// Panics if the band `RwLock` is poisoned. See
    /// [`Self::push_chunks`] for why this is the correct surface.
    #[must_use]
    pub fn band_snapshot(&self) -> BandPeakSnapshot {
        let Some(storage) = self.inner.bands.as_ref() else {
            return BandPeakSnapshot { chunks: Vec::new() };
        };
        let guard = storage
            .chunks
            .read()
            .expect("PeakBuffer band storage poisoned; a producer panicked mid-write");
        BandPeakSnapshot {
            chunks: guard.clone(),
        }
    }
}

/// A point-in-time copy of a [`PeakBuffer`]. Owns its chunks so it
/// outlives the originating buffer without holding any locks.
#[derive(Debug, Clone)]
pub struct PeakSnapshot {
    /// All chunks captured at snapshot time, in order.
    pub chunks: Vec<PeakChunk>,
}

impl PeakSnapshot {
    /// Number of chunks in the snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// True iff no chunks were captured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

/// Point-in-time copy of a [`PeakBuffer`]'s per-band side. Owns its
/// chunks so it outlives the originating buffer without holding any
/// locks.
///
/// Empty when the originating buffer has no band storage; same
/// type-level surface as [`PeakSnapshot`].
#[derive(Debug, Clone)]
pub struct BandPeakSnapshot {
    /// All band chunks captured at snapshot time, in order.
    pub chunks: Vec<BandPeakChunk>,
}

impl BandPeakSnapshot {
    /// Number of band chunks in the snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// True iff no band chunks were captured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn pc(min: f32, max: f32, rms: f32) -> PeakChunk {
        PeakChunk { min, max, rms }
    }

    fn bpc(seed: f32) -> BandPeakChunk {
        BandPeakChunk {
            rms_per_band: std::array::from_fn(|k| {
                #[allow(clippy::cast_precision_loss)]
                let off = k as f32 * 0.01;
                seed + off
            }),
        }
    }

    // -------- Single-thread basics --------

    #[test]
    fn fresh_buffer_is_empty() {
        let b = PeakBuffer::with_capacity(8);
        assert_eq!(b.len(), 0);
        assert!(b.is_empty());
    }

    #[test]
    fn push_increments_len() {
        let b = PeakBuffer::with_capacity(8);
        b.push_chunks(&[pc(-0.1, 0.2, 0.15), pc(-0.3, 0.4, 0.25)]);
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn pushing_empty_slice_is_noop() {
        let b = PeakBuffer::with_capacity(8);
        b.push_chunks(&[]);
        assert_eq!(b.len(), 0);
    }

    #[test]
    fn snapshot_captures_all_pushed_chunks_in_order() {
        let b = PeakBuffer::with_capacity(8);
        b.push_chunks(&[pc(0.0, 0.1, 0.05), pc(0.0, 0.2, 0.1)]);
        b.push_chunks(&[pc(0.0, 0.3, 0.15)]);
        let snap = b.snapshot();
        assert_eq!(snap.len(), 3);
        assert!((snap.chunks[0].max - 0.1).abs() < 1e-6);
        assert!((snap.chunks[1].max - 0.2).abs() < 1e-6);
        assert!((snap.chunks[2].max - 0.3).abs() < 1e-6);
    }

    // -------- Incremental reader --------

    #[test]
    fn extend_chunks_returns_total_len() {
        let b = PeakBuffer::with_capacity(8);
        b.push_chunks(&[pc(0.0, 0.1, 0.05); 3]);
        let mut dst = Vec::new();
        let new_len = b.extend_chunks(0, &mut dst);
        assert_eq!(new_len, 3);
        assert_eq!(dst.len(), 3);
    }

    #[test]
    fn extend_chunks_appends_only_new() {
        let b = PeakBuffer::with_capacity(8);
        b.push_chunks(&[pc(0.0, 0.1, 0.05); 3]);
        let mut dst = Vec::new();
        let n1 = b.extend_chunks(0, &mut dst);
        assert_eq!(n1, 3);
        // Push 2 more chunks; extend with start_idx=3 should append
        // only those 2.
        b.push_chunks(&[pc(0.0, 0.2, 0.1); 2]);
        let n2 = b.extend_chunks(n1, &mut dst);
        assert_eq!(n2, 5);
        assert_eq!(dst.len(), 5, "renderer mirror should have all 5 now");
        // The first 3 should still be the 0.1-max ones (not duplicated).
        for c in &dst[..3] {
            assert!((c.max - 0.1).abs() < 1e-6);
        }
        // The new 2 should be the 0.2-max ones.
        for c in &dst[3..] {
            assert!((c.max - 0.2).abs() < 1e-6);
        }
    }

    #[test]
    fn extend_chunks_with_caught_up_start_idx_is_noop() {
        let b = PeakBuffer::with_capacity(8);
        b.push_chunks(&[pc(0.0, 0.1, 0.05); 5]);
        let mut dst = vec![pc(9.0, 9.0, 9.0)]; // sentinel
        let new_len = b.extend_chunks(5, &mut dst);
        assert_eq!(new_len, 5);
        assert_eq!(dst.len(), 1, "no new chunks → dst untouched");
        assert!((dst[0].max - 9.0).abs() < 1e-6, "sentinel preserved");
    }

    #[test]
    fn extend_chunks_with_start_past_len_is_noop() {
        let b = PeakBuffer::with_capacity(8);
        b.push_chunks(&[pc(0.0, 0.1, 0.05); 5]);
        let mut dst = Vec::new();
        // start_idx 10 > len 5 — defensive: don't panic, don't write.
        let new_len = b.extend_chunks(10, &mut dst);
        assert_eq!(new_len, 5);
        assert!(dst.is_empty());
    }

    // -------- Clone / Arc semantics --------

    #[test]
    fn cloned_buffers_share_storage() {
        let b1 = PeakBuffer::with_capacity(8);
        let b2 = b1.clone();
        b1.push_chunks(&[pc(0.0, 0.1, 0.05)]);
        assert_eq!(b2.len(), 1, "clone should observe pushes through b1");
        b2.push_chunks(&[pc(0.0, 0.2, 0.1)]);
        assert_eq!(b1.len(), 2, "b1 should observe pushes through b2");
    }

    // -------- Per-band storage (M9.5b) -----------------------------

    #[test]
    fn no_band_storage_unless_constructed_with_bands() {
        let b = PeakBuffer::with_capacity(8);
        assert!(!b.has_bands());
        assert_eq!(b.band_len(), 0);
        b.push_band_chunks(&[bpc(0.1)]);
        assert_eq!(b.band_len(), 0, "push must no-op when bands off");
        let mut dst = Vec::new();
        let new_len = b.extend_band_chunks(0, &mut dst);
        assert_eq!(new_len, 0);
        assert!(dst.is_empty());
        assert!(b.band_snapshot().is_empty());
    }

    #[test]
    fn with_capacity_with_bands_enables_band_storage() {
        let b = PeakBuffer::with_capacity_with_bands(8, 4);
        assert!(b.has_bands());
        assert_eq!(b.band_len(), 0);
        b.push_band_chunks(&[bpc(0.1), bpc(0.2)]);
        assert_eq!(b.band_len(), 2);
        let snap = b.band_snapshot();
        assert_eq!(snap.len(), 2);
        assert!((snap.chunks[0].rms_per_band[0] - 0.1).abs() < 1e-6);
        assert!((snap.chunks[1].rms_per_band[0] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn band_extend_chunks_appends_only_new() {
        let b = PeakBuffer::with_capacity_with_bands(8, 4);
        b.push_band_chunks(&[bpc(0.1), bpc(0.2), bpc(0.3)]);
        let mut dst = Vec::new();
        let n1 = b.extend_band_chunks(0, &mut dst);
        assert_eq!(n1, 3);
        b.push_band_chunks(&[bpc(0.4)]);
        let n2 = b.extend_band_chunks(n1, &mut dst);
        assert_eq!(n2, 4);
        assert_eq!(dst.len(), 4);
        assert!((dst[3].rms_per_band[0] - 0.4).abs() < 1e-6);
    }

    #[test]
    fn band_and_broadband_storage_are_independent() {
        // Pushing broadband chunks must not affect band length, and
        // vice versa.
        let b = PeakBuffer::with_capacity_with_bands(8, 4);
        b.push_chunks(&[pc(0.0, 0.5, 0.3); 10]);
        assert_eq!(b.len(), 10);
        assert_eq!(b.band_len(), 0);
        b.push_band_chunks(&[bpc(0.7)]);
        assert_eq!(b.len(), 10);
        assert_eq!(b.band_len(), 1);
    }

    // -------- Concurrent producer + consumer --------

    #[test]
    fn concurrent_producer_consumer_loses_nothing() {
        // Spawn a producer that pushes 1000 chunks one-by-one with
        // tiny yields; a consumer polls len() + extend in parallel.
        // The final consumer-side mirror must equal the producer's
        // total output.
        let b = PeakBuffer::with_capacity(8);
        let b_writer = b.clone();
        let producer = std::thread::spawn(move || {
            for i in 0..1000 {
                #[allow(clippy::cast_precision_loss)]
                let f = i as f32 / 1000.0;
                b_writer.push_chunks(&[pc(-f, f, f.abs())]);
                if i % 50 == 0 {
                    std::thread::yield_now();
                }
            }
        });

        let mut mirror: Vec<PeakChunk> = Vec::new();
        let mut start = 0usize;
        // Poll until we've drained 1000 chunks (or producer joins).
        loop {
            start = b.extend_chunks(start, &mut mirror);
            if start >= 1000 {
                break;
            }
            std::thread::yield_now();
        }
        producer.join().expect("producer panic");

        // One final drain to catch anything that landed after the
        // join. Should be a no-op since we exited the loop above
        // only when start >= 1000, but defensive.
        let final_len = b.extend_chunks(start, &mut mirror);
        assert_eq!(final_len, 1000);
        assert_eq!(mirror.len(), 1000);
        // The chunks should be in producer order: max values
        // monotonically increasing.
        for w in mirror.windows(2) {
            assert!(
                w[1].max >= w[0].max,
                "concurrent drain reordered chunks: {:?} then {:?}",
                w[0],
                w[1]
            );
        }
    }
}
