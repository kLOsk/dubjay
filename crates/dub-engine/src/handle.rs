//! Main-thread proxy for the audio engine.
//!
//! The audio thread owns the engine and decks. The main thread owns this
//! handle. Communication is one-way for mutations (ringbuf SPSC) and
//! read-only via atomic snapshots in the other direction. No locks, no
//! allocations on either side once the channel is created.
//!
//! ```text
//!  Main thread              ringbuf<Command>             Audio thread
//!  EngineHandle      ─────  try_push  ──────────►        Engine
//!     │                                                   │
//!     │             ◄────  Arc<DeckSharedState>  ────►    │
//!     └─ deck(i).position_frames()    atomic snapshot     └─ deck.render()
//! ```
//!
//! API is split deck-by-deck for ergonomics: `handle.deck(0).play()`,
//! `handle.deck(0).seek(48_000.0)`, etc. Each method returns
//! `Result<(), CommandError>` because the channel is bounded — if the audio
//! thread is gone or the buffer is full, the caller learns synchronously.

use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::command::Command;
use crate::deck::DeckSharedState;
use crate::DECK_COUNT;
use dub_io::Track;

/// Capacity of the command channel. Sized for far more than any plausible
/// burst of human input — at 60 Hz UI updates a deck would have to send a
/// command on every frame for ~4 seconds to fill this. Real bursts are
/// dozens of commands at most (a complex performance gesture).
const COMMAND_CHANNEL_CAPACITY: usize = 256;

/// Capacity of the trash channel that ferries old `Arc<Track>` from the
/// audio thread back to the main thread for disposal. Sized so that a
/// reasonable UI (calling `reclaim()` at ≥ 1 Hz) never approaches it.
/// 32 means the user would have to perform 32 untracked load operations
/// without the UI ever calling `reclaim()` to overflow — a programming
/// error that we surface via the overflow counter rather than a silent
/// memory leak or a forbidden audio-thread `dealloc`.
const TRASH_CHANNEL_CAPACITY: usize = 32;

/// Errors that can occur sending a command from the UI thread.
#[allow(missing_docs)]
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// The audio thread is no longer draining commands (channel full).
    /// Indicates either a stuck audio thread or pathological input rate.
    /// Recoverable: caller may retry after the next render block.
    #[error("audio engine command channel is full")]
    ChannelFull,

    /// The deck index is out of range. Indicates a UI bug — log and
    /// ignore.
    #[error("deck index {idx} out of range (have {count})")]
    InvalidDeck { idx: u8, count: u8 },
}

/// Main-thread handle to control a running [`crate::Engine`].
///
/// Drop semantics: dropping the handle drops the producer end of the
/// command channel and the consumer end of the trash channel. Any
/// `Arc<Track>` left in the trash buffer is dropped here on the main
/// thread when this struct is dropped — *not* on the audio thread.
pub struct EngineHandle {
    tx: HeapProd<Command>,
    trash_rx: HeapCons<Arc<Track>>,
    overflow_counter: Arc<AtomicU64>,
    deck_shared: [Arc<DeckSharedState>; DECK_COUNT],
}

/// Bundle returned to the engine constructor: the rx end of the command
/// channel, the tx end of the trash channel, and the shared overflow
/// counter. All three are owned by `Engine` after construction.
pub(crate) struct EngineSide {
    pub(crate) cmd_rx: HeapCons<Command>,
    pub(crate) trash_tx: HeapProd<Arc<Track>>,
    pub(crate) overflow_counter: Arc<AtomicU64>,
}

impl EngineHandle {
    /// Construct a handle / engine-side bundle. Used by
    /// [`crate::Engine::new_with_handle`]; not part of the public API on
    /// its own because the engine-side bundle is internal.
    #[must_use]
    pub(crate) fn new(deck_shared: [Arc<DeckSharedState>; DECK_COUNT]) -> (Self, EngineSide) {
        let cmd_buffer = HeapRb::<Command>::new(COMMAND_CHANNEL_CAPACITY);
        let (cmd_tx, cmd_rx) = cmd_buffer.split();
        let trash_buffer = HeapRb::<Arc<Track>>::new(TRASH_CHANNEL_CAPACITY);
        let (trash_producer, trash_consumer) = trash_buffer.split();
        let overflow_counter = Arc::new(AtomicU64::new(0));
        let handle = Self {
            tx: cmd_tx,
            trash_rx: trash_consumer,
            overflow_counter: overflow_counter.clone(),
            deck_shared,
        };
        let engine_side = EngineSide {
            cmd_rx,
            trash_tx: trash_producer,
            overflow_counter,
        };
        (handle, engine_side)
    }

    /// Get an ergonomic command builder for the given deck.
    #[must_use]
    pub fn deck(&mut self, idx: usize) -> DeckCommand<'_> {
        DeckCommand { handle: self, idx }
    }

    /// Read-only snapshot of the given deck. Cheap (atomic load).
    #[must_use]
    pub fn deck_state(&self, idx: usize) -> Option<DeckSnapshot> {
        let shared = self.deck_shared.get(idx)?;
        Some(DeckSnapshot {
            position_frames: shared.load_position(),
            is_playing: shared.load_playing(),
            at_end: shared.load_at_end(),
        })
    }

    /// Drain any `Arc<Track>` the audio thread has bounced back through
    /// the trash channel and drop them here on the main thread. Returns
    /// the number of Arcs reclaimed.
    ///
    /// Call this regularly from the UI (e.g. once per UI frame, or
    /// before every track-load attempt). It is also called automatically
    /// by [`DeckCommand::load`] before sending a new load command, so
    /// purely UI-driven workflows are safe without explicit calls.
    pub fn reclaim(&mut self) -> usize {
        let mut n = 0;
        // Each `try_pop` returns `Option<Arc<Track>>`. The Arc drops at
        // the end of this scope on the main thread — never on the audio
        // thread.
        while let Some(_arc) = self.trash_rx.try_pop() {
            n += 1;
        }
        n
    }

    /// Total number of times the audio thread had to forget an old
    /// `Arc<Track>` because the trash channel was full when a new track
    /// was loaded. **Should always be zero in correct usage.** Non-zero
    /// values mean memory has been leaked and the UI is not calling
    /// [`reclaim`](Self::reclaim) frequently enough.
    #[must_use]
    pub fn trash_overflow_count(&self) -> u64 {
        self.overflow_counter.load(Ordering::Relaxed)
    }

    /// Set the engine-wide master gain on the debug internal mixer.
    /// `1.0` is unity. PRD §5.3: external-mixer mode (M5+) bypasses the
    /// master, so this command is a no-op there; for v1's debug mixer
    /// it scales the summed-stereo bus.
    ///
    /// # Errors
    /// [`CommandError::ChannelFull`] if the audio thread is not draining
    /// (recoverable; retry on the next render block).
    pub fn set_master_gain(&mut self, gain: f32) -> Result<(), CommandError> {
        self.send(Command::SetMasterGain { gain })
    }

    fn send(&mut self, cmd: Command) -> Result<(), CommandError> {
        self.tx.try_push(cmd).map_err(|_| CommandError::ChannelFull)
    }

    fn check_deck(&self, idx: usize) -> Result<u8, CommandError> {
        if idx < self.deck_shared.len() {
            #[allow(clippy::cast_possible_truncation)]
            return Ok(idx as u8);
        }
        #[allow(clippy::cast_possible_truncation)]
        Err(CommandError::InvalidDeck {
            idx: idx as u8,
            count: self.deck_shared.len() as u8,
        })
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        // Belt-and-braces: any Arcs still in the trash buffer would be
        // dropped by the ringbuf itself when the consumer goes out of
        // scope (running on this main thread, which is fine), but
        // explicit reclaim makes ordering clear and surfaces overflow
        // diagnostics if the consumer wants to log them.
        self.reclaim();
    }
}

/// Lightweight snapshot of one deck's transport state, suitable for UI
/// rendering. All fields are read with `Relaxed` ordering — consistent
/// per-field but not consistent across fields. That's the right trade-off
/// for a 60 Hz UI: tearing on transport changes is invisible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeckSnapshot {
    /// Current playhead in track frames.
    pub position_frames: f64,
    /// Whether the deck is currently advancing the playhead.
    pub is_playing: bool,
    /// Whether the playhead is past either end of the loaded track.
    pub at_end: bool,
}

/// Per-deck command builder. Returned by [`EngineHandle::deck`]; consumes
/// itself on each command so a single call site can only emit one command,
/// preventing accidental fan-out.
pub struct DeckCommand<'h> {
    handle: &'h mut EngineHandle,
    idx: usize,
}

/// Every command-sending method below can fail with the same two errors:
/// [`CommandError::ChannelFull`] (audio thread is not draining fast enough
/// or has stopped) or [`CommandError::InvalidDeck`] (UI sent a bad index).
/// Both are recoverable from the caller's perspective.
impl DeckCommand<'_> {
    /// Start (or resume) playback.
    ///
    /// # Errors
    /// See impl-level docs.
    pub fn play(self) -> Result<(), CommandError> {
        let idx = self.handle.check_deck(self.idx)?;
        self.handle.send(Command::DeckPlay { idx })
    }

    /// Pause playback. Position is preserved.
    ///
    /// # Errors
    /// See impl-level docs.
    pub fn pause(self) -> Result<(), CommandError> {
        let idx = self.handle.check_deck(self.idx)?;
        self.handle.send(Command::DeckPause { idx })
    }

    /// Move the playhead to the given track-frame position. Negative or
    /// out-of-range values are accepted and silently render silence
    /// (matches a real record being lifted off the platter).
    ///
    /// # Errors
    /// See impl-level docs.
    pub fn seek(self, position_frames: f64) -> Result<(), CommandError> {
        let idx = self.handle.check_deck(self.idx)?;
        self.handle.send(Command::DeckSeek {
            idx,
            position_frames,
        })
    }

    /// Set the playback rate. `1.0` = forward unity, `-1.0` = reverse
    /// unity, `2.0` = double-speed, `0.0` = stopped.
    ///
    /// # Errors
    /// See impl-level docs.
    pub fn set_rate(self, rate: f64) -> Result<(), CommandError> {
        let idx = self.handle.check_deck(self.idx)?;
        self.handle.send(Command::DeckSetRate { idx, rate })
    }

    /// Set the linear deck gain.
    ///
    /// # Errors
    /// See impl-level docs.
    pub fn set_gain(self, gain: f32) -> Result<(), CommandError> {
        let idx = self.handle.check_deck(self.idx)?;
        self.handle.send(Command::DeckSetGain { idx, gain })
    }

    /// Hot-load a track onto this deck while the engine is running.
    ///
    /// Auto-drains the trash channel before sending so the user never
    /// has to call [`EngineHandle::reclaim`] manually for the load
    /// path to stay safe.
    ///
    /// On the audio thread this swaps the deck's `Arc<Track>`; the
    /// previous Arc (if any) is sent back through the trash channel
    /// for disposal here on the main thread, never dropped on the
    /// audio thread.
    ///
    /// Position/play state are not implicitly reset — pair this with
    /// [`DeckCommand::seek`] / [`DeckCommand::pause`] to set up the
    /// new track's transport.
    ///
    /// # Errors
    /// On [`CommandError::ChannelFull`] the rejected `Arc<Track>` is
    /// returned back to the caller (cheaper to retry than to re-decode).
    /// On [`CommandError::InvalidDeck`] the Arc is dropped here on the
    /// main thread.
    pub fn load(self, source: Arc<Track>) -> Result<(), (CommandError, Arc<Track>)> {
        let idx = match self.handle.check_deck(self.idx) {
            Ok(idx) => idx,
            Err(e) => return Err((e, source)),
        };
        self.handle.reclaim();
        self.handle
            .tx
            .try_push(Command::DeckLoad { idx, source })
            .map_err(|cmd| match cmd {
                Command::DeckLoad { source, .. } => (CommandError::ChannelFull, source),
                // Unreachable: we just constructed a DeckLoad above.
                other => unreachable!("ringbuf returned wrong Command variant: {other:?}"),
            })
    }

    /// Read the current snapshot for this deck. Cheap (atomic loads).
    #[must_use]
    pub fn snapshot(&self) -> DeckSnapshot {
        // Already validated in deck() that idx is in range, but defend
        // against future API changes.
        self.handle.deck_state(self.idx).unwrap_or(DeckSnapshot {
            position_frames: 0.0,
            is_playing: false,
            at_end: false,
        })
    }
}
