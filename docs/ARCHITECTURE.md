# Dub — Architecture notes

> Companion to `docs/PRD.md`. The PRD describes *what* Dub does; this doc
> describes *how* it's structured.

## Overview

Dub is a Rust audio engine wrapped by a native macOS SwiftUI/AppKit shell.
The Rust core owns the audio thread end-to-end; Swift owns the UI thread
end-to-end. They communicate via lock-free state snapshots and SPSC ring
buffers, never callbacks across thread boundaries.

```
┌────────────────────────────────────────────────────────────────────┐
│                          macOS process                             │
│                                                                    │
│  ┌─────────────────┐         ┌──────────────────────────────────┐  │
│  │   SwiftUI/      │  UniFFI │           Rust core              │  │
│  │   AppKit shell  │◀───────▶│                                  │  │
│  │                 │  (lock- │  ┌────────────┐  ┌────────────┐  │  │
│  │  - Library UI   │   free  │  │  Engine    │  │ Library DB │  │  │
│  │  - Decks UI     │  msgs)  │  │  graph     │  │ (SQLite)   │  │  │
│  │  - Waveforms    │         │  │            │  └────────────┘  │  │
│  │    (Metal)      │         │  │  Decks     │  ┌────────────┐  │  │
│  │  - Preferences  │         │  │  FX        │  │ Track DBs  │  │  │
│  └─────────────────┘         │  │  Sampler   │  │ (in-RAM)   │  │  │
│                              │  └─────┬──────┘  └────────────┘  │  │
│                              │        │ render(rt, out)          │  │
│                              │        ▼                          │  │
│                              │  ┌─────────────────────────────┐  │  │
│                              │  │  CoreAudio AU IO proc       │  │  │
│                              │  │  (audio thread, RT)         │  │  │
│                              │  └─────────────────────────────┘  │  │
│                              └──────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────┘
```

## Crate dependency graph

```
              ┌────────────────┐         ┌────────────────┐
              │     dub-cli    │         │     dub-ffi    │  (UniFFI Swift bindings;
              │ (binary;       │         │  placeholder   │   wired up in M0.5)
              │  smoke /       │         │  in v1 — empty │
              │  play /        │         │  shell)        │
              │  timecode-deck/│         └───────┬────────┘
              │  thru / scope /│                 │
              │  capture /     │                 ▼
              │  levels /      │         ┌────────────────┐
              │  calibrate /   │         │   dub-engine   │ ─── ringbuf
              │  analyze /     │         │  (audio-thread │     coreaudio-rs (via dub-audio)
              │  decode-       │         │   owner; the   │     assert_no_alloc
              │  timecode)     │────────▶│   only RT-     │
              └───────┬────────┘         │   sensitive    │
                      │                  │   crate)       │
                      ▼                  └───────┬────────┘
              ┌────────────────┐                 │
              │   dub-audio    │◀────────────────┤   (engine consumes the
              │  CoreAudio HAL │                 │    ringbuf consumer from
              │  input + output│                 │    AudioInput, owns the
              └────────────────┘                 │    AudioOutput callback)
                                                 │
              ┌────────────────────┬─────────────┬─────────────┬──────────────┬────────────────┐
              ▼                    ▼             ▼             ▼              ▼                ▼
          dub-timecode         dub-dsp       dub-stretch   dub-io          dub-bpm          dub-peaks
          (Serato CV02 +      (rubato,      (Rubber Band  (symphonia       (M7.5+M8+M8.1 —  (M9 — off-RT
           Traktor MK1/MK2,    biquads,      FFI; GPLv3    decoders, in-    pure-Rust log-   decimator +
           clean-room)         FX placeholders) license)   RAM tracks)     band ODF +        PeakBuffer for
                                                                           windowed-energy   live waveform
                                                                           picker)           rendering)

          ┌─────────────────────────────────────────────────────────────────────┐
          │ Off-RT / placeholder for v1:                                        │
          │   dub-thru        (source-detection classifier, §5.1.1 — empty)     │
          │   dub-fingerprint (Chromaprint FFI for v1.1)                        │
          │   dub-library     (SQLite + import adapters for M11/M12)            │
          │   dub-controller  (HID/MIDI abstractions for v1.x+)                 │
          └─────────────────────────────────────────────────────────────────────┘
```

**Two things to note that aren't obvious from the diagram:**

1. **Only `dub-engine` runs on the audio thread.** Everything else is either preparatory work (decoders, format parsers, calibration), off-RT workers (M5.4.5 calibrators, M8 per-Thru-deck BPM analysis threads via `dub_bpm::BpmStream`, M9 per-Thru-deck peak decimators via `dub_peaks::PeakStream`, M5.1.1 source-detection classifier), or non-RT services (library DB, UI bindings). Crates with FFI dependencies — `dub-stretch` (GPLv3) and `dub-fingerprint` (LGPL planned) — are deliberately leaf crates so license boundaries don't leak upstream. `dub-bpm` shipped M7.5+M8 as pure-Rust so it has no FFI obligation today; `dub-peaks` is pure-Rust too. If an aubio backend ever lands behind a feature flag it would stay confined to that same leaf crate.

2. **`dub-cli` depends directly on `dub-engine` + `dub-audio`, not through `dub-ffi`.** The CLI is the headless test harness for the engine — it lives in Rust-land and never crosses the FFI boundary. `dub-ffi` is for the Swift app only. As of M10-A it ships a UniFFI 0.28 surface (proc-macros, no UDL) exposing `greeting()` + `engine_version()` (the M0.5 smoke functions) **plus** a full `DubEngine` interface for the M10-B waveform UI: `list_input_devices`, `start_thru(device, channels)` (throws `EngineError`), `stop_thru`, `peaks_extend(deck, start_idx) -> Vec<u8>` plus its `len` / `chunk_duration_secs` siblings, and the matching `band_peaks_*` trio for M10.1's multi-colour shader. All bytes are `#[repr(C)]` little-endian (`PeakChunk` = 12 B, `BandPeakChunk` = 32 B) so Swift can reinterpret-cast `Data` straight into an `MTLBuffer` upload.

## RT-safety enforcement

Three layers, in order of strength:

1. **Type system (compile-time):** `RealtimeContext<'_>` is the gating token.
   Any function reachable from `Engine::render` takes `&mut RealtimeContext<'_>`.
   The token is `!Send`, `!Sync`, and lifetime-bounded so it cannot leak.
2. **`assert_no_alloc` (runtime, dev/test):** the global allocator wraps an
   `AllocDisabler`. Tests that exercise the render path run inside
   `assert_no_alloc::assert_no_alloc(|| { ... })`; any allocation aborts.
3. **`assert_no_alloc` (runtime, release):** same allocator, configured to
   set a flag and emit a one-shot log entry rather than abort. Protects
   production users while making dev-time violations loud.

See PRD §2.2.3 and `crates/dub-engine/src/realtime.rs`.

## Audio I/O

- macOS only in v1.
- CoreAudio HAL via `coreaudio-rs`. Direct device-property listeners; opt-in
  hog mode for the lowest-latency path.
- `AVAudioEngine` is **not** used (too high-level, hides the IO proc).
- Per-deck input + output assignment in External Mixer mode (PRD §5.3).

### HAL input invariant — sample-rate match (M5.2)

CoreAudio HAL has a load-bearing footgun: if the AudioUnit's stream
format SR does **not** equal the device's hardware nominal SR, the IO
proc silently delivers zero callbacks. `AudioUnitStart` returns OK,
`coreaudiod` logs nothing, the green mic indicator never lights up.
You will think it's a TCC permission issue. It isn't.

`AudioInput::start_with_options` enforces the invariant by:

1. Reading `kAudioDevicePropertyNominalSampleRate` directly off the
   device (not via `AudioUnit::sample_rate()` — a fresh HALOutput AU
   reports its own internal default, 44.1 kHz, regardless of hardware).
2. If the caller asked for a different SR, calling
   `set_device_sample_rate` on the device first (synchronous; blocks
   until the HAL rate listener confirms).
3. Building the AudioUnit *uninitialized*, setting the stream format
   on `(Scope::Output, Element::Input)` to match the now-actual device
   SR, then calling `AudioUnitInitialize`.

Reverse order — initialize, then set format — appears to succeed but
sometimes leaves the IO proc unarmed. Set-then-init is the only
robust sequence.

`list_input_devices` and `query_default_input` likewise report the
device's hardware nominal SR, so the user-visible rate matches the
rate at which input will actually fire.

#### Cold-start capture overshoot — known issue, deferred

Empirically, the *first* `dub capture` against a freshly-opened SL3
records ~1–3 s more audio than wall time accounts for; subsequent
captures within the same process are exact (15.003 s wall ⇒ 14.997 s
audio observed). The decoder still locks at confidence 1.000 across
the entire capture and rate is correct, so the file is real audio, not
duplicated samples — the IO proc simply runs ahead of nominal for the
first second after `AudioUnitStart` on this driver. Levels mode never
sees this because it doesn't write a WAV (the file create was the
suspected trigger; the actual mechanism is undiagnosed). For M5.3 the
deck consumes samples directly off the input ringbuf and never
correlates input-sample-count with wall time, so the issue is invisible
to the live integration. Re-investigate when we add input-clock-vs-
output-clock drift compensation in M5.4+.

## Audio buffers

Per PRD §4.4:

- Tracks are decoded fully into RAM on load. No per-block disk streaming.
- Audio is `Arc<[f32]>`, planar stereo, 32-bit float.
- A 6-minute FLAC ≈ 140 MB at f32; two loaded decks = ~280 MB.
- Forward and backward playback are byte-for-byte symmetric.

## UI ↔ Engine messaging

Bidirectional, lock-free.

### UI → Engine (commands) — implemented in M2

`ringbuf::HeapRb<Command>` (SPSC, capacity 256). UI pushes, audio thread
pops at the start of each render block. Producer side lives in
`dub_engine::EngineHandle`; consumer side is owned by `Engine`.

- `Command` is a small enum, ≤ 64 bytes, no `Box`, no `dyn Trait`. Most
  variants are `Copy`-equivalent; `DeckLoad` carries an `Arc<Track>`
  by value. Variants today: `DeckPlay`, `DeckPause`, `DeckSeek`,
  `DeckSetRate`, `DeckSetGain`, `DeckLoad`. Adding a command is one
  variant + one match arm in `Engine::apply_command`.
- The drain is RT-safe: `try_pop` is a load + index, and every variant
  applies in-place to the deck array. Verified by `rt-audit` with 100k
  blocks, 10k pre-staged transport commands, and 20 hot-loads, all
  under `assert_no_alloc`.

### Trash channels (audio → UI for heap-bearing disposal) — M3 + M5.4.5 + M7

The audio thread NEVER drops anything that owns a heap allocation.
`Arc::drop` decrements the strong count and calls `dealloc()` on
zero; `Box::drop` runs the destructor of the inner type, which for
the engine's heap-bearing payloads (`TimecodeInput`'s `Vec<f32>` +
`Decoder` + `HeapCons<f32>`, `ThruSource`'s `Vec<f32>` scratch +
`HeapCons<f32>`) calls `dealloc` on each. `dealloc` is a syscall,
forbidden on the RT thread.

Three independent SPSC trash channels carry displaced payloads back
to the main thread for disposal:

| Channel | Capacity | What flows through it | When |
|---|---|---|---|
| `HeapRb<Arc<Track>>` | 32 | Old `Arc<Track>` from `Command::DeckLoad` | M3 — track-load on a deck |
| `HeapRb<Box<TimecodeInput>>` | 8 | Displaced `Box<TimecodeInput>` from `Command::AttachTimecodeInput` | M5.4.5 — re-attach on a deck whose slot was already filled |
| `HeapRb<Box<ThruSource>>` | 8 | Displaced `Box<ThruSource>` from `Command::AttachThruSource` | M7 — re-attach on a Thru deck whose slot was already filled |

When the engine applies `DeckLoad`, it `swap_source`s the new Arc
onto the deck and pushes the old Arc into the track trash channel.
When the engine applies `AttachTimecodeInput` or
`AttachThruSource`, it `slot.replace(*payload)` and pushes any
displaced predecessor into the corresponding trash channel. The
main thread drains all three channels via a single
`EngineHandle::reclaim()` (called automatically inside
`DeckCommand::load`, `EngineHandle::attach_timecode_input`,
`EngineHandle::attach_thru_source`, and on `EngineHandle::drop`).

If any channel ever overflows (UI not draining + storm of
operations), the audio thread `mem::forget`s the rejected payload
(leaking it) and increments the corresponding atomic overflow
counter (`trash_overflow_count` for tracks,
`timecode_trash_overflow_count` for timecode inputs,
`thru_trash_overflow_count` for Thru sources). Leaking is the
lesser evil versus a forbidden `dealloc` on the RT thread, and the
counter surfaces the contract violation to the UI for logging.

The timecode-input and thru-source channels are sized smaller (8
vs. 32) because re-attach is at most "one cartridge or input swap
per deck per song" during a tight set — well below half this —
whereas track-load can burst more readily during quick-cue UI
flows.

### De-click envelope on transport changes — M3.5

Any instantaneous transport mutation (track load, seek, play/pause)
would change the value the deck reads from one sample to the next.
A jump function in the time domain is, in the frequency domain, a
brief impulse with infinite-frequency content — the ear hears that
as a click.

`crates/dub-engine/src/declick.rs` precomputes a 2 ms equal-power
crossfade table at engine construction (one per engine, shared as
`Arc<DeclickEnvelope>` across decks). At 48 kHz that's 96 samples ×
4 bytes = 384 bytes — sits in L1 cache.

Each `Deck` carries:

- `declick_envelope: Arc<DeclickEnvelope>` (read-only),
- `declick: DeclickState` (`Idle` or `Active{ prev_source, prev_position,
  prev_rate, prev_playing, samples_remaining }`),
- `pending_disposal: Option<Arc<Track>>` for back-to-back swaps.

Mutators that change what the deck reads (`set_source`, `swap_source`,
`set_position_frames`, `set_playing` on transition, `clear_source`)
all call `start_declick`, which snapshots the *current* state into
`Active{prev_*}` before the caller mutates `self`. The render loop
then runs two phases per block:

1. **Fade phase** (while `samples_remaining > 0`): per sample, read
   `(old_l, old_r)` from `prev_source` at `prev_position` and
   `(new_l, new_r)` from `self.source` at `self.position`. Mix
   `out = old · (1 − fade_in[i]) + new · fade_in[i]` where
   `fade_in[i] = sin²(i · π/(2N))` is read from the envelope table.
2. **Steady phase**: normal additive interpolation, identical to the
   M2 render path.

The audio thread never drops `Arc<Track>`. After every render block
the engine sweeps each deck for finished ramps and `pending_disposal`
slots and ferries any orphaned `Arc<Track>` through the trash channel
(§Trash channel above). Back-to-back transport changes within a single
2 ms window stash one displaced Arc in `pending_disposal`; in the
≥4-deep edge case (physically impossible from human input) we
`mem::forget` and increment the same overflow counter the trash
channel uses.

**Tail-fade**: complementary primitive sharing the same envelope. The
transport declick fires on user-initiated state changes; it does not
fire when the playhead simply walks past the last sample of a track
(that's the data running out, not a transport mutation). Without a
tail-fade, the deck reads "last in-range value, then zero" in one
sample — a step function the ear hears as a click. The `track_tail_fade_scale`
helper applies `cos²` over the last `N` frames of every track read,
on both the steady-state path and inside the M3.5 crossfade's old/new
sides. Gated by a `track_len ≥ 2 × envelope_length` threshold so
sub-millisecond test tracks aren't obliterated.

Verification: 7 declick + tail-fade unit tests cover fade-in monotonicity,
fade-out to silence on pause, A→B crossfade smoothness, no-jump bound
on per-sample deltas, back-to-back-swap Arc accounting, end-of-track
smoothness, and the short-track skip threshold. `rt-audit` exercises
100k blocks with 20 hot-loads each producing a 2 ms fade, all under
`assert_no_alloc`, with zero overflows.

**End-to-end audit**: subjective listening is a poor debug loop for
clicks, so M3.5 also ships a `dub analyze <wav>` subcommand that
reads any 32-bit-float (or 16-bit PCM) WAV and reports peak, RMS,
DC offset, clipping count, and the maximum per-sample first-difference
per channel, flagging samples where `|s[i] − s[i-1]|` exceeds a
configurable threshold (default 0.05). The offline `dub play -o`
path supports the same scheduled transport events as realtime, so a
hot-swap scenario can be rendered deterministically and audited
mathematically — current measured worst-case delta on the M3.5 demo
suite is 0.0187, against a click step of order 0.5+.

### Timecode decoder, relative-mode-only — M5.1 / M6

Lives in `dub-timecode`. Pure DSP, no I/O, no allocations on the hot
path — designed to drop straight onto the audio thread when M5.3 wires
it up to live audio input.

**Signal model.** Both stereo channels carry the same nominal sinusoid
at the format's carrier — **1 kHz** for Serato CV02, **2 kHz** for
Traktor MK1, **2.5 kHz** for Traktor MK2 (since M6) — offset by 90°
between ch0 and ch1. The convention — verified empirically against a real
Serato Control CV02 cartridge through an SL3 — is `ch0 ≈ A·sin(φ)`,
`ch1 ≈ A·cos(φ)`, with ch0 *leading* ch1 by 90° at forward play.
Treating each frame as a complex sample `s = ch1 + j·ch0` makes the
input a single complex exponential `s(t) = A · exp(j·2π·f·t)` whose
frequency is positive when the record turns forward and negative when
reversed. Magnitude `|s|² = ch0² + ch1²` is constant across rotation,
which is what makes amplitude AGC unnecessary for the *phase* tracking
(it'll matter later for AM-bitstream decoding in M6).

The synthetic generator in `dub-timecode::signal` emits the same
quadrature convention so round-trip tests, the `dub decode-timecode`
`--synthetic` mode, and live SL3 captures all share one sign convention:
**forward stylus motion ⇒ +rate, reverse ⇒ −rate**. Getting this wrong
in M5.1 would have looked perfectly reasonable on synthetic data
(generator and decoder would have been internally consistent); only the
first capture from real hardware exposed the channel ordering, which is
why we delayed picking the convention until empirical data was in.

**Per-block algorithm.**

```text
  for each stereo frame n:
    s_n = ch1_n + j·ch0_n                            # Serato CV02 quadrature
    accum  += s_n * conj(s_{n-1})
    amp_acc += |s_n|²
  Δφ_block = arg(accum)                              # coherent phase diff
  f_inst   = Δφ_block / (2π · Δt_per_sample)         # signed Hz
  rate     = f_inst / carrier_hz                      # ±1.0 = ±unity
  position += rate * block_seconds                   # seconds at unity
  confidence = |accum| / amp_acc                      # 1.0 = pure carrier
```

The coherent sum is the key to robustness: noise (uncorrelated across
samples) suppresses by `√N`, signal adds linearly. With a 64-sample
block at 48 kHz that's a ~9 dB noise gain — easily good enough to lock
onto a real cartridge, and orders of magnitude better than per-sample
phase tracking (which is what naive PLLs do).

Direction falls out for free: forward rotation → `f_inst > 0`, reverse
→ `f_inst < 0`. No state machine, no quadrature flag, no zero-crossing
parity tracking. The L/R quadrature relationship of the printed signal
is the only direction encoding we need.

**Limits.** Per-sample phase advance saturates at ±π, which puts a
`Nyquist / carrier` ceiling on trackable rates: 24× at 48 kHz / 1 kHz
(Serato), 12× at 48 kHz / 2 kHz (MK1), 9.6× at 48 kHz / 2.5 kHz (MK2).
Real DJ scratching tops out at ~8×, well clear of all three ceilings —
MK2 is the tightest but still has 20% headroom. Below the limit the
estimator is bias-free and limited only by sample-rate quantization
(~50 µs at 48 kHz, equivalent to ~0.005 of unity rate).

**Multi-format (M6).** All three relative-mode formats — Serato CV02
(1 kHz), **Traktor MK1** (2 kHz, AM modulation), and **Traktor MK2**
(2.5 kHz, offset modulation where the modulation rides as a vertical
DC shift instead of as amplitude changes) — decode through the same
code path. The only per-format parameter the algorithm uses today
is `Format::carrier_hz()`; position-bit count and side-A length are
exposed for future absolute-mode work but not consumed yet. MK2's
offset modulation is AC-coupled out by the cartridge/preamp before
it reaches us, so the relative-mode math sees a clean 2.5 kHz
carrier and works without per-format branches. The L/R quadrature
convention (`ch0 = sin(φ)`, `ch1 = cos(φ)`, ch0 leads ch1 by 90° at
forward play) is empirically the same for all three vendors — all
three copied from the xwax-documented spec — so a single decoder
handles any record without per-format branches. If a future format
needs a different convention, `Format` is the right place to add a
`ch0_is_sin: bool` (or similar) and the decoder gets a one-line
conditional swap of `re`/`im` mapping.

**Why MK1 and MK2 are separate variants** even though only `carrier_hz`
differs: getting the carrier wrong is silent — playback at the wrong
speed, no error, no warning, no log. M6 was first drafted with MK2 at
2 kHz (matching MK1) which would have played MK2 vinyl back at 80%
speed; the bug was caught because the user asked the right question
("can we support both old and new?") before live-testing. The fix:
distinct enum variants, distinct `carrier_hz` (2000 vs 2500), and a
deliberate cross-format regression test (`mk2_vinyl_decoded_as_mk1_
plays_back_too_fast_by_25_percent`) that fails the moment a refactor
collapses the carriers. The CLI also rejects the bare alias `traktor`
for the same reason — forcing the user to know which generation they
own beats silent mis-routing.

**CLI vocabulary.** `Format::from_cli_arg` accepts:

- `serato-cv02` / `serato` / `cv02` → `SeratoCv02`
- `traktor-mk1` / `mk1` → `TraktorMk1`
- `traktor-mk2` / `mk2` → `TraktorMk2`
- bare `traktor` → **rejected** (ambiguous)

Every CLI subcommand (`scope`, `calibrate`, `timecode-deck`,
`decode-timecode`) uses them so the vocabulary is consistent.
`Format::cli_name()` is the inverse — the on-disk format key in
the calibration JSON. Round-tripped by unit test so renaming an
alias can't desync the JSON. Calibration is per-format keyed
already (`device_key("SL 3", Format::TraktorMk1)` is a different
file than `Format::TraktorMk2`), so a user with both records on
the same SL3 keeps two independent calibrations.

**What's *not* here yet.** Absolute position (needs bitstream
demod and the format's 20- or 23-bit code table), stickiness
policy (M5.4 — "confidence dropped below threshold for N ms →
freeze deck" lives in the integration layer, not in the DSP),
and AGC + cartridge calibration (real-world amplitude variation
is handled by M5.4.2's per-rig threshold derivation rather than
by the DSP). The decoder exposes `confidence` and `amplitude` so
the integration layer can implement those policies without
modifying the DSP.

**License + provenance.** Clean-room implementation from the
xwax/Mixxx algorithm description; no xwax code copied (xwax is BSD;
dub is GPL-3.0 — the *direction* of compatibility allows BSD → GPL,
but we want attribution to remain unambiguous, hence the rewrite from
spec).

### Live timecode → deck — M5.3

This is where the offline decoder (M5.1) and the input plumbing
(M5.2) meet the engine. The integration is intentionally narrow:
one new module (`dub_engine::timecode`), one new method
(`Engine::attach_timecode_input`), one new render-loop step
(`Engine::drive_timecode_inputs`). No new threads, no new channels,
no extra IPC.

**Wiring.**

```text
  CoreAudio input IOProc                       AudioOutput callback
  (e.g. SL3 ch3+4, 48 kHz)                     (default device, 48 kHz)
           │                                            │
           ▼                                            ▼
  HeapRb<f32> (1 s capacity)                    Engine::render
           │  (consumer moved into engine               │
           │   via AudioInput::take_consumer)           │
           ▼                                            │
  TimecodeInput { rx, decoder, scratch }                │
           │                                            │
           └────────► drive_timecode_inputs ────────────┤
                          │ (per render block)          │
                          │ pop_slice → Decoder::process│
                          │ → DecodeOutput              │
                          │                             │
                          ▼                             │
                  Intent::Locked { rate }   ──┐         │
                  Intent::DropoutHoldRate    ──┤        │
                                               ▼        │
                                Deck.set_rate / set_playing
                                               │        │
                                               └───►Deck.render
```

The `AudioInput` keeps the AudioUnit alive on the main thread (drop
= stop input). The consumer end of its IOProc → consumer ringbuf
moves into the engine via `AudioInput::take_consumer`, after which
`AudioInput::read_into` returns 0 forever (only one reader on an
SPSC ring).

**Lift policy: amplitude gate + two-edge confidence hysteresis +
sticky window.**

Three iterations on real SL3 hardware drove the design here, each
exposing a class of bug the previous policy missed:

1. *Single-threshold gate.* Confidence wobbles around 0.8 as the
   carrier dies on lift → rapid play/pause toggles → audible
   chatter from repeated 2 ms declick fades.
2. *Two-edge confidence hysteresis (no amplitude gate).* The
   lukewarm `[0.5, 0.8)` band is correct for *scratch* transients
   (cartridge firmly on groove, brief direction reversals) but
   *wrong* for lift: the cartridge picks up handling/rumble noise
   that the decoder finds *some* coherent rotation in (moderate
   confidence) while the RMS is near-zero. The deck stayed
   engaged at `last_locked_rate`, burst-playing track audio for
   as long as the needle was held aloft.
3. *Amplitude gate over confidence hysteresis (current).*
   Amplitude is the truthful "is the cartridge on the groove?"
   signal; confidence alone is not. The gate overrides the
   confidence bands.

```text
  amplitude < amplitude_threshold ─────────── "carrier dead"
      Treated as below-floor regardless of confidence.
      Engaged: counts toward sticky disengage.
      Disengaged: stays disengaged.

  amplitude ≥ amplitude_threshold AND ...
    ┌── conf ≥ engage_threshold ───────────── "fully locked"
    │       set rate = decoded_rate; engaged = true; reset countdown.
    │
    │── disengage_threshold ≤ conf < engage_threshold ── "lukewarm"
    │       if engaged: hold last_locked_rate, stay engaged, reset
    │                    countdown (mid-scratch transients).
    │       if disengaged: stay disengaged (noise floor).
    │
    └── conf < disengage_threshold ─────────── "below floor"
            engaged: increment countdown; disengage when it hits
                     sticky_blocks_to_disengage (deck mutes via
                     M3.5 declick).
            disengaged: stays disengaged.
```

Defaults: engage 0.8, disengage 0.5, sticky 4 blocks (~21 ms @
256-frame / 48 kHz), amplitude 0.01 RMS. CV02 carriers through SL3
sit at 0.1–0.5 RMS; lifted needles drop to <0.005, so the gate has
a wide margin. All four are tunable per attach via
`TimecodeInputConfig`; the CLI exposes `--confidence` (engage),
`--disengage-threshold`, `--sticky-blocks`, and
`--amplitude-threshold`. Setting `amplitude_threshold = 0.0`
disables the gate (confidence-only fallback) — diagnostic only,
pinned by a regression test so we can't lose it.

The factoring deliberately separates the lift policy from data
sourcing. The state machine lives in a public `LiftPolicy` struct
(`dub_engine::LiftPolicy`) with a single `step(DecodeOutput) ->
LiftIntent` method; `TimecodeInput` *embeds* a `LiftPolicy` and
delegates to it from inside `drive(...)`. This lets three callers
share *exactly* the same lift behavior:

1. The audio-thread render path (`Engine::drive_timecode_inputs` →
   `TimecodeInput::drive` → `LiftPolicy::step`) — production
   playback.
2. `dub scope` (M5.4.1) — owns its own `LiftPolicy` + `Decoder` +
   input buffer on the main thread; renders the policy's live
   state in a TUI without touching the audio thread or the engine.
3. `dub calibrate` (M5.4.2) — replays recorded carrier samples
   through a `LiftPolicy` to evaluate candidate thresholds against
   historical data before persisting them.

Single source of truth: if the policy changes, every diagnostic
follows. If `dub scope` says "this would lock", `dub timecode-deck`
will lock at the same thresholds, period. The unit-test suite
covers each pathology the policy was tightened to fix — including
the lukewarm-but-quiet lift bug from the second SL3 validation —
plus the public accessors (`is_engaged`, `consecutive_below`,
`last_locked_rate`) that diagnostic UIs read each frame.

**RT-safety.** `drive_timecode_inputs` is allocation-free and
finite-time:

- `pop_slice` on the SPSC consumer is a memcpy.
- `Decoder::process` is `assert_no_alloc`-clean (M5.1 verified).
- The scratch buffer is pre-allocated at attach time
  (`max_block_frames × 2` interleaved samples) and never resized.
- `Deck::set_rate` / `set_playing` are field writes plus relaxed
  atomic stores; the M3.5 declick start is alloc-free (verified in
  M3.5).

`rt-audit` carries a 10k-block timecode-driven render path under
`assert_no_alloc` so any future regression on this hot path fails
CI rather than reaching audio threads in the wild.

**SR alignment.** v1 requires `input_sample_rate == engine_sample_rate`
to within 0.5 Hz; mismatch is rejected at attach time
(`AttachError::SampleRateMismatch`). Sample-rate conversion between
input and engine isn't in scope. The output device is *also* aligned
to engine SR — `AudioOutput::start_with_buffer_size` queries the
device's nominal rate and forces it via
`kAudioDevicePropertyNominalSampleRate` if it differs (same gauntlet
as `AudioInput`). The first SL3 run shipped with output at 44.1 kHz
and engine at 48 kHz, which the CoreAudio HAL DefaultOutput unit
sometimes resamples and sometimes plays literally at the device
clock — driver-dependent and silent either way. Forcing alignment
removes the resampler from the path; if the device can't honor the
engine SR, output start-up fails with a clear error rather than
shipping audible 8% pitch drift. `dub play --realtime` already
built the engine at the device's reported SR so it sees a no-op
here; only the timecode-deck case (which pins engine to *input* SR)
exercises the new alignment.

**What this is *not*.**

- Position drift correction. Relative-mode in v1 lets deck position
  evolve via integration of rate, which is what the platter
  encodes. M5.4+ may add explicit re-sync if accumulated drift
  becomes audible over long sessions.
- External-mixer multi-channel output routing (M5.5). Output today
  is a single summed stereo bus; per-deck routing waits until
  hardware actually demands it.
- Multi-deck timecode. Engine has slots for `[Option<TimecodeInput>;
  DECK_COUNT]` so M5.5 just attaches a second one — but until then
  CLI's `dub timecode-deck` wires only deck 0.

### Live timecode scope — M5.4.1

`dub scope` is a standalone diagnostic TUI that opens the input
device, runs the same `LiftPolicy` as `dub timecode-deck`, and
renders what the decoder + policy see in real time. It exists
because lift-policy debugging by ear (the M5.3 "ghost noise"
session) was much harder than it needed to be: every iteration
required a full `dub timecode-deck` run with track audio mixed
into the diagnostic. The scope decouples diagnosis from
playback — same code path, no audible side-effects.

```text
  CoreAudio input IOProc                  ratatui frame (30 fps)
  (e.g. SL3 ch3+4)                              ▲
           │                                    │
           ▼                                    │
  HeapRb<f32> (1 s capacity)                    │
           │                                    │
           ▼                                    │
  AudioInput::read_into ──► acc[block_samples]  │
                                  │             │
                  ┌───────────────┴───────────┐ │
                  ▼                           ▼ │
            LissajousTrail            Decoder::process
            (1024-frame ring          → DecodeOutput
             of (L,R) pairs)                 │
                  │                          ▼
                  │                    LiftPolicy::step
                  │                          │
                  │                          ▼
                  │                    LiftIntent::{Locked,
                  │                                 DropoutHoldRate}
                  │                          │
                  └────────────► UiState ◄───┘
                                    │
                                    ▼
                              draw_ui (Canvas + Gauges)
```

**Architecture.** Single-threaded — the main thread drains the
input ringbuf, runs the decoder + policy, and renders the TUI.
No RT constraints apply: the IOProc is RT-safe (M5.2 invariant),
unaffected by what the main thread does with the consumer end.
ratatui + crossterm are pulled in only by the CLI; they never
touch the engine or audio crates.

**Block size.** Fixed at 256 frames (matching `timecode-deck`
default). The lift policy's sticky window is measured in
*blocks*, not seconds — if the scope ran the policy at a
different cadence than playback, the user couldn't trust scope
thresholds to transfer to `dub timecode-deck`. The contract is:
*same block size → same policy behavior*. Both tools call
`policy.step(...)` exactly once per 256-frame block.

**Live thresholds.** Arrow keys mutate the policy thresholds in
place (`↑/↓ engage`, `PgUp/PgDn disengage`, `←/→ amplitude`).
On every change the policy is rebuilt via `LiftPolicy::new` —
which resets `engaged` to `false` so the user sees what would
happen *with these new thresholds, starting from a cold lock*
rather than carrying engagement over from the old thresholds.
This is the calibration sandbox; M5.4.2 (`dub calibrate`)
persists the resulting values per-rig.

**Why not a Lissajous on the audio thread?** The trail buffer
(1024 frame pairs) is tiny enough that pulling it from the
ringbuf consumer on the main thread costs ~µs per frame. Pushing
display data through the audio thread would have meant a second
ringbuf solely for visualization; not worth the surface area for
a diagnostic.

### Calibration + rig fingerprinting — M5.4.2 → M5.4.6

`dub calibrate` and `dub timecode-deck`'s startup calibration
share one code path (`calibrate::measure_inline`), so a JSON file
written by either is indistinguishable. The data model + math live
in `crates/dub-cli/src/calibration.rs` (pure, fully unit-tested);
the interactive driver lives in `calibrate.rs`.

**M5.4.5 worker-thread split.** `measure_inline`,
`wait_for_stable_carrier`, `wait_for_lift`, and `capture_phase`
were refactored from `(&mut AudioInput, pair_idx, …)` to
`(&mut HeapCons<f32>, &MeasurementInputs, …)` so each calibrator
can run on its own thread (owning its consumer ring) without
holding an exclusive borrow on the AudioInput. `MeasurementInputs`
bundles the device name + sample rate + deck index + format that
the old signature pulled off `AudioInput`; the caller fills it
once on the main thread and hands it to the worker by value.

**Status as of M5.4.6:** the JSON file is a *diagnostic artifact*
only. `dub timecode-deck` always runs a fresh calibration on
startup and writes a new file (overwriting any previous one); the
runtime *never* loads it back at startup. The pre-M5.4.6 design
loaded the JSON and probed the carrier briefly to validate a
"rig fingerprint" before deciding whether the saved thresholds
were still valid; that machinery is gone. See the M5.4.6 entry
in `docs/PRD.md` and the M5.4.6 paragraphs further down for the
rationale.

**Why per-rig, not per-soundcard.** The user's literal request:
"the user can always play on a new cartridge — we cannot assume
that because the SL3 is connected, the cartridge and turntable are
the same." Cartridges differ by output level (Concorde Pro ≈ 250 mV
nominal vs. Nightclub MK2 ≈ 500 mV), preamps differ by gain
structure, and turntable cabling has its own loss profile. All
three together set the carrier amplitude that reaches Dub's input.
A soundcard-only calibration would silently misfire on cartridge
swap.

**Single-phase measurement, zero prompts (M5.4.3 default).**

```text
  AudioInput  ──► wait_for_stable_carrier
                   (2 consecutive blocks: conf ≥ 0.90, |rate-1|<0.10)
                              │
                              ▼
              capture_phase("carrier", 3 s) ──► amps[], confs[]
                              │
                              ▼
              measurement_stats_from_samples → P5/P50/P95
                              │
                ┌─────────────┴──────────┐
                ▼                        ▼
   derive_thresholds ───►        RigFingerprint
   (engage = c.conf_p5 - 0.03,   (carrier_amp_p50,
    amplitude = c.amp_p5 / 2,     carrier_amp_p95,
    disengage = 0.50,             carrier_conf_p50)
    sticky = 4)                          │
        │                                │
        └──────────────┬─────────────────┘
                       ▼
                  Calibration { ..,
                                measurements: { carrier, lift: zero() },
                                .. }
                       │
                       ▼
            ~/.dub/calibration/<device_key>.json
```

The single-phase mode is the M5.4.3 default. The lift slot in
`CalibrationMeasurements` is filled with `MeasurementStats::zero()`
(`n_blocks == 0`) for schema compatibility — the loader recognizes
this sentinel and skips the SNR safety check (the lift's only role).
Total wall time on a known-good rig: ≈ 3.5 s (was ~25 s pre-M5.4.3).

**Two-phase measurement, opt-in via `--two-phase` (legacy / diagnostic).**

```text
  AudioInput  ──┬──► wait_for_stable_carrier (2 blocks)
                ├──► capture_phase("carrier", 3 s) ──► amps[], confs[]
                ├──► wait_for_lift (10 blocks below amp 0.005)
                └──► capture_phase("lift", 5 s) ──► amps[], confs[]
                                       │
                                       ▼
                  measurement_stats_from_samples → P5/P50/P95 each
                                       │
                          derive_thresholds (with SNR check)
```

Total wall time: ≈ 25 s. The SNR safety net rejects rigs below
SNR 10× (almost always a stylus / preamp / cabling problem). Use
when single-phase silently shipped thresholds that don't engage
correctly at runtime, or when troubleshooting a misbehaving rig.

The detector uses fixed sensible defaults (carrier ≥ 0.90 conf,
|rate-1| < 0.10) — these are reliable across any rig that passes
the SNR sanity check (in two-phase mode), and tightening to 0.90
makes the M5.4.3 `STABLE_BLOCKS = 2` detection unambiguous.

**Threshold derivation (M5.4.3 update).** Pure functions over the
percentiles — `derive_thresholds(carrier, lift) ->
Option<CalibrationThresholds>`. Returns `None` only when
*both* (a) lift was measured (`n_blocks > 0`) and (b)
carrier-to-lift SNR is below 10× — refusing to ship thresholds
for a rig with a stylus / preamp / cabling problem. Single-phase
mode (`lift.n_blocks == 0`) skips the SNR check by design;
runtime ghost-noise warnings (M5.4.5+M10) take over the safety
role from there. Pinned by tests against the user's actual
hand-found M5.4.1 SL3 values (engage 0.95, amp 0.12) — the
formula reproduces those within 1 % from the same input
percentiles in either mode.

**Fingerprint.** Three carrier-amplitude / confidence percentiles.
Lift noise is *deliberately excluded*: lift noise rises by 10–100×
in clubs vs. lab, which would false-flag every venue change as
"rig changed". Carrier amplitude is the cartridge's signal level —
dominant over ambient noise on the wire — so it tracks the rig
identity, not the room.

**Startup flow on `dub timecode-deck` (parallel calibration + always-fresh, M5.4.5 + M5.4.6).**

```text
main thread:
  1. Open input device, load tracks.
  2. take_consumer_pair(0) → consumer_a; take_consumer_pair(1) → consumer_b (if two-deck).
  3. Engine::new_with_handle → (engine, handle).
     decks default to paused, no TimecodeInput attached on either.
  4. AudioOutput::start_with_options(engine, …)  ← engine moves to audio thread.
     output is producing audio NOW — silence on both decks until they're attached.
  5. spawn worker_a:                  spawn worker_b (if two-deck):
       run_full_calibration              run_full_calibration
       (reads consumer_a)                (reads consumer_b)
       send (0, Ok(consumer_a, cal))     send (1, Ok(consumer_b, cal))
       via mpsc                          via mpsc
  6. main loop (50 ms tick):
       rx.try_recv → for each completed worker:
         apply CLI overrides on top of cal.thresholds
         handle.attach_timecode_input(idx, consumer, cfg)
           → audio for that deck goes live mid-stream
       handle.reclaim()  ← drain trash from any displaced TimecodeInputs
       print stats every 500 ms
       sleep 50 ms
       …until duration_secs elapses or Ctrl-C.
```

Two key shifts vs. pre-M5.4.5:

- **Audio output starts before calibration.** The pre-M5.4.5 flow
  ran calibration first then started the output device with both
  decks pre-attached; the audio output appearing "alive" was the
  user's signal that everything was up. Now the output appears
  alive *immediately* (silence on both decks) and decks attach
  one at a time as they finish calibrating.
- **Calibrators are parallel worker threads owning their own
  ringbuffer consumers.** Each worker takes an owned
  `HeapCons<f32>` + a `MeasurementInputs` bundle by value;
  `measure_inline` and its helpers were refactored to take
  `&mut HeapCons<f32>` instead of `&mut AudioInput + pair_idx`
  precisely so the AudioInput's exclusive borrow doesn't force
  sequential calibration.

**Mid-stream attach via the SPSC command channel (M5.4.5).** Once
the engine has been moved into `AudioOutput::start_with_options`,
no `&mut Engine` access is possible from the main thread. Mid-
stream attach goes through a new `Command` variant:

```text
EngineHandle::attach_timecode_input(idx, rx, cfg)
  → main thread: TimecodeInput::new(rx, cfg)   ← allocates, off-RT
  → main thread: Box::new(timecode_input)      ← 8-byte pointer
  → push Command::AttachTimecodeInput { idx, input: Box<…> }
       through the existing 256-deep SPSC command channel
  → audio thread (Engine::apply_command):
       slot = engine.timecode_inputs.get_mut(idx)
       displaced = slot.replace(*input)  ← move out of the Box
       if Some(old) = displaced:
           send Box::new(old) to second trash channel
            (NEVER drop on the audio thread)
```

Single-deck mode is the same flow with worker_b skipped and
`handle.attach_timecode_input(1, …)` never called — deck 1 stays
paused and silent forever.

**Why no hot-keys for the takeover use case.** Worker_b's
`wait_for_stable_carrier` is called with
`MeasureOptions::detect_timeout_secs = None`: the worker sits
indefinitely waiting for deck B's carrier to appear. During a
takeover, the incoming DJ's app is running with worker_b still
waiting; whenever the previous DJ finally vacates and a record
drops on deck B, worker_b wakes up, completes, attaches mid-
stream. Deck A audio (already attached) is undisturbed. **Hot-
key-driven mid-stream re-attach** (e.g., DJ launches single-deck
and later decides to add deck B, or DJ swaps cartridges mid-set)
is a follow-up — the engine surface is ready (replace-and-trash
on `AttachTimecodeInput`), but the CLI plumbing for crossterm
hot-keys + dynamic `AudioInput` reconfiguration is its own work.

**No-calibrate path (`--no-calibrate`).** Skips the worker
threads entirely; main calls `handle.attach_timecode_input` for
each deck immediately with M5.3-default thresholds + CLI
overrides. Useful for testing the audio path without hardware,
and for first-time users who want to hear the deck immediately
even without a calibrated rig. The pre-M5.4.6 design layered on top of
this a *load-the-saved-JSON-and-fingerprint-probe-on-startup* path
to skip recalibration on repeat sessions. M5.4.3 cut fresh
calibration to ~3.5 s, at which point the probe (~1.7 s) was only
saving wall-time when the rig was unchanged. **Touring DJs see a
different rig at every venue**, the probe always mismatches, and
the auto-recalibration runs anyway on top of the probe. So on the
production path the save+probe model was *worse* than always-fresh
(probe + recal = ~5.2 s vs. recal alone = ~3.5 s). Always-fresh
is also a simpler mental model ("calibrate auto-runs on every
start, period") and immune to "stale calibration silently used"
failure modes.

**Save semantics.** Atomic via temp-file + rename so a crash
mid-write doesn't corrupt a previous file. Save failures (disk
full, sandbox, …) are warnings, not fatal — runtime never reads
these files anyway (M5.4.6), and a live performance setup must
always remain usable even with persistence broken.

**Schema version.** Bumped on incompatible format changes.
Readers (tests + future inspection tooling — `Calibration::load`
remains `pub` for that purpose, marked `#[allow(dead_code)]` since
the binary path doesn't reach it any more) reject files with
`schema_version > SCHEMA_VERSION`. The JSON deliberately stores
the full P5/P50/P95 measurements, not just the derived thresholds;
future formula changes (M6 Traktor calibration tweaks, future
analysis tooling) can re-derive without remeasurement.

**`RigFingerprint` field — diagnostic only since M5.4.6.** The
JSON still carries `fingerprint: { carrier_amp_p50, carrier_amp_p95,
carrier_conf_p50 }` because (a) the schema is shared with M5.4.2 …
M5.4.5 files (forward+backward compat without a schema bump), and
(b) future analysis tooling that wants to compare carrier
signatures across sessions / venues / cartridges can deserialize
directly. The matching code (`RigFingerprint::matches /
max_relative_delta / within_relative / relative_delta`,
`DEFAULT_FINGERPRINT_TOLERANCE`) is gone — nothing compares
fingerprints at runtime any more.

### Thru Mode — M7

Thru Mode lets a deck source audio from the audio interface input
(a real, non-timecode record on the platter) instead of a loaded
file. `Engine::render_routed` dispatches per-deck: if a Thru source
is attached, it owns that deck's output channels for the block and
the deck's own transport (loaded track, position, rate) is *not*
advanced — a real record has no track to walk a playhead through.
When no Thru source is attached the M0–M6 Track render path runs
unchanged, byte-identical to pre-M7.

**One mode, always on.** `ThruSource` is a dumb passthrough: read
input ringbuf → add gain-scaled samples into the deck's routed
output channels → done. No state machine, no FX-engaged refcount,
no Direct/Processed split. The signal is always in software
because that is the entire point of Thru Mode in Dub: BPM
detection (M8), waveform capture (M9), and FX (M15+) all live in
the software path. Hardware-bypass Thru (the interface's physical
button) is intentionally outside Dub's scope — see PRD §5.2.2 for
the design rationale.

**Parallel array layout.** Mirrors the M5.3 `timecode_inputs` shape:

```text
Engine {
    decks:           [Deck; 2],
    timecode_inputs: [Option<TimecodeInput>; 2],   // M5.3
    thru_sources:    [Option<ThruSource>;    2],   // M7
    ...
}
```

`render_routed` walks `0..DECK_COUNT` and, for each routed deck,
picks the right source:

```text
if let Some(thru) = self.thru_sources[idx].as_mut() {
    let gain = self.decks[idx].gain();
    thru.render_into(out, gain, num_channels, first_us);
} else {
    self.decks[idx].render_into(rt, out, sr, num_channels, first_us);
}
```

The deck's `gain` is still respected on a Thru deck — only the
*source* of the audio is swapped, not the per-deck mixer fader.
Master gain applies once across the whole bus after the deck loop,
same as before.

**`ThruSource` internals.**

```text
struct ThruSource {
    rx: HeapCons<f32>,    // SPSC consumer; producer is the CoreAudio IOProc
    scratch: Vec<f32>,    // pre-allocated max_block_frames * 2 interleaved
}
```

`render_into(out, gain, stride, offset)`:

1. `rx.pop_slice(&mut scratch[..frames * 2])` — load + memcpy, no alloc.
2. Zero the tail of `scratch` past whatever was popped, so underrun
   renders as silence-additive (no panic, no audible artifact past
   the dry input continuing).
3. Loop: `out[offset + i * stride] += scratch[2 * i] * gain` and the
   `+1 / +1` companions.

All steps alloc-free under `assert_no_alloc`. Underrun (empty
ring) adds 0.0 to the output and is therefore transparent to
upstream content — important because the IOProc takes a few
hundred microseconds to start producing data after `AudioInput::
start`.

**FX engagement (forward-looking — M15+).** FX modules will live
*inside* the per-deck signal chain on top of Thru's passthrough
output. Each FX owns its own engage/disengage semantics with a
per-module declick on the FX's *wet* output. The *dry* path
through `ThruSource` is never paused, never crossfaded, never re-
timed on FX engagement — so the input-to-output latency stays
constant across the whole set, which is what makes scratch muscle
memory transferable from a session's first scratch to its last.

This is the Option A in-chain bypass model. The alternative
("Option B": switch between an FX-loaded chain and an FX-free
chain on engage) was prototyped in M7's first ship (the
`ThruMode { Direct, Processed, ProcessingHold }` state machine,
the 5 ms equal-power Direct↔Processed crossfade, the
`Command::SetDeckThruFxEngaged` refcount-driven auto-switch) and
removed in the same milestone for two reasons:

1. *Hardware-Thru incompatibility.* `Direct` mode was supposed to
   render silence in software and rely on the interface's hardware
   monitoring for audible passthrough. CoreAudio doesn't drive the
   hw-monitor switch on SL3-class devices under plain HAL access,
   so `Direct` was silent in practice. A follow-up PR could have
   added vendor-specific hw-monitor control to fix that — but
   that path takes BPM/waveform/FX off the table for the deck,
   defeating Thru's purpose.
2. *Latency-jitter on FX engage.* Any path-swap model introduces
   a latency delta between the two paths (FX modules with
   look-ahead, slightly different DSP chains, etc.). Toggling FX
   would shift the input-to-output delay by sub-millisecond
   amounts, which scratch DJs *can* feel and which would break
   muscle-memory calibration. Constant-latency Option A defends
   the M3.5 / M6 / M7 latency work end-to-end.

The simplified `ThruSource` keeps the engine RT-safe, makes Thru
testable as a pure data type with eight tight unit tests, and
matches the user-facing model in PRD §5.2.1.

**Trash channel.** Mid-stream re-attach (operator switches input
pairs or swaps cartridges on a Thru deck mid-set) replaces the
existing `ThruSource` in the slot; the displaced predecessor is
shipped through the `HeapRb<Box<ThruSource>>` trash channel for
main-thread disposal — mirroring M5.4.5's `Box<TimecodeInput>`
pattern. See "Trash channels" above for the full picture.

**Off-RT construction.** The `ThruSource` is constructed on the
main thread, boxed, and pushed through the command channel as a
single 8-byte pointer; the audio thread does `*Box<ThruSource>`
and a `slot.replace`, both alloc-free. No `DeclickEnvelope`
plumbing in or out of `ThruSource` — the simplified design has
no audibility crossfade to drive (constant audibility means no
transition to declick).

### BPM engine — M7.5 (offline DSP core) + M8 (streaming driver on Thru) + M8.1 (octave fix) — all shipped

The BPM stack is built in two layers, both shipped. **M7.5** shipped
the DSP core as the `dub-bpm` crate (offline `analyze_bpm` +
streaming-agnostic `BpmEstimator`, plus `Track::bpm` field on
`dub-io::Track`). **M8** wrapped that core in a streaming driver
plumbed into Thru-attached decks via a per-deck audio-thread tee +
per-deck off-RT analysis thread + confidence state machine. Both
halves share the same `BpmEstimator` so the offline answer remains
the oracle for the streaming convergence test
(`crates/dub-bpm/tests/known_bpm.rs::streaming_estimator_converges_to_offline_result`,
plus the end-to-end `crates/dub-bpm/src/stream.rs::click_track_streams_to_lock`).

```text
                          ┌──────────────────────────┐
                          │      dub-bpm crate       │
                          │  (M7.5 + M8, leaf,       │
                          │   pure Rust)             │
                          │                          │
                          │   BpmEstimator           │
                          │     feed(block)          │
                          │     recompute()          │
                          │     current()            │
                          │                          │
                          │   ConfidenceTracker      │
                          │   (searching/tentative/  │
                          │    locked hysteresis)    │
                          │                          │
                          │   BpmTracker             │
                          │   (estimator + state     │
                          │    machine + throttle)   │
                          │                          │
                          │   BpmStream              │
                          │   (analysis thread +     │
                          │    events ringbuf)      │
                          │                          │
                          │   analyze_bpm(...)       │
                          │   (offline whole-buffer  │
                          │    driver)               │
                          └─────────┬────────────────┘
                                    │
                  ┌─────────────────┼────────────────────┐
                  │                                      │
                  ▼ (M7.5 — file path)                   ▼ (M8 — live Thru path)
        ┌──────────────────┐                  ┌─────────────────────────────────┐
        │  let est =       │                  │  ThruSource::with_bpm_tee:      │
        │   analyze_bpm(   │                  │   audio thread mono-downmixes   │
        │     track.samples│                  │   each block & push_slice's it  │
        │     , track.sr,  │                  │   into the tee SPSC ringbuf     │
        │     track.chans);│                  │   (alloc-free, drop-on-full).   │
        │  let track =     │                  │                                 │
        │   track.with_bpm │                  │  BpmStream's analysis thread    │
        │   (Some(est.bpm));│                 │   reads the tee ring off-RT,    │
        │                  │                  │   feeds BpmTracker, emits       │
        │  Runs at load    │                  │   StateChanged events to a      │
        │  time, off-RT.   │                  │   second SPSC ring the UI polls.│
        │  Used by §8.3    │                  │                                 │
        │  beatgrid auto-  │                  │  Audio thread NEVER runs        │
        │  detect fallback │                  │  the estimator. ThruSource      │
        │  + display path. │                  │  stays a pure passthrough.      │
        └──────────────────┘                  └─────────────────────────────────┘
```

**Algorithm (M8.1 current, used by both paths).** Pure-Rust
**log-band-weighted spectral-flux** onset detection function
(Hann-windowed FFT magnitude differences at `FRAME_SIZE = 1024` /
`HOP_SIZE = 512`, summed equally across 8 log-spaced bands from
30 Hz to 16 kHz, Klapuri-2006 `ln(1 + λ|X|)` magnitude
compression) → unbiased autocorrelation → **windowed local-energy
harmonic-mean** peak picker (5-bin window sum at each integer-lag
candidate, harmonic mean over the first 4 multiples, smaller-lag
tiebreak inside a 1 % score tie window) → centroid sub-bin
refinement → confidence = peak / acf-at-zero, refused below 0.05.
The M7.5 baseline (single-band flux + harmonic-sum + smoothed-ACF
parabolic interpolation) hit a hard regression on real hip-hop
audio in M8 — the hi-hat-on-8ths sub-beat ostinato made the
autocorrelation peak at lag `P/2` beat the one at `P` because the
high-frequency bin count dominated the flux. The M8.1 rewrite
(log-band ODF rebalances FFT bin contribution per band; windowed
local-energy removes the parabolic-vertex shoulder-asymmetry
bias) resolves the user's stated genre mix (reggae 65, hip-hop
90/100, rolling dnb 174) at the correct octave out of the box.
See [`docs/SHIPPED.md#m75`](SHIPPED.md#m75) for the M7.5 baseline
walkthrough and [`docs/SHIPPED.md#m81`](SHIPPED.md#m81) for the
M8.1 multi-band + windowed-energy derivation, including the
"why not biased ACF" and "why not wider tie tolerance"
trade-offs that landed on the chosen design.

**Bpm range as escape hatch.** The M8.1 algorithm cannot in
principle resolve genres whose autocorrelation legitimately peaks
at a different octave than the convention (dubstep at 140 / 70,
K-S-backbeat dnb at 174 / 87) without a tempo or genre prior. The
[`BpmRange`](../crates/dub-bpm/src/lib.rs) value type +
`analyze_bpm_with_range(samples, sr, channels, range)` (offline)
+ `TrackerConfig::bpm_range` (streaming) + `dub thru --bpm-range
MIN,MAX` (CLI) plumb a user-chosen `[min, max]` BPM window
through the whole stack. The default is the full `[60, 200]`
window the M8.1 algorithm is calibrated for; constraining the
range is the explicit user-driven path for the irreducibly-
ambiguous edges.

**Why the streaming side doesn't touch the audio thread for
analysis.** The autocorrelation search is O(odf_len × max_lag) and
grows quadratically with track length — too expensive to run inside
the per-block budget alongside the existing decoder + resampler +
declick + render load. M8 splits `BpmEstimator::process` into
`feed` (cheap, runs every block) + `recompute` (expensive, runs on
demand) so the `BpmTracker` can drive the search at a throttled
~1 Hz cadence on the off-RT analysis thread while the audio thread
just does an alloc-free mono-downmix + `push_slice` into the tee
ringbuf. The audio-thread cost is ≈ 3 floating-point ops per stereo
frame plus one SPSC write per block — well within budget and
verified alloc-free under `assert_no_alloc`.

**Why the tee, not the existing input ring.** The Thru source's
ring is consumed by the audio thread to produce output; reading
the same consumer end from the BPM analyzer would race the engine.
M8 takes the audio-thread-duplicates approach: after `pop_slice`
fills the per-block scratch buffer (for the output path), the
audio thread mono-downmixes that scratch into a second pre-allocated
buffer and `push_slice`s it into the BPM tee ring. The alternative
(IOProc demuxer pushes into both rings on the producer side) would
have coupled the demuxer to BPM, which is the wrong direction of
dependency.

**Confidence state machine** lives in `dub_bpm::ConfidenceTracker`,
not in `BpmEstimator`. The estimator emits raw `BpmEstimate { bpm,
confidence }`; the tracker applies hysteresis (`LOCK_CONSECUTIVE`
agreeing updates inside `LOCK_TOLERANCE_BPM` to transition
`tentative → locked`, asymmetric loss thresholds so brief silence
doesn't break lock) to give PRD §5.2.3's UI states a clean,
well-defined behaviour. Same separation we already have between
`dub-timecode::Decoder` (pure DSP) and `dub_engine::LiftPolicy`
(state machine on top). Tuning constants live in
`crates/dub-bpm/src/confidence.rs` and are re-exported at the
crate root for future per-genre profiles to bind to.

### Live waveform-peak capture — M9 (shipped)

The data layer underneath M10's Metal waveform UI. New `dub-peaks`
crate, sibling of `dub-bpm`: pure-Rust off-RT decimator that consumes
a mono-downmixed tap from `ThruSource` and produces a growing
append-only sequence of `PeakChunk { min, max, rms }` envelope
records, exposed to readers via a thread-safe `PeakBuffer` handle.

```text
        ┌────────────────────────────────┐
        │     ThruSource (audio thread)  │
        │       ┌────────────────┐       │
        │  input ring  pop_slice │       │
        │       └──────┬─────────┘       │
        │              ▼                 │
        │   stereo render → routed out   │
        │              │                 │
        │              ▼                 │
        │   mono-downmix (one pass)      │
        │     ├─ bpm_tx.push_slice  ─────┼─► dub-bpm   (M8)
        │     └─ peaks_tx.push_slice ────┼─► dub-peaks (M9)
        │                                │      │
        └────────────────────────────────┘      ▼
                                   ┌─────────────────────────┐
                                   │    PeakStream thread    │
                                   │  Decimator(spc=64)      │
                                   │  PeakBuffer::push_chunks│
                                   └────────────┬────────────┘
                                                ▼
                                       ┌────────────────────┐
                                       │   PeakBuffer       │
                                       │   len: AtomicUsize │
                                       │   chunks: RwLock<  │
                                       │     Vec<PeakChunk> │
                                       │   >                │
                                       └────────┬───────────┘
                                                ▼
                                 ┌────────────────────────────┐
                                 │ Renderer / CLI (60 fps)    │
                                 │  len() lock-free poll      │
                                 │  extend_chunks(idx, &mut)  │
                                 │  → local Vec mirror        │
                                 └────────────────────────────┘
```

**Single mono-downmix, two taps.** The M8 BPM tee and M9 peaks tap
share the same `mono_scratch` inside `ThruSource`. The audio thread
computes the mono downmix once per block (only if at least one tap
is enabled) and dispatches with two `push_slice` calls. Cost of
adding the peaks tap on top of the BPM tee: one extra memcpy into
an SPSC ring. Verified alloc-free by `both_taps_render_is_alloc_free`.

**Decimator.** Pure online aggregator. Given `samples_per_chunk = N`
(default 64 ≈ 1.33 ms at 48 kHz), the decimator carries a
`(min, max, sumsq, count)` running aggregate across `feed` calls and
emits one `PeakChunk { min, max, rms }` exactly every `N` samples.
RMS is `sqrt(sumsq / N)` with `sumsq` accumulated in `f64` for
numerical stability at longer mip levels. Block-size invariant
(verified by `block_size_does_not_change_output`): feeding the same
input 1 sample at a time, 7 samples at a time, or all at once yields
identical chunk sequences. `flush` emits a partial chunk on
shutdown.

**PeakBuffer.** Shared, append-only, lock-free length check + brief
read-lock for bulk access:

- `len()` is an Acquire-load on an `AtomicUsize`. The renderer's
  "anything new?" check at 60 fps never touches the RwLock.
- `extend_chunks(start_idx, &mut Vec<PeakChunk>)` is the renderer
  fast path: O(new chunks), not O(total). Takes a read lock,
  appends `[start_idx..len]` into the caller's mirror, releases.
  Returns the new total length for the next call.
- `push_chunks` (used by the decimator thread) briefly takes the
  write-lock; happens at the decimator's 20 ms drain cadence.
- `snapshot` returns a full owned `Vec` clone — for CLI tools and
  tests; renderers use `extend_chunks` for the incremental path.

We don't use a fully lock-free design (e.g. pre-allocated
`UnsafeCell<[PeakChunk]>` with atomic length) because the write
contention is one bulk `extend_from_slice` per 20 ms and read
contention is one snapshot per 16.7 ms — RwLock is comfortably in
range. The `AtomicUsize` length already eliminates lock acquisition
on the common "no new data" path.

**Initial capacity.** `DEFAULT_BUFFER_CAPACITY_SECS = 600` (10 min)
× sample_rate / samples_per_chunk pre-allocates the Vec. Common
mix-track lengths don't reallocate; longer records (90 min vinyl
side) reallocate once or twice off-RT — the audio thread never
reallocates.

**`PeakChunk` is wire format.** `#[repr(C)]`, 12 bytes (3 × `f32`).
The M10 consumer contract is documented at the crate root: cache
`start_idx` per stream, call `extend_chunks` each render frame,
treat the returned slice as something you can `glBufferSubData`
(or Metal equivalent) into a vertex buffer with zero further
packing. min/max/rms (not just peak) because properly mastered
drums are asymmetric and RMS gives perceived-loudness shading for
free.

**`PeakStream` lifecycle.** Identical idiom to `BpmStream`: spawn
returns a joinable handle, `Drop` shuts down + joins, `shutdown()`
is the explicit form that surfaces join panics. The buffer is a
clone-on-fetch `Arc` so a renderer can keep its `PeakBuffer` handle
alive past stream teardown without lifetime gymnastics.

**Engine integration.** Four `EngineHandle::attach_thru_source*`
variants:

- `attach_thru_source` — M7, no telemetry.
- `attach_thru_source_with_bpm_tracking` — M8.
- `attach_thru_source_with_peaks_tracking` — M9.
- `attach_thru_source_with_telemetry` — M8 + M9 combined.

The combined attach is strictly cheaper than two separate attaches:
one `ThruSource`, one mono-downmix on the audio thread, two analysis
threads sharing the deck. Each method validates the relevant
sample rate before sending the attach command, and returns the
spawned stream handle(s) for the caller to drive.

**CLI surface.** `dub thru` defaults to peaks-tracking on; the
periodic stats line shows `peaks=[A=N B=M]` (captured chunk counts
per deck). `--no-peaks-track` opts out. `--dump-peaks PATH` writes
the per-deck buffers to a CSV file on shutdown
(`deck,chunk_idx,min,max,rms`) so the operator can validate capture
end-to-end before M10's UI exists.

**Multi-resolution mip pyramids — deliberately deferred.** M9 ships
one base mip level (64-sample chunks). Overview rendering for a 90
min record on a 4K screen needs ~67k samples per pixel — the
renderer can derive that with one averaging pass over the M9
chunks. The crate doesn't pre-build a mip pyramid because the
renderer knows how many pixels it has and the downsample is cheap;
adding a `MipLevel` enum or `with_decimation` config when M10
profiles in a real bottleneck is a small follow-up.

### Two decks + debug internal mixer — M4

The engine has always declared `DECK_COUNT = 2`; M4 makes the second
deck driveable end-to-end and adds a master gain to the debug internal
mixer. The mixer is intentionally minimal: each deck has its own
linear `gain`, both decks render additively into one summed stereo
bus, and `Engine::master_gain` (default 1.0) multiplies the bus once
after the deck loop. The multiply is skipped when master is unity
(`(g - 1.0).abs() <= f32::EPSILON`) so the common case has zero
arithmetic cost.

```text
                   ┌────────────────────────────┐
  Deck 0 ──gain──► │                            │
                   │   Σ   ──── master_gain ──► │ ──► CoreAudio (one stereo bus)
  Deck 1 ──gain──► │                            │
                   └────────────────────────────┘
```

Master gain is mutable through the lock-free command channel via
`Command::SetMasterGain` (engine-wide; carries no deck index). The
public surface on `EngineHandle` is `set_master_gain(g)`; per-deck
gain stays on `DeckCommand::set_gain`. Both compose multiplicatively
inside the render loop — no separate "channel strip" abstraction —
because v1's debug mixer doesn't need EQ/filters/sends and a flat
implementation keeps the audio thread's data dependency graph tiny.

External-mixer multi-channel routing arrives in M5.5 and reuses the
same primitive — see the next section.

### External-mixer routing — M5.5.1 (engine primitive)

The M4 internal mixer is a *special case* of a more general
multi-channel routing primitive that lands in M5.5.1. The two
mechanics — sum into one stereo bus (M4) vs. send each deck to its
own physical output pair (M5.5) — are unified under
`Engine::render_routed(rt, out, num_channels, &OutputRouting)`,
where `OutputRouting = [Option<u32>; DECK_COUNT]`. Each deck either
gets a *first-channel index* into the multi-channel buffer or
`None` (don't render).

```text
                                   ┌─ render_routed(out, 4, [Some(0), Some(2)])
                                   │
  Deck 0 ──gain──► render_into(stride=4, offset=0) ──► out[ch 0+1]
                                                            ▲
  Deck 1 ──gain──► render_into(stride=4, offset=2) ──► out[ch 2+3]
                                   │
                                   └─ master_gain × whole 4-ch buffer ──► CoreAudio
```

Two decks routing to the *same* `Some(c)` sum (= internal mixer);
non-overlapping `Some` values isolate them (= external-mixer
routing). `Engine::render` is now a thin wrapper around
`render_routed(out, 2, INTERNAL_MIXER_ROUTING)` where
`INTERNAL_MIXER_ROUTING = [Some(0), Some(0)]`, so M0–M5 callers
keep working byte-identically.

The strided write happens on the deck side: `Deck::render_into(rt,
out, sr, stride, offset)` iterates `out.chunks_exact_mut(stride)`
and writes the L/R samples at `chunk[offset]` and
`chunk[offset+1]`. The dense-stereo case (`stride=2, offset=0`)
matches the legacy code path exactly; the strided case adds zero
allocations and one extra arithmetic op per frame (`offset` is a
constant per-block, so LLVM hoists it).

**`None` semantics.** A deck with `routing[i] == None` is skipped
entirely — its transport state does NOT advance for that block.
This is deliberate: routing is a *hardware-mapping* concern, not a
mute mechanism. To silence a deck while keeping its transport
running (so the playhead stays in sync for when the user un-mutes),
use the M2 per-deck `Deck::set_gain(0.0)` knob. Reusing routing as
mute would couple unrelated concerns (declick envelope progress,
EngineHandle position snapshot, end-of-track flagging) and make
routing flips click. The
`render_routed_none_does_not_advance_transport` and
`render_routed_mute_via_gain_keeps_transport_advancing` tests pin
this distinction.

**Master gain in routed mode.** Master applies once across the
whole multi-channel buffer at the end. Unrouted channels stay zero
(zero × g == zero), so master never accidentally introduces signal
on an unrouted pair. Per-deck gain still composes multiplicatively
upstream of master — same as M4.

### External-mixer routing — M5.5.2 (CLI + CoreAudio + device profiles)

M5.5.1's engine primitive runs all the way to physical hardware via
three layered changes:

1. **`dub_audio::OutputOptions` + `AudioOutput::start_with_options`.**
   Mirror image of M5.2's `InputOptions`: configurable channel
   count, buffer size, optional channel map, optional SR override.
   Same SR-alignment guarantee as the legacy stereo path — the
   device is forced to engine SR (or the call fails loudly) so
   CoreAudio doesn't insert a silent SRC. The render callback
   captures a `dub_engine::OutputRouting` (Copy) and calls
   `Engine::render_routed` per block; no allocations on the audio
   thread.

2. **`DeviceInfo` learns `device_name`.** `query_default_output`
   now returns the CoreAudio device name and the *physical*
   channel count (queried via
   `kAudioDevicePropertyStreamConfiguration` on the output scope —
   the same property M5.2 uses on the input scope, generalised to
   `device_channel_count(scope)` so input/output share one path).
   This is what lets the CLI reason about which interface is
   plugged in without touching the audio thread.

3. **`device_profiles::KNOWN_DEVICES` table.** A small static list
   of validated interfaces with their canonical per-deck routing.
   Currently:

   - **Serato SL 3** (verified): `output_channels = 6`, deck A on
     ch 3+4, deck B on ch 5+6 (aux on 1+2). Matches the SL3's
     internal per-deck wiring; the same physical pair carries deck
     A's input (timecode in) and deck A's output (track audio
     back to the mixer), so a user who's already wired
     `--input-channels 3,4` for timecode automatically gets deck A
     audio on the matching output pair. No `2N+1/2N+2` formula —
     that formula is wrong for our reference device.
   - **Traktor Audio 6** (unverified): best-effort guess
     deck A 1+2, deck B 3+4. Warns at startup until validated.

The CLI's resolution priority (in `timecode_deck.rs::resolve_output_routing`):

```text
  --internal-mixer
    └─→ 2-ch internal mixer (debug only, "not for live" warning)

  --deck-a-out-ch N + --deck-b-out-ch M (must be paired)
    └─→ manual routing, channels = --output-channels or device.channels

  --device-profile NAME (exact name_pattern match)
    └─→ profile's routing

  no flags + device.device_name matches a KNOWN_DEVICES pattern
    └─→ profile's routing (auto-detect — the SL3 path)

  no flags + unknown device
    └─→ 2-ch internal mixer fallback (loud warning,
        "for an external mixer pass --deck-a-out-ch / --deck-b-out-ch")
```

The fallback is opinionated about being a dev path: live
performance on a laptop output isn't supported because there's no
per-deck physical separation, which violates the "no mouse DJ
ever" rule. The user can always hear playback for prep / library
work; they just can't hand-mix.

**Why a table, not a formula.** Earlier drafts assumed
`2N+1, 2N+2` (deck 0 → ch 1+2, deck 1 → ch 3+4). That's wrong for
the SL3 — its aux is on 1+2 and decks are on 3+4 / 5+6. A formula
that's wrong for our reference device is worse than no formula:
the CLI would silently send deck audio to the wrong physical
pairs and the user would have a mystery-silence debug session.
Explicit table + opt-in for unknown devices is the safer default.
Adding a new device is one entry in `KNOWN_DEVICES` plus a unit
test (see the module-level docs).

**1-based vs. 0-based.** CLI flags are 1-based (`--deck-a-out-ch 3`
matches what's printed on the back of the SL3, what the driver
panel shows, and what every DJ knows by heart) but the engine
routing is 0-based (`Some(2)` for ch 3+4). Conversion happens once
in `device_profiles::one_based_to_zero_based`; tests pin the round
trip.

### Two-deck timecode input — M5.6 (CoreAudio demux)

M5.5.2 made the *output* path two-deck capable. M5.6 closes the
symmetry on the *input* path so a real two-record timecode session
on a single audio interface (SL3) can drive both engine decks
independently.

The constraint that shapes the design: **CoreAudio does not allow
two `AudioUnit`s to open the same physical input device in the
same process.** A naive "open one stereo AU per deck" approach
fails at `audio_unit.start()` for the second AU. Real DJ apps —
including the historical Scratch Live we're modelling — solve
this by opening one multi-channel AU and demuxing in software.

```
                  CoreAudio input AU (4 logical channels)
                  channel_map = [a_l-1, a_r-1, b_l-1, b_r-1]
                              │
                  ┌───────────┴───────────┐
                  │     IOProc thread     │
                  │  (push_demuxed_frames) │
                  └───┬───────────────┬───┘
                      │               │
                ringbuf 0          ringbuf 1
              (deck A pair)     (deck B pair)
                      │               │
              attach_timecode   attach_timecode
              _input(0, …)      _input(1, …)
                      │               │
                Engine deck 0    Engine deck 1
```

**`InputOptions::output_pairs: Option<Vec<(u32, u32)>>`** declares
the per-pair `(L, R)` indices into the AU's logical (post
channel-map) interleaved frame. `None` and `Some(vec![(0, 1)])`
are equivalent — both mean "single stereo pair" — preserving
M5.2 / M5.3 / M5.4 byte-identical RT behaviour. Two-deck mode
sets `Some(vec![(0, 1), (2, 3)])`.

**`push_demuxed_frames(buf, channels, pairs, &mut txs)`** is the
extracted IOProc inner loop:

```rust
for frame in buf.chunks_exact(channels) {
    for (p_idx, &(l, r)) in pairs.iter().enumerate() {
        let pair_samples = [frame[l as usize], frame[r as usize]];
        let pushed = txs[p_idx].push_slice(&pair_samples);
        if pushed < 2 { overflow = true; }
    }
}
```

Linear in `frames × pairs` (256 × 2 ≈ 50 µs / callback at 48 k);
no allocations; lock-free `push_slice` (atomic-CAS index update);
overflow is signalled once per callback (matches the M5.2
single-pair convention so existing rt-audit traces stay
comparable). The function lives outside the closure so it's
unit-testable without standing up an audio device — five tests
pin single-pair pass-through, 4-channel isolation, swapped
`(L, R)`, overflow signalling, and partial-frame handling.

**Per-pair `AudioInput` API.** New methods alongside the
single-pair API:

```rust
fn pair_count(&self) -> usize;
fn take_consumer_pair(&mut self, idx: usize) -> Option<HeapCons<f32>>;
fn read_into_pair(&mut self, idx: usize, dst: &mut [f32]) -> usize;
fn available_pair(&self, idx: usize) -> usize;
```

`take_consumer()`, `read_into()`, `available()` keep their
existing semantics by aliasing to pair 0. Calibration and
`dub levels` / `dub capture` continue to read from pair 0 with
zero code changes.

**CLI surface.** `dub timecode-deck <track-a> [<track-b>] \
  --input-channels 3,4 --deck-b-input-channels 5,6` triggers
two-deck mode. The helper `build_input_options` constructs the
4-channel `InputOptions` (channel_map = `[2, 3, 4, 5]`,
output_pairs = `[(0, 1), (2, 3)]`) and the run loop attaches
pair 0 to engine deck 0 and pair 1 to engine deck 1. Validation
rejects: `--deck-b-input-channels` without track B (silent
deck B); track B without `--deck-b-input-channels` (no transport
source); overlapping deck-A / deck-B pairs (would silently
mis-route to the audio thread); deck-B channels without deck-A
(ambiguous logical layout).

**Calibration semantics — per-deck since M5.4.4, always-fresh
since M5.4.6.** Two-deck mode runs `resolve_thresholds` once per
deck. The flow is the M5.4.6 always-fresh path (no JSON load, no
fingerprint probe): a full single-phase carrier-only calibration
(M5.4.3, ≈ 3.5 s per deck) for each deck, deck A first then deck
B, with per-deck status banners (`calibration A:`, `calibration
B:`) so the user knows which side they're spinning at any moment.
Each deck's thresholds land independently in
`attach_timecode_input`'s `TimecodeInputConfig`, so a mismatched-
cartridge rig (Concorde on A, Nightclub on B — common in scratch
DJing where you want a more aggressive cartridge for routine play
and a smoother one for cueing) gets correct lift behaviour on
both sides. The legacy two-phase calibration flow remains
reachable via `dub calibrate --two-phase` for diagnostics. The
JSON file each deck writes is a *diagnostic artifact* only — the
runtime never reads it back at the next startup. **Limitation
(M5.4.5):** today both decks calibrate sequentially before audio
starts; the takeover use case (incoming DJ has no access to deck
B's record) requires per-deck independent readiness with audio
starting on first-deck attach. M5.4.5 ships that.

Calibration JSON keys by `(device, deck_index, format)`: the
on-disk pattern is `~/.dub/calibration/<device>_deck_<idx>_<format>.json`.
Pre-M5.4.4 single-deck files (`<device>_<format>.json`) used to
load as deck-0 fallbacks during the M5.4.4 → M5.4.5 migration
window; M5.4.6 deleted the load-from-disk path entirely so those
files are now ignored (existing JSONs on disk remain harmless —
just orphaned bytes).

The `pair_idx` (which AudioInput pair to read on, M5.6 demuxing)
and `deck_index` (on-disk metadata, which engine deck this
calibration belongs to) are intentionally separate parameters in
`measure_inline`. They coincide in `dub timecode-deck`'s two-deck
path (deck N reads from pair N) but diverge in `dub calibrate`:
that command opens a dedicated 2-channel input regardless of
which deck the user wants the result to apply to (`dub calibrate
--input-channels 5,6 --deck 1` opens a 2-channel SL3 input over
physical channels 5/6 — that's still pair 0 of the AudioInput —
and stamps the result as deck 1). Conflating them in the first
M5.4.4 draft caused a self-found bug ("user picked deck 1 →
tried to read pair 1 → only pair 0 exists → silent silence
read"); the fix is the two-parameter signature preserved in the
public API.

The earlier "library of named cartridge profiles" framing was
dropped during M5.4.4: with M5.4.3-fast calibration, "always
recalibrate on startup" is simpler than "manage a profile
library", has no UX surface, and matches what real DJs expect
(auto-calibrate on app start, manual button on cartridge swap —
the latter belongs in M10's UI; on the CLI today it's
`dub calibrate --deck 0` or `--deck 1`). M5.4.6 took this one
step further by gutting the entire load-from-disk + fingerprint-
probe machinery — every startup runs a fresh calibration against
the rig in front of the user, no caching, no per-rig migration
plumbing.

**Output side untouched.** M5.5.2's per-deck output routing
already supports two decks — M5.6 just provides two real input
sources to drive them. The startup banner `output routing:
Serato SL 3 (6 ch, deck A → ch 3+4, deck B → ch 5+6)` now
describes a fully-symmetric two-deck path on both input and
output.

### Engine → UI (state snapshot) — implemented in M2

Per-deck `Arc<DeckSharedState>` carrying:

- `position_bits: AtomicU64` (`f64::to_bits` of current track frame),
- `is_playing: AtomicBool`,
- `at_end: AtomicBool`.

Audio thread writes (Relaxed) once per render block. UI reads (Relaxed)
at whatever rate it likes — typically 60 fps for waveforms. There is no
synchronization guarantee across fields; tearing during a transport
change is invisible at 60 fps and we deliberately avoid the cost of
`SeqCst` here.

### Engine → UI (events) — pending M5+

`ringbuf::HeapRb<EngineEvent>` for discrete events (xrun detected, source
mode changed, end-of-track reached, etc.). Not yet wired; the snapshot
covers everything we need through M4.

## Build / link / ship

- `crates/dub-ffi` has `crate-type = ["lib", "staticlib", "cdylib"]`. The
  `lib` form is what Rust workspace consumers (`cargo test`) link against;
  the `staticlib` is what the xcframework wraps for the Apple app; the
  `cdylib` is what UniFFI's *library-mode* bindgen reads metadata from
  when generating Swift bindings.
- UniFFI generates Swift bindings from `#[uniffi::export]` proc-macros on
  Rust items in `crates/dub-ffi/src/lib.rs`. No UDL file — the Rust
  signatures are the single source of truth. Bindings live in
  `apple/DubShared/Sources/DubCore/Generated/` (gitignored).
- `scripts/build-xcframework.sh` (M0.5) orchestrates: `cargo build
  --target aarch64-apple-darwin --target x86_64-apple-darwin -p
  dub-ffi`, `lipo -create` for a universal static lib,
  `cargo run --bin uniffi-bindgen --features uniffi-cli -- generate
  --library libdub_ffi.dylib --language swift` for the Swift bindings,
  and `xcodebuild -create-xcframework` to assemble
  `apple/DubCore.xcframework/`.
- `scripts/bootstrap.sh` (M0.5) wraps the above and then runs
  `xcodegen generate` against `apple/project.yml` to (re)produce
  `apple/Dub.xcodeproj`. The Xcode project itself is gitignored —
  `project.yml` is the diffable source of truth.
- Apple app (`apple/Dub/`) is hybrid AppKit + SwiftUI: `@main`
  `NSApplicationDelegate` opens an `NSWindow`; the window's content
  view controller is an `NSHostingController` rendering SwiftUI views.
  The audio HUD path (M10) stays on the AppKit side for the lowest
  overhead; everything non-realtime is SwiftUI.
- Distribution: GitHub Releases, unsigned in v1.0 ("Sign to Run
  Locally" only), notarized in v1.1 once an Apple Developer account
  is in place. Sandbox is off in M0.5 / M10 to keep CoreAudio device
  access frictionless during development.

## Tests

- Unit + property tests live next to source.
- Integration tests in `crates/<name>/tests/`.
- Soak harness lives in `crates/dub-cli/` (offline render with synthetic input).
- Fuzz targets in `fuzz/fuzz_targets/` (added per parser as they land).
- Snapshot tests for SwiftUI views via `swift-snapshot-testing`.

## Open architecture questions

(These are tracked here, not as commitments — answers emerge during implementation.)

- Should the audio worker (decoder + waveform pre-render) be a single thread
  with cooperative work-stealing, or one thread per deck? **Decision: M3.**
- Engine state snapshot: one big atomic struct, or many small atomics? Trade-off
  is cache-line traffic vs. update granularity. **Decision: M4.**
- ~~UniFFI vs `swift-bridge` for the FFI surface — UniFFI is more polished,
  `swift-bridge` allows tighter integration. **Decision: M0.5.**~~ Resolved
  in M0.5 — UniFFI 0.28 proc-macros. See [`docs/SHIPPED.md#m05`](SHIPPED.md#m05).

## See also

- [`docs/PRD.md`](PRD.md) — product spec (source of truth)
- [`docs/SHIPPED.md`](SHIPPED.md) — full design history of M0 → M7 (per-milestone rationale, what was deliberately removed, etc.)
- [`docs/LIBRARY-FORMATS.md`](LIBRARY-FORMATS.md) — Serato / Traktor / rekordbox / iTunes / Lexicon parsing notes
