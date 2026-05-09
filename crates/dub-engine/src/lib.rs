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
pub struct Engine {
    sample_rate: f32,
    block_size: usize,
    decks: [Deck; DECK_COUNT],
    /// Optional consumer end of the UI → engine command channel. `None`
    /// for offline/test engines built via [`Engine::new`]; populated by
    /// [`Engine::new_with_handle`].
    cmd_rx: Option<ringbuf::HeapCons<Command>>,
}

impl Engine {
    /// Create a new engine without a control channel. Suitable for
    /// offline rendering, golden tests, or any single-threaded use where
    /// the caller drives state directly via [`Engine::deck_mut`].
    ///
    /// **Not the audio thread.** Allocations occur.
    #[must_use]
    pub fn new(sample_rate: f32, block_size: usize) -> Self {
        Self {
            sample_rate,
            block_size,
            decks: std::array::from_fn(|_| Deck::new()),
            cmd_rx: None,
        }
    }

    /// Create a new engine paired with an [`EngineHandle`]. The handle
    /// lives on the main thread; the engine moves into the audio thread
    /// (typically via `dub_audio::AudioOutput::start`). This is the
    /// production constructor.
    ///
    /// **Not the audio thread.** Allocates the ringbuf and the per-deck
    /// shared state Arcs.
    #[must_use]
    pub fn new_with_handle(sample_rate: f32, block_size: usize) -> (Self, EngineHandle) {
        let decks: [Deck; DECK_COUNT] = std::array::from_fn(|_| Deck::new());
        let shared: [std::sync::Arc<deck::DeckSharedState>; DECK_COUNT] =
            std::array::from_fn(|i| decks[i].shared());
        let (handle, rx) = EngineHandle::new(shared);
        let engine = Self {
            sample_rate,
            block_size,
            decks,
            cmd_rx: Some(rx),
        };
        (engine, handle)
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
    }

    /// Drain every pending command, applying each to the engine.
    ///
    /// Bounded by the channel capacity (256). At a 48 kHz / 64-frame block
    /// rate that is ~750 audio blocks per second; a single block draining
    /// 256 small commands is trivial (~µs). Also safe to call when there
    /// is no channel (returns immediately).
    fn drain_commands(&mut self) {
        use ringbuf::traits::Consumer;
        let Some(rx) = self.cmd_rx.as_mut() else {
            return;
        };
        while let Some(cmd) = rx.try_pop() {
            apply_command(&mut self.decks, cmd);
        }
    }
}

/// Apply a single command to the deck array. Free function so it borrows
/// only `decks`, leaving the rest of `Engine` untouched (avoids fighting
/// the borrow checker over `cmd_rx` while we mutate decks during drain).
fn apply_command(decks: &mut [Deck; DECK_COUNT], cmd: Command) {
    match cmd {
        Command::DeckPlay { idx } => {
            if let Some(d) = decks.get_mut(idx as usize) {
                d.set_playing(true);
            }
        }
        Command::DeckPause { idx } => {
            if let Some(d) = decks.get_mut(idx as usize) {
                d.set_playing(false);
            }
        }
        Command::DeckSeek {
            idx,
            position_frames,
        } => {
            if let Some(d) = decks.get_mut(idx as usize) {
                d.set_position_frames(position_frames);
            }
        }
        Command::DeckSetRate { idx, rate } => {
            if let Some(d) = decks.get_mut(idx as usize) {
                d.set_rate(rate);
            }
        }
        Command::DeckSetGain { idx, gain } => {
            if let Some(d) = decks.get_mut(idx as usize) {
                d.set_gain(gain);
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
        let track = Arc::new(Track::from_interleaved(vec![0.5; 16], 48_000, 2).unwrap());
        let (mut engine, mut handle) = Engine::new_with_handle(48_000.0, 4);
        engine.deck_mut(0).set_source(track);

        // Default state: not playing → first render is silent.
        let mut buffer = vec![0.0f32; 8];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut buffer);
        #[allow(clippy::float_cmp)]
        for s in &buffer {
            assert_eq!(*s, 0.0);
        }

        // Send Play → next render produces audio (the track is 0.5s).
        handle.deck(0).play().unwrap();
        let mut buffer = vec![0.0f32; 8];
        engine.render(&mut rt, &mut buffer);
        for s in &buffer {
            assert!((s - 0.5).abs() < 1e-6, "expected 0.5, got {s}");
        }

        // Snapshot reflects audio thread state.
        let snap = handle.deck_state(0).unwrap();
        assert!(snap.is_playing);
        assert!((snap.position_frames - 4.0).abs() < 1e-9);

        // Send Pause → next render advances no further (output starts
        // from where we paused, but with playing=false we render silence).
        handle.deck(0).pause().unwrap();
        let mut buffer = vec![0.0f32; 8];
        engine.render(&mut rt, &mut buffer);
        #[allow(clippy::float_cmp)]
        for s in &buffer {
            assert_eq!(*s, 0.0);
        }
        let snap = handle.deck_state(0).unwrap();
        assert!(!snap.is_playing);
    }

    #[test]
    fn handle_seek_repositions_playhead() {
        let mut samples = Vec::with_capacity(40);
        for i in 0..20 {
            #[allow(clippy::cast_precision_loss)]
            let v = i as f32 * 0.01;
            samples.push(v);
            samples.push(v);
        }
        let track = Arc::new(Track::from_interleaved(samples, 48_000, 2).unwrap());

        let (mut engine, mut handle) = Engine::new_with_handle(48_000.0, 4);
        engine.deck_mut(0).set_source(track);
        handle.deck(0).play().unwrap();

        // Seek to frame 10 then render 4 frames.
        handle.deck(0).seek(10.0).unwrap();
        let mut buffer = vec![0.0f32; 8];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut buffer);

        // Expected: frames 10..14 → 0.10, 0.11, 0.12, 0.13.
        let expected = [0.10, 0.10, 0.11, 0.11, 0.12, 0.12, 0.13, 0.13];
        for (g, w) in buffer.iter().zip(expected.iter()) {
            assert!((g - w).abs() < 1e-6, "got {g} want {w}");
        }
        assert!((handle.deck_state(0).unwrap().position_frames - 14.0).abs() < 1e-9);
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
    fn two_decks_mix_additively() {
        let track_a = Arc::new(Track::from_interleaved(vec![0.5; 8], 48_000, 2).unwrap());
        let track_b = Arc::new(Track::from_interleaved(vec![0.25; 8], 48_000, 2).unwrap());
        let mut engine = Engine::new(48_000.0, 4);
        engine.deck_mut(0).set_source(track_a);
        engine.deck_mut(0).set_playing(true);
        engine.deck_mut(1).set_source(track_b);
        engine.deck_mut(1).set_playing(true);

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
}
