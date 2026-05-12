//! Pure online decimator: feed it samples, get [`PeakChunk`]s.
//!
//! No locks, no allocation on the hot path; the entire state is a
//! `(min, max, sumsq, count)` running aggregate. The caller streams
//! samples in (any block size) and supplies a closure that's invoked
//! once per *completed* chunk. The decimator carries a partial chunk
//! across `feed` calls so block-boundary alignment is transparent —
//! whether you feed 1 sample or 4096, the chunks come out at exactly
//! `samples_per_chunk` boundaries.
//!
//! Tested in isolation so the streaming driver in `stream.rs` doesn't
//! have to reason about chunk boundaries.

use crate::PeakChunk;

/// Online min/max/rms aggregator over fixed-size sample windows.
///
/// Construct with [`Decimator::new`], feed with [`Decimator::feed`],
/// reset with [`Decimator::reset`]. Holds a partial-chunk aggregate
/// internally between `feed` calls; the [`Decimator::pending`]
/// observation is for tests and diagnostics.
#[derive(Debug, Clone)]
pub struct Decimator {
    /// Configured chunk size, in samples. Validated `> 0` at
    /// construction.
    samples_per_chunk: usize,

    /// Running aggregate state for the chunk currently being built.
    accum_min: f32,
    accum_max: f32,
    /// `f64` to keep summation numerically stable for long chunks
    /// (we only currently use 64 samples, but a future mip-2
    /// 4096-sample chunk would otherwise lose precision on a
    /// near-full-scale signal — the f64 cost is one int-add per
    /// sample, completely negligible).
    accum_sumsq: f64,
    /// How many samples of the current chunk have been consumed.
    /// Wraps to 0 on each emit.
    accum_count: usize,
}

impl Decimator {
    /// Construct an empty decimator with the given chunk size.
    ///
    /// # Panics
    ///
    /// Panics if `samples_per_chunk == 0`. A zero-size chunk has
    /// no defined `rms` and would emit infinitely; caller bug.
    #[must_use]
    pub fn new(samples_per_chunk: usize) -> Self {
        assert!(
            samples_per_chunk > 0,
            "samples_per_chunk must be > 0; got 0"
        );
        Self {
            samples_per_chunk,
            accum_min: f32::INFINITY,
            accum_max: f32::NEG_INFINITY,
            accum_sumsq: 0.0,
            accum_count: 0,
        }
    }

    /// Configured chunk size in samples.
    #[must_use]
    pub fn samples_per_chunk(&self) -> usize {
        self.samples_per_chunk
    }

    /// How many samples of the *current* partial chunk have been
    /// consumed. `0` means a fresh-chunk boundary; equal to
    /// `samples_per_chunk - 1` means the next sample emits.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.accum_count
    }

    /// Feed a block of samples; the closure is invoked once per
    /// completed chunk, in order. Partial chunks (the tail of the
    /// block) stay in the decimator's state for the next `feed` call.
    ///
    /// Alloc-free: no internal Vec, no Box, no per-call allocation.
    /// The closure must also avoid allocation if alloc-freeness is
    /// required end-to-end (the streaming driver's
    /// `PeakBuffer::push` does briefly take a write-lock and can
    /// reallocate if the Vec needs to grow, which is why decimation
    /// runs off the audio thread, not on it).
    pub fn feed<F: FnMut(PeakChunk)>(&mut self, samples: &[f32], mut emit: F) {
        for &s in samples {
            if s < self.accum_min {
                self.accum_min = s;
            }
            if s > self.accum_max {
                self.accum_max = s;
            }
            self.accum_sumsq += f64::from(s) * f64::from(s);
            self.accum_count += 1;

            if self.accum_count == self.samples_per_chunk {
                // Cast loss is bounded: samples_per_chunk fits easily
                // in f64 mantissa for any plausible mip level (64,
                // 512, 4096, 32768 are all exact).
                #[allow(clippy::cast_precision_loss)]
                let mean_sq = self.accum_sumsq / self.samples_per_chunk as f64;
                // rms is sqrt-of-sumsq/N; cast down to f32 for the
                // wire format. Range is [0, 1] for normalized audio,
                // far above f32 precision floor.
                #[allow(clippy::cast_possible_truncation)]
                let rms = mean_sq.sqrt() as f32;

                emit(PeakChunk {
                    min: self.accum_min,
                    max: self.accum_max,
                    rms,
                });

                self.accum_min = f32::INFINITY;
                self.accum_max = f32::NEG_INFINITY;
                self.accum_sumsq = 0.0;
                self.accum_count = 0;
            }
        }
    }

    /// Drop the partial-chunk state and start fresh. Does **not**
    /// emit a partial chunk; callers that want "flush whatever you
    /// have" should call [`Self::flush`] first.
    pub fn reset(&mut self) {
        self.accum_min = f32::INFINITY;
        self.accum_max = f32::NEG_INFINITY;
        self.accum_sumsq = 0.0;
        self.accum_count = 0;
    }

    /// Emit a partial chunk now if one is in progress, then reset.
    ///
    /// The emitted chunk's `rms` is computed over `pending()` samples
    /// (not `samples_per_chunk`), so it represents the true mean of
    /// what's in the partial aggregate. Use sparingly — partial
    /// chunks misalign the timeline if you continue feeding
    /// afterwards, so this is for end-of-stream / shutdown only.
    pub fn flush<F: FnMut(PeakChunk)>(&mut self, mut emit: F) {
        if self.accum_count > 0 {
            #[allow(clippy::cast_precision_loss)]
            let mean_sq = self.accum_sumsq / self.accum_count as f64;
            #[allow(clippy::cast_possible_truncation)]
            let rms = mean_sq.sqrt() as f32;
            emit(PeakChunk {
                min: self.accum_min,
                max: self.accum_max,
                rms,
            });
        }
        self.reset();
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss)]
mod tests {
    use super::*;

    /// Helper: feed `samples` and collect all emitted chunks.
    fn run(spc: usize, samples: &[f32]) -> Vec<PeakChunk> {
        let mut d = Decimator::new(spc);
        let mut out = Vec::new();
        d.feed(samples, |c| out.push(c));
        out
    }

    #[test]
    #[should_panic(expected = "samples_per_chunk must be > 0")]
    fn zero_chunk_panics() {
        let _ = Decimator::new(0);
    }

    // -------- Chunk boundary --------

    #[test]
    fn emits_one_chunk_per_full_block() {
        let chunks = run(4, &[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn no_emit_until_full_chunk() {
        let chunks = run(4, &[0.1, 0.2, 0.3]);
        assert_eq!(chunks.len(), 0, "partial chunk must not emit");
    }

    #[test]
    fn three_chunks_for_twelve_samples_at_spc4() {
        let chunks = run(
            4,
            &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.1, 1.2],
        );
        assert_eq!(chunks.len(), 3);
    }

    #[test]
    fn partial_tail_carries_over_to_next_feed() {
        let mut d = Decimator::new(4);
        let mut out = Vec::new();
        // Feed 3 samples — no emit.
        d.feed(&[0.1, 0.2, 0.3], |c| out.push(c));
        assert!(out.is_empty());
        // Feed 5 more — total now 8 samples, expect 2 chunks.
        d.feed(&[0.4, 0.5, 0.6, 0.7, 0.8], |c| out.push(c));
        assert_eq!(out.len(), 2);
    }

    // -------- Value correctness --------

    #[test]
    fn min_max_match_input_extremes() {
        // Input: -0.7, 0.1, 0.9, -0.3 — min=-0.7, max=0.9
        let chunks = run(4, &[-0.7, 0.1, 0.9, -0.3]);
        assert_eq!(chunks.len(), 1);
        assert!((chunks[0].min - -0.7).abs() < 1e-6);
        assert!((chunks[0].max - 0.9).abs() < 1e-6);
    }

    #[test]
    fn rms_of_constant_signal_equals_signal() {
        let chunks = run(8, &[0.5; 8]);
        assert!(
            (chunks[0].rms - 0.5).abs() < 1e-6,
            "rms of constant 0.5 should be 0.5, got {}",
            chunks[0].rms
        );
    }

    #[test]
    fn rms_of_alternating_unit_signal_equals_one() {
        // [1, -1, 1, -1] → mean(sq) = 1, rms = 1
        let chunks = run(4, &[1.0, -1.0, 1.0, -1.0]);
        assert!((chunks[0].rms - 1.0).abs() < 1e-6);
        assert!((chunks[0].min - -1.0).abs() < 1e-6);
        assert!((chunks[0].max - 1.0).abs() < 1e-6);
    }

    #[test]
    fn silence_chunk_is_zero() {
        let chunks = run(4, &[0.0, 0.0, 0.0, 0.0]);
        assert_eq!(chunks[0].min, 0.0);
        assert_eq!(chunks[0].max, 0.0);
        assert_eq!(chunks[0].rms, 0.0);
    }

    // -------- Reset / flush --------

    #[test]
    fn reset_drops_partial_without_emit() {
        let mut d = Decimator::new(8);
        let mut out = Vec::new();
        d.feed(&[0.5, 0.5, 0.5], |c| out.push(c));
        assert_eq!(d.pending(), 3);
        d.reset();
        assert_eq!(d.pending(), 0);
        assert!(out.is_empty());
        // After reset, the next full chunk should be correct.
        d.feed(&[0.1; 8], |c| out.push(c));
        assert_eq!(out.len(), 1);
        assert!((out[0].rms - 0.1).abs() < 1e-6);
    }

    #[test]
    fn flush_emits_partial_with_correct_rms() {
        let mut d = Decimator::new(8);
        let mut out = Vec::new();
        // Feed 3 samples then flush — should emit a partial whose
        // rms is over those 3 samples (not 8).
        d.feed(&[1.0, 1.0, 1.0], |c| out.push(c));
        d.flush(|c| out.push(c));
        assert_eq!(out.len(), 1);
        assert!(
            (out[0].rms - 1.0).abs() < 1e-6,
            "partial-chunk rms should be over the 3 fed samples, got {}",
            out[0].rms
        );
        assert_eq!(d.pending(), 0, "flush must reset state");
    }

    #[test]
    fn flush_with_no_partial_emits_nothing() {
        let mut d = Decimator::new(4);
        let mut out = Vec::new();
        d.feed(&[0.1, 0.2, 0.3, 0.4], |c| out.push(c));
        assert_eq!(out.len(), 1);
        d.flush(|c| out.push(c));
        assert_eq!(out.len(), 1, "flush with empty partial must not emit");
    }

    // -------- Stress / large input --------

    #[test]
    fn ramp_of_known_length_produces_expected_chunk_count() {
        // 1000 samples at spc=64 → 1000 / 64 = 15 full chunks, 40
        // leftover samples in the partial.
        let mut samples = vec![0.0_f32; 1000];
        for (i, s) in samples.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            {
                *s = (i as f32) / 1000.0;
            }
        }
        let mut d = Decimator::new(64);
        let mut out = Vec::new();
        d.feed(&samples, |c| out.push(c));
        assert_eq!(out.len(), 15);
        assert_eq!(d.pending(), 40);
    }

    #[test]
    fn ramp_chunks_are_strictly_increasing_in_max() {
        // For a monotonically-increasing input, max of chunk N must
        // be strictly greater than max of chunk N-1.
        let n_samples = 8 * 64;
        let mut samples = vec![0.0_f32; n_samples];
        for (i, s) in samples.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            {
                *s = (i as f32) / n_samples as f32;
            }
        }
        let chunks = run(64, &samples);
        assert_eq!(chunks.len(), 8);
        for w in chunks.windows(2) {
            assert!(
                w[1].max > w[0].max,
                "ramp chunks should have monotonically increasing max"
            );
        }
    }

    #[test]
    fn block_size_does_not_change_output() {
        // Feed the same 256-sample buffer in 1-sample, 7-sample, and
        // whole-buffer increments. All three runs must produce
        // identical chunk sequences.
        let mut samples = vec![0.0_f32; 256];
        for (i, s) in samples.iter_mut().enumerate() {
            let f = (i as f32).mul_add(0.01, -1.28);
            *s = f.sin();
        }

        let mut by_one = Vec::new();
        let mut d1 = Decimator::new(64);
        for s in &samples {
            d1.feed(std::slice::from_ref(s), |c| by_one.push(c));
        }

        let mut by_seven = Vec::new();
        let mut d7 = Decimator::new(64);
        for chunk in samples.chunks(7) {
            d7.feed(chunk, |c| by_seven.push(c));
        }

        let mut by_all = Vec::new();
        let mut da = Decimator::new(64);
        da.feed(&samples, |c| by_all.push(c));

        assert_eq!(by_one, by_seven, "block size 1 vs 7 differs");
        assert_eq!(by_one, by_all, "block size 1 vs all differs");
    }
}
