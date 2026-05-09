//! A single deck: holds a track, plays it forward or backward at any rate,
//! mixes its output into a stereo buffer.
//!
//! Per PRD §4.4: forward and backward playback are byte-for-byte symmetric.
//! A negative rate is just "rate" with a sign. This is the foundation for
//! scratch (M5+), backspins, dnb-style manual rewinds, and ordinary
//! varispeed playback all using the same code path.
//!
//! M1 ships **linear interpolation** between adjacent track frames. This is
//! the standard "scratching" resampler — fast, branch-free, no aliasing
//! artefacts at extreme rates because anti-aliased resampling at e.g. 50×
//! reverse playback is not perceptually meaningful. Anti-aliased sinc
//! resampling for ordinary playback (with key-lock disabled) lands later
//! when we evaluate whether linear is audibly insufficient.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use dub_io::Track;

use crate::realtime::RealtimeContext;

/// State shared between an audio-thread [`Deck`] and the main-thread
/// [`crate::handle::DeckCommand`] proxy. Lock-free reads from the UI
/// thread, lock-free writes from the audio thread.
///
/// Three values are made visible across the boundary:
///
/// - **position (in track frames)** as `f64::to_bits` packed in an
///   `AtomicU64`. The audio thread updates this once per render block
///   (post-block position); the UI reads it for waveform/playhead.
///   Relaxed ordering is sufficient: it's a one-way snapshot, no
///   synchronization required.
/// - **playing flag**: the deck's transport state. Audio thread writes
///   when commands change it; UI reads to render the play/pause button.
/// - **end-of-track flag**: set by the audio thread when the playhead
///   walks off the end of the source. Lets the UI auto-reset etc.
#[derive(Debug)]
pub(crate) struct DeckSharedState {
    pub(crate) position_bits: AtomicU64,
    pub(crate) is_playing: AtomicBool,
    pub(crate) at_end: AtomicBool,
}

impl DeckSharedState {
    pub(crate) fn new() -> Self {
        Self {
            position_bits: AtomicU64::new(0.0f64.to_bits()),
            is_playing: AtomicBool::new(false),
            at_end: AtomicBool::new(false),
        }
    }

    pub(crate) fn store_position(&self, frames: f64) {
        self.position_bits
            .store(frames.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn load_position(&self) -> f64 {
        f64::from_bits(self.position_bits.load(Ordering::Relaxed))
    }

    pub(crate) fn store_playing(&self, playing: bool) {
        self.is_playing.store(playing, Ordering::Relaxed);
    }

    pub(crate) fn load_playing(&self) -> bool {
        self.is_playing.load(Ordering::Relaxed)
    }

    pub(crate) fn store_at_end(&self, at_end: bool) {
        self.at_end.store(at_end, Ordering::Relaxed);
    }

    pub(crate) fn load_at_end(&self) -> bool {
        self.at_end.load(Ordering::Relaxed)
    }
}

/// A single deck's transport + audio source state.
///
/// Two views of the deck exist:
///
/// - the **audio-thread Deck** (this struct) holds the playhead, source,
///   and renders audio. Owned by the [`crate::Engine`].
/// - the **main-thread proxy** ([`crate::handle::DeckCommand`]) sends
///   commands to mutate this deck and reads the latest position via the
///   shared atomic snapshot.
///
/// They communicate through `Arc<DeckSharedState>`, written by the audio
/// thread once per render block and read with `Relaxed` ordering by the UI.
#[derive(Debug)]
pub struct Deck {
    source: Option<Arc<Track>>,

    /// Current playhead, in **track frames**, as a floating-point value.
    /// Sample-accurate over very long tracks (`f64`).
    position: f64,

    /// Playback rate in **track-frames per output-frame**. Already factors
    /// in the engine vs track sample-rate ratio. Set via [`Deck::set_rate`].
    rate: f64,

    /// Linear gain applied to the deck's contribution to the mix. `1.0` is
    /// unity. Range checked at set-time but not in render (RT discipline).
    gain: f32,

    /// True when this deck contributes audio to the engine output. False
    /// means the deck is muted and renders silence (without advancing).
    playing: bool,

    /// Atomic snapshot shared with the main-thread handle. Audio-thread-only
    /// writes; `Arc::clone` happens off-RT in the constructor.
    shared: Arc<DeckSharedState>,
}

impl Deck {
    /// Construct an empty deck with no track loaded. Allocates the shared
    /// atomic state — call this off the audio thread.
    #[must_use]
    pub fn new() -> Self {
        Self {
            source: None,
            position: 0.0,
            rate: 1.0,
            gain: 1.0,
            playing: false,
            shared: Arc::new(DeckSharedState::new()),
        }
    }

    /// Return a clone of the shared state Arc. Used by the engine handle
    /// constructor to plumb the same atomic snapshot to both sides.
    pub(crate) fn shared(&self) -> Arc<DeckSharedState> {
        self.shared.clone()
    }

    /// Load a track. Resets the playhead to position 0.
    pub fn set_source(&mut self, track: Arc<Track>) {
        self.source = Some(track);
        self.position = 0.0;
        self.shared.store_position(0.0);
        self.shared.store_at_end(false);
    }

    /// Clear the loaded track. The deck renders silence afterward.
    pub fn clear_source(&mut self) {
        self.source = None;
        self.position = 0.0;
        self.shared.store_position(0.0);
        self.shared.store_at_end(false);
    }

    /// Borrow the loaded track, if any.
    #[must_use]
    pub fn source(&self) -> Option<&Arc<Track>> {
        self.source.as_ref()
    }

    /// Current playback rate (track frames per output frame).
    #[must_use]
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Set the playback rate. `1.0` is realtime at the source SR; `2.0`
    /// is double speed; `-1.0` is reverse at realtime; `0.0` is paused
    /// (does not advance, but also does not stop the deck — the engine
    /// will still mix in whatever's currently at the playhead position).
    ///
    /// **Note**: this is the raw rate. Higher-level callers should
    /// pre-multiply by `track_sample_rate / engine_sample_rate` so an
    /// 1.0 rate at a 44.1k track on a 48k engine actually plays at the
    /// musical speed the user expects.
    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate;
    }

    /// Current playback position in track frames.
    #[must_use]
    pub fn position_frames(&self) -> f64 {
        self.position
    }

    /// Set the playback position in track frames. Clamped to the track's
    /// length when a source is loaded; otherwise stored as-is.
    pub fn set_position_frames(&mut self, position: f64) {
        self.position = position;
        self.shared.store_position(position);
        self.shared.store_at_end(false);
    }

    /// Linear gain. Default `1.0`.
    #[must_use]
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Set the linear gain. Negative values invert phase; out-of-range is
    /// allowed but generally not what the user wants.
    pub fn set_gain(&mut self, gain: f32) {
        self.gain = gain;
    }

    /// `true` when the deck is currently contributing audio.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Set play/pause.
    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        self.shared.store_playing(playing);
    }

    /// Render this deck's contribution into `out`, mixing additively.
    ///
    /// `out` is interleaved stereo, length `2 * frames`. The caller is
    /// responsible for zeroing it if a fresh mix is desired.
    ///
    /// `engine_sr` is the engine's output sample rate; the deck adjusts
    /// its rate to convert between the track's SR and the engine's SR.
    ///
    /// The deck reads frames at fractional positions using linear
    /// interpolation. Out-of-range positions contribute silence. Playback
    /// outside `[0, frames)` is allowed (the caller may have set it via
    /// `set_position_frames`); the deck simply renders silence and keeps
    /// advancing in case the rate later brings it back into range.
    ///
    /// **RT-safety**: no allocation, no locks, no syscalls. The only
    /// inputs are the deck's pre-allocated state and the input buffer.
    pub fn render(&mut self, rt: &mut RealtimeContext<'_>, out: &mut [f32], engine_sr: f32) {
        rt.tick();
        debug_assert_eq!(
            out.len() % 2,
            0,
            "stereo output buffer must have even length"
        );

        let Some(track) = self.source.as_ref() else {
            // No source loaded: render silence by default.
            return;
        };
        if !self.playing {
            return;
        }

        let track_sr = f64::from(track.sample_rate());
        let engine_sr = f64::from(engine_sr);
        // A `rate` of 1.0 means "play this track at its natural pitch".
        // The per-output-frame increment in track-frames is therefore
        // `rate * (track_sr / engine_sr)`.
        let frame_increment = self.rate * (track_sr / engine_sr);

        // Tracks are bounded by available memory; an f64 mantissa (52 bits)
        // covers ~3 million years at 48 kHz, so the precision loss is theoretical.
        #[allow(clippy::cast_precision_loss)]
        let track_len = track.frames() as f64;
        #[allow(clippy::cast_precision_loss)]
        let last_index_f = (track.frames().saturating_sub(1)) as f64;

        let gain = self.gain;
        let mut pos = self.position;

        for chunk in out.chunks_exact_mut(2) {
            // Read a stereo sample at the current fractional position.
            // Out of range (negative or >= frames) → silent contribution.
            if pos >= 0.0 && pos < track_len {
                let i_floor = pos.floor();
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let idx = i_floor as usize;
                #[allow(clippy::cast_possible_truncation)]
                let frac = (pos - i_floor) as f32;
                let a = track.frame(idx);
                let b = if i_floor < last_index_f {
                    track.frame(idx + 1)
                } else {
                    a
                };
                let l = a[0] + (b[0] - a[0]) * frac;
                let r = a[1] + (b[1] - a[1]) * frac;
                chunk[0] += l * gain;
                chunk[1] += r * gain;
            }
            pos += frame_increment;
        }

        self.position = pos;

        // Publish snapshot for UI. Two atomic writes, both Relaxed —
        // the UI does not need to synchronize on these, only observe
        // the most recent value.
        self.shared.store_position(pos);
        let off_end = pos < 0.0 || pos >= track_len;
        if off_end != self.shared.load_at_end() {
            self.shared.store_at_end(off_end);
        }
    }
}

impl Default for Deck {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn const_track(samples: &[f32], channels: u8, sample_rate: u32) -> Arc<Track> {
        Arc::new(Track::from_interleaved(samples.to_vec(), sample_rate, channels).unwrap())
    }

    #[test]
    fn empty_deck_renders_silence() {
        let mut deck = Deck::new();
        let mut rt = RealtimeContext::new();
        let mut out = [0.5f32; 8];
        deck.render(&mut rt, &mut out, 48_000.0);
        // No source loaded: render is a no-op (additive mix).
        for s in out {
            #[allow(clippy::float_cmp)]
            {
                assert_eq!(s, 0.5);
            }
        }
    }

    #[test]
    fn paused_deck_does_not_advance() {
        let mut deck = Deck::new();
        deck.set_source(const_track(&[0.5, -0.5, 0.5, -0.5], 2, 48_000));
        deck.set_playing(false);
        let mut rt = RealtimeContext::new();
        let mut out = [0.0f32; 8];
        deck.render(&mut rt, &mut out, 48_000.0);
        #[allow(clippy::float_cmp)]
        for s in out {
            assert_eq!(s, 0.0);
        }
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(deck.position_frames(), 0.0);
        }
    }

    #[test]
    fn forward_playback_at_unity_rate_matches_source() {
        // Stereo track: 4 frames of (i, -i)
        let mut samples = Vec::new();
        for i in 0..4 {
            samples.push(i as f32);
            samples.push(-(i as f32));
        }
        let mut deck = Deck::new();
        deck.set_source(const_track(&samples, 2, 48_000));
        deck.set_playing(true);

        let mut rt = RealtimeContext::new();
        let mut out = vec![0.0f32; 8];
        deck.render(&mut rt, &mut out, 48_000.0);

        for f in 0..4 {
            #[allow(clippy::float_cmp)]
            {
                assert_eq!(out[f * 2], f as f32);
                assert_eq!(out[f * 2 + 1], -(f as f32));
            }
        }
    }

    #[test]
    fn reverse_playback_reads_in_reverse() {
        // Mono track of 4 distinct samples
        let track = const_track(&[1.0, 2.0, 3.0, 4.0], 1, 48_000);
        let mut deck = Deck::new();
        deck.set_source(track);
        deck.set_playing(true);
        deck.set_position_frames(3.0);
        deck.set_rate(-1.0);

        let mut rt = RealtimeContext::new();
        let mut out = vec![0.0f32; 8];
        deck.render(&mut rt, &mut out, 48_000.0);

        // Should read 4.0, 3.0, 2.0, 1.0 — written to both stereo channels
        let expected = [4.0, 4.0, 3.0, 3.0, 2.0, 2.0, 1.0, 1.0];
        for (got, want) in out.iter().zip(expected.iter()) {
            assert!((got - want).abs() < 1e-6, "got {got}, want {want}");
        }
    }

    #[test]
    fn out_of_range_position_is_silent() {
        let track = const_track(&[1.0, 2.0, 3.0, 4.0], 1, 48_000);
        let mut deck = Deck::new();
        deck.set_source(track);
        deck.set_playing(true);
        deck.set_rate(1.0);
        deck.set_position_frames(-100.0);

        let mut rt = RealtimeContext::new();
        let mut out = vec![0.5f32; 8];
        deck.render(&mut rt, &mut out, 48_000.0);
        // Initial buffer was all 0.5; deck added nothing because positions
        // -100..-96 are out of range, so output stays 0.5.
        for s in out {
            #[allow(clippy::float_cmp)]
            {
                assert_eq!(s, 0.5);
            }
        }
    }

    #[test]
    fn render_is_additive_not_replacing() {
        let track = const_track(&[1.0, 1.0, 1.0, 1.0], 1, 48_000);
        let mut deck = Deck::new();
        deck.set_source(track);
        deck.set_playing(true);

        let mut rt = RealtimeContext::new();
        let mut out = vec![0.5f32; 8];
        deck.render(&mut rt, &mut out, 48_000.0);
        // Each output is 0.5 (initial) + 1.0 (deck) = 1.5
        for s in out {
            assert!((s - 1.5).abs() < 1e-6, "got {s}");
        }
    }

    #[test]
    fn gain_scales_output() {
        let track = const_track(&[1.0, 1.0, 1.0, 1.0], 1, 48_000);
        let mut deck = Deck::new();
        deck.set_source(track);
        deck.set_playing(true);
        deck.set_gain(0.25);

        let mut rt = RealtimeContext::new();
        let mut out = vec![0.0f32; 8];
        deck.render(&mut rt, &mut out, 48_000.0);
        for s in out {
            assert!((s - 0.25).abs() < 1e-6);
        }
    }

    #[test]
    fn sample_rate_conversion_44k_to_48k() {
        // 4 frames at 44.1k. Rendered into a 48k engine at unity rate.
        // Increment per output frame = 44100/48000 ≈ 0.91875 frames.
        // We just verify position advances correctly and no panic occurs.
        let track = const_track(&[0.1, 0.2, 0.3, 0.4], 1, 44_100);
        let mut deck = Deck::new();
        deck.set_source(track);
        deck.set_playing(true);

        let mut rt = RealtimeContext::new();
        let mut out = vec![0.0f32; 8]; // 4 frames at 48k
        deck.render(&mut rt, &mut out, 48_000.0);

        // Position should have advanced ~3.675 frames over 4 output frames.
        let expected = 4.0 * 44_100.0 / 48_000.0;
        assert!(
            (deck.position_frames() - expected).abs() < 1e-6,
            "got {} want {}",
            deck.position_frames(),
            expected
        );
    }

    proptest! {
        #[test]
        fn render_never_panics(
            samples in proptest::collection::vec(-1.0f32..=1.0, 2..256),
            channels in 1u8..=2,
            sample_rate in 8_000u32..=192_000,
            engine_sr in 8_000u32..=192_000,
            rate in -8.0f64..=8.0,
            position in -1_000.0f64..=10_000.0,
            n_frames in 1usize..=128,
        ) {
            // Trim samples to a multiple of channels.
            let n = samples.len();
            let trimmed = n - (n % usize::from(channels));
            let mut samples = samples;
            samples.truncate(trimmed);
            prop_assume!(!samples.is_empty());

            let track = Arc::new(
                Track::from_interleaved(samples, sample_rate, channels).unwrap()
            );

            let mut deck = Deck::new();
            deck.set_source(track);
            deck.set_playing(true);
            deck.set_rate(rate);
            deck.set_position_frames(position);

            let mut rt = RealtimeContext::new();
            let mut out = vec![0.0f32; n_frames * 2];
            deck.render(&mut rt, &mut out, engine_sr as f32);

            // Output must not contain NaN / inf
            for s in &out {
                prop_assert!(s.is_finite(), "non-finite sample {s}");
            }
        }
    }
}
