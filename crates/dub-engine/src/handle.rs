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

use ringbuf::traits::{Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::sync::Arc;

use crate::command::Command;
use crate::deck::DeckSharedState;
use crate::DECK_COUNT;

/// Capacity of the command channel. Sized for far more than any plausible
/// burst of human input — at 60 Hz UI updates a deck would have to send a
/// command on every frame for ~4 seconds to fill this. Real bursts are
/// dozens of commands at most (a complex performance gesture).
const COMMAND_CHANNEL_CAPACITY: usize = 256;

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
/// channel. The audio thread keeps running on its last commanded state.
/// Restoring control requires constructing a new engine + handle pair.
pub struct EngineHandle {
    tx: HeapProd<Command>,
    deck_shared: [Arc<DeckSharedState>; DECK_COUNT],
}

impl EngineHandle {
    /// Construct a handle / engine-side consumer pair. Used by
    /// [`crate::Engine::new_with_handle`]; not part of the public API on
    /// its own because the consumer side is internal.
    #[must_use]
    pub(crate) fn new(
        deck_shared: [Arc<DeckSharedState>; DECK_COUNT],
    ) -> (Self, HeapCons<Command>) {
        let rb = HeapRb::<Command>::new(COMMAND_CHANNEL_CAPACITY);
        let (tx, rx) = rb.split();
        (Self { tx, deck_shared }, rx)
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
