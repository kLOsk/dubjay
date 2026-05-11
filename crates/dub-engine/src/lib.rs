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
pub mod thru;
pub mod timecode;

pub use command::Command;
pub use deck::Deck;
pub use handle::{
    CommandError, DeckCommand, DeckSnapshot, EngineHandle, ThruAttachWithBpmError,
    BPM_TEE_RING_CAPACITY_SECS,
};
pub use realtime::{RealtimeContext, RtError};
pub use thru::{ThruAttachError, ThruInputConfig, ThruSource};
pub use timecode::{
    AttachError as TimecodeAttachError, LiftIntent, LiftPolicy, TimecodeInput, TimecodeInputConfig,
    DEFAULT_AMPLITUDE_THRESHOLD, DEFAULT_CONFIDENCE_THRESHOLD, DEFAULT_DISENGAGE_THRESHOLD,
    DEFAULT_STICKY_BLOCKS_TO_DISENGAGE,
};

/// Library version reported by the crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Number of decks in v1 (PRD §3 / §6).
pub const DECK_COUNT: usize = 2;

/// Per-deck output routing for [`Engine::render_routed`].
///
/// Each entry is an `Option<u32>` naming the *first* (0-based) output
/// channel for that deck:
///
/// - `None`  — the deck is not routed; its audio is dropped this block.
/// - `Some(c)` — the deck's stereo pair is added into channels
///   `c, c+1` of the multi-channel output buffer.
///
/// Two decks with the same `Some(c)` *sum* into the same channel pair
/// (= M4 internal mixer). Two decks with non-overlapping `Some(c)`
/// values are isolated (= M5.5 external-mixer routing — deck A on
/// physical mixer's channel 1, deck B on channel 2, etc.). The
/// internal-mixer behaviour is therefore not a special case in the
/// engine: it's just the routing
/// `[Some(0), Some(0)]` over a 2-channel buffer, which is what
/// [`Engine::render`] produces for backward compatibility.
pub type OutputRouting = [Option<u32>; DECK_COUNT];

/// Internal-mixer routing: both decks summed into channels 0+1 of a
/// 2-channel buffer. This is what [`Engine::render`] produces.
pub const INTERNAL_MIXER_ROUTING: OutputRouting = [Some(0), Some(0)];

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
    /// Producer end of the M5.4.5 timecode-input trash channel. `None`
    /// for offline/test engines (built via [`Self::new`]). Used when
    /// `Command::AttachTimecodeInput` lands on a slot that was already
    /// filled — the displaced [`TimecodeInput`] is sent here for
    /// disposal on the main thread.
    trash_tx_timecode: Option<ringbuf::HeapProd<Box<TimecodeInput>>>,
    /// Atomic counter incremented every time the timecode-input trash
    /// channel was full when an old [`TimecodeInput`] needed to go
    /// back. Surfaced to the UI via
    /// [`EngineHandle::timecode_trash_overflow_count`].
    trash_overflow_timecode: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    /// Producer end of the M7 thru-source trash channel. `None` for
    /// offline/test engines. Used when `Command::AttachThruSource`
    /// lands on a slot that was already filled — the displaced
    /// [`ThruSource`] is sent here for main-thread disposal.
    trash_tx_thru: Option<ringbuf::HeapProd<Box<ThruSource>>>,
    /// Atomic counter incremented every time the thru-source trash
    /// channel was full when an old [`ThruSource`] needed to go back.
    /// Surfaced to the UI via [`EngineHandle::thru_trash_overflow_count`].
    trash_overflow_thru: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    /// Per-deck timecode input. `None` means the deck runs free under
    /// normal command/handle control; `Some` means the deck's transport
    /// is driven by the decoded carrier each block (M5.3). One slot per
    /// deck so M5+ external-mixer routing can mix-and-match (deck A on
    /// timecode, deck B on file playback, or vice versa).
    timecode_inputs: [Option<TimecodeInput>; DECK_COUNT],
    /// Per-deck Thru source (M7). When `Some`, the deck's own
    /// transport+source state is bypassed entirely and the Thru source
    /// owns the deck's output channels for this block — engine reads
    /// from the input ringbuf and writes additively into the routed
    /// output channels at the deck's gain. When `None`, the deck
    /// renders normally (file playback or timecode-driven; the M0-M6
    /// path).
    ///
    /// Thru and Track are mutually exclusive per deck within one
    /// engine lifetime: a real record on the platter is the source,
    /// not a loaded file underneath. FX engagement (M15+) does not
    /// flip this slot — FX modules will live inside the per-deck
    /// signal chain and own their own bypass semantics. See
    /// `crate::thru` module docs.
    thru_sources: [Option<ThruSource>; DECK_COUNT],
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
        let decks = std::array::from_fn(|_| Deck::new(envelope.clone()));
        drop(envelope);
        Self {
            sample_rate,
            block_size,
            decks,
            master_gain: 1.0,
            cmd_rx: None,
            trash_tx: None,
            trash_overflow: None,
            trash_tx_timecode: None,
            trash_overflow_timecode: None,
            trash_tx_thru: None,
            trash_overflow_thru: None,
            timecode_inputs: std::array::from_fn(|_| None),
            thru_sources: std::array::from_fn(|_| None),
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
        drop(envelope);
        let shared: [std::sync::Arc<deck::DeckSharedState>; DECK_COUNT] =
            std::array::from_fn(|i| decks[i].shared());
        let (handle, side) = EngineHandle::new(shared, sample_rate);
        let engine = Self {
            sample_rate,
            block_size,
            decks,
            master_gain: 1.0,
            cmd_rx: Some(side.cmd_rx),
            trash_tx: Some(side.trash_tx),
            trash_overflow: Some(side.overflow_counter),
            trash_tx_timecode: Some(side.timecode_trash_tx),
            trash_overflow_timecode: Some(side.timecode_trash_overflow),
            trash_tx_thru: Some(side.thru_trash_tx),
            trash_overflow_thru: Some(side.thru_trash_overflow),
            timecode_inputs: std::array::from_fn(|_| None),
            thru_sources: std::array::from_fn(|_| None),
        };
        (engine, handle)
    }

    /// Attach a timecode input to a deck. After this call, the deck's
    /// transport (rate + play/pause) is driven each render block by the
    /// decoder; commands sent through the [`EngineHandle`] still apply
    /// for non-transport state (gain, source loads), but `play`,
    /// `pause`, and `set_rate` will be overwritten by the next block's
    /// decoder output.
    ///
    /// Off-RT — call once during engine setup, before moving the engine
    /// into the [`dub_audio::AudioOutput`] callback.
    ///
    /// # Errors
    /// See [`TimecodeAttachError`] for the failure modes — bad deck
    /// index, slot already occupied, SR mismatch with the engine, or a
    /// pathological config.
    pub fn attach_timecode_input(
        &mut self,
        deck_idx: usize,
        rx: ringbuf::HeapCons<f32>,
        config: TimecodeInputConfig,
    ) -> Result<(), TimecodeAttachError> {
        if deck_idx >= DECK_COUNT {
            return Err(TimecodeAttachError::InvalidDeck {
                idx: deck_idx,
                count: DECK_COUNT,
            });
        }
        config.validate(self.sample_rate)?;
        if self.timecode_inputs[deck_idx].is_some() {
            return Err(TimecodeAttachError::AlreadyAttached { idx: deck_idx });
        }
        self.timecode_inputs[deck_idx] = Some(TimecodeInput::new(rx, config));
        Ok(())
    }

    /// Detach the timecode input from a deck (off-RT). Returns the
    /// previously-attached input so the caller can drop it on the main
    /// thread; the deck is then under handle/command control again.
    #[must_use = "the returned TimecodeInput holds a ringbuf consumer; \
                  drop it on the main thread, not the audio thread"]
    pub fn detach_timecode_input(&mut self, deck_idx: usize) -> Option<TimecodeInput> {
        self.timecode_inputs
            .get_mut(deck_idx)
            .and_then(Option::take)
    }

    /// Read-only view of the most recent decoder output for a deck.
    /// Off-RT only (the audio thread mutates `last_output`); call from
    /// the main thread between blocks for UI display.
    #[must_use]
    pub fn timecode_last_output(&self, deck_idx: usize) -> Option<dub_timecode::DecodeOutput> {
        self.timecode_inputs
            .get(deck_idx)
            .and_then(|s| s.as_ref())
            .and_then(TimecodeInput::last_output)
    }

    /// Attach a Thru source to a deck (M7). After this call, the deck
    /// is in Thru mode: the deck's own [`Deck`] state (loaded track,
    /// transport) is bypassed by [`Self::render_routed`], and audio
    /// for that deck's output channels comes from the
    /// [`ThruSource`] — the ringbuf'd audio interface input, passed
    /// through unchanged at the deck's gain.
    ///
    /// Off-RT — call once during engine setup (mirroring
    /// [`Self::attach_timecode_input`]). The
    /// [`EngineHandle::attach_thru_source`] command-channel path is
    /// the production wire-up since the engine moves into the audio
    /// thread before deck attach typically happens.
    ///
    /// # Errors
    /// See [`ThruAttachError`] for the failure modes.
    pub fn attach_thru_source(
        &mut self,
        deck_idx: usize,
        rx: ringbuf::HeapCons<f32>,
        config: ThruInputConfig,
    ) -> Result<(), ThruAttachError> {
        if deck_idx >= DECK_COUNT {
            return Err(ThruAttachError::InvalidDeck {
                idx: deck_idx,
                count: DECK_COUNT,
            });
        }
        config.validate(self.sample_rate)?;
        self.thru_sources[deck_idx] = Some(ThruSource::new(rx, config));
        Ok(())
    }

    /// Detach the Thru source from a deck (off-RT). Returns the
    /// previously-attached source so the caller can drop it on the
    /// main thread — the audio thread never drops a
    /// [`ThruSource`] (it owns a `HeapCons<f32>` + a `Vec<f32>`
    /// scratch, both of which `dealloc` on drop).
    #[must_use = "the returned ThruSource holds a ringbuf consumer; \
                  drop it on the main thread, not the audio thread"]
    pub fn detach_thru_source(&mut self, deck_idx: usize) -> Option<ThruSource> {
        self.thru_sources.get_mut(deck_idx).and_then(Option::take)
    }

    /// Whether a [`ThruSource`] is currently attached on deck
    /// `deck_idx`. `false` for an out-of-range index. Off-RT
    /// diagnostic / UI probe ("is this deck routing a real record?").
    #[must_use]
    pub fn thru_attached(&self, deck_idx: usize) -> bool {
        self.thru_sources.get(deck_idx).is_some_and(Option::is_some)
    }

    /// Number of input samples buffered on a deck's Thru source.
    /// Off-RT diagnostic ("is the IOProc keeping up?"). `None` if no
    /// Thru source is attached.
    #[must_use]
    pub fn thru_available(&self, deck_idx: usize) -> Option<usize> {
        self.thru_sources
            .get(deck_idx)
            .and_then(|s| s.as_ref())
            .map(ThruSource::available)
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

    /// Render one block of audio in stereo internal-mixer mode.
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
    ///
    /// Equivalent to `render_routed(rt, out, 2, &INTERNAL_MIXER_ROUTING)`
    /// — preserved as a top-level method so all the M0-M5 callers stay
    /// untouched.
    pub fn render(&mut self, rt: &mut RealtimeContext<'_>, out: &mut [f32]) {
        self.render_routed(rt, out, 2, &INTERNAL_MIXER_ROUTING);
    }

    /// Render one block with per-deck routing onto an N-channel output
    /// buffer.
    ///
    /// `num_channels` is the device's output channel count (2 for the
    /// debug stereo path, 4 / 6 / … for external-mixer routing on
    /// multi-channel hardware). `out` is interleaved across all N
    /// channels, length must be a multiple of `num_channels`.
    ///
    /// `routing[deck_idx]` controls where each deck's stereo output
    /// lands. See [`OutputRouting`] for the full semantics. Decks with
    /// `routing[i] == None` are skipped entirely — their transport
    /// state does NOT advance for that block. This matches the M5.5
    /// design intent: routing is a *hardware-mapping* concern, not a
    /// muting mechanism. Use per-deck `Deck::set_gain(0.0)` to mute
    /// while keeping the transport running; reserve `routing[i] = None`
    /// for the case where the deck genuinely has no output destination
    /// (e.g. a 2-channel device with deck B physically disconnected).
    ///
    /// The buffer is zeroed at the start; deck contributions are added
    /// (`+=`); master gain applies once across the whole multi-channel
    /// buffer at the end (so unrouted channels stay zero, and routed
    /// channels are scaled identically to the M4 stereo path).
    pub fn render_routed(
        &mut self,
        rt: &mut RealtimeContext<'_>,
        out: &mut [f32],
        num_channels: usize,
        routing: &OutputRouting,
    ) {
        debug_assert!(
            num_channels >= 2,
            "num_channels must be at least 2 to hold a stereo pair"
        );
        debug_assert_eq!(
            out.len() % num_channels,
            0,
            "output buffer length must be a multiple of num_channels"
        );
        rt.tick();

        // Drain the command queue first so the upcoming block reflects
        // the latest UI state. RT-safe: try_pop is a load + index, no
        // allocation; apply_command writes plain fields and atomics.
        self.drain_commands();

        // M5.3: drain each attached timecode input, run the decoder,
        // and translate the result into deck transport intents. This
        // happens BEFORE deck render so the new rate / play state is
        // in effect for this block's output.
        self.drive_timecode_inputs();

        for sample in out.iter_mut() {
            *sample = 0.0;
        }
        let sr = self.sample_rate;
        // Per-deck render dispatch (M7). We can't use
        // `decks.iter_mut().enumerate()` here because the body needs
        // *disjoint* mutable access to both `self.thru_sources[idx]`
        // and `self.decks[idx]` — a single `.iter_mut()` on `decks`
        // would also alias with `self.thru_sources` through `&mut self`.
        // The index-loop pattern is the idiomatic Rust workaround for
        // parallel-array dispatch like this, and the bounded loop is
        // trivially bounds-checked at the array level.
        #[allow(clippy::needless_range_loop)]
        for idx in 0..DECK_COUNT {
            let Some(first) = routing[idx] else { continue };
            let first_us = first as usize;
            debug_assert!(
                first_us + 2 <= num_channels,
                "deck {idx} routed to first channel {first_us} but only {num_channels} \
                 channels are available; needs at least 2 channels for stereo"
            );
            // If a Thru source is attached on this deck, it owns the
            // deck's output channels for this block — Direct renders
            // silence, Processed/Hold reads from the input ring and
            // writes additively. The Deck struct's transport state is
            // not advanced (a Thru deck has no track to advance).
            // Otherwise we fall through to the M0-M6 deck render path,
            // byte-identical to pre-M7.
            if let Some(thru) = self.thru_sources[idx].as_mut() {
                let gain = self.decks[idx].gain();
                thru.render_into(out, gain, num_channels, first_us);
            } else {
                self.decks[idx].render_into(rt, out, sr, num_channels, first_us);
            }
        }

        // Master gain (M4 / M5.5): single multiplicative scale across the
        // entire summed N-channel bus. Applied after deck mixing so
        // per-deck gains and the master compose multiplicatively.
        // Unrouted channels stay zero (zero × master == zero) so master
        // never accidentally introduces signal on an unrouted pair.
        // Skipping the multiply when master==1.0 saves a per-block branch
        // on the common case.
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

    /// For each deck with a timecode input attached, drain whatever
    /// audio has arrived since last block, decode it, and translate
    /// the decoder's `(rate, confidence)` into deck transport. This is
    /// the M5.3 hot path.
    ///
    /// **RT-safety**: zero allocations. `pop_slice` is a memcpy,
    /// `Decoder::process` is pure float math, and the deck transport
    /// setters are field writes plus atomic stores. Verified by the
    /// engine's `assert_no_alloc` tests + the rt-audit binary.
    ///
    /// Borrow gymnastics: `self.timecode_inputs[idx]` and
    /// `self.decks[idx]` overlap through `&mut self`, so we run the
    /// decoder inside an inner scope that drops the input borrow
    /// before reaching for the deck. The intermediate
    /// [`timecode::LiftIntent`] carries only `Copy` data across the
    /// borrow boundary.
    fn drive_timecode_inputs(&mut self) {
        for idx in 0..DECK_COUNT {
            let intent = match self.timecode_inputs[idx].as_mut() {
                Some(input) => input.drive(),
                None => continue,
            };
            let Some(intent) = intent else {
                // No new input data this block — keep the deck at its
                // current rate/play state. Single-block dropouts are
                // common (CoreAudio jitter, USB scheduling).
                continue;
            };
            let deck = &mut self.decks[idx];
            match intent {
                timecode::LiftIntent::Locked { rate } => {
                    deck.set_rate(rate);
                    if !deck.is_playing() {
                        // First lock after silence / dropout: start
                        // playing. The 2 ms declick handles the
                        // smooth fade-in from silence.
                        deck.set_playing(true);
                    }
                }
                timecode::LiftIntent::DropoutHoldRate { rate } => {
                    // Keep the rate in case confidence comes back next
                    // block (single-tick dropouts shouldn't reset
                    // scratch state), but mute the deck so the user
                    // doesn't hear a held-DC tone while the stylus is
                    // off.
                    deck.set_rate(rate);
                    if deck.is_playing() {
                        deck.set_playing(false);
                    }
                }
            }
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
            Command::AttachTimecodeInput { idx, input } => {
                let Some(slot) = self.timecode_inputs.get_mut(idx as usize) else {
                    // Bad idx: we cannot drop the box here on the
                    // audio thread (it owns a HeapCons + Vec<f32> +
                    // Decoder, all of which dealloc on drop). Bounce
                    // it back through the timecode trash channel for
                    // main-thread disposal — symmetric with the
                    // bad-idx branch in `Command::DeckLoad`.
                    self.send_timecode_input_to_trash(input);
                    return;
                };
                // Replace-and-trash. If the slot was empty, the
                // displaced value is `None` and there's nothing to
                // dispose. If it was occupied (mid-stream re-cal),
                // the old `TimecodeInput` is moved out *boxed* and
                // shipped back through the trash channel for
                // main-thread drop.
                let displaced = slot.replace(*input);
                if let Some(old) = displaced {
                    self.send_timecode_input_to_trash(Box::new(old));
                }
            }
            Command::AttachThruSource { idx, source } => {
                let Some(slot) = self.thru_sources.get_mut(idx as usize) else {
                    // Bad idx: bounce back through the thru-source
                    // trash channel — symmetric with the M5.4.5
                    // bad-idx branch above.
                    self.send_thru_source_to_trash(source);
                    return;
                };
                let displaced = slot.replace(*source);
                if let Some(old) = displaced {
                    self.send_thru_source_to_trash(Box::new(old));
                }
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

    /// Push an old [`Box<TimecodeInput>`] back to the main thread for
    /// disposal (M5.4.5). Symmetric to [`Self::send_to_trash`] for
    /// `Arc<Track>`. On overflow (channel full) `mem::forget` the box
    /// and bump the timecode overflow counter — leaking is the lesser
    /// evil compared with a `dealloc` on the audio thread.
    fn send_timecode_input_to_trash(&mut self, boxed: Box<TimecodeInput>) {
        use ringbuf::traits::Producer;
        let Some(trash_tx) = self.trash_tx_timecode.as_mut() else {
            // No trash channel — offline engine. `Engine::new` callers
            // never go through `Command::AttachTimecodeInput` because
            // the command is only producible by an `EngineHandle`,
            // which is only paired with the channel-bearing engine.
            // Defensive fallback: drop here.
            drop(boxed);
            return;
        };
        if let Err(rejected) = trash_tx.try_push(boxed) {
            std::mem::forget(rejected);
            if let Some(counter) = self.trash_overflow_timecode.as_ref() {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    /// Push an old [`Box<ThruSource>`] back to the main thread for
    /// disposal (M7). Symmetric to [`Self::send_timecode_input_to_trash`].
    /// On overflow (channel full) `mem::forget` the box and bump the
    /// thru overflow counter — leaking is the lesser evil compared
    /// with a `dealloc` on the audio thread.
    fn send_thru_source_to_trash(&mut self, boxed: Box<ThruSource>) {
        use ringbuf::traits::Producer;
        let Some(trash_tx) = self.trash_tx_thru.as_mut() else {
            // Offline engine — `Engine::new` callers never go through
            // `Command::AttachThruSource`. Defensive fallback: drop
            // here. (Equivalent to the timecode variant's reasoning.)
            drop(boxed);
            return;
        };
        if let Err(rejected) = trash_tx.try_push(boxed) {
            std::mem::forget(rejected);
            if let Some(counter) = self.trash_overflow_thru.as_ref() {
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

    // --- M5.5 routing tests --------------------------------------------
    //
    // These pin the contract that:
    //  1. `render` and `render_routed(2, INTERNAL_MIXER_ROUTING)` produce
    //     bit-identical output (M4 backwards compatibility).
    //  2. With non-overlapping routing on a 4-channel buffer, each deck's
    //     audio lands ONLY in its assigned channel pair — the other half
    //     is exactly zero (no cross-talk between deck A and deck B).
    //  3. Both decks routed to the same channel pair sum (= internal
    //     mixer); same as the M4 path.
    //  4. `None` in routing fully drops a deck's contribution (transport
    //     state continues to advance, but the buffer stays zero where
    //     that deck would have written).

    /// Build an engine with two decks loaded with distinct constant-
    /// valued tracks. Deck 0 outputs 0.4 on every sample; deck 1
    /// outputs 0.7. With unity gain + master gain, the engine output
    /// reflects deck 0's L=0.4 R=0.4 + deck 1's L=0.7 R=0.7 in the
    /// internal mixer (= 1.1 per sample).
    fn engine_with_two_decks(deck0_v: f32, deck1_v: f32) -> Engine {
        let t0 = Arc::new(Track::from_interleaved(vec![deck0_v; 64], 48_000, 2).unwrap());
        let t1 = Arc::new(Track::from_interleaved(vec![deck1_v; 64], 48_000, 2).unwrap());
        let mut engine = Engine::new(48_000.0, 16);
        engine.deck_mut(0).set_source(t0);
        engine.deck_mut(0).set_playing(true);
        engine.deck_mut(0).quiesce_declick_for_test();
        engine.deck_mut(1).set_source(t1);
        engine.deck_mut(1).set_playing(true);
        engine.deck_mut(1).quiesce_declick_for_test();
        engine
    }

    #[test]
    fn render_routed_internal_mixer_matches_render() {
        // M4 backward compat: render_routed with INTERNAL_MIXER_ROUTING
        // and 2 channels must produce the same samples as render().
        let mut engine_a = engine_with_two_decks(0.3, 0.5);
        let mut engine_b = engine_with_two_decks(0.3, 0.5);
        let mut buf_a = vec![0.0_f32; 32];
        let mut buf_b = vec![0.0_f32; 32];
        let mut rt = RealtimeContext::new();
        engine_a.render(&mut rt, &mut buf_a);
        engine_b.render_routed(&mut rt, &mut buf_b, 2, &INTERNAL_MIXER_ROUTING);
        for (i, (a, b)) in buf_a.iter().zip(buf_b.iter()).enumerate() {
            assert!(
                (a - b).abs() < 1e-9,
                "frame {i}: render={a} render_routed={b}"
            );
        }
        // Both should equal 0.3 + 0.5 = 0.8 on every sample (deck 0 and
        // deck 1 sum into ch 0+1).
        for s in &buf_a {
            assert!((s - 0.8).abs() < 1e-6, "got {s}, expected 0.8");
        }
    }

    #[test]
    fn render_routed_external_4ch_isolates_decks() {
        // Deck 0 → ch 0+1, Deck 1 → ch 2+3 (the M5.5 SL3 / external-
        // mixer scenario). Each deck's audio MUST land ONLY in its
        // pair; the other pair MUST be exactly the level it would be
        // without that deck (i.e. 0 here, because the OTHER deck isn't
        // routed there).
        let mut engine = engine_with_two_decks(0.3, 0.7);
        // 16 frames × 4 channels = 64 samples
        let mut out = vec![0.0_f32; 64];
        let mut rt = RealtimeContext::new();
        engine.render_routed(&mut rt, &mut out, 4, &[Some(0), Some(2)]);
        for (i, frame) in out.chunks_exact(4).enumerate() {
            assert!(
                (frame[0] - 0.3).abs() < 1e-6 && (frame[1] - 0.3).abs() < 1e-6,
                "frame {i}: deck 0 should be on ch 0+1, got [{}, {}, {}, {}]",
                frame[0],
                frame[1],
                frame[2],
                frame[3],
            );
            assert!(
                (frame[2] - 0.7).abs() < 1e-6 && (frame[3] - 0.7).abs() < 1e-6,
                "frame {i}: deck 1 should be on ch 2+3, got [{}, {}, {}, {}]",
                frame[0],
                frame[1],
                frame[2],
                frame[3],
            );
        }
    }

    #[test]
    fn render_routed_overlapping_decks_sum_like_internal_mixer() {
        // Both decks → ch 0+1 of a 4-channel buffer. Behaves exactly
        // like the internal mixer for the first pair; ch 2+3 stay
        // zero. This is the property that lets the engine support both
        // M4 and M5.5 with one primitive.
        let mut engine = engine_with_two_decks(0.3, 0.5);
        let mut out = vec![0.0_f32; 64];
        let mut rt = RealtimeContext::new();
        engine.render_routed(&mut rt, &mut out, 4, &[Some(0), Some(0)]);
        for (i, frame) in out.chunks_exact(4).enumerate() {
            assert!(
                (frame[0] - 0.8).abs() < 1e-6 && (frame[1] - 0.8).abs() < 1e-6,
                "frame {i}: ch 0+1 should sum to 0.8, got {frame:?}",
            );
            #[allow(clippy::float_cmp)]
            {
                assert_eq!(frame[2], 0.0, "frame {i}: ch 2 must stay zero");
                assert_eq!(frame[3], 0.0, "frame {i}: ch 3 must stay zero");
            }
        }
    }

    #[test]
    fn render_routed_none_drops_deck() {
        // Deck 0 routed; deck 1 = None (dropped). Deck 1's audio never
        // appears in the buffer. Deck 1's transport state does NOT
        // advance — see render_routed_none_does_not_advance_transport
        // for the rationale (routing is hardware-mapping, not muting;
        // use Deck::set_gain(0.0) for mute semantics that preserve
        // transport ticking).
        let mut engine = engine_with_two_decks(0.3, 0.7);
        let mut out = vec![0.0_f32; 64];
        let mut rt = RealtimeContext::new();
        engine.render_routed(&mut rt, &mut out, 4, &[Some(0), None]);
        for (i, frame) in out.chunks_exact(4).enumerate() {
            assert!(
                (frame[0] - 0.3).abs() < 1e-6 && (frame[1] - 0.3).abs() < 1e-6,
                "frame {i}: deck 0 on ch 0+1 expected 0.3, got {frame:?}",
            );
            #[allow(clippy::float_cmp)]
            {
                assert_eq!(frame[2], 0.0, "frame {i}: ch 2 must stay zero");
                assert_eq!(frame[3], 0.0, "frame {i}: ch 3 must stay zero");
            }
        }
    }

    #[test]
    fn render_routed_none_does_not_advance_transport() {
        // Unrouted decks are skipped end-to-end — their transport
        // doesn't tick. This pins the M5.5 design choice: routing is a
        // hardware-mapping concern, not a mute mechanism. If the user
        // wants to silence a deck while letting it play through, the
        // M2 per-deck gain knob (`Deck::set_gain(0.0)`) is the right
        // tool. Reusing routing as a mute would couple unrelated
        // concerns and invite weird gotchas (declick envelope state
        // continues to advance on a deck the user thinks is "off",
        // making the next routing flip click).
        let mut engine = engine_with_two_decks(0.3, 0.7);
        let pos_before = engine.deck(1).position_frames();
        let mut out = vec![0.0_f32; 64];
        let mut rt = RealtimeContext::new();
        engine.render_routed(&mut rt, &mut out, 4, &[Some(0), None]);
        let pos_after = engine.deck(1).position_frames();
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(
                pos_before, pos_after,
                "deck 1 routed=None must not advance: before={pos_before}, after={pos_after}"
            );
        }
    }

    #[test]
    fn render_routed_mute_via_gain_keeps_transport_advancing() {
        // Companion to render_routed_none_does_not_advance_transport:
        // gain==0 silences the deck's contribution to the output but
        // its transport keeps ticking, so the playhead is in the right
        // place when gain comes back up.
        let mut engine = engine_with_two_decks(0.3, 0.7);
        engine.deck_mut(1).set_gain(0.0);
        let pos_before = engine.deck(1).position_frames();
        let mut out = vec![0.0_f32; 64];
        let mut rt = RealtimeContext::new();
        engine.render_routed(&mut rt, &mut out, 4, &[Some(0), Some(2)]);
        let pos_after = engine.deck(1).position_frames();
        // Deck 1 is routed but muted; its samples should be zero on
        // ch 2+3, but its position must have advanced ~16 frames.
        for (i, frame) in out.chunks_exact(4).enumerate() {
            #[allow(clippy::float_cmp)]
            {
                assert_eq!(frame[2], 0.0, "frame {i}: muted deck 1 should be zero");
                assert_eq!(frame[3], 0.0, "frame {i}: muted deck 1 should be zero");
            }
        }
        assert!(
            (pos_after - pos_before - 16.0).abs() < 0.5,
            "deck 1 with gain=0 still advances: before={pos_before}, after={pos_after}"
        );
    }

    #[test]
    fn render_routed_master_gain_applies_only_to_routed_channels() {
        // Master gain scales the whole buffer (zero × g == zero) so
        // unrouted channels stay zero. Routed channels are scaled.
        let mut engine = engine_with_two_decks(0.4, 0.4);
        engine.set_master_gain(0.5);
        let mut out = vec![0.0_f32; 64];
        let mut rt = RealtimeContext::new();
        engine.render_routed(&mut rt, &mut out, 4, &[Some(0), Some(2)]);
        for (i, frame) in out.chunks_exact(4).enumerate() {
            // 0.4 deck output × 0.5 master = 0.2.
            for (ch, s) in frame.iter().enumerate() {
                assert!(
                    (s - 0.2).abs() < 1e-6,
                    "frame {i} ch {ch}: expected 0.2 with master 0.5, got {s}"
                );
            }
        }
    }

    #[test]
    fn render_routed_4ch_is_alloc_free() {
        let mut engine = engine_with_two_decks(0.3, 0.5);
        let mut out = vec![0.0_f32; 64];
        let mut rt = RealtimeContext::new();
        assert_no_alloc::assert_no_alloc(|| {
            engine.render_routed(&mut rt, &mut out, 4, &[Some(0), Some(2)]);
        });
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

    // ============================================================
    //                    M5.3 timecode integration
    // ============================================================

    use ringbuf::traits::{Producer as _, Split as _};
    use ringbuf::HeapRb;

    /// Build (engine, deck-0 producer): a configured engine with a
    /// 1-second input ringbuf wired into deck 0 via the timecode path.
    /// Returns the *producer* end so the test can synthesize timecode
    /// audio in lockstep with renders.
    fn engine_with_tc_deck0(sr: f32, block: usize) -> (Engine, ringbuf::HeapProd<f32>) {
        let mut engine = Engine::new(sr, block);
        // 1 s of stereo headroom — comfortable for tests that render
        // a few hundred ms.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let rb = HeapRb::<f32>::new((sr as usize) * 2);
        let (tx, rx) = rb.split();
        let cfg = TimecodeInputConfig {
            format: dub_timecode::Format::SeratoCv02,
            input_sample_rate: sr,
            max_block_frames: block.max(64),
            confidence_threshold: 0.7,
            // Tight hysteresis + minimal stickiness so existing
            // tests, which feed steady-state synthetic carriers,
            // engage on the very first block (no warm-up frames).
            disengage_threshold: 0.5,
            sticky_blocks_to_disengage: 1,
            // Synthetic carrier in tests is full-amplitude (~0.5
            // RMS); a tiny gate lets the carrier through but still
            // exercises the gate code path so any future regression
            // here trips integration tests too.
            amplitude_threshold: 0.001,
        };
        engine
            .attach_timecode_input(0, rx, cfg)
            .expect("attach should succeed");
        (engine, tx)
    }

    #[test]
    fn timecode_attach_rejects_bad_deck_idx() {
        let mut engine = Engine::new(48_000.0, 64);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let err = engine
            .attach_timecode_input(99, rx, TimecodeInputConfig::default())
            .expect_err("idx 99 is out of range");
        assert!(matches!(err, TimecodeAttachError::InvalidDeck { .. }));
    }

    #[test]
    fn timecode_attach_rejects_sr_mismatch() {
        let mut engine = Engine::new(48_000.0, 64);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let cfg = TimecodeInputConfig {
            input_sample_rate: 44_100.0,
            ..TimecodeInputConfig::default()
        };
        let err = engine
            .attach_timecode_input(0, rx, cfg)
            .expect_err("44.1k input vs 48k engine should be rejected");
        assert!(matches!(
            err,
            TimecodeAttachError::SampleRateMismatch { .. }
        ));
    }

    #[test]
    fn timecode_attach_rejects_double_attach() {
        let mut engine = Engine::new(48_000.0, 64);
        let rb1 = HeapRb::<f32>::new(64);
        let (_tx1, rx1) = rb1.split();
        engine
            .attach_timecode_input(0, rx1, TimecodeInputConfig::default())
            .unwrap();
        let rb2 = HeapRb::<f32>::new(64);
        let (_tx2, rx2) = rb2.split();
        let err = engine
            .attach_timecode_input(0, rx2, TimecodeInputConfig::default())
            .expect_err("second attach should fail");
        assert!(matches!(err, TimecodeAttachError::AlreadyAttached { .. }));
    }

    #[test]
    fn timecode_lock_drives_deck_rate_and_plays() {
        // Synthesize forward unity timecode at 48 kHz, push it through
        // the input ringbuf, render the engine, assert the deck:
        // (a) is playing,
        // (b) has rate ≈ 1.0 from the decoder,
        // (c) the loaded track's playhead has advanced.
        let sr = 48_000.0_f32;
        let block = 256_usize;
        let (mut engine, mut tx) = engine_with_tc_deck0(sr, block);

        // Stick a short loop onto the deck so we can read its position.
        let track =
            Arc::new(Track::from_interleaved(vec![0.5_f32; 48_000 * 2], 48_000, 2).unwrap());
        engine.deck_mut(0).set_source(track);
        // We deliberately do NOT call set_playing(true) — the decoder
        // is supposed to do that on the first locked block.
        engine.deck_mut(0).quiesce_declick_for_test();
        assert!(!engine.deck(0).is_playing(), "deck should start paused");

        // Generate 4 blocks worth of forward unity timecode.
        let n = block * 4;
        let mut sig = vec![0.0_f32; n * 2];
        let mut gen = dub_timecode::signal::Generator::new(dub_timecode::Format::SeratoCv02, sr);
        gen.render(&mut sig, 1.0, 0.5);
        let pushed = tx.push_slice(&sig);
        assert_eq!(pushed, sig.len(), "ring should be large enough");

        let mut rt = RealtimeContext::new();
        let mut buf = vec![0.0_f32; block * 2];
        // Render a few blocks so the decoder primes and locks.
        for _ in 0..4 {
            engine.render(&mut rt, &mut buf);
        }

        let last = engine
            .timecode_last_output(0)
            .expect("decoder ran at least once");
        assert!(
            last.confidence > 0.99,
            "synthetic input should lock 1.000 (got {})",
            last.confidence
        );
        assert!(
            (last.rate - 1.0).abs() < 0.01,
            "rate should be ≈1.0 (got {})",
            last.rate
        );
        assert!(
            engine.deck(0).is_playing(),
            "deck should be playing after lock"
        );
        // 4 blocks × 256 frames = 1024 output frames. Deck 0 advanced
        // at rate ≈ 1.0 → position ≈ 1024 frames. Allow some slack for
        // the M3.5 fade-in and decoder priming on the first block.
        let pos = engine.deck(0).position_frames();
        assert!(pos > 200.0, "deck position should advance (got {pos})");
        assert!(pos < 1100.0, "and not run away (got {pos})");
    }

    #[test]
    fn timecode_silence_pauses_deck() {
        // Push pure silence through the input. The decoder reports
        // ~0 confidence; engine's confidence-gated policy mutes the
        // deck.
        let sr = 48_000.0_f32;
        let block = 256_usize;
        let (mut engine, mut tx) = engine_with_tc_deck0(sr, block);

        let track =
            Arc::new(Track::from_interleaved(vec![0.5_f32; 48_000 * 2], 48_000, 2).unwrap());
        engine.deck_mut(0).set_source(track);
        engine.deck_mut(0).quiesce_declick_for_test();

        // 2 blocks of silence into the input ring.
        let silence = vec![0.0_f32; block * 2 * 2];
        let pushed = tx.push_slice(&silence);
        assert_eq!(pushed, silence.len());

        let mut rt = RealtimeContext::new();
        let mut buf = vec![0.0_f32; block * 2];
        engine.render(&mut rt, &mut buf);
        engine.render(&mut rt, &mut buf);

        let last = engine.timecode_last_output(0).unwrap();
        assert!(
            last.confidence < 0.2,
            "silence should yield low confidence (got {})",
            last.confidence
        );
        assert!(
            !engine.deck(0).is_playing(),
            "low confidence should mute the deck"
        );
        // Output buffer should be silence — deck is paused, no other
        // decks loaded.
        for s in &buf {
            assert!(s.abs() < 0.01, "expected silence on output, got {s}");
        }
    }

    #[test]
    fn timecode_reverse_lock_drives_negative_rate() {
        let sr = 48_000.0_f32;
        let block = 256_usize;
        let (mut engine, mut tx) = engine_with_tc_deck0(sr, block);

        let track =
            Arc::new(Track::from_interleaved(vec![0.5_f32; 48_000 * 2], 48_000, 2).unwrap());
        engine.deck_mut(0).set_source(track);
        engine.deck_mut(0).set_position_frames(10_000.0); // start mid-track for reverse
        engine.deck_mut(0).quiesce_declick_for_test();

        let n = block * 4;
        let mut sig = vec![0.0_f32; n * 2];
        let mut gen = dub_timecode::signal::Generator::new(dub_timecode::Format::SeratoCv02, sr);
        gen.render(&mut sig, -1.0, 0.5);
        tx.push_slice(&sig);

        let mut rt = RealtimeContext::new();
        let mut buf = vec![0.0_f32; block * 2];
        for _ in 0..4 {
            engine.render(&mut rt, &mut buf);
        }

        let last = engine.timecode_last_output(0).unwrap();
        assert!(
            (last.rate - (-1.0)).abs() < 0.01,
            "reverse should give rate ≈-1.0 (got {})",
            last.rate
        );
        // Position should have moved backward from 10_000.
        assert!(
            engine.deck(0).position_frames() < 10_000.0,
            "reverse should walk position back from 10000 (got {})",
            engine.deck(0).position_frames()
        );
    }

    #[test]
    fn timecode_drive_path_is_alloc_free() {
        // Hot-loop steady-state: synthetic timecode in, decode, drive
        // deck transport. Must never allocate. This is the M5.3
        // RT-safety contract.
        let sr = 48_000.0_f32;
        let block = 256_usize;
        let (mut engine, mut tx) = engine_with_tc_deck0(sr, block);

        let track =
            Arc::new(Track::from_interleaved(vec![0.5_f32; 48_000 * 2], 48_000, 2).unwrap());
        engine.deck_mut(0).set_source(track);
        engine.deck_mut(0).quiesce_declick_for_test();

        let mut gen = dub_timecode::signal::Generator::new(dub_timecode::Format::SeratoCv02, sr);

        // Pre-allocate the per-block working buffer and prime the
        // decoder with one block before entering the assert.
        let mut sig = vec![0.0_f32; block * 2];
        gen.render(&mut sig, 1.0, 0.5);
        tx.push_slice(&sig);

        let mut rt = RealtimeContext::new();
        let mut buf = vec![0.0_f32; block * 2];
        engine.render(&mut rt, &mut buf);

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..50 {
                gen.render(&mut sig, 1.0, 0.5);
                let _ = tx.push_slice(&sig);
                engine.render(&mut rt, &mut buf);
            }
        });
    }

    #[test]
    fn timecode_detach_returns_input_for_main_thread_drop() {
        let mut engine = Engine::new(48_000.0, 64);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        engine
            .attach_timecode_input(0, rx, TimecodeInputConfig::default())
            .unwrap();
        let detached = engine.detach_timecode_input(0);
        assert!(detached.is_some(), "detach should return the input");
        // After detach the slot is free for another attach.
        let rb2 = HeapRb::<f32>::new(64);
        let (_tx2, rx2) = rb2.split();
        engine
            .attach_timecode_input(0, rx2, TimecodeInputConfig::default())
            .expect("re-attach after detach should succeed");
    }

    // ============================================================
    //  M5.4.5: command-channel attach (EngineHandle::attach_*)
    // ============================================================

    /// Synthetic forward-unity timecode helper used by the M5.4.5
    /// command-channel attach tests. Pushes one block of carrier into
    /// `tx`, drives the engine, returns the post-block decoder
    /// snapshot if there is one.
    fn drive_one_block_with_synth_carrier(
        engine: &mut Engine,
        tx: &mut ringbuf::HeapProd<f32>,
        gen: &mut dub_timecode::signal::Generator,
        sig: &mut [f32],
        rt: &mut RealtimeContext<'_>,
        buf: &mut [f32],
    ) {
        gen.render(sig, 1.0, 0.5);
        let _ = tx.push_slice(sig);
        engine.render(rt, buf);
    }

    #[test]
    fn handle_attach_to_empty_slot_starts_decoding() {
        // M5.4.5 happy path: build engine with handle, move the engine
        // through a render block (mimicking AudioOutput taking
        // ownership), then attach a timecode input mid-stream via the
        // handle. After the next render, decoder output should appear.
        let sr = 48_000.0_f32;
        let block = 256_usize;
        let (mut engine, mut handle) = Engine::new_with_handle(sr, block);

        // Pre-attach: nothing decoded.
        assert!(engine.timecode_last_output(0).is_none());

        // Build the input ringbuf on the main thread, push a few blocks
        // of synthetic carrier, then attach via handle. (In the live
        // CLI flow the calibrator thread is what populates the ring,
        // not the test, but the assert is the same: input audio +
        // attach → decode output.)
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let rb = HeapRb::<f32>::new((sr as usize) * 2);
        let (mut tx, rx) = rb.split();

        let mut gen = dub_timecode::signal::Generator::new(dub_timecode::Format::SeratoCv02, sr);
        let mut sig = vec![0.0_f32; block * 2];
        gen.render(&mut sig, 1.0, 0.5);
        let _ = tx.push_slice(&sig);

        handle
            .attach_timecode_input(
                0,
                rx,
                TimecodeInputConfig {
                    format: dub_timecode::Format::SeratoCv02,
                    input_sample_rate: sr,
                    max_block_frames: block.max(64),
                    confidence_threshold: 0.7,
                    disengage_threshold: 0.5,
                    sticky_blocks_to_disengage: 1,
                    amplitude_threshold: 0.001,
                },
            )
            .expect("empty-slot attach should succeed");

        // First render: command channel is drained, AttachTimecodeInput
        // is applied, decoder primes on the buffered carrier.
        let mut rt = RealtimeContext::new();
        let mut buf = vec![0.0_f32; block * 2];
        for _ in 0..4 {
            drive_one_block_with_synth_carrier(
                &mut engine,
                &mut tx,
                &mut gen,
                &mut sig,
                &mut rt,
                &mut buf,
            );
        }

        let last = engine
            .timecode_last_output(0)
            .expect("decoder should have run after handle attach");
        assert!(
            last.confidence > 0.95,
            "synthetic input should lock high (got {})",
            last.confidence
        );
    }

    #[test]
    fn handle_attach_to_filled_slot_replaces_and_trashes_previous() {
        // M5.4.5 mid-stream re-attach: a deck already has a
        // TimecodeInput, the calibrator runs again (e.g. cartridge
        // swap), `attach_timecode_input` is called a second time. The
        // previous TimecodeInput must be trashed (sent back to main
        // thread for disposal), not dropped on the audio thread.
        let sr = 48_000.0_f32;
        let block = 256_usize;
        let (mut engine, mut handle) = Engine::new_with_handle(sr, block);

        let cfg = TimecodeInputConfig {
            format: dub_timecode::Format::SeratoCv02,
            input_sample_rate: sr,
            max_block_frames: block.max(64),
            confidence_threshold: 0.7,
            disengage_threshold: 0.5,
            sticky_blocks_to_disengage: 1,
            amplitude_threshold: 0.001,
        };

        // First attach.
        let rb1 = HeapRb::<f32>::new(1024);
        let (_tx1, rx1) = rb1.split();
        handle.attach_timecode_input(0, rx1, cfg).unwrap();

        // Drain the command channel into the engine so the slot is
        // actually filled by the time the second attach lands.
        let mut rt = RealtimeContext::new();
        let mut buf = vec![0.0_f32; block * 2];
        engine.render(&mut rt, &mut buf);

        // Second attach (different ringbuf; would replace).
        let rb2 = HeapRb::<f32>::new(1024);
        let (_tx2, rx2) = rb2.split();
        handle
            .attach_timecode_input(0, rx2, cfg)
            .expect("re-attach should succeed via command channel");

        // Drain the second command. The audio thread should now have
        // sent the displaced (first) TimecodeInput back through the
        // timecode trash channel.
        engine.render(&mut rt, &mut buf);

        // reclaim() drops the displaced box on the main thread and
        // returns 1 (one item drained).
        let n = handle.reclaim();
        assert_eq!(
            n, 1,
            "re-attach should have produced exactly one trashed TimecodeInput"
        );
        assert_eq!(
            handle.timecode_trash_overflow_count(),
            0,
            "trash channel should not have overflowed"
        );
    }

    #[test]
    fn handle_attach_rejects_invalid_deck_idx() {
        let sr = 48_000.0_f32;
        let block = 256_usize;
        let (_engine, mut handle) = Engine::new_with_handle(sr, block);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let err = handle
            .attach_timecode_input(99, rx, TimecodeInputConfig::default())
            .expect_err("idx 99 is out of range");
        assert!(matches!(err, TimecodeAttachError::InvalidDeck { .. }));
    }

    #[test]
    fn handle_attach_rejects_sr_mismatch_before_sending_command() {
        // Bad config caught early — the handle should not push a
        // bogus command into the channel.
        let sr = 48_000.0_f32;
        let block = 256_usize;
        let (mut engine, mut handle) = Engine::new_with_handle(sr, block);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let cfg = TimecodeInputConfig {
            input_sample_rate: 44_100.0,
            ..TimecodeInputConfig::default()
        };
        let err = handle
            .attach_timecode_input(0, rx, cfg)
            .expect_err("44.1k vs 48k engine should be rejected");
        assert!(matches!(
            err,
            TimecodeAttachError::SampleRateMismatch { .. }
        ));

        // Confirm nothing landed on the deck — render with no input
        // produces no decoder output.
        let mut rt = RealtimeContext::new();
        let mut buf = vec![0.0_f32; block * 2];
        engine.render(&mut rt, &mut buf);
        assert!(engine.timecode_last_output(0).is_none());
    }

    #[test]
    fn handle_attach_to_invalid_idx_does_not_leak() {
        // Defensive belt: even if a buggy command-channel attach gets
        // through to the audio thread with a bad idx (which can't
        // actually happen because EngineHandle::attach_timecode_input
        // validates), the engine-side handler must trash the box
        // rather than drop it on the audio thread.
        //
        // We can't construct that bad command via the handle (it
        // returns InvalidDeck before sending), so we drive
        // `apply_command` directly with a synthesised command — the
        // only test in this file that does so, justified because
        // we're pinning the audio-side leak-safety contract, not
        // the handle-side validation contract.
        let sr = 48_000.0_f32;
        let block = 64_usize;
        let (mut engine, mut handle) = Engine::new_with_handle(sr, block);

        let cfg = TimecodeInputConfig {
            format: dub_timecode::Format::SeratoCv02,
            input_sample_rate: sr,
            max_block_frames: block.max(64),
            confidence_threshold: 0.7,
            disengage_threshold: 0.5,
            sticky_blocks_to_disengage: 1,
            amplitude_threshold: 0.001,
        };
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let bogus_input = Box::new(TimecodeInput::new(rx, cfg));
        engine.apply_command(Command::AttachTimecodeInput {
            idx: 99,
            input: bogus_input,
        });
        // Bad idx should have routed the box to the timecode trash,
        // not dropped on the audio thread.
        assert_eq!(handle.timecode_trash_overflow_count(), 0);
        let n = handle.reclaim();
        assert_eq!(n, 1, "bad-idx box must trash, not leak or panic");
    }

    // ===================================================================
    // M7 (Thru Mode) — engine integration tests.
    //
    // These verify the engine-side dispatch: when a deck has a thru
    // source attached, `render_routed` calls `ThruSource::render_into`
    // for that deck's channel pair and SKIPS the Deck's own render path
    // entirely (the deck's transport doesn't advance even if a track is
    // loaded). When no thru source is attached, the deck renders
    // normally — proving the M0-M6 Track path is untouched.
    //
    // Thru behaviour itself (additive passthrough, alloc-free, stride/
    // offset correctness) is tested in `thru.rs`. Here we test the
    // engine's *routing* of audio between Track and Thru decks,
    // including the multi-deck case where one is Track and the other
    // is Thru, plus the additive M5.5.2 4-channel external-mixer
    // routing.
    // ===================================================================

    fn thru_cfg(sr: f32) -> ThruInputConfig {
        ThruInputConfig {
            max_block_frames: 1024,
            input_sample_rate: sr,
        }
    }

    /// Push `n` stereo frames of `(l, r)` into the producer. Defensive
    /// against the ring filling up (unlikely with capacity 4096 and
    /// n ≤ 1024, but the loop short-circuits gracefully).
    fn push_thru_input(tx: &mut ringbuf::HeapProd<f32>, n: usize, l: f32, r: f32) {
        for _ in 0..n {
            if tx.try_push(l).is_err() {
                return;
            }
            if tx.try_push(r).is_err() {
                return;
            }
        }
    }

    #[test]
    fn thru_attach_rejects_bad_deck_idx() {
        let mut engine = Engine::new(48_000.0, 256);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let err = engine
            .attach_thru_source(DECK_COUNT, rx, thru_cfg(48_000.0))
            .unwrap_err();
        assert!(
            matches!(err, ThruAttachError::InvalidDeck { .. }),
            "expected InvalidDeck, got {err:?}"
        );
    }

    #[test]
    fn thru_attach_rejects_sr_mismatch() {
        let mut engine = Engine::new(48_000.0, 256);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let cfg = ThruInputConfig {
            max_block_frames: 256,
            input_sample_rate: 44_100.0,
        };
        let err = engine.attach_thru_source(0, rx, cfg).unwrap_err();
        assert!(
            matches!(err, ThruAttachError::SampleRateMismatch { .. }),
            "expected SampleRateMismatch, got {err:?}"
        );
    }

    #[test]
    fn thru_attached_deck_skips_track_render_path() {
        // Setup: deck 0 has a (non-silent) Track loaded AND a Thru
        // source attached, but no audio pushed into the Thru ring.
        // The dispatch should hit ThruSource::render_into and skip
        // the Deck's track-render path, so the rendered output is
        // silence (underrun → 0.0 added to the zeroed output). The
        // Track is not rendered — this proves "Thru wins" over
        // Track when both are present.
        let sr = 48_000.0;
        let mut engine = Engine::new(sr, 256);

        // Load a non-silent track on deck 0.
        let track = Arc::new(Track::from_interleaved(vec![0.7; 8], 48_000, 2).unwrap());
        engine.deck_mut(0).set_source(track);
        engine.deck_mut(0).set_playing(true);
        engine.deck_mut(0).quiesce_declick_for_test();

        // Attach an empty Thru source.
        let rb = HeapRb::<f32>::new(4096);
        let (_tx, rx) = rb.split();
        engine.attach_thru_source(0, rx, thru_cfg(sr)).unwrap();

        let mut out = vec![1.0_f32; 4 * 2];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut out);

        // The render zeros the output buffer first, then the empty
        // Thru ring adds 0.0 → all samples zero. If the Track-render
        // path had run, we'd see ~0.7 here.
        for (i, s) in out.iter().enumerate() {
            assert!(s.abs() < 1e-9, "frame {i}: expected silence, got {s}");
        }
    }

    #[test]
    fn thru_attached_deck_passes_input_through() {
        let sr = 48_000.0;
        let mut engine = Engine::new(sr, 256);
        let rb = HeapRb::<f32>::new(8192);
        let (mut tx, rx) = rb.split();
        engine.attach_thru_source(0, rx, thru_cfg(sr)).unwrap();

        push_thru_input(&mut tx, 64, 0.3, -0.4);
        let mut out = vec![0.0_f32; 64 * 2];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut out);
        for i in 0..64 {
            assert!(
                (out[i * 2] - 0.3).abs() < 1e-5,
                "frame {i} L = {} expected 0.3",
                out[i * 2]
            );
            assert!(
                (out[i * 2 + 1] - (-0.4)).abs() < 1e-5,
                "frame {i} R = {} expected -0.4",
                out[i * 2 + 1]
            );
        }
    }

    #[test]
    fn thru_render_does_not_advance_deck_transport() {
        // A Thru deck's track-side transport must not tick — the deck
        // has no notion of position when sourced from a real record.
        // Load a track on deck 0, attach Thru, render some blocks,
        // confirm the deck's position is exactly where it was at attach
        // time (zero, since set_source resets it).
        let sr = 48_000.0;
        let mut engine = Engine::new(sr, 256);
        let track = Arc::new(Track::from_interleaved(vec![0.5; 1024], 48_000, 2).unwrap());
        engine.deck_mut(0).set_source(track);
        engine.deck_mut(0).set_playing(true);
        engine.deck_mut(0).quiesce_declick_for_test();
        let pos_before = engine.deck(0).position_frames();

        let rb = HeapRb::<f32>::new(4096);
        let (_tx, rx) = rb.split();
        engine.attach_thru_source(0, rx, thru_cfg(sr)).unwrap();

        let mut out = vec![0.0_f32; 256 * 2];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut out);
        engine.render(&mut rt, &mut out);
        engine.render(&mut rt, &mut out);

        let pos_after = engine.deck(0).position_frames();
        assert!(
            (pos_after - pos_before).abs() < 1e-9,
            "deck transport advanced under thru: {pos_before} -> {pos_after}"
        );
    }

    #[test]
    fn track_deck_unaffected_when_other_deck_is_thru() {
        // Two-deck setup: deck 0 in Thru (empty ring → silent), deck 1
        // in Track (playing). The 4-channel external-mixer routing
        // sends deck 0 → ch 0+1 and deck 1 → ch 2+3. Pin the contract:
        // deck 1's audio is unchanged by deck 0's Thru attachment.
        let sr = 48_000.0;
        let mut engine = Engine::new(sr, 256);

        // Deck 1: playing track of 0.5.
        let track = Arc::new(Track::from_interleaved(vec![0.5; 1024], 48_000, 2).unwrap());
        engine.deck_mut(1).set_source(track);
        engine.deck_mut(1).set_playing(true);
        engine.deck_mut(1).quiesce_declick_for_test();

        // Deck 0: Thru attached, empty ring → silent.
        let rb = HeapRb::<f32>::new(4096);
        let (_tx, rx) = rb.split();
        engine.attach_thru_source(0, rx, thru_cfg(sr)).unwrap();

        let mut out = vec![0.0_f32; 16 * 4]; // 16 frames, 4 channels.
        let mut rt = RealtimeContext::new();
        engine.render_routed(&mut rt, &mut out, 4, &[Some(0), Some(2)]);

        for frame in 0..16 {
            assert!(out[frame * 4].abs() < 1e-9, "frame {frame} ch0 nonzero");
            assert!(out[frame * 4 + 1].abs() < 1e-9, "frame {frame} ch1 nonzero");
            assert!(
                (out[frame * 4 + 2] - 0.5).abs() < 1e-5,
                "frame {frame} ch2 = {} expected 0.5",
                out[frame * 4 + 2]
            );
            assert!(
                (out[frame * 4 + 3] - 0.5).abs() < 1e-5,
                "frame {frame} ch3 = {} expected 0.5",
                out[frame * 4 + 3]
            );
        }
    }

    #[test]
    fn thru_routing_to_4ch_lands_on_correct_pair() {
        // Deck 0 Thru → ch 2+3 on a 4-channel buffer. Confirm the
        // Thru audio lands on the right pair and ch 0+1 stay zero.
        let sr = 48_000.0;
        let mut engine = Engine::new(sr, 256);
        let rb = HeapRb::<f32>::new(8192);
        let (mut tx, rx) = rb.split();
        engine.attach_thru_source(0, rx, thru_cfg(sr)).unwrap();

        push_thru_input(&mut tx, 32, 0.6, 0.6);
        let mut out = vec![0.0_f32; 32 * 4];
        let mut rt = RealtimeContext::new();
        engine.render_routed(&mut rt, &mut out, 4, &[Some(2), None]);
        for frame in 0..32 {
            assert!(out[frame * 4].abs() < 1e-9, "frame {frame} ch0");
            assert!(out[frame * 4 + 1].abs() < 1e-9, "frame {frame} ch1");
            assert!(
                (out[frame * 4 + 2] - 0.6).abs() < 1e-5,
                "frame {frame} ch2 = {}",
                out[frame * 4 + 2]
            );
            assert!(
                (out[frame * 4 + 3] - 0.6).abs() < 1e-5,
                "frame {frame} ch3 = {}",
                out[frame * 4 + 3]
            );
        }
    }

    #[test]
    fn thru_render_respects_deck_gain() {
        let sr = 48_000.0;
        let mut engine = Engine::new(sr, 256);
        let rb = HeapRb::<f32>::new(8192);
        let (mut tx, rx) = rb.split();
        engine.attach_thru_source(0, rx, thru_cfg(sr)).unwrap();
        engine.deck_mut(0).set_gain(0.5);

        push_thru_input(&mut tx, 32, 1.0, 1.0);
        let mut out = vec![0.0_f32; 32 * 2];
        let mut rt = RealtimeContext::new();
        engine.render(&mut rt, &mut out);
        for frame in 0..32 {
            assert!(
                (out[frame * 2] - 0.5).abs() < 1e-5,
                "frame {frame}: out_l = {} expected 0.5",
                out[frame * 2]
            );
        }
    }

    #[test]
    fn thru_render_is_alloc_free() {
        let sr = 48_000.0;
        let mut engine = Engine::new(sr, 256);
        let rb = HeapRb::<f32>::new(8192);
        let (mut tx, rx) = rb.split();
        engine.attach_thru_source(0, rx, thru_cfg(sr)).unwrap();
        push_thru_input(&mut tx, 1024, 0.3, 0.4);
        let mut out = vec![0.0_f32; 256 * 2];
        let mut rt = RealtimeContext::new();
        assert_no_alloc::assert_no_alloc(|| {
            engine.render(&mut rt, &mut out);
        });
    }

    #[test]
    fn detach_thru_returns_source_for_main_thread_drop() {
        let sr = 48_000.0;
        let mut engine = Engine::new(sr, 256);
        let rb = HeapRb::<f32>::new(4096);
        let (_tx, rx) = rb.split();
        engine.attach_thru_source(0, rx, thru_cfg(sr)).unwrap();
        assert!(engine.thru_attached(0));
        let detached = engine.detach_thru_source(0);
        assert!(detached.is_some(), "expected Some(ThruSource)");
        assert!(
            !engine.thru_attached(0),
            "slot should be empty after detach"
        );
        // The returned ThruSource owns its HeapCons; dropping it here
        // (on the test/main thread) is the correct disposal path.
        drop(detached);
    }

    // -- Command-channel attach path ---------------------------------

    /// Drive one block through the engine so any pending commands
    /// take effect. Uses the simplest stereo path; the command-side
    /// behaviour under test is independent of what audio comes out.
    fn pump_one_block(engine: &mut Engine, rt: &mut RealtimeContext<'_>) {
        let mut out = vec![0.0_f32; 256 * 2];
        engine.render(rt, &mut out);
    }

    #[test]
    fn handle_attach_thru_to_empty_slot_starts_dispatching() {
        let sr = 48_000.0;
        let (mut engine, mut handle) = Engine::new_with_handle(sr, 256);
        let rb = HeapRb::<f32>::new(4096);
        let (_tx, rx) = rb.split();
        handle.attach_thru_source(0, rx, thru_cfg(sr)).unwrap();
        let mut rt = RealtimeContext::new();
        pump_one_block(&mut engine, &mut rt);
        assert!(engine.thru_attached(0));
        assert!(!engine.thru_attached(1));
    }

    #[test]
    fn handle_attach_thru_to_filled_slot_replaces_and_trashes_previous() {
        let sr = 48_000.0;
        let (mut engine, mut handle) = Engine::new_with_handle(sr, 256);
        let rb = HeapRb::<f32>::new(4096);
        let (_tx, rx) = rb.split();
        handle.attach_thru_source(0, rx, thru_cfg(sr)).unwrap();
        let mut rt = RealtimeContext::new();
        pump_one_block(&mut engine, &mut rt);
        // Second attach: displaces the first; old box should land in
        // the thru trash.
        let rb2 = HeapRb::<f32>::new(4096);
        let (_tx2, rx2) = rb2.split();
        handle.attach_thru_source(0, rx2, thru_cfg(sr)).unwrap();
        pump_one_block(&mut engine, &mut rt);
        assert_eq!(handle.thru_trash_overflow_count(), 0);
        let n = handle.reclaim();
        assert_eq!(
            n, 1,
            "expected exactly one displaced ThruSource to be reclaimed"
        );
    }

    #[test]
    fn handle_attach_thru_rejects_invalid_deck_idx() {
        let sr = 48_000.0;
        let (_engine, mut handle) = Engine::new_with_handle(sr, 256);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let err = handle
            .attach_thru_source(DECK_COUNT, rx, thru_cfg(sr))
            .unwrap_err();
        assert!(matches!(err, ThruAttachError::InvalidDeck { .. }));
    }

    #[test]
    fn handle_attach_thru_rejects_sr_mismatch_before_sending_command() {
        let sr = 48_000.0;
        let (mut engine, mut handle) = Engine::new_with_handle(sr, 256);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let bad_cfg = ThruInputConfig {
            max_block_frames: 256,
            input_sample_rate: 44_100.0,
        };
        let err = handle.attach_thru_source(0, rx, bad_cfg).unwrap_err();
        assert!(matches!(err, ThruAttachError::SampleRateMismatch { .. }));
        // No command should have been enqueued.
        let mut rt = RealtimeContext::new();
        pump_one_block(&mut engine, &mut rt);
        assert!(!engine.thru_attached(0));
    }

    // -------- M8 — BPM-tracked Thru attach --------

    #[test]
    fn handle_attach_thru_with_bpm_tracking_spawns_stream() {
        let sr = 48_000.0;
        let (mut engine, mut handle) = Engine::new_with_handle(sr, 256);
        let rb = HeapRb::<f32>::new(4096);
        let (_tx, rx) = rb.split();
        let stream = handle
            .attach_thru_source_with_bpm_tracking(
                0,
                rx,
                thru_cfg(sr),
                dub_bpm::TrackerConfig {
                    sample_rate: 48_000,
                    channels: 1,
                    analysis_period_samples: 48_000,
                    bpm_range: dub_bpm::BpmRange::DEFAULT,
                },
            )
            .expect("attach with bpm");
        let mut rt = RealtimeContext::new();
        pump_one_block(&mut engine, &mut rt);
        assert!(engine.thru_attached(0));
        // Drop the stream explicitly — joins the analysis thread.
        // If the analysis thread is wedged this test will hang.
        stream.shutdown();
    }

    #[test]
    fn handle_attach_thru_with_bpm_rejects_engine_sr_mismatch() {
        let sr = 48_000.0;
        let (_engine, mut handle) = Engine::new_with_handle(sr, 256);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        // Tracker SR is 44.1k but engine is 48k → mismatch.
        let bad_tracker = dub_bpm::TrackerConfig {
            sample_rate: 44_100,
            channels: 1,
            analysis_period_samples: 44_100,
            bpm_range: dub_bpm::BpmRange::DEFAULT,
        };
        let err = handle
            .attach_thru_source_with_bpm_tracking(0, rx, thru_cfg(sr), bad_tracker)
            .unwrap_err();
        assert!(matches!(
            err,
            ThruAttachWithBpmError::SampleRateMismatch { .. }
        ));
    }

    #[test]
    fn handle_attach_thru_with_bpm_rejects_invalid_tracker_config() {
        let sr = 48_000.0;
        let (_engine, mut handle) = Engine::new_with_handle(sr, 256);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        // analysis_period_samples = 0 is rejected by BpmTracker::new
        // → propagates up as BadTrackerConfig.
        let bad_tracker = dub_bpm::TrackerConfig {
            sample_rate: 48_000,
            channels: 1,
            analysis_period_samples: 0,
            bpm_range: dub_bpm::BpmRange::DEFAULT,
        };
        let err = handle
            .attach_thru_source_with_bpm_tracking(0, rx, thru_cfg(sr), bad_tracker)
            .unwrap_err();
        assert!(matches!(err, ThruAttachWithBpmError::BadTrackerConfig(_)));
    }

    #[test]
    fn handle_attach_thru_with_bpm_forwards_invalid_deck_idx() {
        let sr = 48_000.0;
        let (_engine, mut handle) = Engine::new_with_handle(sr, 256);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let err = handle
            .attach_thru_source_with_bpm_tracking(
                DECK_COUNT,
                rx,
                thru_cfg(sr),
                dub_bpm::TrackerConfig::at(48_000),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            ThruAttachWithBpmError::Thru(ThruAttachError::InvalidDeck { .. })
        ));
    }

    #[test]
    fn handle_apply_command_bad_idx_thru_attach_routes_to_trash() {
        // Bypass `attach_thru_source`'s validation by hand-crafting a
        // bogus AttachThruSource command directly and feeding it to
        // the engine. The engine must not panic and must NOT drop the
        // box on the audio thread.
        let sr = 48_000.0;
        let (mut engine, mut handle) = Engine::new_with_handle(sr, 256);
        let rb = HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let bogus = Box::new(ThruSource::new(rx, thru_cfg(sr)));
        engine.apply_command(Command::AttachThruSource {
            idx: 99,
            source: bogus,
        });
        assert_eq!(handle.thru_trash_overflow_count(), 0);
        let n = handle.reclaim();
        assert_eq!(n, 1, "bad-idx box must trash, not leak");
    }
}
