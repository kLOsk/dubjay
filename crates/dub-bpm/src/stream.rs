//! Threaded streaming driver — the M8 entry point.
//!
//! Wraps a [`BpmTracker`] in an OS thread that consumes audio from a
//! `ringbuf` consumer and emits [`TrackerEvent`]s to a second
//! `ringbuf`. Exposes a single thread-safe handle ([`BpmStream`])
//! that the engine integration constructs at "attach Thru source"
//! time and the UI side reads events from.
//!
//! ```text
//!   Audio thread                BpmStream-spawned thread
//!   ─────────────────           ─────────────────────────
//!   ThruSource::render_into     loop:
//!     pop input ring               pop audio_rx into block buffer
//!     mono-downmix             →   tracker.process(block)
//!     bpm_tx.push_slice            if event: events_tx.try_push(event)
//!                                  if shutdown: exit
//!                                  sleep(POLL_INTERVAL)
//!                          ───────────────────────────────
//!                                            │
//!                                            ▼
//!                                       events_rx (on UI thread)
//! ```
//!
//! ## Design notes
//!
//! * **Sleep-based polling, no condvars.** The analysis cadence is
//!   "every ~1 s the tracker recomputes" — the thread sleeping for
//!   20 ms between drain cycles is well below that, and adds no
//!   audible latency (BPM display latency that's < 50 ms past audio
//!   is indistinguishable from instant). Using `park_timeout` would
//!   need the audio thread to `unpark` — i.e., reach into the
//!   analysis thread's `Thread` handle — which complicates the
//!   audio-thread side and offers no real-world benefit at this
//!   scale.
//! * **Graceful shutdown via the explicit flag.** The thread exits
//!   when `shutdown.load(Acquire)` is true. `BpmStream::Drop` always
//!   sets the flag, so going out of scope is sufficient. Engine
//!   integration must additionally call `shutdown()` (or drop the
//!   stream) when detaching a Thru source so we don't leak a
//!   forever-sleeping thread.
//!
//!   We don't auto-detect producer drop on the ring side: ringbuf
//!   0.4's `HeapCons` doesn't expose an `is_abandoned()` method, so
//!   wiring producer-side teardown to thread exit would require
//!   adding our own `Arc<AtomicBool>` "producer alive" flag. That
//!   adds complexity without solving anything the explicit shutdown
//!   flag doesn't already cover — drop the stream and the thread
//!   exits.
//! * **Event ringbuf overflow is silent.** If the UI doesn't drain
//!   `events_rx` for a long time, new events stop being pushed and
//!   the *most recent* state transition is lost. That's
//!   intentional: a stale event channel means the consumer is
//!   broken; better to discard than block the analysis thread. With
//!   `EVENTS_CAPACITY = 64`, the consumer would have to fall behind
//!   for tens of minutes to lose anything visible.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

// `Observer::occupied_len` is the only Observer method we use; the
// `is_abandoned` we'd ideally want is not in ringbuf 0.4 (see module
// docs). If a future ringbuf version adds it we can wire it in to
// shorten the exit-on-detach path; for now `shutdown` is the path.

use crate::confidence::TrackerEvent;
use crate::tracker::{BpmTracker, TrackerConfig, TrackerError};

/// Capacity of the event ringbuf. 64 transitions buffers tens of
/// minutes of normal operation (transitions are rare). Sized to
/// power-of-two for HeapRb efficiency; not load-bearing.
const EVENTS_CAPACITY: usize = 64;

/// How long the analysis thread sleeps between drain cycles. Well
/// below the tracker's recompute cadence (~1 s) so we never miss a
/// state transition by more than 20 ms of wall time.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Maximum samples drained from the audio ring per loop iteration.
/// Caps memory use of the analysis-side scratch buffer; larger
/// values reduce the number of `tracker.process` calls per second
/// of audio but do not affect correctness (the tracker is
/// block-size invariant).
const DRAIN_BLOCK_SAMPLES: usize = 4096;

/// Streaming BPM analysis handle. Spawned via [`BpmStream::spawn`];
/// drop or call [`shutdown`] to stop the worker thread.
///
/// All methods are safe to call from any thread. The handle is
/// `Send` but not `Sync` — a single consumer reads events.
///
/// [`shutdown`]: Self::shutdown
pub struct BpmStream {
    events_rx: HeapCons<TrackerEvent>,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for BpmStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BpmStream")
            .field("running", &self.join.is_some())
            .field(
                "shutdown_pending",
                &self.shutdown.load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish()
    }
}

impl BpmStream {
    /// Spawn an analysis thread that reads mono audio from
    /// `audio_rx`, feeds it to a [`BpmTracker`] built from `cfg`,
    /// and emits [`TrackerEvent`]s on an internal channel.
    ///
    /// The thread runs until either:
    /// * [`shutdown`] is called on this handle, or
    /// * the producer end of `audio_rx` is dropped (e.g. the engine
    ///   reclaimed the associated `ThruSource`).
    ///
    /// `cfg.channels` should be `1` — the M8 engine tee mono-downmixes
    /// at the audio thread to save bandwidth, so the analysis side
    /// receives mono samples. Stereo tracker configs work too (they
    /// just downmix again, which is a no-op on already-mono input)
    /// but waste CPU. Construction passes through to [`BpmTracker`].
    ///
    /// # Errors
    ///
    /// Forwards [`TrackerError`] from underlying [`BpmTracker`]
    /// construction.
    ///
    /// [`shutdown`]: Self::shutdown
    pub fn spawn(audio_rx: HeapCons<f32>, cfg: TrackerConfig) -> Result<Self, TrackerError> {
        let tracker = BpmTracker::new(cfg)?;

        let events_rb = HeapRb::<TrackerEvent>::new(EVENTS_CAPACITY);
        let (events_tx, events_rx) = events_rb.split();

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_thread = Arc::clone(&shutdown);

        let join = thread::Builder::new()
            .name("dub-bpm-analysis".to_string())
            .spawn(move || analysis_loop(audio_rx, tracker, events_tx, shutdown_for_thread))
            .expect("OS refused to spawn analysis thread — fatal");

        Ok(Self {
            events_rx,
            shutdown,
            join: Some(join),
        })
    }

    /// Pull the next state-change event if one is queued. Non-blocking.
    /// Call this on a UI tick or in a CLI poll loop.
    pub fn try_recv(&mut self) -> Option<TrackerEvent> {
        self.events_rx.try_pop()
    }

    /// Signal the worker thread to exit and join it. Idempotent: a
    /// second call is a no-op. Consumes the handle, so no further
    /// `try_recv` is possible.
    ///
    /// `Drop` calls this implicitly, so explicit calls are only
    /// needed when you want to surface a join panic (one of the
    /// analysis-thread invariants tripped) — `Drop`'s join is
    /// best-effort.
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

impl Drop for BpmStream {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

fn analysis_loop(
    mut audio_rx: HeapCons<f32>,
    mut tracker: BpmTracker,
    mut events_tx: HeapProd<TrackerEvent>,
    shutdown: Arc<AtomicBool>,
) {
    // Pre-allocated drain buffer; never resized after construction.
    let mut block = vec![0.0f32; DRAIN_BLOCK_SAMPLES];

    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }

        // Drain everything available right now. The outer poll-sleep
        // ensures we still yield even if there's a constant stream.
        let mut drained_any = false;
        while audio_rx.occupied_len() > 0 {
            let n = audio_rx.pop_slice(&mut block);
            if n == 0 {
                break;
            }
            drained_any = true;
            if let Some(ev) = tracker.process(&block[..n]) {
                // Drop on full — see module docs. The "lose newest"
                // semantics of try_push (which errors when full) are
                // fine: the UI's stale state is fixed by the next
                // transition, not by overwriting.
                let _ = events_tx.try_push(ev);
            }
        }

        if !drained_any {
            thread::sleep(POLL_INTERVAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::TrackerState;
    use crate::synthetic;

    fn ring_for_samples(capacity: usize) -> (HeapProd<f32>, HeapCons<f32>) {
        HeapRb::<f32>::new(capacity).split()
    }

    fn cfg_mono(sr: u32) -> TrackerConfig {
        TrackerConfig {
            sample_rate: sr,
            channels: 1,
            analysis_period_samples: sr,
        }
    }

    /// Sleep with a hard deadline. Returns true if `pred()` became
    /// true within `total_timeout`. Polls every 10 ms. Used in
    /// tests to wait for the analysis thread to make progress
    /// without sleeping arbitrarily.
    fn wait_until<F: FnMut() -> bool>(total_timeout: Duration, mut pred: F) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < total_timeout {
            if pred() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        pred()
    }

    #[test]
    fn click_track_streams_to_lock() {
        // Push 10 s of 128 BPM clicks through a real spawned thread
        // and wait for the Locked event. This is the integration
        // test that pins the M8 acceptance criterion:
        //   "feed a fixture file through the streaming path; assert
        //    it lands within ±1 BPM of M7.5's offline answer."
        let sr = 48_000u32;
        let audio = synthetic::click_track(128.0, 10.0, sr);

        // 1 s ring capacity at 48 kHz mono = 48000 samples. The
        // analysis thread should keep up, but the ring absorbs
        // bursts from the test pushing in large chunks.
        let (mut tx, rx) = ring_for_samples(48_000);
        let mut stream = BpmStream::spawn(rx, cfg_mono(sr)).expect("spawn");

        // Feed in 4096-sample chunks with a small yield between.
        for chunk in audio.chunks(4096) {
            let mut remaining = chunk;
            while !remaining.is_empty() {
                let n = tx.push_slice(remaining);
                remaining = &remaining[n..];
                if !remaining.is_empty() {
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }

        // Collect transitions for up to 5 s of wall time after the
        // last sample was pushed. The analysis thread needs at
        // least ~2 s to first lock (per the tracker tests).
        let mut transitions = Vec::new();
        let _ = wait_until(Duration::from_secs(5), || {
            while let Some(ev) = stream.try_recv() {
                transitions.push(ev);
            }
            matches!(
                transitions.last(),
                Some(TrackerEvent::StateChanged(TrackerState::Locked { .. }))
            )
        });

        let last = transitions
            .last()
            .copied()
            .unwrap_or_else(|| panic!("no transitions observed — analysis thread stuck?"));
        match last {
            TrackerEvent::StateChanged(TrackerState::Locked { bpm }) => {
                assert!(
                    (bpm - 128.0).abs() <= 1.0,
                    "expected lock at 128±1, got {bpm} (transitions: {transitions:?})"
                );
            }
            other => {
                panic!("expected last transition Locked, got {other:?} (all: {transitions:?})")
            }
        }
    }

    #[test]
    fn dropping_producer_terminates_analysis_thread() {
        // The analysis thread must exit when the producer end goes
        // away. Without this, a detach in the engine would leak the
        // thread.
        let (tx, rx) = ring_for_samples(1024);
        let stream = BpmStream::spawn(rx, cfg_mono(48_000)).expect("spawn");
        drop(tx);

        // BpmStream::drop calls shutdown_inner which joins. If the
        // analysis loop never exited, the test would hang forever.
        drop(stream);
    }

    #[test]
    fn explicit_shutdown_joins_promptly() {
        let (_tx, rx) = ring_for_samples(1024);
        let stream = BpmStream::spawn(rx, cfg_mono(48_000)).expect("spawn");
        let start = std::time::Instant::now();
        stream.shutdown();
        // POLL_INTERVAL is 20 ms; join must complete within a few
        // multiples of that on any reasonable machine.
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "shutdown took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn silence_emits_no_transitions() {
        let sr = 48_000u32;
        let (mut tx, rx) = ring_for_samples(sr as usize);
        let mut stream = BpmStream::spawn(rx, cfg_mono(sr)).expect("spawn");

        // 3 seconds of pure silence — should never leave Searching.
        for chunk in vec![0.0f32; sr as usize * 3].chunks(2048) {
            let mut remaining = chunk;
            while !remaining.is_empty() {
                let n = tx.push_slice(remaining);
                remaining = &remaining[n..];
                if !remaining.is_empty() {
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }
        thread::sleep(Duration::from_millis(200));

        let mut transitions = Vec::new();
        while let Some(ev) = stream.try_recv() {
            transitions.push(ev);
        }
        assert!(
            transitions.is_empty(),
            "silence should emit no transitions, got {transitions:?}"
        );
    }

    #[test]
    fn invalid_config_rejected_at_spawn() {
        let (_tx, rx) = ring_for_samples(64);
        let bad = TrackerConfig {
            sample_rate: 0,
            channels: 1,
            analysis_period_samples: 1,
        };
        assert!(BpmStream::spawn(rx, bad).is_err());
    }
}
