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
//! A `Command` is `Copy` and small (≤ 16 bytes today). The ringbuf carries
//! it by value; popping the consumer copies the value out of the buffer.

/// One mutation request to the engine. Variants name the deck index where
/// applicable; transport-wide commands use no index.
///
/// Field naming is uniform across variants: `idx` is the deck index,
/// other fields name the property being set.
// All v1 commands target a deck. Engine-wide commands (master gain etc.)
// will land in M3+ and use a different prefix; the `Deck` prefix here is
// load-bearing namespacing, not redundancy.
#[allow(missing_docs, clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq)]
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_is_copy_and_small() {
        // Commands are passed by value through the ringbuf, so they must
        // not own heap allocations and should stay tight.
        const _: fn() = || {
            fn assert_copy<T: Copy>() {}
            fn assert_send_sync<T: Send + Sync>() {}
            assert_copy::<Command>();
            assert_send_sync::<Command>();
        };
        // 24 bytes today (variant tag + f64). Soft cap at 32 to catch
        // accidental bloat — push variants above this through indirection.
        assert!(
            std::mem::size_of::<Command>() <= 32,
            "Command grew to {} bytes; consider redesigning",
            std::mem::size_of::<Command>()
        );
    }
}
