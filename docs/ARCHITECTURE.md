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
                      ┌────────────────┐
                      │     dub-cli    │   (binary, smoke harness)
                      └───────┬────────┘
                              │
                              ▼
                      ┌────────────────┐
                      │     dub-ffi    │   (UniFFI surface)
                      └───────┬────────┘
                              │
                              ▼
                      ┌────────────────┐
                      │   dub-engine   │ ─┬─ ringbuf
                      └───────┬────────┘  │
              ┌───────────────┼──────────┐
              ▼               ▼          ▼
         dub-dsp         dub-stretch   dub-thru
         dub-io          dub-timecode  dub-fingerprint  dub-library
         dub-controller  (placeholders for v1+)
```

Only `dub-engine` is on the audio thread. Everything else is either
preparatory work, off-thread workers, or non-RT services.

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

### Trash channel (audio → UI for `Arc<Track>` disposal) — M3

`ringbuf::HeapRb<Arc<Track>>` (SPSC, capacity 32). The audio thread
NEVER drops `Arc<Track>` — `Arc::drop` decrements the strong count and
calls `dealloc()` if it hits zero. `dealloc` is a syscall, forbidden on
the RT thread.

When the engine applies `DeckLoad`, it `swap_source`s the new Arc onto
the deck and pushes the old Arc into the trash channel. The main thread
drains the channel via `EngineHandle::reclaim()` (called automatically
inside `DeckCommand::load` and on `EngineHandle::drop`).

If the trash channel ever overflows (UI not draining + storm of loads),
the audio thread `mem::forget`s the rejected Arc (leaking it) and
increments an atomic `trash_overflow_count`. Leaking is the lesser evil
versus a forbidden `dealloc` on the RT thread, and the counter surfaces
the contract violation to the UI for logging.

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

### Timecode decoder, relative-mode-only — M5.1

Lives in `dub-timecode`. Pure DSP, no I/O, no allocations on the hot
path — designed to drop straight onto the audio thread when M5.3 wires
it up to live audio input.

**Signal model.** Both stereo channels carry the same nominal sinusoid
at the format's carrier (1 kHz for Serato CV02), offset by 90° between
L and R. Treating each frame as a complex sample `s = L + jR`, the
input becomes a single complex exponential `s(t) = A · exp(j·2π·f·t)`
whose frequency is positive when the record turns forward and negative
when reversed. Magnitude `|s|² = L² + R²` is constant across rotation,
which is what makes amplitude AGC unnecessary for the *phase* tracking
(it'll matter later for AM-bitstream decoding in M6).

**Per-block algorithm.**

```text
  for each stereo frame n:
    s_n = L_n + j R_n
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
`Nyquist / carrier = 24×` ceiling on trackable rates at 48 kHz / 1 kHz.
Real DJ scratching tops out at ~8×, well clear. Below that limit the
estimator is bias-free and limited only by sample-rate quantization
(~50 µs at 48 kHz, equivalent to ~0.005 of unity rate).

**What's *not* here yet.** Absolute position (M6 — needs bitstream
demod and the format's 20-bit code table), stickiness policy (M5.4 —
"confidence dropped below threshold for N ms → freeze deck" lives in
the integration layer, not in the DSP), and AGC + cartridge
calibration (M6 — real-world amplitude variation). The decoder
exposes `confidence` and `amplitude` so the integration layer can
implement those policies without modifying the DSP.

**License + provenance.** Clean-room implementation from the
xwax/Mixxx algorithm description; no xwax code copied (xwax is BSD;
dub is GPL-3.0 — the *direction* of compatibility allows BSD → GPL,
but we want attribution to remain unambiguous, hence the rewrite from
spec).

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

External-mixer 4-channel routing (deck 0 → output channels 1+2,
deck 1 → output channels 3+4) is **deliberately deferred** to M5/M6.
That's the milestone where the timecode hardware (SL3, Audio 6) makes
multi-channel routing actually testable. v1's debug mixer covers
single-stereo-output development and is what every existing CLI
analyze workflow runs against.

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

- Rust core builds to a static library + cdylib.
- UniFFI generates Swift bindings from `dub-ffi`'s UDL.
- `scripts/build-xcframework.sh` (M0.5) orchestrates: cargo build for both
  arches, lipo, xcodebuild -create-xcframework, UniFFI bindgen.
- Apple app links the `DubCore.xcframework`.
- Distribution: GitHub Releases, unsigned in v1.0, notarized in v1.1.

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
- UniFFI vs `swift-bridge` for the FFI surface — UniFFI is more polished,
  `swift-bridge` allows tighter integration. **Decision: M0.5.**

## See also

- `docs/PRD.md` — product spec (source of truth)
- `docs/LIBRARY-FORMATS.md` — Serato / Traktor / rekordbox / iTunes / Lexicon
- `docs/adr/` — architecture decision records (not yet populated)
