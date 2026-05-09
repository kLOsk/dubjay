//! Dub audio engine — core types, decks, and RT-safety primitives.
//!
//! See PRD §4 for the design principles. The audio thread is sacred:
//! no allocation, no locks, no syscalls inside the render callback.
//!
//! M1 ships:
//!
//! - [`RealtimeContext`] — a lifetime-bounded token type that gates which
//!   APIs may be called inside a render callback.
//! - [`Deck`] — a single deck that plays a [`dub_io::Track`] forward or
//!   backward at any rate, with linear interpolation. Forward and backward
//!   share one code path (PRD §4.4).
//! - [`Engine`] — a 2-deck mixing engine, routing each deck's render into
//!   a single stereo output bus.
//!
//! Everything substantive (graph wiring, transport, FX) lands in subsequent
//! milestones.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
// Apple/vendor product names ("CoreAudio", "AudioUnit") are not Rust symbols;
// clippy::doc_markdown is wrong to demand backticks in prose.
#![allow(clippy::doc_markdown)]

mod command;
mod deck;
pub mod declick;
mod handle;
pub mod realtime;

pub use command::Command;
pub use deck::Deck;
pub use handle::{CommandError, DeckCommand, DeckSnapshot, EngineHandle};
pub use realtime::{RealtimeContext, RtError};

/// Library version reported by the crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Number of decks in v1 (PRD §3 / §6).
pub const DECK_COUNT: usize = 2;

/// Top-level engine. Sits between the platform audio I/O and the audio graph.
///
/// Holds [`DECK_COUNT`] decks. The render method drains the command queue
/// (M2), zeros the output buffer, renders each deck additively into it,
/// and returns. CoreAudio (M1.4) calls this once per audio block from the
/// IO proc.
///
/// When constructed via [`Engine::new_with_handle`] the engine also owns
/// the producer end of the trash channel: any `Arc<Track>` swapped off a
/// deck (M3) is bounced back to the main thread for disposal there. The
/// audio thread *never* drops `Arc<Track>` (which would call `dealloc`,
/// a syscall, on a real-time thread).
pub struct Engine {
    sample_rate: f32,
    block_size: usize,
    decks: [Deck; DECK_COUNT],
    /// Engine-wide master gain applied after the deck sum in the debug
    /// internal mixer (M4). `1.0` is unity. PRD §5.3: external-mixer mode
    /// (M5+) bypasses the master since each deck would route to its own
    /// physical output pair raw — for now the engine has only one summed
    /// output bus, so the master always applies.
    master_gain: f32,
    /// Optional consumer end of the UI → engine command channel. `None`
    /// for offline/test engines built via [`Engine::new`]; populated by
    /// [`Engine::new_with_handle`].
    cmd_rx: Option<ringbuf::HeapCons<Command>>,
    /// Producer end of the audio → main trash channel. `None` for
    /// offline/test engines.
    trash_tx: Option<ringbuf::HeapProd<std::sync::Arc<dub_io::Track>>>,
    /// Atomic counter incremented every time the trash channel was full
    /// when an old `Arc<Track>` needed to go back. Surfaced to the UI
    /// via [`EngineHandle::trash_overflow_count`].
    trash_overflow: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl Engine {
    /// Create a new engine without a control channel. Suitable for
    /// offline rendering, golden tests, or any single-threaded use where
    /// the caller drives state directly via [`Engine::deck_mut`].
    ///
    /// **Not the audio thread.** Allocations occur.
    #[must_use]
    pub fn new(sample_rate: f32, block_size: usize) -> Self {
        let envelope = declick::DeclickEnvelope::new(sample_rate, declick::DEFAULT_DECLICK_MS);
        Self {
            sample_rate,
            block_size,
            decks: std::array::from_fn(|_| Deck::new(envelope.clone())),
            master_gain: 1.0,
            cmd_rx: None,
            trash_tx: None,
            trash_overflow: None,
        }
    }

    /// Create a new engine paired with an [`EngineHandle`]. The handle
    /// lives on the main thread; the engine moves into the audio thread
    /// (typically via `dub_audio::AudioOutput::start`). This is the
    /// production constructor.
    ///
    /// **Not the audio thread.** Allocates the ringbufs and the per-deck
    /// shared state Arcs.
    #[must_use]
    pub fn new_with_handle(sample_rate: f32, block_size: usize) -> (Self, EngineHandle) {
        let envelope = declick::DeclickEnvelope::new(sample_rate, declick::DEFAULT_DECLICK_MS);
        let decks: [Deck; DECK_COUNT] = std::array::from_fn(|_| Deck::new(envelope.clone()));
        let shared: [std::sync::Arc<deck::DeckSharedState>; DECK_COUNT] =
            std::array::from_fn(|i| decks[i].shared());
        let (handle, side) = EngineHandle::new(shared);
        let engine = Self {
            sample_rate,
            block_size,
            decks,
            master_gain: 1.0,
            cmd_rx: Some(side.cmd_rx),
            trash_tx: Some(side.trash_tx),
            trash_overflow: Some(side.overflow_counter),
        };
        (engine, handle)
    }

    /// Engine-wide master gain (linear, default 1.0). Used by the debug
    /// internal mixer.
    #[must_use]
    pub fn master_gain(&self) -> f32 {
        self.master_gain
    }

    /// Set the master gain. Off-RT (called from `Engine::new_with_handle`'s
    /// owning thread, or via [`Command::SetMasterGain`] on the audio
    /// thread). Negative values invert overall phase; out-of-range values
    /// are accepted (the engine doesn't clamp — that's a UI concern).
    pub fn set_master_gain(&mut self, gain: f32) {
        self.master_gain = gain;
    }

    /// Sample rate this engine was configured for.
    #[must_use]
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Block size (frames per render call).
    #[must_use]
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Borrow a deck mutably. Caller is responsible for not invoking
    /// non-RT-safe operations from the audio thread.
    #[must_use]
    pub fn deck_mut(&mut self, idx: usize) -> &mut Deck {
        &mut self.decks[idx]
    }

    /// Borrow a deck.
    #[must_use]
    pub fn deck(&self, idx: usize) -> &Deck {
        &self.decks[idx]
    }

    /// Render one block of audio.
    ///
    /// Called from the audio thread. The [`RealtimeContext`] argument is
    /// the only way to invoke RT-safe APIs.
    ///
    /// `out` is interleaved stereo, length must be even (`2 * frames`).
    /// The buffer is **zeroed** at the start; each deck's contribution is
    /// mixed in additively (`+=`).
    ///
    /// `block_size` on the engine is a hint, not a hard constraint:
    /// CoreAudio (and other host APIs) may hand us variable buffer sizes
    /// on each callback, and we honour whatever we're given.
    pub fn render(&mut self, rt: &mut RealtimeContext<'_>, out: &mut [f32]) {
        debug_assert_eq!(
            out.len() % 2,
            0,
            "stereo output buffer must have even length"
        );
        rt.tick();

        // Drain the command queue first so the upcoming block reflects
        // the latest UI state. RT-safe: try_pop is a load + index, no
        // allocation; apply_command writes plain fields and atomics.
        self.drain_commands();

        for sample in out.iter_mut() {
            *sample = 0.0;
        }
        let sr = self.sample_rate;
        for deck in &mut self.decks {
            deck.render(rt, out, sr);
        }

        // Master gain (M4): single multiplicative scale across the entire
        // summed stereo bus. Applied after deck mixing so per-deck gains
        // and the master compose multiplicatively. Skipping the multiply
        // when master==1.0 saves a per-block branch on the common case
        // and lets future LTO inline this loop away entirely.
        if (self.master_gain - 1.0).abs() > f32::EPSILON {
            let g = self.master_gain;
            for sample in out.iter_mut() {
                *sample *= g;
            }
        }

        // After render, harvest any Arc<Track> orphaned by completed
        // de-click ramps in this block (and any pending disposals from
        // back-to-back transport changes that the per-command sweep
        // didn't catch). This is the contract that lets the audio
        // thread mutate transport state without ever calling Arc::drop.
        for idx in 0..DECK_COUNT {
            self.sweep_deck_disposal(idx);
        }
    }

    /// Drain every pending command, applying each to the engine.
    ///
    /// Bounded by the channel capacity (256). At a 48 kHz / 64-frame block
    /// rate that is ~750 audio blocks per second; a single block draining
    /// 256 small commands is trivial (~µs). Also safe to call when there
    /// is no channel (returns immediately).
    fn drain_commands(&mut self) {
        use ringbuf::traits::Consumer;
        // `take()` lets us pop from `rx` while still mutating
        // `self.decks` / `self.trash_tx` inside the loop. The Option is
        // restored at the end; no allocation.
        let Some(mut rx) = self.cmd_rx.take() else {
            return;
        };
        while let Some(cmd) = rx.try_pop() {
            self.apply_command(cmd);
        }
        self.cmd_rx = Some(rx);
    }

    /// Apply a single command. Inlined as a method so
    /// [`Command::DeckLoad`] can route the displaced `Arc<Track>`
    /// through the trash channel without dropping it on the audio thread.
    fn apply_command(&mut self, cmd: Command) {
        match cmd {
            Command::DeckPlay { idx } => {
                if let Some(d) = self.decks.get_mut(idx as usize) {
                    d.set_playing(true);
                }
            }
            Command::DeckPause { idx } => {
                if let Some(d) = self.decks.get_mut(idx as usize) {
                    d.set_playing(false);
                }
            }
            Command::DeckSeek {
                idx,
                position_frames,
            } => {
                if let Some(d) = self.decks.get_mut(idx as usize) {
                    d.set_position_frames(position_frames);
                }
            }
            Command::DeckSetRate { idx, rate } => {
                if let Some(d) = self.decks.get_mut(idx as usize) {
                    d.set_rate(rate);
                }
            }
            Command::DeckSetGain { idx, gain } => {
                if let Some(d) = self.decks.get_mut(idx as usize) {
                    d.set_gain(gain);
                }
            }
            Command::DeckLoad { idx, source } => {
                let Some(d) = self.decks.get_mut(idx as usize) else {
                    // Bad idx: bounce the new Arc back through the trash
                    // channel rather than drop here. Symmetric with the
                    // valid path — the audio thread never drops Arcs.
                    self.send_to_trash(source);
                    return;
                };
                d.swap_source(source);
                // The OLD Arc<Track> (if any) now lives inside the
                // deck's de-click state for the duration of the ramp;
                // we'll harvest it after the next render block via
                // `take_finished_declick_source`. If there was already
                // a ramp in flight when we called swap, an even-older
                // Arc may have been displaced into the deck's
                // pending_disposal slot — sweep that immediately.
                self.sweep_deck_disposal(idx as usize);
            }
            Command::SetMasterGain { gain } => {
                self.master_gain = gain;
            }
        }
    }

    /// Harvest any `Arc<Track>` orphaned by this deck — finished
    /// crossfades and pending disposals from back-to-back transport
    /// changes — and route them through the trash channel. Cheap
    /// (mostly two `Option::take()` checks) and called from the
    /// engine's command-application and post-render sweeps.
    fn sweep_deck_disposal(&mut self, idx: usize) {
        // Two-step take: borrow `decks` only as long as we're popping
        // each Option, then release before calling `send_to_trash`
        // which borrows `self.trash_tx`. Avoids `cannot borrow *self
        // as mutable more than once`.
        let pending = self
            .decks
            .get_mut(idx)
            .and_then(Deck::take_pending_disposal);
        if let Some(arc) = pending {
            self.send_to_trash(arc);
        }
        let finished = self
            .decks
            .get_mut(idx)
            .and_then(Deck::take_finished_declick_source);
        if let Some(arc) = finished {
            self.send_to_trash(arc);
        }
    }

    /// Push an old `Arc<Track>` back to the main thread for disposal.
    /// On overflow (channel full) `mem::forget` the Arc and bump the
    /// overflow counter — leaking is the lesser evil compared with a
    /// `dealloc` on the audio thread, and the counter surfaces the
    /// violation to the UI.
    fn send_to_trash(&mut self, arc: std::sync::Arc<dub_io::Track>) {
        use ringbuf::traits::Producer;
        let Some(trash_tx) = self.trash_tx.as_mut() else {
            // No trash channel — offline engine. `Engine::new` is by
            // definition not the audio thread, so a `dealloc` here is
            // acceptable. (DeckLoad is unreachable here in practice
            // because it can only be sent through an EngineHandle, and
            // EngineHandle is only paired with the channel-bearing
            // Engine.)
            drop(arc);
            return;
        };
        if let Err(rejected) = trash_tx.try_push(arc) {
            std::mem::forget(rejected);
            if let Some(counter) = self.trash_overflow.as_ref() {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use assert_no_alloc::AllocDisabler;
    use dub_io::Track;

    #[global_allocator]
    static A: AllocDisabler = AllocDisabler;

    #[test]
    fn engine_constructs() {
        let engine = Engine::new(48_000.0, 64);
        assert!((engine.sample_rate() - 48_000.0).abs() < f32::EPSILON);
        assert_eq!(engine.block_size(), 64);
        for i in 0..DECK_COUNT {
            assert!(engine.deck(i).source().is_none());
        }
    }

    #[test]
    fn render_with_no_decks_loaded_is_silent() {
        let mut engine = Engine::new(48_000.0, 64);
        let mut buffer = vec![1.0f32; 128];
        let mut rt = RealtimeContext::new();

        assert_no_alloc::assert_no_alloc(|| {
            engine.render(&mut rt, &mut buffer);
        });

        #[allow(clippy::float_cmp)]
        for sample in &buffer {
            assert_eq!(*sample, 0.0);
        }
    }

    #[test]
    fn render_with_loaded_deck_is_alloc_free() {
        let track = Arc::new(
            Track::from_interleaved(vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8], 48_000, 2)
                .unwrap(),
        );
        let mut engine = Engine::new(48_000.0, 4);
        engine.deck_mut(0).set_source(track);
        engine.deck_mut(0).set_playing(true);
        // Skip past the M3.5 fade-in ramp so we observe raw playback.
        engine.deck_mut(0).quiesce_declick_for_test();

        let mut buffer = vec![0.0f32; 8];
        let mut rt = RealtimeContext::new();

        assert_no_alloc::assert_no_alloc(|| {
            engine.render(&mut rt, &mut buffer);
        });

        // Track is { 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8 } interleaved
        // (4 stereo frames). Engine renders 4 frames at unity rate from
        // position 0. Output should match the track exactly.
        let expected = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];
        for (got, want) in buffer.iter().zip(expected.iter()) {
            assert!((got - want).abs() < 1e-6, "got {got} want {want}");
        }
    }

    #[test]
    fn handle_play_and_pause_apply_on_render() {
        // Track value is constant 0.5 across all samples. The deck
        // applies a 2 ms fade on play and pause (M3.5 de-click). We
        // render enough frames to span the fade and assert on the
        // post-fade region.
        let track = Arc::new(Track::from_interleaved(vec![0.5; 1024], 48_000, 2).unwrap());
        let (mut engine, mut handle) = Engine::new_with_handle(48_000.0, 4);
        engine.deck_mut(0).set_source(track);
        engine.deck_mut(0).quiesce_declick_for_test();

        // Default state: not playing → first render is silent.
        let mut buffer = vec![0.0f32; 8];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut buffer);
        #[allow(clippy::float_cmp)]
        for s in &buffer {
            assert_eq!(*s, 0.0);
        }

        // Send Play → fade-in starts. Render 256 frames (well past the
        // 96-sample / 2 ms fade at 48k) and check post-fade samples.
        handle.deck(0).play().unwrap();
        let mut buffer = vec![0.0f32; 256 * 2];
        engine.render(&mut rt, &mut buffer);
        // Before fade: small values (fade-in starts at 0). After fade
        // (samples ≥ 96): exactly track value 0.5.
        for (i, s) in buffer.chunks_exact(2).enumerate().skip(100) {
            assert!(
                (s[0] - 0.5).abs() < 1e-6,
                "post-fade frame {i} L={}, expected 0.5",
                s[0]
            );
        }

        // Snapshot reflects audio thread state.
        let snap = handle.deck_state(0).unwrap();
        assert!(snap.is_playing);
        assert!((snap.position_frames - 256.0).abs() < 1e-9);

        // Send Pause → fade-out starts. Render 256 frames again; the
        // post-fade region should be silence (deck paused).
        handle.deck(0).pause().unwrap();
        let mut buffer = vec![0.0f32; 256 * 2];
        engine.render(&mut rt, &mut buffer);
        for (i, s) in buffer.chunks_exact(2).enumerate().skip(100) {
            assert!(
                s[0].abs() < 1e-6,
                "post-fade frame {i} after pause: L={} (want silence)",
                s[0]
            );
        }
        let snap = handle.deck_state(0).unwrap();
        assert!(!snap.is_playing);
    }

    #[test]
    fn handle_seek_repositions_playhead() {
        // 1024-frame track of distinct ramp samples. Long enough to
        // span the fade window so we can read past it.
        let mut samples = Vec::with_capacity(2048);
        for i in 0..1024 {
            #[allow(clippy::cast_precision_loss)]
            let v = i as f32 * 0.001;
            samples.push(v);
            samples.push(v);
        }
        let track = Arc::new(Track::from_interleaved(samples, 48_000, 2).unwrap());

        let (mut engine, mut handle) = Engine::new_with_handle(48_000.0, 4);
        engine.deck_mut(0).set_source(track);
        engine.deck_mut(0).quiesce_declick_for_test();
        engine.deck_mut(0).set_playing(true);
        engine.deck_mut(0).quiesce_declick_for_test();

        // Seek to frame 100 via the handle. The seek triggers a 96-frame
        // declick fade. Render 256 frames and check the post-fade region.
        handle.deck(0).seek(100.0).unwrap();
        let mut buffer = vec![0.0f32; 256 * 2];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut buffer);

        // After the fade (frame ≥ 100 in the BUFFER, frame ≥ 200 in the
        // TRACK), output should be the linear ramp 100+i offset.
        for (i, s) in buffer.chunks_exact(2).enumerate().skip(100) {
            #[allow(clippy::cast_precision_loss)]
            let expected = (100 + i) as f32 * 0.001;
            assert!(
                (s[0] - expected).abs() < 1e-6,
                "frame {i}: got {} want {expected}",
                s[0]
            );
        }
        assert!((handle.deck_state(0).unwrap().position_frames - (100.0 + 256.0)).abs() < 1e-9);
    }

    #[test]
    fn drain_is_alloc_free() {
        let track = Arc::new(Track::from_interleaved(vec![0.1; 16], 48_000, 2).unwrap());
        let (mut engine, mut handle) = Engine::new_with_handle(48_000.0, 4);
        engine.deck_mut(0).set_source(track);

        // Pre-stage commands so the next render must drain them.
        handle.deck(0).play().unwrap();
        handle.deck(0).set_gain(0.5).unwrap();
        handle.deck(0).seek(2.0).unwrap();

        let mut buffer = vec![0.0f32; 8];
        let mut rt = RealtimeContext::new();
        assert_no_alloc::assert_no_alloc(|| {
            engine.render(&mut rt, &mut buffer);
        });
    }

    #[test]
    fn hot_load_swaps_source_and_returns_old_via_trash() {
        // Long tracks so the post-declick samples are still in range.
        let track_a = Arc::new(Track::from_interleaved(vec![0.5; 1024], 48_000, 2).unwrap());
        let track_b = Arc::new(Track::from_interleaved(vec![0.25; 1024], 48_000, 2).unwrap());

        // Track-A: us + the Arc::clone we'll hand to the engine = 2.
        let track_a_for_engine = track_a.clone();
        assert_eq!(Arc::strong_count(&track_a), 2);

        let (mut engine, mut handle) = Engine::new_with_handle(48_000.0, 4);
        engine.deck_mut(0).set_source(track_a_for_engine);
        engine.deck_mut(0).set_playing(true);
        engine.deck_mut(0).quiesce_declick_for_test();

        // Steady-state render: 4 frames of track A → 0.5.
        let mut buffer = vec![0.0f32; 8];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut buffer);
        for s in &buffer {
            assert!((s - 0.5).abs() < 1e-6);
        }

        // Hot-load track B. The DeckLoad applies during the next render
        // and triggers a declick fade A → B. Render 256 frames so we
        // span the fade and observe steady-state B in the tail.
        handle.deck(0).load(track_b.clone()).unwrap();
        let mut buffer = vec![0.0f32; 256 * 2];
        engine.render(&mut rt, &mut buffer);
        for (i, s) in buffer.chunks_exact(2).enumerate().skip(100) {
            assert!(
                (s[0] - 0.25).abs() < 1e-6,
                "post-fade frame {i}: got {} want 0.25",
                s[0]
            );
        }
        // The trash channel is fed by the post-render sweep when the
        // declick ramp finishes. With a 96-sample fade and a 256-frame
        // block, the ramp completed in this block, so the old Arc is
        // already in the trash channel.
        //
        // Strong count: us + trash channel slot = 2.
        assert_eq!(
            Arc::strong_count(&track_a),
            2,
            "old Arc should be in the trash channel after fade completes"
        );

        // Reclaim drops the old Arc on the main thread → strong_count
        // drops back to 1.
        let n = handle.reclaim();
        assert_eq!(n, 1, "reclaim should have dropped exactly one Arc");
        assert_eq!(Arc::strong_count(&track_a), 1);
        assert_eq!(handle.trash_overflow_count(), 0);
    }

    #[test]
    fn hot_load_drain_is_alloc_free() {
        let track_a = Arc::new(Track::from_interleaved(vec![0.1; 16], 48_000, 2).unwrap());
        let track_b = Arc::new(Track::from_interleaved(vec![0.2; 16], 48_000, 2).unwrap());
        let (mut engine, mut handle) = Engine::new_with_handle(48_000.0, 4);
        engine.deck_mut(0).set_source(track_a);

        // Pre-stage a load. The audio thread's drain must:
        //  1. pop the DeckLoad command (no alloc),
        //  2. swap the deck's Arc<Track> (no alloc — Arc::clone on
        //     handoff was already done by the sender),
        //  3. push the old Arc into the trash channel (no alloc — the
        //     channel storage is pre-allocated).
        handle.deck(0).load(track_b).unwrap();

        let mut buffer = vec![0.0f32; 8];
        let mut rt = RealtimeContext::new();
        assert_no_alloc::assert_no_alloc(|| {
            engine.render(&mut rt, &mut buffer);
        });
    }

    #[test]
    fn auto_reclaim_on_load_keeps_trash_drained() {
        // A user-style usage: load 100 tracks back to back without ever
        // calling reclaim explicitly. The auto-drain in `load()` should
        // keep the trash channel from filling up (capacity is 32).
        let (mut engine, mut handle) = Engine::new_with_handle(48_000.0, 4);
        let mut buffer = vec![0.0f32; 8];
        let mut rt = RealtimeContext::new();
        for _ in 0..100 {
            let t = Arc::new(Track::from_interleaved(vec![0.1; 8], 48_000, 2).unwrap());
            handle.deck(0).load(t).unwrap();
            engine.render(&mut rt, &mut buffer);
        }
        assert_eq!(handle.trash_overflow_count(), 0);
    }

    #[test]
    fn two_decks_mix_additively() {
        let track_a = Arc::new(Track::from_interleaved(vec![0.5; 8], 48_000, 2).unwrap());
        let track_b = Arc::new(Track::from_interleaved(vec![0.25; 8], 48_000, 2).unwrap());
        let mut engine = Engine::new(48_000.0, 4);
        engine.deck_mut(0).set_source(track_a);
        engine.deck_mut(0).set_playing(true);
        engine.deck_mut(0).quiesce_declick_for_test();
        engine.deck_mut(1).set_source(track_b);
        engine.deck_mut(1).set_playing(true);
        engine.deck_mut(1).quiesce_declick_for_test();

        let mut buffer = vec![0.0f32; 8];
        let mut rt = RealtimeContext::new();
        assert_no_alloc::assert_no_alloc(|| {
            engine.render(&mut rt, &mut buffer);
        });

        // 0.5 + 0.25 = 0.75 on every sample.
        for s in buffer {
            assert!((s - 0.75).abs() < 1e-6);
        }
    }

    // ============================================================
    //                       M4 mixer tests
    // ============================================================

    #[test]
    fn master_gain_scales_summed_output() {
        // Each deck contributes its raw value; master multiplies the sum.
        // Deck 0 = 0.5, deck 1 = 0.25, master = 0.5 → output = 0.5 * 0.75 = 0.375.
        let track_a = Arc::new(Track::from_interleaved(vec![0.5; 8], 48_000, 2).unwrap());
        let track_b = Arc::new(Track::from_interleaved(vec![0.25; 8], 48_000, 2).unwrap());
        let mut engine = Engine::new(48_000.0, 4);
        engine.deck_mut(0).set_source(track_a);
        engine.deck_mut(0).set_playing(true);
        engine.deck_mut(0).quiesce_declick_for_test();
        engine.deck_mut(1).set_source(track_b);
        engine.deck_mut(1).set_playing(true);
        engine.deck_mut(1).quiesce_declick_for_test();
        engine.set_master_gain(0.5);

        let mut buffer = vec![0.0f32; 8];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut buffer);
        for s in buffer {
            assert!((s - 0.375).abs() < 1e-6, "got {s} want 0.375");
        }
    }

    #[test]
    fn master_gain_unity_is_pass_through() {
        // Default master_gain = 1.0; output equals raw deck sum.
        let track_a = Arc::new(Track::from_interleaved(vec![0.5; 8], 48_000, 2).unwrap());
        let mut engine = Engine::new(48_000.0, 4);
        engine.deck_mut(0).set_source(track_a);
        engine.deck_mut(0).set_playing(true);
        engine.deck_mut(0).quiesce_declick_for_test();

        let mut buffer = vec![0.0f32; 8];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut buffer);
        for s in buffer {
            assert!((s - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn master_gain_command_applies_via_handle() {
        let track = Arc::new(Track::from_interleaved(vec![0.5; 1024], 48_000, 2).unwrap());
        let (mut engine, mut handle) = Engine::new_with_handle(48_000.0, 64);
        engine.deck_mut(0).set_source(track);
        engine.deck_mut(0).set_playing(true);
        engine.deck_mut(0).quiesce_declick_for_test();

        // Send master gain = 0.25 via the lock-free channel.
        handle.set_master_gain(0.25).unwrap();

        let mut buffer = vec![0.0f32; 64 * 2];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut buffer);

        for s in buffer {
            assert!(
                (s - 0.125).abs() < 1e-6,
                "got {s} want 0.125 (= 0.5 * 0.25)"
            );
        }
    }

    #[test]
    fn two_decks_independent_transport_via_handle() {
        // Deck 0 plays, deck 1 paused → output should be deck 0 only.
        let track_a = Arc::new(Track::from_interleaved(vec![0.5; 1024], 48_000, 2).unwrap());
        let track_b = Arc::new(Track::from_interleaved(vec![0.25; 1024], 48_000, 2).unwrap());
        let (mut engine, mut handle) = Engine::new_with_handle(48_000.0, 64);
        engine.deck_mut(0).set_source(track_a);
        engine.deck_mut(0).set_playing(true);
        engine.deck_mut(0).quiesce_declick_for_test();
        engine.deck_mut(1).set_source(track_b);
        // Deck 1 source is loaded but not playing.
        engine.deck_mut(1).quiesce_declick_for_test();

        let mut buffer = vec![0.0f32; 64 * 2];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut buffer);
        // Only deck 0 contributes.
        for s in &buffer {
            assert!((s - 0.5).abs() < 1e-6, "deck 1 should be silent: got {s}");
        }

        // Now play deck 1 too via handle.
        handle.deck(1).play().unwrap();
        let mut buffer = vec![0.0f32; 256 * 2];
        engine.render(&mut rt, &mut buffer);
        // Past the per-deck declick fade-in, output is 0.5 + 0.25 = 0.75.
        for (i, s) in buffer.chunks_exact(2).enumerate().skip(100) {
            assert!(
                (s[0] - 0.75).abs() < 1e-6,
                "post-fade frame {i}: got {} want 0.75",
                s[0]
            );
        }
    }

    #[test]
    fn master_gain_path_is_alloc_free() {
        let track = Arc::new(Track::from_interleaved(vec![0.5; 1024], 48_000, 2).unwrap());
        let (mut engine, mut handle) = Engine::new_with_handle(48_000.0, 64);
        engine.deck_mut(0).set_source(track);
        engine.deck_mut(0).set_playing(true);
        handle.set_master_gain(0.5).unwrap();

        let mut buffer = vec![0.0f32; 64 * 2];
        let mut rt = RealtimeContext::new();
        assert_no_alloc::assert_no_alloc(|| {
            engine.render(&mut rt, &mut buffer);
        });
    }
}
