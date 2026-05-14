//! Commands sent from the UI thread to the audio engine.
//!
//! Every transport/state mutation that needs to happen mid-playback flows
//! through this enum. The producer side lives in [`crate::EngineHandle`]
//! (main thread); the consumer side is drained by [`crate::Engine::render`]
//! at the start of each block.
//!
//! Per PRD §4.2: lock-free SPSC, no allocation on send/receive. Adding a
//! new command means adding an enum variant and its match arm in
//! [`crate::Engine::apply_command`] — that's all.
//!
//! **Heap-bearing variants.** Most commands are tiny `Copy` values
//! (≤ 24 bytes). Three variants carry heap pointers because the work
//! being commanded is fundamentally about handing a heap-allocated
//! resource to the audio thread:
//!
//! - [`Command::DeckLoad`] carries an `Arc<Track>` (PCM samples + metadata).
//! - [`Command::AttachTimecodeInput`] carries a `Box<TimecodeInput>` (M5.4.5;
//!   the box owns the SPSC consumer end of the input ringbuffer plus the
//!   pre-allocated decoder + lift policy).
//! - [`Command::AttachThruSource`] carries a `Box<ThruSource>` (M7;
//!   the box owns the SPSC consumer end of the input ringbuffer plus
//!   the scratch buffer for the Thru render path).
//!
//! The audio thread *never* drops these allocations — when it swaps
//! any onto its slot, any displaced predecessor is bounced back
//! through a corresponding trash channel for disposal on the main thread
//! (see crate-level docs in `lib.rs`). Track trash, TimecodeInput trash,
//! and ThruSource trash are separate ringbufs because their item types
//! differ; all three follow the same overflow-counter "leak rather than
//! drop" pattern.

use std::sync::Arc;

use dub_io::Track;

use crate::thru::ThruSource;
use crate::timecode::TimecodeInput;

/// One mutation request to the engine. Variants name the deck index where
/// applicable; engine-wide commands use no index.
///
/// Field naming is uniform across variants: `idx` is the deck index,
/// other fields name the property being set.
///
/// **Not [`Clone`].** Two variants ([`Self::DeckLoad`],
/// [`Self::AttachTimecodeInput`]) carry uniquely-owned heap resources
/// (`Arc<Track>` and `Box<TimecodeInput>`). Cloning a command would
/// bump the `Arc` refcount silently or fail outright on the `Box`, so
/// we don't derive [`Clone`]; consumers move commands through the
/// SPSC channel as the only path.
///
/// [`Debug`] is hand-written (rather than derived) for the same
/// reason: [`TimecodeInput`] is not [`Debug`] (it owns a decoder + a
/// scratch buffer with no useful Debug rendering); we render a
/// placeholder so the `unreachable!` and trace formatting in
/// [`crate::EngineHandle`] stay usable.
//
// `Deck` prefix on per-deck variants is load-bearing namespacing
// (engine-wide commands such as `SetMasterGain` distinguish themselves
// by *not* having it). Allow the `enum_variant_names` lint accordingly.
#[allow(missing_docs, clippy::enum_variant_names)]
pub enum Command {
    /// Start playback on deck `idx`.
    DeckPlay { idx: u8 },

    /// Pause deck `idx` (playhead does not advance, but the source remains
    /// loaded).
    DeckPause { idx: u8 },

    /// Move deck `idx`'s playhead to the given position in track frames.
    DeckSeek { idx: u8, position_frames: f64 },

    /// Set deck `idx`'s playback rate. `1.0` = normal forward; `-1.0` =
    /// reverse at unity speed; `0.0` = paused without resetting state.
    DeckSetRate { idx: u8, rate: f64 },

    /// Engage Panic-Play (M10.6b, PRD §6.1.2) on deck `idx`. The
    /// engine captures the deck's current "last known good"
    /// velocity (preferring `LiftPolicy::last_locked_rate()` if a
    /// timecode input is attached, falling back to the deck's
    /// commanded rate otherwise), forces the policy into a
    /// disengaged state so the next `LiftIntent::Locked` is a
    /// fresh re-engagement, and starts the deck playing at the
    /// captured rate. From this point on the deck ignores
    /// timecode-driven rate / play-state updates (the `Locked` /
    /// `DropoutHoldRate` branches of `apply_timecode_intents`)
    /// until either:
    ///
    /// - the policy reports a clean `LiftIntent::Locked` (carrier
    ///   alive + confidence above the engage threshold), at which
    ///   point panic auto-cancels and normal timecode handling
    ///   resumes — the held playhead position becomes the new zero
    ///   reference for the LFSR's relative motion; or
    /// - the user issues [`Self::DeckCancelPanicPlay`], which
    ///   hands transport authority back to the timecode driver.
    ///
    /// `Locked`-with-cached-rate sticky-window samples don't count
    /// as a clean re-lock because the policy stays disengaged
    /// until it sees an above-engage-threshold confidence sample.
    DeckPanicPlay { idx: u8 },

    /// Cancel Panic-Play on deck `idx` (PRD §6.1.2 / M10.6d). The
    /// engine clears its panic-play flag and hands transport
    /// authority back to the timecode driver: a clean carrier
    /// keeps the deck playing at the platter rate (Serato INT→ABS
    /// path); a silent carrier pauses it on the next block via the
    /// existing `DropoutHoldRate` arm. Crucially this command does
    /// **not** flip `is_playing` itself — the driver does, on the
    /// next render block. Idempotent on decks not in panic mode.
    DeckCancelPanicPlay { idx: u8 },

    /// Set deck `idx`'s linear gain. `1.0` = unity, `0.0` = silence.
    DeckSetGain { idx: u8, gain: f32 },

    /// Load a new track on deck `idx`. The `Arc<Track>` is sent by value;
    /// the engine swaps it onto the deck on the audio thread without
    /// dropping the previous `Arc` — that goes back through the trash
    /// channel.
    DeckLoad { idx: u8, source: Arc<Track> },

    /// Set the engine-wide master gain applied after deck summing in the
    /// debug internal mixer. `1.0` = unity. PRD §5.3 calls for this only
    /// in the debug/internal mixer mode; external-mixer mode (M5+) bypasses
    /// the master and routes each deck to its own output pair raw.
    SetMasterGain { gain: f32 },

    /// Mid-stream attach of a [`TimecodeInput`] to deck `idx` (M5.4.5).
    /// The box is constructed on the main thread — `TimecodeInput::new`
    /// allocates a scratch buffer and a decoder — and handed across the
    /// command channel as a single 8-byte pointer.
    ///
    /// On the audio thread, [`crate::Engine::apply_command`] takes the
    /// box out of the variant and slots it into
    /// `engine.timecode_inputs[idx]`. If the slot was already occupied
    /// (mid-stream re-calibration after a cartridge swap, M5.4.5+
    /// extension), the *displaced* `Box<TimecodeInput>` is sent back
    /// through the timecode-input trash channel for main-thread
    /// disposal — never dropped on the audio thread.
    ///
    /// Why command-channel attach (vs. the existing `&mut Engine`
    /// [`crate::Engine::attach_timecode_input`]): once the engine has
    /// been moved into `dub_audio::AudioOutput`, no `&mut` access from
    /// the main thread is possible. M5.4.5 needs to attach decks
    /// *while audio is running* (the DJ-takeover use case), so the
    /// attach must route through the SPSC channel like every other
    /// runtime mutation.
    AttachTimecodeInput { idx: u8, input: Box<TimecodeInput> },

    /// Mid-stream attach of a [`ThruSource`] to deck `idx` (M7). The
    /// box is constructed on the main thread —
    /// [`crate::ThruSource::new`] allocates a scratch buffer — and
    /// handed across the command channel as a single 8-byte pointer.
    ///
    /// On the audio thread, [`crate::Engine::apply_command`] takes the
    /// box out of the variant and slots it into
    /// `engine.thru_sources[idx]`. If the slot was already occupied
    /// (the operator re-attaches mid-set, e.g. swaps cartridges or
    /// switches inputs), the *displaced* `Box<ThruSource>` is sent
    /// back through the thru-source trash channel for main-thread
    /// disposal — never dropped on the audio thread.
    ///
    /// Mirrors [`Self::AttachTimecodeInput`]'s shape exactly because
    /// the constraint (heap-bearing payload that the audio thread
    /// can't drop) is the same; M5.4.5's trash-channel pattern
    /// generalises trivially to a third channel.
    ///
    /// Thru Mode in Dub is a single always-on passthrough — there
    /// are no mode commands. FX engagement is handled inside the
    /// per-deck signal chain by individual FX modules (M15+), not
    /// by switching the Thru source between paths. See
    /// `crate::thru` module docs for the design rationale.
    AttachThruSource { idx: u8, source: Box<ThruSource> },
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeckPlay { idx } => f.debug_struct("DeckPlay").field("idx", idx).finish(),
            Self::DeckPause { idx } => f.debug_struct("DeckPause").field("idx", idx).finish(),
            Self::DeckSeek {
                idx,
                position_frames,
            } => f
                .debug_struct("DeckSeek")
                .field("idx", idx)
                .field("position_frames", position_frames)
                .finish(),
            Self::DeckSetRate { idx, rate } => f
                .debug_struct("DeckSetRate")
                .field("idx", idx)
                .field("rate", rate)
                .finish(),
            Self::DeckPanicPlay { idx } => {
                f.debug_struct("DeckPanicPlay").field("idx", idx).finish()
            }
            Self::DeckCancelPanicPlay { idx } => f
                .debug_struct("DeckCancelPanicPlay")
                .field("idx", idx)
                .finish(),
            Self::DeckSetGain { idx, gain } => f
                .debug_struct("DeckSetGain")
                .field("idx", idx)
                .field("gain", gain)
                .finish(),
            Self::DeckLoad { idx, .. } => f
                .debug_struct("DeckLoad")
                .field("idx", idx)
                .field("source", &"<Arc<Track>>")
                .finish(),
            Self::SetMasterGain { gain } => {
                f.debug_struct("SetMasterGain").field("gain", gain).finish()
            }
            Self::AttachTimecodeInput { idx, .. } => f
                .debug_struct("AttachTimecodeInput")
                .field("idx", idx)
                .field("input", &"<Box<TimecodeInput>>")
                .finish(),
            Self::AttachThruSource { idx, .. } => f
                .debug_struct("AttachThruSource")
                .field("idx", idx)
                .field("source", &"<Box<ThruSource>>")
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_is_send_and_bounded() {
        // The non-Copy variants (DeckLoad's Arc<Track>, M5.4.5's
        // AttachTimecodeInput's Box<TimecodeInput>) make the enum
        // non-Copy, but each only adds an 8-byte pointer to the
        // payload.
        //
        // We require `Send` only — the SPSC channel moves a command
        // from the producer thread to the consumer thread by value,
        // never sharing &Command across threads. We dropped the
        // `Sync` bound when M5.4.5 added `Box<TimecodeInput>`: the
        // inner `HeapCons<f32>` is intentionally `!Sync` (an SPSC
        // consumer is unsound to access from two threads at once),
        // and `Sync` was never actually needed by the channel
        // contract.
        const _: fn() = || {
            fn assert_send<T: Send>() {}
            assert_send::<Command>();
        };
        // 32 bytes upper bound today (DeckLoad: 1 tag + 1 idx + 8-byte
        // pad + 8-byte Arc pointer = 24, padded). AttachTimecodeInput
        // is the same shape (1 tag + 1 idx + 6-byte pad + 8-byte Box).
        // Cap at 64 to catch accidental bloat — push variants above
        // this through indirection.
        assert!(
            std::mem::size_of::<Command>() <= 64,
            "Command grew to {} bytes; consider redesigning",
            std::mem::size_of::<Command>()
        );
    }
}
