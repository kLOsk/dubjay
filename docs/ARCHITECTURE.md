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

- `Command` is a `#[repr]`-friendly enum (`Copy`, ≤ 32 bytes), no `Box`,
  no `dyn Trait`. Variants today: `DeckPlay`, `DeckPause`, `DeckSeek`,
  `DeckSetRate`, `DeckSetGain`. Adding a command is one variant + one
  match arm in `apply_command`.
- The drain is RT-safe: `try_pop` is a load + index, and every variant
  applies in-place to the deck array. Verified by `rt-audit` with 100k
  blocks and 10k pre-staged commands under `assert_no_alloc`.
- Track loading is **not** a command yet. Tracks are pre-loaded before
  `AudioOutput::start`. The reason: swapping `Arc<Track>` on the audio
  thread risks dropping the old `Arc` to zero, which calls `dealloc`
  (a syscall). M3 (disk streaming) introduces a "trash channel" sending
  the old `Arc` back to the main thread for drop, then enables a
  `LoadTrack` command.

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

### Engine → UI (events) — pending M4+

`ringbuf::HeapRb<EngineEvent>` for discrete events (xrun detected, source
mode changed, end-of-track reached, etc.). Not yet wired; the snapshot
covers everything we need through M3.

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
