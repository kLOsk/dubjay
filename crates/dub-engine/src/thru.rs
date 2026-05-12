//! Per-deck Thru source (M7) — wires the audio-interface input
//! ringbuffer through the engine for FX processing on real records.
//!
//! ## What Thru Mode is in Dub
//!
//! Thru Mode in Dub is "real record on the platter, software path
//! always hot." The audio interface delivers preamp'd phono input
//! through CoreAudio; that signal is *always* read into the engine
//! and *always* written back out. There is **no** hardware-bypass
//! mode in Dub. Software bypass would be incompatible with our
//! value proposition: BPM detection (M8), waveform capture (M9), and
//! FX (M15+) all live in the software path and need the signal to
//! flow through it.
//!
//! Latency cost: one buffer of round-trip software latency, ~2.7 ms
//! at a 64-frame buffer / 48 kHz. Same cost a loaded track pays in
//! the deck-render path. Well below the ~5 ms scratch-imperceptibility
//! threshold from PRD §6.1, and — critically — *constant*: it doesn't
//! change when FX engage, so scratch muscle memory stays calibrated.
//!
//! ## Hardware Thru is outside Dub's scope
//!
//! Some interfaces (SL3, TA6, …) expose a physical Thru button that
//! routes the analog preamp output directly to the device's analog
//! output, bypassing USB / the host entirely. That gives zero
//! latency, but the audio never enters software, so we have no way
//! to detect BPM, render a waveform, or apply FX. If the operator
//! engages hardware Thru on the interface itself, Dub simply sees no
//! input on that pair — it cannot tell the difference between
//! "stylus is in the air" and "the box is routing around me." That
//! is intentional. The cost of integrating with vendor-specific
//! hardware monitoring would be high and the value low (we'd be
//! optimising for a workflow that gives up the features the rest of
//! the app exists to deliver).
//!
//! ## FX engagement (preview for M15+)
//!
//! FX modules will be inserted *inside* the engine's per-deck signal
//! chain. Each FX owns its own engage/disengage semantics with a
//! per-module declick on its *wet* output. The dry path through
//! [`ThruSource`] is untouched on engage/disengage — input flows
//! unchanged underneath, the wet bloom layers on top. This is what
//! a hardware insert effect feels like (Boss DD-3, RE-201 send/
//! return), and it gives a constant input-to-output latency the DJ
//! can internalise in muscle memory.
//!
//! The earlier ("M7 ship") design had `ThruSource` itself flip
//! between Direct (engine-silent) and Processed (engine-active)
//! modes via an FX-engaged refcount. That was wrong on two counts:
//! (a) Direct depended on hardware monitoring we don't drive, so it
//! produced silence in practice; (b) the path-swap latency-jitter
//! between modes was exactly the timing instability the rest of the
//! engine works to avoid. Both are fixed by collapsing to one mode
//! and letting FX bypass be a per-FX, in-chain concern.
//!
//! ## RT-safety
//!
//! - `pop_slice` on the SPSC consumer is a load + memcpy.
//! - The scratch buffer is pre-allocated at attach time to
//!   `max_block_frames * 2` interleaved samples; never resized on
//!   the audio thread.
//! - The mono-downmix scratch (shared by the M8 BPM tee and M9
//!   peaks tap) is pre-allocated to `max_block_frames` mono samples
//!   at [`ThruSource::new`]; the per-block mono-downmix is two
//!   reads + one write per frame, computed *once* when any tap is
//!   attached and pushed to each enabled tap. `push_slice` to a tee
//!   ring is a memcpy that silently drops on overflow (consumer too
//!   slow → drop newest).
//! - No `Box`, no `Vec` resize, no `dealloc`, no syscall on the hot
//!   path. Verified by `render_is_alloc_free` below.

use ringbuf::traits::{Consumer, Observer, Producer};
use ringbuf::{HeapCons, HeapProd};

/// Off-RT config for [`ThruSource::new`]. Validated against the engine
/// sample rate by [`ThruInputConfig::validate`] before attach.
#[derive(Debug, Clone, Copy)]
pub struct ThruInputConfig {
    /// Worst-case render block size. Sizes the internal pop-scratch
    /// buffer at attach time so the audio thread does no allocation.
    pub max_block_frames: usize,
    /// Engine sample rate. Must match (within 0.5 Hz) so the source
    /// audio plays at the right speed — sample-rate conversion is not
    /// in v1 scope.
    pub input_sample_rate: f32,
}

impl Default for ThruInputConfig {
    fn default() -> Self {
        Self {
            max_block_frames: 1024,
            input_sample_rate: 48_000.0,
        }
    }
}

/// Errors from [`crate::handle::EngineHandle::attach_thru_source`].
#[allow(missing_docs)]
#[derive(Debug, thiserror::Error)]
pub enum ThruAttachError {
    #[error("deck index {idx} out of range (have {count})")]
    InvalidDeck { idx: usize, count: usize },

    #[error(
        "thru input SR {input_sr} Hz != engine SR {engine_sr} Hz \
         (sample-rate conversion is not in v1 scope)"
    )]
    SampleRateMismatch { input_sr: f32, engine_sr: f32 },

    #[error("max_block_frames must be > 0")]
    BadBlockSize,

    /// The engine command channel is full. Surface this so the caller
    /// can back off; we never block on the audio thread to drain.
    #[error("engine command channel full")]
    ChannelFull,
}

impl ThruInputConfig {
    /// Off-RT validation. Run from
    /// [`crate::handle::EngineHandle::attach_thru_source`] so the audio
    /// thread never sees a bad config.
    pub(crate) fn validate(&self, engine_sr: f32) -> Result<(), ThruAttachError> {
        if (self.input_sample_rate - engine_sr).abs() > 0.5 {
            return Err(ThruAttachError::SampleRateMismatch {
                input_sr: self.input_sample_rate,
                engine_sr,
            });
        }
        if self.max_block_frames == 0 {
            return Err(ThruAttachError::BadBlockSize);
        }
        Ok(())
    }
}

/// One deck's-worth of Thru source. Owned by the audio thread after
/// [`crate::handle::EngineHandle::attach_thru_source`] succeeds; the
/// command-channel transfer goes through a `Box<ThruSource>` payload
/// so the heap-bearing construction happens off-RT.
///
/// Behaviour is a single mode: read input ring → add gain-scaled
/// samples into the deck's routed output slot. No state machine, no
/// mode flip — that's the Option A FX-bypass model documented in the
/// module-level docs.
///
/// ## Optional mono-downmix taps (M8 + M9)
///
/// `with_bpm_tee` (M8) and `with_peaks_tap` (M9) each attach a
/// mono-downmix tap that pushes one mono sample per stereo input
/// frame into a second ringbuf on every `render_into` call. The
/// downmix is computed *once* per block and dispatched to whichever
/// taps are enabled — feeding both BPM analysis and peak capture
/// adds one memcpy, not two passes over the buffer.
///
/// Consumer ends are read by:
/// * M8 [`dub_bpm::BpmStream`] off-RT analysis thread (tempo).
/// * M9 [`dub_peaks::PeakStream`] off-RT decimator thread
///   (waveform).
///
/// Tee writes are alloc-free, non-blocking, and lose samples
/// silently on overflow (consumer too slow → newest samples drop;
/// the resulting brief hole in the BPM ODF or peak buffer only
/// affects telemetry, not the audio path).
pub struct ThruSource {
    /// Single-producer/single-consumer ring; producer is the CoreAudio
    /// input IOProc inside `dub-audio`'s `AudioInput`.
    rx: HeapCons<f32>,

    /// Pre-allocated workspace for one block of input samples. Sized
    /// `max_block_frames * 2` (interleaved stereo) at attach time.
    scratch: Vec<f32>,

    /// Optional producer end of the M8 BPM tee ring. Present iff
    /// [`Self::with_bpm_tee`] was called.
    bpm_tx: Option<HeapProd<f32>>,

    /// Optional producer end of the M9 peaks tap ring. Present iff
    /// [`Self::with_peaks_tap`] was called.
    peaks_tx: Option<HeapProd<f32>>,

    /// Pre-allocated mono-downmix scratch shared between the BPM
    /// tee and the peaks tap. Sized to `max_block_frames` mono
    /// samples at [`Self::new`] regardless of whether any tap is
    /// attached — 4 KB at the default 1024-frame block, which is
    /// negligible. Lets `with_bpm_tee` / `with_peaks_tap` be pure
    /// "flip a flag" operations off-RT.
    mono_scratch: Vec<f32>,
}

impl ThruSource {
    /// Off-RT constructor. Allocates the scratch buffer; the audio
    /// thread side touches no heap from here.
    #[must_use]
    pub fn new(rx: HeapCons<f32>, cfg: ThruInputConfig) -> Self {
        let scratch_len = cfg.max_block_frames.saturating_mul(2).max(2);
        Self {
            rx,
            scratch: vec![0.0_f32; scratch_len],
            bpm_tx: None,
            peaks_tx: None,
            mono_scratch: vec![0.0_f32; cfg.max_block_frames.max(1)],
        }
    }

    /// Builder-style: wire a mono-downmix tap to feed M8's
    /// [`dub_bpm::BpmStream`] off-RT analysis thread.
    ///
    /// `max_block_frames` is no longer used for sizing (the shared
    /// `mono_scratch` is allocated at [`Self::new`]); the parameter
    /// is kept for source-compatibility with M8 callers but
    /// `debug_assert!`ed to match the constructor's value, so a
    /// disagreement is loudly diagnosed in debug builds.
    ///
    /// Returns `self` for chaining. Off-RT only.
    #[must_use]
    pub fn with_bpm_tee(mut self, bpm_tx: HeapProd<f32>, max_block_frames: usize) -> Self {
        debug_assert_eq!(
            self.mono_scratch.len(),
            max_block_frames.max(1),
            "with_bpm_tee max_block_frames {max_block_frames} != mono_scratch capacity {}; \
             pass the same value as ThruInputConfig::max_block_frames",
            self.mono_scratch.len()
        );
        self.bpm_tx = Some(bpm_tx);
        self
    }

    /// Builder-style: wire a mono-downmix tap to feed M9's
    /// [`dub_peaks::PeakStream`] off-RT decimator thread.
    ///
    /// Shares the same per-block mono buffer with [`Self::with_bpm_tee`]
    /// — calling both is exactly as cheap as either one alone plus
    /// one extra ring `push_slice`.
    ///
    /// Returns `self` for chaining. Off-RT only.
    #[must_use]
    pub fn with_peaks_tap(mut self, peaks_tx: HeapProd<f32>) -> Self {
        self.peaks_tx = Some(peaks_tx);
        self
    }

    /// Whether a BPM tee was attached at construction. Diagnostic /
    /// test-only observability.
    #[must_use]
    pub fn has_bpm_tee(&self) -> bool {
        self.bpm_tx.is_some()
    }

    /// Whether a peaks tap was attached at construction. Diagnostic /
    /// test-only observability.
    #[must_use]
    pub fn has_peaks_tap(&self) -> bool {
        self.peaks_tx.is_some()
    }

    /// Number of input samples currently buffered between the IOProc
    /// and the engine. UI-side observability for "is the input alive?".
    #[must_use]
    pub fn available(&self) -> usize {
        self.rx.occupied_len()
    }

    /// Audio-thread render. Additive into `out` at `(stride, offset)`,
    /// matching [`crate::Deck::render_into`]'s contract so this
    /// composes with [`crate::Engine::render_routed`] (M5.5.1) for
    /// external-mixer per-deck routing.
    ///
    /// The L sample goes at `out[offset + n*stride]`, the R at
    /// `out[offset + n*stride + 1]`, for `n` in `0..frames`. Existing
    /// content in those cells is **added to**, not overwritten —
    /// same convention as `Deck::render_into`. Cells outside the
    /// `(stride, offset)` pair are untouched.
    ///
    /// Underrun (empty ring) is rendered as silence: the trailing
    /// frames of the scratch buffer are zeroed, so the additive
    /// write adds 0.0 and the output is unchanged. This is the
    /// correct behaviour while the IOProc warms up (a few hundred
    /// microseconds at startup) and after a transient input
    /// glitch.
    pub fn render_into(&mut self, out: &mut [f32], gain: f32, stride: usize, offset: usize) {
        debug_assert!(
            stride >= 2,
            "stride must be at least 2 to hold a stereo pair"
        );
        debug_assert!(
            offset + 2 <= stride,
            "offset {offset} + 2 must fit inside stride {stride}"
        );
        debug_assert_eq!(
            out.len() % stride,
            0,
            "output buffer length must be a multiple of stride"
        );

        let frames = out.len() / stride;
        if frames == 0 {
            return;
        }

        // Pull n stereo frames from the input ring. Cap at scratch
        // size — any overflow simply waits for the next block (a
        // 4 s ring at 48 kHz absorbs any plausible block size).
        let want = (frames * 2).min(self.scratch.len());
        let popped = self.rx.pop_slice(&mut self.scratch[..want]);
        // Defensive: align to whole stereo frames. The IOProc only
        // ever pushes whole frames (channels × N), so `popped` is
        // even in practice; masking guards against a hypothetical
        // misaligned producer.
        let popped_even = popped & !1;
        // Zero the tail of scratch beyond what we got, so frames that
        // weren't filled this block render as silence (underrun-safe).
        for s in &mut self.scratch[popped_even..want] {
            *s = 0.0;
        }

        for (i, chunk) in out.chunks_exact_mut(stride).enumerate() {
            let src_l = self.scratch.get(i * 2).copied().unwrap_or(0.0);
            let src_r = self.scratch.get(i * 2 + 1).copied().unwrap_or(0.0);
            chunk[offset] += src_l * gain;
            chunk[offset + 1] += src_r * gain;
        }

        // M8 / M9 telemetry taps. Mono-downmix the popped stereo
        // frames (NOT including the gain — analysis wants the raw
        // input level so confidence/envelope stay calibrated
        // independently of any subsequent gain ride) and push to
        // whichever taps are enabled. The downmix is computed once
        // and dispatched; both taps see the same mono samples.
        //
        // Alloc-free: `mono_scratch` is pre-allocated, `push_slice`
        // is a memcpy that silently writes only what fits.
        if self.bpm_tx.is_some() || self.peaks_tx.is_some() {
            let mono_frames = want / 2;
            debug_assert!(
                self.mono_scratch.len() >= mono_frames,
                "mono_scratch undersized: {} < {mono_frames}",
                self.mono_scratch.len()
            );
            for i in 0..mono_frames {
                let l = self.scratch[i * 2];
                let r = self.scratch[i * 2 + 1];
                self.mono_scratch[i] = 0.5 * (l + r);
            }
            let mono = &self.mono_scratch[..mono_frames];
            // Drop on overflow (analysis thread too slow). Returns
            // how many were pushed; we don't care for v1.
            if let Some(tx) = &mut self.bpm_tx {
                let _ = tx.push_slice(mono);
            }
            if let Some(tx) = &mut self.peaks_tx {
                let _ = tx.push_slice(mono);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;
    use ringbuf::traits::{Producer, Split};
    use ringbuf::HeapProd;
    use ringbuf::HeapRb;

    const SR: f32 = 48_000.0;

    fn build(cfg: ThruInputConfig, ring_capacity: usize) -> (ThruSource, HeapProd<f32>) {
        let rb = HeapRb::<f32>::new(ring_capacity);
        let (tx, rx) = rb.split();
        let src = ThruSource::new(rx, cfg);
        (src, tx)
    }

    fn cfg_default() -> ThruInputConfig {
        ThruInputConfig {
            max_block_frames: 256,
            input_sample_rate: SR,
        }
    }

    /// Fill `n` stereo frames of constant `(l, r)` into the producer.
    fn push_const(tx: &mut HeapProd<f32>, n: usize, l: f32, r: f32) -> usize {
        let mut pushed = 0;
        for _ in 0..n {
            if tx.try_push(l).is_err() {
                break;
            }
            pushed += 1;
            if tx.try_push(r).is_err() {
                break;
            }
            pushed += 1;
        }
        pushed
    }

    fn zeros(frames: usize) -> Vec<f32> {
        vec![0.0_f32; frames * 2]
    }

    // -------- Configuration validation --------

    #[test]
    fn validate_sample_rate_mismatch() {
        let cfg = ThruInputConfig {
            max_block_frames: 256,
            input_sample_rate: 44_100.0,
        };
        let err = cfg.validate(48_000.0).unwrap_err();
        assert!(matches!(err, ThruAttachError::SampleRateMismatch { .. }));
    }

    #[test]
    fn validate_zero_block_size() {
        let cfg = ThruInputConfig {
            max_block_frames: 0,
            input_sample_rate: SR,
        };
        let err = cfg.validate(SR).unwrap_err();
        assert!(matches!(err, ThruAttachError::BadBlockSize));
    }

    #[test]
    fn validate_matching_sr_within_tolerance() {
        let cfg = ThruInputConfig {
            max_block_frames: 256,
            input_sample_rate: 48_000.4,
        };
        cfg.validate(48_000.0).unwrap();
    }

    // -------- Passthrough render --------

    #[test]
    fn passes_input_through_at_unit_gain() {
        let (mut src, mut tx) = build(cfg_default(), 4096);
        let pushed = push_const(&mut tx, 128, 0.4, -0.2);
        assert_eq!(pushed, 256);

        let mut out = zeros(128);
        src.render_into(&mut out, 1.0, 2, 0);

        for i in 0..128 {
            assert!(
                (out[i * 2] - 0.4).abs() < 1e-6,
                "frame {i} L = {} expected 0.4",
                out[i * 2]
            );
            assert!(
                (out[i * 2 + 1] - (-0.2)).abs() < 1e-6,
                "frame {i} R = {} expected -0.2",
                out[i * 2 + 1]
            );
        }
    }

    #[test]
    fn render_is_additive_not_replacing() {
        // Pre-populate the output with 0.5 and confirm we ADD 0.1
        // (not overwrite) for a final 0.6 per channel.
        let (mut src, mut tx) = build(cfg_default(), 4096);
        let _ = push_const(&mut tx, 64, 0.1, 0.2);
        let mut out = vec![0.5_f32; 64 * 2];
        src.render_into(&mut out, 1.0, 2, 0);
        for i in 0..64 {
            assert!(
                (out[i * 2] - 0.6).abs() < 1e-5,
                "frame {i}: out_l = {} expected 0.6 (0.5 base + 0.1 input)",
                out[i * 2]
            );
            assert!(
                (out[i * 2 + 1] - 0.7).abs() < 1e-5,
                "frame {i}: out_r = {} expected 0.7 (0.5 base + 0.2 input)",
                out[i * 2 + 1]
            );
        }
    }

    // -------- Stride / offset for M5.5.1 routing --------

    #[test]
    fn render_into_writes_at_offset_only() {
        let (mut src, mut tx) = build(cfg_default(), 4096);
        let _ = push_const(&mut tx, 32, 0.7, 0.7);

        // 8-channel output: write to channels 4+5 (offset=4, stride=8).
        let mut out = vec![9.99_f32; 32 * 8];
        src.render_into(&mut out, 1.0, 8, 4);

        for frame in 0..32 {
            // Channels other than 4 and 5 should be untouched (9.99).
            for ch in [0, 1, 2, 3, 6, 7] {
                let v = out[frame * 8 + ch];
                assert!(
                    (v - 9.99).abs() < 1e-6,
                    "frame {frame} ch {ch} touched: {v}"
                );
            }
            // Channels 4 and 5 should be additive: 9.99 + 0.7 = 10.69.
            assert!(
                (out[frame * 8 + 4] - 10.69).abs() < 1e-4,
                "frame {frame} ch4 = {}",
                out[frame * 8 + 4]
            );
            assert!(
                (out[frame * 8 + 5] - 10.69).abs() < 1e-4,
                "frame {frame} ch5 = {}",
                out[frame * 8 + 5]
            );
        }
    }

    // -------- Gain applied --------

    #[test]
    fn gain_scales_output() {
        let (mut src, mut tx) = build(cfg_default(), 4096);
        let _ = push_const(&mut tx, 64, 1.0, 1.0);
        let mut out = zeros(64);
        src.render_into(&mut out, 0.5, 2, 0);
        for i in 0..64 {
            assert!(
                (out[i * 2] - 0.5).abs() < 1e-6,
                "frame {i}: out_l = {} expected 0.5",
                out[i * 2]
            );
            assert!(
                (out[i * 2 + 1] - 0.5).abs() < 1e-6,
                "frame {i}: out_r = {} expected 0.5",
                out[i * 2 + 1]
            );
        }
    }

    // -------- Underrun --------

    #[test]
    fn empty_ring_renders_silence_additively() {
        let (mut src, _tx) = build(cfg_default(), 4096);
        // Pre-populate output with 0.5; passing through an empty
        // ring should add 0.0 and leave 0.5 in place.
        let mut out = vec![0.5_f32; 64 * 2];
        src.render_into(&mut out, 1.0, 2, 0);
        for s in &out {
            assert!(
                (s - 0.5).abs() < 1e-9,
                "expected 0.5 (underrun adds 0.0), got {s}"
            );
        }
    }

    #[test]
    fn partial_underrun_renders_received_then_silence() {
        // Push 16 frames; ask for 64. The first 16 should pass
        // through additively, the remaining 48 should be silence.
        let (mut src, mut tx) = build(cfg_default(), 4096);
        let _ = push_const(&mut tx, 16, 0.3, -0.3);
        let mut out = vec![1.0_f32; 64 * 2];
        src.render_into(&mut out, 1.0, 2, 0);
        // First 16 frames: 1.0 + 0.3 = 1.3 L, 1.0 - 0.3 = 0.7 R.
        for i in 0..16 {
            assert!(
                (out[i * 2] - 1.3).abs() < 1e-5,
                "frame {i} L = {}",
                out[i * 2]
            );
            assert!(
                (out[i * 2 + 1] - 0.7).abs() < 1e-5,
                "frame {i} R = {}",
                out[i * 2 + 1]
            );
        }
        // Remaining 48 frames: untouched 1.0 (silence added).
        for i in 16..64 {
            assert!(
                (out[i * 2] - 1.0).abs() < 1e-9,
                "frame {i} L = {}",
                out[i * 2]
            );
            assert!(
                (out[i * 2 + 1] - 1.0).abs() < 1e-9,
                "frame {i} R = {}",
                out[i * 2 + 1]
            );
        }
    }

    // -------- Allocation discipline --------

    #[test]
    fn render_is_alloc_free() {
        let (mut src, mut tx) = build(cfg_default(), 4096);
        let _ = push_const(&mut tx, 1024, 0.3, 0.4);
        let mut out = zeros(256);
        assert_no_alloc(|| {
            src.render_into(&mut out, 1.0, 2, 0);
        });
    }

    #[test]
    fn empty_ring_render_is_alloc_free() {
        let (mut src, _tx) = build(cfg_default(), 4096);
        let mut out = zeros(256);
        assert_no_alloc(|| {
            src.render_into(&mut out, 1.0, 2, 0);
        });
    }

    // -------- Available observability --------

    #[test]
    fn available_reports_buffered_samples() {
        let (src, mut tx) = build(cfg_default(), 4096);
        assert_eq!(src.available(), 0);
        let _ = push_const(&mut tx, 32, 0.1, 0.2);
        assert_eq!(src.available(), 64);
    }

    // -------- M8 BPM tee --------

    #[test]
    fn fresh_thru_source_has_no_bpm_tee() {
        let (src, _tx) = build(cfg_default(), 4096);
        assert!(!src.has_bpm_tee());
    }

    #[test]
    fn with_bpm_tee_attaches() {
        let (src, _tx) = build(cfg_default(), 4096);
        let tee_ring = HeapRb::<f32>::new(4096);
        let (bpm_tx, _bpm_rx) = tee_ring.split();
        let src = src.with_bpm_tee(bpm_tx, 256);
        assert!(src.has_bpm_tee());
    }

    #[test]
    fn bpm_tee_receives_mono_downmix_of_input() {
        // L = 0.4, R = -0.2 → mono = 0.5 * (0.4 + (-0.2)) = 0.10
        let (src, mut tx) = build(cfg_default(), 4096);
        let tee_ring = HeapRb::<f32>::new(4096);
        let (bpm_tx, mut bpm_rx) = tee_ring.split();
        let mut src = src.with_bpm_tee(bpm_tx, 256);

        let pushed = push_const(&mut tx, 128, 0.4, -0.2);
        assert_eq!(pushed, 256);

        let mut out = zeros(128);
        src.render_into(&mut out, 1.0, 2, 0);

        let mut mono = [0.0f32; 128];
        let n = bpm_rx.pop_slice(&mut mono);
        assert_eq!(n, 128, "expected 128 mono samples in the tee, got {n}");
        for (i, &s) in mono.iter().enumerate() {
            assert!(
                (s - 0.10).abs() < 1e-6,
                "frame {i}: mono = {s}, expected 0.10"
            );
        }
    }

    #[test]
    fn bpm_tee_unaffected_by_gain() {
        // Gain rides on the output path; the tee gets the raw input
        // so BPM confidence stays calibrated even if the deck gain
        // changes. Audible at gain=0.5 should be half, tee should
        // be the raw downmix.
        let (src, mut tx) = build(cfg_default(), 4096);
        let tee_ring = HeapRb::<f32>::new(4096);
        let (bpm_tx, mut bpm_rx) = tee_ring.split();
        let mut src = src.with_bpm_tee(bpm_tx, 256);
        let _ = push_const(&mut tx, 64, 1.0, 1.0);

        let mut out = zeros(64);
        src.render_into(&mut out, 0.5, 2, 0);

        // Output L/R should be 0.5 (gain 0.5 × 1.0)
        for i in 0..64 {
            assert!(
                (out[i * 2] - 0.5).abs() < 1e-6,
                "out L {} != 0.5",
                out[i * 2]
            );
        }

        // Tee should be 1.0 (raw downmix of [1.0, 1.0]; gain not applied)
        let mut mono = [0.0f32; 64];
        let n = bpm_rx.pop_slice(&mut mono);
        assert_eq!(n, 64);
        for (i, &s) in mono.iter().enumerate() {
            assert!(
                (s - 1.0).abs() < 1e-6,
                "frame {i}: tee = {s}, expected 1.0 (pre-gain)"
            );
        }
    }

    #[test]
    fn bpm_tee_silently_drops_on_full_ring() {
        // Tiny tee ring (16 samples) being fed a 64-sample block —
        // push_slice writes only what fits and drops the rest. The
        // audio path is unaffected.
        let (src, mut tx) = build(cfg_default(), 4096);
        let tee_ring = HeapRb::<f32>::new(16);
        let (bpm_tx, mut bpm_rx) = tee_ring.split();
        let mut src = src.with_bpm_tee(bpm_tx, 256);
        let _ = push_const(&mut tx, 64, 0.5, 0.5);

        let mut out = zeros(64);
        src.render_into(&mut out, 1.0, 2, 0);

        // Audio output is the full 64 frames as usual.
        for i in 0..64 {
            assert!((out[i * 2] - 0.5).abs() < 1e-6);
        }

        // Tee captured up to 16 mono samples (ring capacity).
        let mut mono = [0.0f32; 64];
        let n = bpm_rx.pop_slice(&mut mono);
        assert!(
            n <= 16,
            "tee ring capacity 16 should cap reads at 16, got {n}"
        );
    }

    #[test]
    fn bpm_tee_render_is_alloc_free() {
        let (src, mut tx) = build(cfg_default(), 4096);
        let tee_ring = HeapRb::<f32>::new(4096);
        let (bpm_tx, _bpm_rx) = tee_ring.split();
        let mut src = src.with_bpm_tee(bpm_tx, 256);
        let _ = push_const(&mut tx, 256, 0.3, 0.4);

        let mut out = zeros(256);
        assert_no_alloc(|| {
            src.render_into(&mut out, 1.0, 2, 0);
        });
    }

    // -------- M9 peaks tap --------

    #[test]
    fn fresh_thru_source_has_no_peaks_tap() {
        let (src, _tx) = build(cfg_default(), 4096);
        assert!(!src.has_peaks_tap());
    }

    #[test]
    fn with_peaks_tap_attaches() {
        let (src, _tx) = build(cfg_default(), 4096);
        let tap_ring = HeapRb::<f32>::new(4096);
        let (peaks_tx, _peaks_rx) = tap_ring.split();
        let src = src.with_peaks_tap(peaks_tx);
        assert!(src.has_peaks_tap());
    }

    #[test]
    fn peaks_tap_receives_mono_downmix() {
        // L = 0.4, R = -0.2 → mono = 0.5 * (0.4 + (-0.2)) = 0.10
        let (src, mut tx) = build(cfg_default(), 4096);
        let tap_ring = HeapRb::<f32>::new(4096);
        let (peaks_tx, mut peaks_rx) = tap_ring.split();
        let mut src = src.with_peaks_tap(peaks_tx);

        let _ = push_const(&mut tx, 128, 0.4, -0.2);
        let mut out = zeros(128);
        src.render_into(&mut out, 1.0, 2, 0);

        let mut mono = [0.0f32; 128];
        let n = peaks_rx.pop_slice(&mut mono);
        assert_eq!(n, 128);
        for (i, &s) in mono.iter().enumerate() {
            assert!(
                (s - 0.10).abs() < 1e-6,
                "frame {i}: peaks_tap mono = {s}, expected 0.10"
            );
        }
    }

    #[test]
    fn peaks_tap_unaffected_by_gain() {
        // Mirrors bpm_tee_unaffected_by_gain: the peaks envelope
        // should reflect the raw input, not whatever the deck gain
        // is doing on the output.
        let (src, mut tx) = build(cfg_default(), 4096);
        let tap_ring = HeapRb::<f32>::new(4096);
        let (peaks_tx, mut peaks_rx) = tap_ring.split();
        let mut src = src.with_peaks_tap(peaks_tx);
        let _ = push_const(&mut tx, 64, 1.0, 1.0);

        let mut out = zeros(64);
        src.render_into(&mut out, 0.25, 2, 0);

        for i in 0..64 {
            assert!(
                (out[i * 2] - 0.25).abs() < 1e-6,
                "out L {} != 0.25",
                out[i * 2]
            );
        }

        let mut mono = [0.0f32; 64];
        let n = peaks_rx.pop_slice(&mut mono);
        assert_eq!(n, 64);
        for (i, &s) in mono.iter().enumerate() {
            assert!(
                (s - 1.0).abs() < 1e-6,
                "frame {i}: peaks_tap = {s}, expected 1.0 (pre-gain)"
            );
        }
    }

    #[test]
    fn peaks_tap_silently_drops_on_full_ring() {
        let (src, mut tx) = build(cfg_default(), 4096);
        let tap_ring = HeapRb::<f32>::new(16);
        let (peaks_tx, mut peaks_rx) = tap_ring.split();
        let mut src = src.with_peaks_tap(peaks_tx);
        let _ = push_const(&mut tx, 64, 0.5, 0.5);

        let mut out = zeros(64);
        src.render_into(&mut out, 1.0, 2, 0);

        for i in 0..64 {
            assert!((out[i * 2] - 0.5).abs() < 1e-6);
        }

        let mut mono = [0.0f32; 64];
        let n = peaks_rx.pop_slice(&mut mono);
        assert!(
            n <= 16,
            "tap ring capacity 16 should cap reads at 16, got {n}"
        );
    }

    #[test]
    fn peaks_tap_render_is_alloc_free() {
        let (src, mut tx) = build(cfg_default(), 4096);
        let tap_ring = HeapRb::<f32>::new(4096);
        let (peaks_tx, _peaks_rx) = tap_ring.split();
        let mut src = src.with_peaks_tap(peaks_tx);
        let _ = push_const(&mut tx, 256, 0.3, 0.4);

        let mut out = zeros(256);
        assert_no_alloc(|| {
            src.render_into(&mut out, 1.0, 2, 0);
        });
    }

    #[test]
    fn peaks_tap_underrun_pushes_zeros() {
        let (src, _tx) = build(cfg_default(), 4096);
        let tap_ring = HeapRb::<f32>::new(4096);
        let (peaks_tx, mut peaks_rx) = tap_ring.split();
        let mut src = src.with_peaks_tap(peaks_tx);

        let mut out = zeros(128);
        src.render_into(&mut out, 1.0, 2, 0);

        let mut mono = [0.0f32; 128];
        let n = peaks_rx.pop_slice(&mut mono);
        assert_eq!(n, 128, "peaks tap should push zero-fill on underrun");
        for &s in &mono {
            assert!(s.abs() < 1e-9);
        }
    }

    // -------- Both taps simultaneously --------

    #[test]
    fn bpm_and_peaks_tap_both_receive_same_mono_downmix() {
        // Attach both; both must see the same mono stream.
        let (src, mut tx) = build(cfg_default(), 4096);
        let bpm_ring = HeapRb::<f32>::new(4096);
        let (bpm_tx, mut bpm_rx) = bpm_ring.split();
        let peaks_ring = HeapRb::<f32>::new(4096);
        let (peaks_tx, mut peaks_rx) = peaks_ring.split();
        let mut src = src.with_bpm_tee(bpm_tx, 256).with_peaks_tap(peaks_tx);
        assert!(src.has_bpm_tee());
        assert!(src.has_peaks_tap());

        let _ = push_const(&mut tx, 64, 0.6, 0.2);

        let mut out = zeros(64);
        src.render_into(&mut out, 1.0, 2, 0);

        let mut bpm_mono = [0.0f32; 64];
        let mut peaks_mono = [0.0f32; 64];
        let nb = bpm_rx.pop_slice(&mut bpm_mono);
        let np = peaks_rx.pop_slice(&mut peaks_mono);
        assert_eq!(nb, 64);
        assert_eq!(np, 64);
        // Expected mono = 0.5 * (0.6 + 0.2) = 0.4
        for i in 0..64 {
            assert!(
                (bpm_mono[i] - 0.4).abs() < 1e-6 && (peaks_mono[i] - 0.4).abs() < 1e-6,
                "frame {i}: bpm = {}, peaks = {}, expected both 0.4",
                bpm_mono[i],
                peaks_mono[i]
            );
            assert!(
                (bpm_mono[i] - peaks_mono[i]).abs() < 1e-9,
                "frame {i}: bpm and peaks taps must see identical mono samples"
            );
        }
    }

    #[test]
    fn both_taps_render_is_alloc_free() {
        let (src, mut tx) = build(cfg_default(), 4096);
        let bpm_ring = HeapRb::<f32>::new(4096);
        let (bpm_tx, _bpm_rx) = bpm_ring.split();
        let peaks_ring = HeapRb::<f32>::new(4096);
        let (peaks_tx, _peaks_rx) = peaks_ring.split();
        let mut src = src.with_bpm_tee(bpm_tx, 256).with_peaks_tap(peaks_tx);
        let _ = push_const(&mut tx, 256, 0.3, 0.4);

        let mut out = zeros(256);
        assert_no_alloc(|| {
            src.render_into(&mut out, 1.0, 2, 0);
        });
    }

    #[test]
    fn bpm_tee_underrun_pushes_zeros() {
        // Empty input ring → render adds silence to output and
        // pushes zeros to the tee. The tee shouldn't drop entirely
        // (the analysis thread expects a continuous stream; zeros
        // are honestly silence and the M7.5 estimator handles them).
        let (src, _tx) = build(cfg_default(), 4096);
        let tee_ring = HeapRb::<f32>::new(4096);
        let (bpm_tx, mut bpm_rx) = tee_ring.split();
        let mut src = src.with_bpm_tee(bpm_tx, 256);

        let mut out = zeros(128);
        src.render_into(&mut out, 1.0, 2, 0);

        let mut mono = [0.0f32; 128];
        let n = bpm_rx.pop_slice(&mut mono);
        assert_eq!(n, 128, "tee should push zero-fill on underrun");
        for &s in &mono {
            assert!(s.abs() < 1e-9, "underrun frame {s} should be 0");
        }
    }
}
