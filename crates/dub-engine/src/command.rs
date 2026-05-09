//! Commands sent from the UI thread to the audio engine.
//!
//! Every transport/state mutation that needs to happen mid-playback flows
//! through this enum. The producer side lives in [`crate::EngineHandle`]
//! (main thread); the consumer side is drained by [`crate::Engine::render`]
//! at the start of each block.
//!
//! Per PRD §4.2: lock-free SPSC, no allocation on send/receive, no `Box`,
//! no `dyn Trait`. Adding a new command means adding an enum variant and
//! its match arm in [`crate::Engine::apply_command`] — that's all.
//!
//! Most commands are tiny `Copy` values (≤ 24 bytes). The exception is
//! [`Command::DeckLoad`], which carries an `Arc<Track>`. The audio thread
//! never drops this Arc — when it swaps it onto the deck, the *old* Arc
//! is bounced back through the trash channel for disposal on the main
//! thread (see crate-level docs in `lib.rs`).

use std::sync::Arc;

use dub_io::Track;

/// One mutation request to the engine. Variants name the deck index where
/// applicable; engine-wide commands use no index.
///
/// Field naming is uniform across variants: `idx` is the deck index,
/// other fields name the property being set.
//
// `Deck` prefix on per-deck variants is load-bearing namespacing
// (engine-wide commands such as `SetMasterGain` distinguish themselves
// by *not* having it). Allow the `enum_variant_names` lint accordingly.
#[allow(missing_docs, clippy::enum_variant_names)]
#[derive(Debug, Clone)]
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_is_send_sync_and_bounded() {
        // The non-DeckLoad commands are still Copy-equivalent in size,
        // but we now carry an Arc<Track> in DeckLoad so the enum itself
        // is not Copy. Send+Sync still required for cross-thread use.
        const _: fn() = || {
            fn assert_send_sync<T: Send + Sync>() {}
            assert_send_sync::<Command>();
        };
        // 32 bytes upper bound today (DeckLoad: 1 tag + 1 idx + 8-byte
        // pad + 8-byte Arc pointer = 24, padded). Cap at 64 to catch
        // accidental bloat — push variants above this through indirection.
        assert!(
            std::mem::size_of::<Command>() <= 64,
            "Command grew to {} bytes; consider redesigning",
            std::mem::size_of::<Command>()
        );
    }
}
