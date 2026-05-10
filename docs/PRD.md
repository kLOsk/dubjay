# Dub — Product Requirements Document

> Workspace / repo: `dubjay`. Product / app name: **Dub**. Bundle id: `com.klos.dub`.

**Version:** 0.1 (draft, pre-scaffold)
**Author:** klos + Cursor
**Date:** 2026-05-08
**Status:** Working draft. Editable. Items marked **[ASSUMPTION]** are author defaults to be confirmed.

---

## 1. Vision

**Dub** is a desktop DJ application for **scratch DJs and vinyl enthusiasts** built around a single, uncompromising commitment: **best-in-class control-vinyl latency and feel on macOS**, plus first-class support for playing real records *through* the software (Thru mode) with effects, auto-BPM, and — eventually — automatic waveform recognition.

It is the spiritual successor to Serato Scratch Live for the urban music scene — hip hop, reggae/dub, dnb, dubstep, jungle, scratch — DJs whose audience comes for the music, not the production. The product is opinionated: it does a small set of things extremely well, and it explicitly does not try to be a club-DJ all-in-one.

The ethos:

- **The software is a tool, not a stage.** Reduced UI. Fast workflows. No feature shall exist if it has no function.
- **No Mouse DJ Ever.** Performance interactions never require a mouse. The mouse is for setup; the keyboard, the turntable, and (later) the controller are for playing. EQ, crossfade, cue, and gain live on the user's hardware mixer, never in the UI.
- **Real records are first-class citizens.** A scratch DJ playing alongside another DJ on real wax should get the same Dub features (FX, BPM detection, eventually waveform recognition) without recabling.
- **Reliability is the product.** This software runs in front of audiences of hundreds to thousands of paying people. A crash on stage is a career moment for the DJ. We treat reliability as the *primary* feature, ahead of every other capability. Test-driven development is not optional, not negotiable, and not deferable. See §2.2.

---

## 2. Target user

Primary persona: **scratch / urban / sound-system DJ** with the following profile:

- Plays on real turntables with control vinyl (Serato CV02 / Traktor MK2 timecode).
- Uses an **external hardware mixer** (Rane TTM/MP, DJM-S, Numark Scratch, vintage Vestax, etc.) for cueing, EQ, filters, and crossfading.
- Often plays a mix of real records and digital files.
- Wants software that gets out of the way and gives back what hardware can't: large library access, smart utility FX, quick sample-throws, loops.

Secondary persona: **vinyl enthusiast / home DJ** wanting to play their digital library with timecode vinyl on a small home setup.

### Non-goals (audience)

- Club/festival DJs running CDJs in sync. (rekordbox/Engine territory.)
- Controller-only DJs. (Serato/rekordbox territory.)
- Producers/remixers needing stems, AI separation, or arrangement tools.
- Streamers/influencers needing OBS integration, video sync, etc.

---

## 2.1 Foundational technical decisions & rationale

### Why Rust (not C++)

We chose Rust as the language of the engine. The reasoning is not marketing.

1. **Performance parity with C++.** Rust has no GC, no runtime, no ARC. The audio render callback compiles to the same kind of machine code C++ would produce. Both languages depend on the same OS audio APIs (CoreAudio, ASIO) for their latency floor.
2. **Memory safety without runtime cost.** The borrow checker eliminates entire classes of bugs (use-after-free, data races, iterator invalidation) at compile time. For a real-time audio codebase maintained by a small team, this is decisive.
3. **The audio thread can be statically guarded.** Rust's type system + the `assert_no_alloc` crate + a custom allocator allow us to *prove* that no allocation, lock, or syscall happens inside the render callback — something C++ requires discipline and code review to enforce.
4. **Better tooling.** `cargo` (build + dependency + test + bench in one tool) vs. CMake/Conan/Catch2/Google Bench. `clippy` is a real linter; C++ has no comparable mainstream equivalent.
5. **Better Apple FFI in 2026.** UniFFI generates safe Swift bindings from a Rust crate; `swift-bridge` and `cargo-xcode` integrate with Xcode. C++ ↔ Swift interop is younger and rougher.
6. **Production-grade audio ecosystem.** `coreaudio-rs`, `symphonia`, `rubato`, `ringbuf`, `assert_no_alloc`, `aubio-rs` — all mature, all maintained.
7. **One developer can maintain it.** This is the load-bearing reason. Rust's compile-time guarantees compound; the project gets *easier* to evolve as it grows, which is the opposite of how C++ codebases age.

C++ would only be the right call if (a) we leveraged a large existing C++ codebase, (b) JUCE was a hard requirement, or (c) we hired from a senior C++ DSP talent pool. None apply.

We *do* link to C/C++ libraries (Rubber Band, Aubio, optionally Chromaprint) via FFI. This is fine; FFI is one-way and well-isolated.

### Performance philosophy

> **We optimize until the cost is no longer audibly justified, then we stop.**

Specifically:

- **Best-in-class at the same buffer size as Serato/Traktor.** Not 10× lower latency at 5× the CPU.
- **CPU headroom is a feature.** Users want to run a browser, Slack, OBS alongside Dub. We target < 25 % of one P-core under heavy use (2 decks + key lock + FX + sampler).
- **No micro-optimization theatre.** SIMD where it measurably helps, plain code elsewhere. Profile first.
- **Battery is a constraint.** A scratch DJ on tour using a MacBook Air should not see Dub drain the battery faster than a video call.
- **Marketing claims must hold under real conditions.** "Sub-5 ms latency" with no asterisks. If it requires hog mode + closing other apps + a specific interface, we say so.

---

## 2.2 Quality, testing & reliability — first principle

> **A crash on stage is a career moment for the DJ.** This software is used in front of audiences of hundreds to thousands of paying people. Reliability is our primary feature. Every other priority — features, performance, UI polish — is subordinate to "it works, every time, in front of a crowd."

This section is binding for every line of code in the project. We accept the cost knowingly, because the alternative is unacceptable.

### 2.2.0 Staged rigor — pragmatism before users, rigor before stable

**This is the load-bearing pragmatism of this section.** Until Dub has real users on real gigs, the most stringent reliability gates ("100 cumulative gig-hours zero-crash") are *theatre* — there is nobody to accumulate gig-hours from, and 100 % of pre-alpha-tester crashes are caught and fixed by the developer in seconds. Spending 20–30 % of velocity on those gates pre-users delays the day there *are* users.

We therefore stage the rigor in three phases. **All ground rules are in from M0** because they are cheap to set up and expensive to retrofit. **The release-blocking gates activate progressively** as the project earns the right to enforce them.

| Phase | Trigger | Rules in effect | Gates |
|---|---|---|---|
| **Phase A — Pre-Alpha** (M0 → M17) | No external users yet | TDD discipline (§2.2.1), test taxonomy (§2.2.2), RT-safety enforcement (§2.2.3), parser fuzzing (§2.2.5), CI green required to merge to main, branch protection, snapshot tests for UI | None for "release"; "release" means "the developer dogfoods the latest main daily" |
| **Phase B — Alpha** (M18) | Invite-only, 3–5 trusted DJs run on real gigs | Phase A + soak harness in nightly CI + manual rig checklist (§2.2.10) signed off before each alpha cut + 24h hotfix discipline for crashes reported by alpha testers | "Cut alpha" gated only by the manual rig checklist |
| **Phase C — Beta and Stable** (M19, M20+) | Public opt-in beta, then stable | Phase B + full §2.2.6 SLOs including 100 cumulative beta-gig-hours zero-crash + zero fuzz crashes in last 7 days + no benchmark regressions | Stable release gated by full §2.2.6 SLOs |

**Practical implication for Phase A:**

- Tests are written. CI is green. RT-safety is enforced. Fuzzing runs. **All the framework is operational.**
- We do **NOT** wait for a soak test to merge a feature.
- We do **NOT** require gig-hours to ship a Phase-A "release" — there's no release in this phase, just `main`.
- We **DO** enforce: every PR has tests; every PR is RT-safe; CI is green; no hand-merging around CI.
- The author dogfoods on their own setup daily. Bugs found in dogfooding go through the same fix-test-merge loop as future user-reported bugs.

**Crossing into Phase B happens at M17 / M18** (Polish + Alpha). At this point we activate the soak nightly + manual rig checklist + hotfix discipline.

**Crossing into Phase C happens at M19** (public Beta). At this point the gig-hour gate, the public-beta hotfix turnaround, and the full §2.2.6 SLOs become release-blocking for stable.

**Why this works:** the cost of TDD discipline + RT-safety + fuzzing is high in *culture* but low in *time* once it's set up. The cost of soak tests, manual rig checklists, gig-hour gates, and 24h hotfix discipline is high in *time*. We pay the culture cost from day one (cheap, shapes the codebase), and we defer the time cost until it's earned (expensive, but only meaningful with users).

This makes the engineering bar *higher* for v1.0 stable than for any other phase, and *appropriately matched* to the project's stage at every step before that.

### 2.2.1 Test-driven development (TDD) is the default

For all Rust code (engine, DSP, parsers, library, controllers, FFI surface):

1. **Write a failing test first.** Then write the minimum code to pass it. Then refactor. The standard TDD loop, applied uncompromisingly.
2. **Tests live next to source** (`#[cfg(test)] mod tests` blocks for unit tests; `tests/` directories for integration tests).
3. **Coverage target: ≥ 85 %** of branches in non-trivial modules (verified via `cargo-llvm-cov` in CI). UI code is exempt; see §2.2.4.
4. **No PR is mergeable without tests** for the changed behavior. Reviewer rejects PRs that change behavior without a corresponding test.

Carve-outs (where TDD doesn't apply):

- **SwiftUI views** — use snapshot tests (§2.2.4).
- **CoreAudio I/O proc setup** — physical hardware required; covered by manual checklist.
- **Throwaway exploratory spikes** — explicitly marked `experimental/` and never merged to main.

### 2.2.2 Test taxonomy

Every commit pushes through this stack:

| Type | What it tests | Scope | Run when |
|---|---|---|---|
| **Unit** | Pure functions, small modules | All Rust modules | Per commit (every push, every PR) |
| **Property** (`proptest`) | Invariants over generated input | State machines, DSP buffer math, parsers, timecode decoder | Per commit |
| **Golden** | DSP regression — hash a reference output, compare | All DSP stages, Rubber Band integration, resampler, FX | Per commit |
| **Integration** | Multi-crate orchestration via offline render | Full engine pipelines (load track → render N seconds with synthetic input → assert output) | Per commit |
| **RT-safety** | `assert_no_alloc` engaged during render call | Audio thread code path | Per commit (**hard gate**) |
| **Fuzz** (`cargo-fuzz`) | Malformed input does not crash | All file-format parsers (NML, GEOB, DB6, ID3, ALAC, FLAC, MP3 frame headers) | Continuous (dedicated fuzzer host or CI nightly) |
| **Soak** | Long-running stability | 1+ hour offline playback with synthetic timecode and FX rotation | CI nightly |
| **Performance** | Latency / CPU regression | Microbenchmarks (`criterion`) and macro RT-render benchmarks | Per commit (warn on regression > 5 %, fail on > 15 %) |
| **Snapshot** | UI hasn't changed unexpectedly | SwiftUI views via Swift snapshot library | Per commit on Apple side |
| **Manual rig checklist** | Real hardware behavior | Full release readiness on test rig | Pre-release only |

### 2.2.3 RT-safety is the hardest gate

The audio thread is special. A single allocation, mutex, or syscall on it can cause an audible glitch. The CI pipeline enforces this:

1. **Compile-time hint:** the engine code path inside the render callback only takes a `&mut RealtimeContext<'_>` token. Methods that allocate, lock, or perform I/O are not implemented for `RealtimeContext`. This catches many issues at compile time.
2. **Dev-build runtime check:** `assert_no_alloc` wraps the render closure. If anything allocates during render, the test process aborts. Tests run with this engaged.
3. **Release-build runtime check:** the same wrapper exists in release builds, but on alloc it sets a flag and emits a one-shot log entry post-render rather than aborting. This protects production users while making dev-time violations loud.
4. **CI failure on any RT alloc:** any test that triggers an RT-thread alloc fails the build. No exceptions, no `#[allow]`-style escape hatches.

### 2.2.4 UI testing

SwiftUI views are tested via:

- **Snapshot tests** (`swift-snapshot-testing` library) — every PR that changes a view must include updated snapshots. Reviewer visually confirms the diff.
- **Logic-layer tests** — view models / observable state are pure Swift code, fully unit-tested.
- **No UI flow is untested** — accept lower coverage on raw view code, but the state machines that drive views are fully tested.

### 2.2.5 Fuzzing parsers — special priority

This is the highest-leverage investment for our use case. Imagine: DJ at a gig, imports a friend's library on a USB stick mid-set, file is subtly corrupted. **We must not crash.**

- Every parser (`dub-library/src/serato.rs`, `traktor.rs`, `rekordbox.rs`, `itunes.rs`, ID3 readers, audio frame parsers) has a dedicated fuzz target.
- Fuzz corpus seeded with real-world examples and known-malformed samples.
- Run continuously on a dedicated machine or CI nightly job for ≥ 30 minutes per parser.
- Any crash discovered = blocking bug, fix before any further feature work.

### 2.2.6 Reliability SLOs (Phase C — Stable releases only)

These gates apply to **stable** releases only (v1.0 stable and beyond). They do not apply to Phase A (pre-alpha development) or Phase B (alpha cuts). See §2.2.0 for the staging rationale.

Before any **stable** release, all of the following hold:

1. **Zero crashes** in the last 100 cumulative hours of beta-tester gig-time.
2. **Zero xruns** in a 60-minute soak test at 64-sample buffer on the reference rig (M2 Air + SL3 or Audio 6).
3. **Zero RT-thread allocations** detected in soak test.
4. **Zero parser fuzz-discovered crashes** in the last 7 days of fuzzing.
5. **No regression** in latency or CPU benchmarks vs. previous stable.
6. **Manual rig checklist** signed off by at least one human on real hardware (see §2.2.10).
7. **All CI tests green on `main`** for ≥ 24 h before tag.

**Phase A and Phase B equivalents** (much weaker, intentionally):

- Phase A: CI green to merge. No release exists; `main` is the rolling target.
- Phase B (alpha cuts): manual rig checklist signed off + soak test green. No gig-hour requirement (alpha *generates* gig-hours).

### 2.2.7 Production observability (without telemetry-creep)

DJs hate phoning home. We respect that.

- **Local crash dumps**: stored in `~/Library/Logs/Dub/crashes/` automatically. Never uploaded automatically.
- **Local verbose log**: `~/Library/Logs/Dub/session.log` with a configurable retention. Includes audio-engine events (xruns, source mode changes, FX engagements, errors) but no PII.
- **Optional opt-in crash reporting** (Sentry or similar): off by default. Explicit toggle in preferences. When enabled, redacts file paths and library content.
- **Performance Mode** (preference): when enabled, Dub disables its own non-essential background work *and* asks the OS to enable Do Not Disturb (via the macOS Focus API). Mid-set notifications are disabled; Spotlight scope can be reduced via a one-click button (best effort).

### 2.2.8 Release process (staged)

Mapped to the rigor phases in §2.2.0:

| Stage | Phase | Channel | Audience | Gates |
|---|---|---|---|---|
| **Internal** | A | author's machine | author only | CI green |
| **Dev** | A | optional `dev` GitHub Releases channel | author + ad-hoc collaborators | CI green |
| **Alpha** | B | private GitHub Releases | ~3–5 invited DJs running on real gigs | CI green + soak nightly + manual rig checklist (§2.2.10) signed off |
| **Beta** | C | public opt-in GitHub Releases (marked beta) | community | All Phase B gates + feature freeze + accumulating gig-hours toward §2.2.6 |
| **Stable** | C | public GitHub Releases | everyone | All §2.2.6 SLOs met |

**Hotfix discipline (Phase B onward):** any crash bug reported against alpha, beta, or stable triggers a hotfix branch within **24 hours**. No exceptions. We may temporarily yank an unstable release rather than let it linger broken. **Phase A has no hotfix obligation** — there is no release to fix.

### 2.2.9 What this is NOT

Honest about the limits:

- **Not a "zero bugs ever" promise.** That's impossible. We promise: zero **show-stopping** bugs in stable releases (crash, freeze, audio dropout > 1 second, library corruption, data loss).
- **Not 100 % test coverage.** Coverage is a proxy; the real goal is meaningful tests for meaningful behavior. UI rendering code can hover at 30–40 % coverage without concern as long as the state machines underneath are fully tested.
- **Not a substitute for real-world testing.** CI tests prove the code works *in the simulator*. The manual rig checklist (§2.2.8) and gig-time soak (§2.2.6) prove it works in the world.

### 2.2.10 Items in the Manual Rig Checklist

Every release runs through this on real hardware. All must pass.

1. Cold launch with no library configured → first-run experience appears, no crash.
2. Import a 50k-track Serato library → completes within 60 s, no crash, no missing metadata.
3. Import a Traktor library, a rekordbox library, an iTunes XML, all sequentially → no conflicts.
4. Plug in SL3, route control vinyl to inputs 1/2 + 3/4, both decks under timecode → no calibration glitches.
5. Same with Audio 6 + Traktor MK2 vinyl.
6. Play a track for 30 minutes with timecode active, key lock on, occasional scratching → zero xruns, no audible glitches.
7. Engage Echo-Out 50 times in a row → no degradation, tail decays cleanly each time.
8. Switch a deck to Thru: Direct, scratch a real record → audio passes through, no software involvement.
9. Switch the same deck to Thru: Processed, engage Echo-Out → FX applies to real record audio.
10. Auto-BPM on Thru locks within 15 s on a 4/4 hip-hop record, a reggae record, and a dnb record.
11. Unplug the audio interface mid-playback → engine stops cleanly, UI shows "interface lost", reconnect → playback resumes.
12. Run 60 minutes continuous use (any combination of features), close the app cleanly → no crash, no orphan processes.
13. Open the macOS sleep/wake cycle while Dub is running → audio resumes without artifacts on wake.
14. Run with a deliberately corrupted Serato library file → graceful error, app does not crash.

---

## 3. Platforms & roadmap

| Version | Platforms | Headline additions |
|---------|-----------|-------------------|
| **v1.0** | macOS (Apple Silicon + Intel) | Timecode vinyl, 2-deck, sampler, smart FX, library import |
| **v1.x** | macOS | Polishing, controller/mapping support if requested by community |
| **v2.0** | macOS + Windows | **Phase support**, hot cues, recording, Windows port (ASIO/WASAPI) |
| **v3.0** | macOS + Windows + iPadOS | iOS/iPadOS port (USB-C iPads), cloud library sync |

**v1 is macOS only.** No iOS, no Windows, no Phase. Time-to-first-release is the constraint.

---

## 4. Audio architecture & performance targets

### 4.1 Performance targets (hard requirements)

| Metric | Target | Test rig |
|---|---|---|
| Round-trip latency | **< 5 ms** at 48 kHz / 64-sample buffer | M-series Mac, class-compliant USB interface (Rane TWELVE / NI Audio 10) |
| Round-trip latency | **< 8 ms** at 48 kHz / 128-sample buffer | Same |
| xrun rate | **0** in 60-min scratch session, 64-sample buffer | M2 Air with browser/email open |
| Timecode-to-audio response | **< 10 ms** total (input → DSP → output) | Same |
| CPU @ idle (1 deck playing, no FX) | **< 5 %** of one P-core | M2 Air |
| CPU @ stress (2 decks, key lock, echo-out, sampler firing) | **< 25 %** of one P-core | M2 Air |
| Cold start to ready-to-play | **< 2 s** | 50k-track library |

### 4.2 Audio engine principles

- **Internal sample format:** 32-bit float, interleaved or planar per stage.
- **Internal sample rate:** track device native; never silently resample. Resample at file→engine boundary only when track SR ≠ device SR (using `rubato` SincFixedOut). Note that **bitrate (e.g. MP3 320 kbps) is unrelated to sample rate** — bitrate is the compressed file's bandwidth; sample rate is in the file's PCM header. Most DJ MP3s are 44.1 kHz (CD-ancestry); a growing minority are 48 kHz (DAW/streaming exports). The engine doesn't pick a sample rate; it follows the device.
- **Sample-rate UI policy (deferred to v1.x):** We let the user run any device rate the OS allows in v1, including 96 kHz and 192 kHz. **Open question for the audio settings UI**: should we soft-warn or hide rates above 96 kHz? At 192 kHz the engine does 4× the work for no audible benefit on music playback, and several mid-range DACs exhibit IM distortion above 96 kHz. Defer the decision until we have a settings UI; this note is the reminder.
- **Audio thread is sacred.** No allocations, no locks, no syscalls, no logging, no file I/O, no Mutex, no Box, no Vec growth. Enforced by:
  - `assert_no_alloc` crate in dev/test
  - Custom allocator that aborts on RT-thread alloc in CI builds
  - A `RealtimeContext` lifetime token type that gates which APIs are callable in the render callback
- **Pre-allocation everywhere:** all buffers (file ring, decoder scratch, FX scratch, sampler voices) sized at session start.
- **Lock-free communication:** UI ↔ audio uses SPSC ring buffers (`ringbuf`) for command/event passing and atomic snapshots for read-only state (transport position, peak meters).
- **Sample-accurate scheduling:** all transport changes timestamped in samples; no millisecond rounding.

### 4.3 Audio I/O strategy

- **v1:** CoreAudio HAL via `coreaudio-rs`, not `cpal`. We need direct device-property listeners and hog mode opt-in for ultra-low-latency mode.
- **AVAudioEngine** is **not** used (too high level, hides the IO proc).
- We will support:
  - Default output device (built-in)
  - External multi-channel USB interfaces (the primary case)
- **Aggregate devices not officially supported in v1.** They'll work if the OS configures them, but we don't test against them and we don't expose UI for assembling them. Real DJs plug one interface in.
- We require the user to assign **per-deck output pairs** in External Mixer mode (see §5.3).

### 4.4 Track loading & in-memory audio

> **All audio for loaded tracks lives in RAM.** No per-block disk streaming. This is the simplest design that supports the full bidirectional, sample-accurate, scratchable, rewindable, instant-seek behavior our target users demand.

- File formats supported: MP3, WAV, AIFF, FLAC, ALAC, M4A (AAC).
- Decoders: `symphonia` for everything.
- **On load**: decode the entire track into a `Arc<[f32]>` (32-bit float, planar stereo). A 6-minute FLAC at 48 kHz stereo = ~140 MB at f32 (or we can store as f32 throughout the engine and accept the size). Two loaded decks = ~280 MB. Acceptable on any modern Mac.
- **Rationale**: scratch DJs and reggae/dnb DJs perform manual rewinds (DJ holds the pitch slider full negative, sometimes spinning the platter back by hand at high speed for 30+ seconds — common gesture for the "rewind!" moment in dnb and dub). Disk streaming with bidirectional ring buffers can keep up *most* of the time but introduces edge cases where the worker thread can't refill fast enough on a large backwards seek. In-memory eliminates the entire class of problem and makes forward and backward playback **fundamentally identical** at the engine level.
- **Forward and backward playback are byte-for-byte symmetric.** The audio engine reads a `f32` slice with a sample-accurate floating-point playhead and direction-agnostic resampler. There is no "forward path" and "backward path" — there is only "read sample at offset X with rate R," where R can be any real number including negative.
- **Memory budget**: a hard ceiling of 1 GB total audio cache across decks + sampler. Tracks loaded but not on a deck are LRU-evicted. We never load 96 kHz/24-bit material at full resolution if it would breach the budget — we downsample at load to engine SR.
- **Pre-render waveform during load**: as the decoder fills the buffer, we compute multi-resolution peak data for the waveform display. Track is "ready to play" before decode finishes (we fade in playback availability when 5 seconds of head are decoded), but full bidirectional access requires full decode (≤ 1 s for typical track on Apple Silicon).
- **Sampler / Quick Scratch slot audio** loaded the same way, persistently held in RAM (samples are small).

---

## 5. Input & control architecture

### 5.1 Per-deck source modes

Each deck has a **source** that drives its audio output. The user does not normally choose this — Dub detects what the deck is doing and switches automatically (§5.1.1). Manual override is available in preferences.

| Mode | Behavior | v1 |
|---|---|---|
| **Internal** | Software transport controls. Debug-only in release builds. | Yes (debug menu) |
| **Timecode** | Position driven by Serato or Traktor control vinyl. File from library is the audio source. **Relative mode only in v1** — needle drop is ignored, only motion is tracked. | **Yes — primary v1 mode** |
| **Thru: Direct** | Audio interface input on this deck's input pair routed directly to its output pair, **zero software latency**, no FX possible. Used when the user is playing a real record on the deck and no FX are engaged. | **Yes — v1, automatic** |
| **Thru: Processed** | Audio interface input routed *through* the engine. FX apply. Auto-BPM runs on the input. Adds one buffer of latency (~1.3 ms @ 64 samples). Auto-engaged when any FX activates on a Thru deck. | **Yes — v1, automatic** |
| **Phase** | Position driven by Phase wireless (file from library is audio source). | v2 |

**Source switching is live and seamless.** Mode transitions use a 5 ms equal-power crossfade. A DJ swapping a timecode record for a real record mid-set should not hear any artifact from Dub during the swap.

#### 5.1.1 Automatic source detection

Dub continuously analyzes each deck's audio input and decides which mode to engage, without user intervention.

**Algorithm:**

A short-window classifier (running on a worker thread, NOT the audio thread) examines a 250 ms sliding window of the deck's input audio:

- **Spectral test**: timecode signal has a dominant tone at 1 kHz with harmonics at 2 kHz (Serato CV02) or 2 kHz fundamental (Traktor). Compute `power_at_1k / total_power` and `power_at_2k / total_power`. Timecode: ratio > 0.6. Music: ratio < 0.1.
- **LFSR/phase test**: timecode has a deterministic phase relationship between L and R channels (the LFSR-modulated absolute position). We attempt to lock onto the LFSR. Lock acquired = high confidence timecode. Lock failed for 500 ms = high confidence non-timecode.
- **Silence test**: input below noise floor (-60 dBFS RMS for 250 ms) → "no signal."

**State machine per deck:**

```
              ┌──────────────────────────────────────┐
              │             SILENT                   │ ← all decks start here
              │  (no input above noise floor)        │
              └──────────────────────────────────────┘
                  │ signal detected
                  ▼
              ┌──────────────────────────────────────┐
              │           DETECTING                  │
              │  (250–500 ms classification window)  │
              └──────────────────────────────────────┘
                  │ timecode      │ music
                  ▼               ▼
              ┌─────────┐     ┌─────────────────┐
              │TIMECODE │     │  THRU: DIRECT   │
              └─────────┘     └─────────────────┘
                                       │ FX engaged on this deck
                                       ▼
                                ┌─────────────────┐
                                │ THRU: PROCESSED │
                                └─────────────────┘
```

**Switch rules:**

- Music → Timecode requires 500 ms of clean LFSR lock.
- Timecode → Music requires 500 ms of LFSR lock failure AND clear non-timecode spectral signature.
- During active scratching (timecode lock plus motion), **detection is frozen** — we never switch out of Timecode mid-scratch. Even if signal degrades briefly (dust, rough handling), Stickiness (§5.4) holds the mode.
- Silence (needle lifted) does not trigger a switch. Mode is held until next signal arrives, then re-evaluated.

**User-facing behavior:**

The detected mode is shown in the deck's status indicator, but **no menus or buttons require the user to think about source switching**. They drop a needle, Dub figures it out. UI shows: 🎛 Timecode • 🎵 Real Record • 🎚 FX Active. State changes animate in/out so the DJ can see what's happening at a glance.

**Why this works:** scratch-DJ workflows already involve dropping different records on the same turntable mid-set. Dub respecting this physical reality — instead of forcing a menu pick — *is* the headline UX win for this category of user.

**Confidence and edge cases:**

- Very low-volume music (e.g., a quiet intro under -40 dBFS) might fail spectral classification. Behavior: stay in last-known mode. Won't engage Thru-mode FX until the music is loud enough to classify, but won't accidentally trigger Timecode either.
- A record cut with a 1 kHz tone in the music (rare but exists, e.g., test tones, some experimental records) might false-positive timecode. Mitigation: LFSR lock is the second gate; pure 1 kHz won't lock the LFSR. Both gates together have very low false-positive rate.
- Manual override always available in preferences for users who want explicit control.

**Implementation note:** the classifier runs **off the audio thread** (worker thread, ~250 ms cadence). It informs a "desired mode" for the audio thread to honor on the next block boundary, with crossfade. RT-thread is never blocked by classification.

**Manual override:** preferences include a "Source mode override" per deck: `Auto` (default) / `Timecode` / `Thru` / `Internal` (debug). Pro DJs may pin the mode for predictability.

### 5.2 Thru Mode (real records through the software)

Thru Mode is a **headline feature** of v1 and a key differentiator. Modeled on Traktor's "Thru" but extended.

#### 5.2.1 Routing

- The user's real turntable plugs into the audio interface's input pair (e.g. SL3 inputs A or B, Audio 6 inputs 1/2 or 3/4).
- In **Thru: Direct**: the interface's hardware-monitor or driver-level routing sends input → output with zero software involvement. We expose a UI toggle that triggers this via the interface's control surface (where supported via CoreAudio AU monitoring) or instructs the user to enable hardware monitoring.
- In **Thru: Processed**: input pair → engine bus → FX chain → output pair. The engine treats the live audio identically to file-decoded audio for everything downstream of the source.

#### 5.2.2 Direct vs Processed — fully automatic

The user **does not pick** between Thru: Direct and Thru: Processed. The engine picks.

- **No FX active on the deck** → Thru: Direct (zero software latency, audio passes through driver-level routing).
- **Any FX engaged on the deck** → Thru: Processed (audio passes through engine, FX apply, ~1.3 ms added latency at 64-sample buffer).

When all FX disengage, the deck holds Processed for 500 ms (to absorb FX-tap-twice patterns) then drops back to Direct. Crossfade is 5 ms equal-power; no audible artifact at any transition.

**No UI control for this.** It just happens. This is consistent with "No Mouse DJ Ever" — the DJ engages FX, Dub does the routing.

#### 5.2.3 Auto-BPM on live audio

A Thru deck runs continuous tempo tracking on its input:

- Algorithm: `aubio` tempo object (LGPL, dynamic-link), which uses spectral flux + complex-domain onset detection + comb-filter tempo estimation.
- Stabilization: ~10–15 s of music for a confident reading.
- UI: BPM display with a **confidence indicator** (3-state: searching / tentative / locked). Tentative readings shown italicized.
- The detected BPM feeds the FX (echo-out divisions, loop length references) so the user can apply tempo-synced FX to a real record.

#### 5.2.4 Live waveform capture (v1)

While a Thru deck is active, the engine accumulates a **multi-resolution peak waveform** of the input audio in real time. This waveform is rendered live as the record plays. When the record finishes (or the user disengages Thru), the captured waveform is held in memory and optionally persisted (see §5.2.5).

#### 5.2.5 Audio fingerprint recognition (v1.1 — *not v1.0*)

The differentiating feature, planned for v1.1.

- As a Thru deck plays, the engine continuously computes a rolling **audio fingerprint** over a 5-second window. Algorithm: Chromaprint (LGPL, FFI) or a clean-room implementation if licensing forces it.
- Fingerprints are matched against a local database of records the user has played before.
- **First play** of a record: no match. Engine creates a new entry, captures the waveform, captures the auto-BPM, captures the fingerprint, persists everything to `library.sqlite` keyed by fingerprint hash. The user can optionally tag the entry with title/artist (or it stays anonymous).
- **Subsequent plays**: fingerprint matches within 5–10 s of needle-drop. Engine loads the saved waveform, beatgrid, and BPM. UI animates: waveform "fades in" as recognition completes, BPM stops searching and locks, beatgrid overlays appear. Effects that need beat-sync become available.
- **Robustness considerations**: pitch variation (turntable ±8 %), surface noise, mixer EQ, room sound. Chromaprint is designed for exactly this and handles ±10 % pitch reliably. Our fingerprint hashing is pitch-tolerant (use the Shazam-style constellation approach over Chromaprint's chroma if Chromaprint proves insufficient).

**v1.0 ships:** Thru routing (Direct + Processed), auto-mode switching, auto-BPM, **live waveform capture and rendering** (in-memory only — not persisted, no recognition). v1.0 already shows the user "this record I'm playing has BPM 92 and here's its waveform as it plays." That alone is unique.

**v1.1 adds:** persistence, fingerprinting, recognition, beatgrid storage. This is the magic.

#### 5.2.6 Constraints

- Thru: Processed adds **one buffer of round-trip latency** (input + output), e.g. ~2.7 ms at 64 samples / 48 kHz. This is unavoidable physics.
- The user must drop the needle near the start of a record for waveform capture to be meaningful. We do not "stitch" partial captures across plays in v1; that's a v1.x consideration.
- Auto-BPM cannot detect tempo on solo a-cappella or beat-less ambient sections. UI must communicate "no beat detected" honestly, not lie with a fake number.

### 5.3 External mixer mode (only mode in v1)

- **Required:** audio interface with ≥ 4 outputs (2 stereo pairs) AND ≥ 4 inputs if Thru mode is used.
- Per-deck output assignment: Deck A → Out 1/2, Deck B → Out 3/4 (configurable).
- Per-deck input assignment (for Thru and timecode): Deck A → In 1/2, Deck B → In 3/4 (configurable).
- **No software cue/preview channel.** Cueing is the hardware mixer's job.
- **No software crossfader.** External mixer's crossfader is the only crossfader.
- **No software EQ.** External mixer's EQ is the only EQ.
- **No mouse-driven mixing of any kind.** Per the "No Mouse DJ Ever" principle.
- **Smart FX (echo-out, dub siren) are inserted into the per-deck output bus pre-output.**

The internal debug mixer (§5.6) is **not** the same as a user-facing internal mixer mode. v1 ships only External Mixer mode in the UI; the debug mixer is dev-only.

### 5.4 Timecode subsystem

**Supported control records:**
- Serato Control Vinyl CV02 (1 kHz reference + LFSR position)
- Traktor MK2 Timecode

**Both supported in v1.** User selects which in preferences; auto-detect attempted on input.

**Required behaviors:**
- 33⅓ and 45 RPM detection (auto)
- **Relative mode only in v1.** Needle drops are ignored; only motion is tracked. Absolute mode is deferred — almost no scratch DJ uses absolute mode in practice. Skipping it cuts calibration UI complexity significantly.
- **Pitch range** as wide as the user's turntable (typically ±8 / ±16 / ±50 %)
- Slow-down to stop: tracks pitch through zero cleanly without click/glitch
- Backspin: tracks negative pitch with no audible artifact up to the resampler's limits
- **Drop-out detection (Stickiness)**: if signal quality degrades (dust, scratch, end of run-out groove), hold last known velocity for a grace window (default 250 ms), then engage internal playback at the last pitch until signal returns.
- **Through groove handling**: detect run-out and hold position
- **Calibration UI**: show signal scope, S/N ratio, RPM detection, pitch readout. Live calibration with vinyl spinning. **No A/B side toggle, no abs/rel mode selector** — relative mode is universal.

**Algorithm:** port the xwax decoder (well-understood, ~2k lines C, BSD-licensed). Our port lives in `crates/dub-timecode/`. Both Serato and Traktor LFSR tables included.

**Absolute mode is deferred to v1.x or later** if user demand emerges. Most likely, never.

### 5.5 Keyboard input

First-class keyboard mapping. v1 default bindings (configurable):

| Key | Action |
|---|---|
| `Q W E R` | Quick Scratch slots 1–4 (toggle, see §7.2) |
| `A S D F` | Sampler slots 1–4 (one-shot trigger) |
| `Z X` | Loop In / Loop Out (Deck A) |
| `, .` | Loop In / Loop Out (Deck B) |
| `1 2 3 4` | Loop length 1/2/4/8 beats |
| `Tab` | Switch keyboard focus deck A↔B |
| `Space` | Play/pause focused deck (debug / internal mode only) |
| `[ ]` | Echo-Out tap (focused deck) |
| `\` | Dub Siren toggle |
| `←→ ↑↓` | Library navigation |
| `Enter` | Load track to focused deck |

User can rebind any action. Key binding profiles saved per-user.

### 5.6 HID / MIDI controllers

**Out of scope for v1.** Scratch DJs use external mixers, not controllers. The codebase will include the abstraction (`crates/dub-controller/`) so this is additive in v1.x without rework.

When controllers do land (v1.x / v2), they map to the **same** external-mixer mental model: a controller represents a turntable + its mixer channel. The controller's mixer section (faders, EQ) controls the user's *external* mixer if there is one, or — only if the user explicitly enables it — a software mixer. Software mixer is **not** a v1/v2 commitment; it's a v3 question.

### 5.7 Debug internal mixer

For testing forward/backward play, scratch sample feel, FX behavior without a turntable rig. Behind a **Debug menu** (hidden in release builds unless `--dev` flag).

Includes: per-deck play/pause/scrub-bar, master gain, channel gain, primitive crossfader, master output. **No EQ, no filter, no FX UI parity** — just enough to verify the engine.

---

## 6. Feature set — v1

### 6.1 Core transport (per deck)

- Timecode-driven play/scrub/scratch
- Slip mode (always on for timecode mode; configurable for internal)
- **Key lock** (master tempo) toggle, via Rubber Band — see §6.1.1 for scratch-aware auto-bypass
- **Pitch range** display (informational; pitch is set by the turntable, not software)
- Auto **gain trim** based on track loudness (LUFS-I or peak normalization, user choice)

#### 6.1.1 Key Lock with scratch-aware auto-bypass

Rubber Band cannot handle the rate excursions of scratching (rapid back/forward, very high `|rate|`, sub-millisecond rate changes). When Key Lock is enabled, the engine **automatically bypasses** the time-stretcher during scratching and re-engages it when the playhead settles, transparently to the user.

**Decision logic** (runs every audio block):

- Compute current playback rate `r` (samples-per-output-sample) and rate-of-change `dr/dt`.
- **Bypass** Rubber Band when ANY of:
  - `|r|` > 1.5× (scratching at speed)
  - `|dr/dt|` > threshold (rapid rate change, e.g. needle just hit)
  - `r` < 0.05 or `r` < 0 (near-stop or reverse)
- **Re-engage** Rubber Band when ALL of:
  - `|r - r_user|` < 0.1 (rate has settled near user's set tempo, where `r_user` is the turntable's current pitch slider position as inferred from timecode)
  - This condition has held for ≥ 200 ms

**Crossfade**: bypass → engaged transition uses a 20–30 ms equal-power crossfade between the resampler-only signal and the Rubber Band signal to avoid clicks. Engaged → bypass is instantaneous (drop the Rubber Band stage; resampler picks up the same input pointer).

**UI**: a "Key Lock" indicator with two states:
- **Green / on** — Rubber Band currently active (deck is playing in tempo).
- **Dim green / standby** — Rubber Band bypassed for now (user is scratching), will re-engage automatically.

User does not see or configure thresholds. It just works.

### 6.2 Looping

- Manual Loop In / Loop Out (sample-accurate)
- Auto-loop: 1/8, 1/4, 1/2, 1, 2, 4, 8, 16, 32 beats
- Loop halve / double
- Loop relocation (move loop while active)
- Reloop / exit
- Loops respect timecode position when looping (loop is in-engine, not driven by needle position)

### 6.3 Smart FX (per deck, mutually compatible)

**Echo-Out**
- Hold-to-engage button (or keyboard tap with sustain)
- Captures the last N beats of the deck's output into a delay line, freezes the deck's main signal, plays the captured loop with feedback decay.
- Parameters: divisions (1/4, 1/2, 1, 2, 4 beats), feedback (default 60 %), filter (low-pass, default 8 kHz)
- One-button workflow: tap and hold → echo-out engages; release → tail decays naturally; deck's actual playback continues where it would have been (slip-aware).

**Dub Siren**
- Classic dub-siren synth: oscillator (sine/saw/square), envelope, slap-back delay, optional spring reverb modeling
- Trigger via keyboard or on-screen button
- Pitch-bend mod wheel via mouse drag or trackpad
- Routed to a configurable output (default: Deck A output, but should support a dedicated "FX bus" output for users with mixer aux returns — **decision deferred to v1.1** unless trivial)

### 6.4 Sampler / Quick Scratch (see §7 for detail)

### 6.5 Library

See §8 for detail.

### 6.6 Out of scope for v1 (deferred)

- Hot cues → **v2**
- Recording → **v2**
- Streaming services (Tidal, Beatport, SoundCloud) → **v2+ or never**
- Phase → **v2**
- HID controllers → **v1.x or v2**
- Audio fingerprint recognition + persistent waveform learning → **v1.1** (post-launch)
- Apple Developer ID / notarization / auto-update → **v1.1**
- Stems / AI separation → **never**
- Video / OBS → **never**
- Software mixer / internal mixing mode (user-facing) → **never in v1/v2** (philosophy: external mixer is the product). v3 may reconsider for controller-only users.
- Mouse-driven mixing of any kind → **never** ("No Mouse DJ Ever")
- Cloud sync → **v3+**

---

## 7. Sampler, Quick Scratch & Instant Doubles

Dub has **three distinct sample/track-throw mechanisms** — each solves a different problem.

### 7.1 Sampler (one-shot, additive)

Classic DJ sampler. v1: **4 slots**.

- One-shot trigger (key press → sample plays through and ends).
- Per-slot: gain, output assignment (default: master out / Deck A's output bus, configurable).
- Loadable via drag-and-drop from finder or library, or right-click "Assign to slot".
- Output is **additive** — sample plays *over* whatever Deck A/B are currently playing. Mixed into the deck's output bus, post-FX.
- Use case: air horns, vocal stabs, dub-siren one-shots, "rewind!" FX, drops.

### 7.2 Quick Scratch (hotkey-bound fast load)

Hotkey-triggered fast load of a sample to a deck. Semantically identical to dragging a track from the library — just instant.

- **4 slots** in v1 (`Q W E R` by default).
- Each slot is bound to a sample file (drag-and-drop to assign, or right-click).
- Each slot has a **target deck** (default: Deck A; configurable per slot).
- **Behavior**: pressing the hotkey **loads the sample to the target deck** as if the user had loaded it from the library. The deck reset to position 0 of the new sample, plays from the start, fully under timecode control. The user can scratch the sample using their needle.
- **Returning to a track**: the user loads a track normally afterward (drag, search, or another hotkey). There is no automatic "restore previous track" feature — that proved more complicated than valuable.
- **Workflow**:
  > Deck B plays. User wants to scratch a sample over Deck B. User presses `Q`. Deck A now has the assigned sample loaded at position 0; user scratches it with their needle. When done, user drags the next track to Deck A (or presses another Quick Scratch hotkey).

This is exactly the same load operation as the library's "load to deck", just keyboard-instant. Internally it shares the same code path as a library drag-and-drop. Quick Scratch slots are persisted across sessions per user.

### 7.3 Instant Doubles

Press a hotkey → the track currently loaded on one deck is duplicated to the other deck at the current play position. Used for juggling.

- **Hotkeys:** `Cmd+→` (Deck A → Deck B), `Cmd+←` (Deck B → Deck A). User-rebindable.
- Position alignment: sample-accurate.
- Both decks remain independently controlled afterward.
- If the destination deck has a track loaded, it is replaced (no confirmation; this is a performance feature).

### 7.4 Sample bundling

v1 ships with **no bundled samples**. UI prompts user to load samples on first run with a "Browse..." button. We may publish a curated CC0/royalty-free starter pack as a separate optional download from the GitHub releases page once we've vetted samples that don't sound like a free pack. **Decision deferred until late in v1 development.**

---

## 8. Library

### 8.1 Imports

Dub reads (does not own) external libraries. **One-shot import + manual re-scan**, no continuous live sync.

| Source | Format | What we read |
|---|---|---|
| Serato | ID3 GEOB tags, `_Serato_/database V2`, crate files | Tracks, BPM, beatgrids, hot cues (stored for v2), cue points, loops, file paths, custom tags |
| Traktor | `collection.nml` (XML) | Tracks, BPM, beatgrids, cues, key, comments, gain |
| rekordbox | `master.db` (SQLite, encrypted), XML export | Tracks, BPM, beatgrids, cues, key |
| iTunes / Apple Music | `Library.xml` | Tracks, BPM (often missing), playlists, ratings |
| Lexicon DJ | Indirect — reads its rekordbox / Serato exports | As above |

**rekordbox `master.db` decryption:** the DB6 key is community-known. We will use a clean-room implementation. **Risk:** Pioneer could change the format; we degrade to XML-export-only if the DB6 path breaks.

**Format-specific gotchas documented in `docs/library-formats.md`** (to be written during implementation).

### 8.2 Dub's own data

- SQLite database for: imported tracks, dedupe (canonical track identity = audio fingerprint hash + size + duration), Dub-specific data (cues for v2, custom crates, play history, gain analysis cache), beatgrid cache (whether imported or computed), Thru-mode fingerprint index (v1.1).
- File: `~/Library/Application Support/Dub/library.sqlite`.
- We **never modify source files or source library databases**.
- Re-import is idempotent: matches by canonical identity, updates metadata, preserves Dub-only data.

### 8.3 Beatgrids

- **Prefer imported.** When importing from Serato/Traktor/rekordbox, we use their grid as authoritative.
- **Fall back to auto-detect** when no grid exists. Algorithm: `aubio` (LGPL, FFI-safe — links dynamically as required by LGPL) for onset detection + tempo estimation, plus our own beat-grid placement (anchor point + BPM, with downbeat detection from low-frequency emphasis).
- **Manual correction = tap-to-grid only in v1.** That's the entire user-facing tooling.

#### 8.3.1 Tap-to-grid (the only manual editing)

The user plays a track. They listen. When they hear a downbeat they like, they tap a key (default `G`). Dub:

1. Records the timestamp of the tap as the **downbeat anchor**.
2. Searches a window of ±200 ms around the tap for the strongest transient (snapping to the actual onset, not the user's reaction time).
3. Looks at transients within ±20 BPM of any imported BPM (or auto-detected BPM, or 120 if nothing). Picks the BPM that minimizes per-beat error over the next 8 bars.
4. Done. Grid is set.

If the user taps multiple times (`G G G G`):
- Three+ taps with consistent intervals = explicit BPM (`60 / interval_seconds`). Anchor at the **first** tap.
- Useful for very sparse-beat tracks where the auto-detect can't lock.

That's the whole feature. No drag, no halve/double, no nudge. **If the auto-detected grid is wrong, the user re-taps once and Dub re-fits everything around the new anchor.**

#### 8.3.2 Drift on non-quantized recordings

Honest disclosure: tracks that drift (vintage soul cuts, live-played reggae bands, breakbeat samples cut from drummer-played records) **will not stay grid-locked over a long mix**. A grid that's perfect at the intro will be 50–100 ms off by minute 5.

- **Indicator**: when Dub auto-detects that a track's transients deviate > 5 % from the fitted grid over its length, the deck shows a small ⚠ "May drift" indicator.
- **The mitigation is the DJ's hand on the pitch slider** — exactly as it has always been. We don't pretend otherwise.
- Multi-anchor flex grid (à la Ableton's "warp markers") is a v2 consideration, gated by user demand.

### 8.4 Track analysis

On import, on-demand, or background:
- Waveform (multi-resolution overview + zoom) — pre-rendered, cached on disk in `~/Library/Caches/Dub/waveforms/{hash}.wf`
- Loudness (LUFS-I) for auto-gain
- Beatgrid (if not imported)
- Key detection — **deferred to v1.x** (use existing key from imports if available)

### 8.5 Browser UI

- Source tree (left): All Tracks, Crates (Dub), Imported Sources (Serato/Traktor/rekordbox/iTunes — each as a top-level node with their crates/playlists nested), Real Records (v1.1 — fingerprint-recognized records the user has played).
- Track list (center): file, artist, BPM, key, length, last-played, source. Sortable. Filterable by search box (artist + title + comment).
- Load: drag-to-deck or `Enter` to load to focused deck.

**No in-browser preview.** Cueing happens *in the deck*, on the user's hardware mixer headphones, exactly as a real DJ pulls a record out of a crate, drops it on the deck, and cues with their headphone monitor. The browser's job is finding tracks, not previewing them.

Performance: list virtualization required (Lexicon-class libraries can hit 100k+ tracks).

---

## 9. UI principles

### 9.1 Design ethos

> **Design means usability.** Every pixel justifies itself. If a control isn't used in a typical scratch session, it's not on the main view.

- **Modern, dark, calm.** Not Las-Vegas neon, not skeuomorphic decks. Something closer to Logic Pro / Ableton 12 than to rekordbox 7.
- **Two decks, equal weight, side-by-side**, with library below or in a togglable panel.
- **Waveforms front and center, side-by-side, horizontal.** Like Serato Scratch Live — Deck A's waveform on the left, Deck B's waveform on the right. Stacked vertical waveforms (à la Traktor) are not the model — the side-by-side layout is what scratch DJs are mentally calibrated to.
- **Playhead at 25 % from the left.** The user sees 25 % of what just played and **75 % of what's coming up**. What's coming is more important than what's gone. Most DJ apps (and Serato itself) put the playhead at center; this is wrong for the audience-facing DJ who needs to see *into the future* of the track. We deliberately depart from convention here.
- **Type-driven** — readable type at performance distance (1–2 m from the screen).
- **Color = function, not decoration.** Deck A and Deck B each have a single accent color; everything else is neutral.
- **No skeuomorphism, no jog wheel graphics, no fake CDJ overlays.** This is software, not a stage prop.

### 9.2 Layout (v1)

```
┌──────────────────────────────────────────────────────────────────────┐
│ DECK A overview  ▌25%─────────────────────────────────75%        ─┐  │
│ DECK A zoomed    ▌ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏  │  │
│ A: Track | BPM | Pitch    A: Loop | Key Lock | Slip                  │
├──────────────────────────────────────────────────────────────────────┤
│ DECK B overview  ▌25%─────────────────────────────────75%        ─┐  │
│ DECK B zoomed    ▌ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏ ▏  │  │
│ B: Track | BPM | Pitch    B: Loop | Key Lock | Slip                  │
├──────────────────────────────────────────────────────────────────────┤
│  FX Bar:  Echo-Out [A]  Echo-Out [B]  Dub Siren                       │
│  Quick Scratch: [Q][W][E][R]    Sampler: [A][S][D][F]                 │
├──────────────────────────────────────────────────────────────────────┤
│  Library / Browser                                                    │
│  [Source tree]  |  [Track list]                                       │
└──────────────────────────────────────────────────────────────────────┘
```

The `▌` is the playhead at 25 % from the left of each waveform.

Each deck has both views (overview + zoomed) on a single horizontal strip, full width of the display. This maximizes per-deck waveform real estate, which is what scratch DJs spend the most time looking at.

### 9.3 Waveform rendering

- GPU-rendered via Metal (`MTKView`).
- Two views per deck: **overview** (whole track) + **zoomed** (≈ 4 bars).
- **Playhead is fixed at 25 % of waveform width**; the waveform scrolls *under* the playhead at `1 / (engine_rate)` pixels per sample. The user sees the future on the right, the past on the left.
- Beatgrid overlay (white tick = downbeat, gray tick = beat).
- 60 fps minimum, 120 fps where supported (ProMotion).
- Timecode signal scope is also Metal-rendered (cheap — small circular buffer), shown as a small overlay on the deck (not a separate panel).
- During scratching, waveform tracks position with no visible lag relative to needle (i.e. ≤ 1 frame).

### 9.4 Accessibility

- Full keyboard control (any feature reachable without a mouse).
- VoiceOver labels on all controls (best-effort in v1, full in v1.x).
- High-contrast mode (v1.x).

---

## 10. Tech stack

### 10.1 Workspace

```
dubjay/                              # repo / workspace name
├── Cargo.toml                       # Rust workspace
├── crates/
│   ├── dub-engine/                  # Audio graph, transport, mixer, no_std-ish hot path
│   ├── dub-dsp/                     # rubato, biquads, dub-siren synth, echo-out
│   ├── dub-stretch/                 # Rubber Band FFI wrapper (separate crate for license clarity)
│   ├── dub-io/                      # symphonia-based decoders, ring-buffered streaming
│   ├── dub-timecode/                # Serato + Traktor decoder (xwax-derived)
│   ├── dub-thru/                    # Thru-mode input pipeline, aubio-based BPM tracking
│   ├── dub-fingerprint/             # v1.1 — Chromaprint FFI + match index
│   ├── dub-library/                 # SQLite + import adapters
│   ├── dub-controller/              # HID/MIDI abstractions (placeholder in v1)
│   ├── dub-ffi/                     # UniFFI-generated bindings to Swift
│   └── dub-cli/                     # Headless test harness
├── apple/
│   ├── DubCore.xcframework/         # Built artifact from dub-ffi
│   ├── Dub.xcodeproj                # SwiftUI + AppKit shell
│   ├── Dub/                         # App source (bundle name: "Dub", bundle id: com.klos.dub)
│   └── DubShared/                   # Swift package consuming the xcframework
├── scripts/
│   ├── build-xcframework.sh         # cargo build (aarch64+x86_64) + lipo + xcodebuild -create-xcframework
│   ├── codesign.sh                  # v1.1
│   └── notarize.sh                  # v1.1
├── tools/
│   └── rt-audit/                    # Static + runtime check: no alloc on audio thread
├── docs/
│   ├── PRD.md                       # ← this file
│   ├── ARCHITECTURE.md
│   ├── LIBRARY-FORMATS.md
│   └── adr/                         # Architecture Decision Records
└── README.md
```

Workspace path remains `dubjay/`; the macOS app bundle is `Dub.app`.

### 10.2 Key dependencies

| Crate | Purpose | License | Notes |
|---|---|---|---|
| `coreaudio-rs` | macOS audio I/O | MIT/Apache | Direct HAL access |
| `symphonia` | Decoding | MPL-2.0 | All formats incl. ALAC |
| `rubato` | Resampling | MIT | Sinc-based, FixedOut variant |
| `rubberband` (FFI) | Time-stretch / key lock | **GPLv3** | Forces whole project to GPL — accepted. |
| `aubio` (FFI) | Beat detection (fallback) + live tempo tracking on Thru | **LGPL-3.0** | Dynamic linking required |
| `chromaprint` (FFI) | Audio fingerprinting (v1.1 only) | **LGPL-2.1** | Dynamic linking required |
| `ringbuf` | Lock-free SPSC | MIT | RT-safe |
| `crossbeam` | Concurrency primitives | MIT/Apache | Off-RT only |
| `assert_no_alloc` | RT-safety check (dev + release) | MIT | Aborts if alloc on RT thread |
| `rusqlite` | Library DB | MIT | Bundled SQLite |
| `serde` + `quick-xml` | NML / iTunes XML / rekordbox XML | MIT | |
| `id3` + `metaflac` | Tag reading + Serato GEOB | MIT/MPL | |
| `hidapi-rs` | HID (placeholder, v1.x+) | BSD | |
| `midir` | MIDI (placeholder) | MIT | |

**Testing-only dependencies** (per §2.2):

| Crate | Purpose | License |
|---|---|---|
| `proptest` | Property-based testing | MIT/Apache |
| `insta` | Golden / snapshot testing | Apache |
| `criterion` | Microbenchmarks with regression tracking | MIT/Apache |
| `cargo-fuzz` | Coverage-guided fuzzing | MIT/Apache |
| `cargo-llvm-cov` | Coverage reporting | MIT/Apache |
| `cargo-nextest` | Faster test runner | MIT/Apache |
| `mockall` | Mocks for hardware abstractions | MIT/Apache |

Apple side: `swift-snapshot-testing` for SwiftUI snapshot tests.

### 10.3 Apple frontend

- **SwiftUI** for the chrome (toolbar, library, preferences).
- **AppKit** + **MTKView** for the performance surface (waveforms, scopes).
- **UniFFI**-generated Swift bindings into the Rust core for state queries and command dispatch. The audio render callback is set up entirely on the Rust side; Swift doesn't touch it.
- **Combine** for binding observable engine state (transport position, peak meters) to SwiftUI views, polled at 60 fps from the lock-free state snapshots.

### 10.4 Build & CI

The CI pipeline encodes the §2.2 quality bar. Every PR runs through the following GitHub Actions workflow:

**Per-PR (blocking):**
1. `cargo fmt --check` — formatting
2. `cargo clippy --all-targets -- -D warnings` — lints, treated as errors
3. `cargo nextest run` — full test suite (unit, property, golden, integration, RT-safety)
4. `cargo llvm-cov` — coverage report; PR fails if non-trivial modules drop below 85 %
5. `cargo bench --no-run` — benchmark compile check
6. RT-audit assertion: any test that triggers RT-thread allocation fails the build
7. Build matrix: `aarch64-apple-darwin` + `x86_64-apple-darwin`
8. Apple side: `xcodebuild test` for snapshot tests

**Per-merge to main (blocking the release pipeline):**
9. xcframework artifact build (universal: aarch64 + x86_64)
10. Apple `Dub.app` build & test
11. Tag-based release artifact assembly

**Nightly (non-blocking but tracked):**
12. Soak tests: 1+ hour offline render with synthetic timecode and FX rotation
13. Fuzz runs: ≥ 30 minutes per parser fuzz target
14. Benchmark history pushed to a tracking dashboard

**Branch protection on `main`:**
- No direct pushes
- Required: green CI, ≥ 1 review (when team grows beyond 1; until then, author self-review with a 24h cool-off for non-trivial changes)
- Required: linear history (rebase merges, no merge commits)

Notarization via `notarytool` arrives in **v1.1** (M22) once Developer ID is acquired.

---

## 11. Distribution & licensing

- **License:** GPLv3, top-level `LICENSE` file.
- **Distribution:** GitHub Releases. Notarized DMG. Apple Silicon + Intel universal binary.
- **No Mac App Store** in v1 (sandboxing breaks USB HID access for v1.x controller plans, and the Phase RF/SDK story for v2 is hostile to MAS).
- **Source:** public on GitHub from day one.
- **Funding model:** TBD (not part of v1 scope). Could be Patreon / GitHub Sponsors / paid commercial license dual-tier later. Listed as a v2+ open question.

---

## 12. Milestones

Each milestone has a **demo criterion** — a single sentence describing what the user can observably do at the end.

| # | Name | Demo criterion | Estimate |
|---|---|---|---|
| **M0** | **Scaffold + CI + test discipline** | `cargo nextest` passes, `clippy -D warnings` green, RT-audit harness runs on a no-op render, xcframework builds, blank SwiftUI app launches and prints "engine OK" from Rust. **GitHub Actions CI configured per §10.4. Branch protection on `main` enabled.** First TDD-discipline test exists and runs. | 2–3 days |
| **M1** | **First Sound** | Single deck, internal mode, plays a WAV through CoreAudio at < 8 ms latency. **Property tests for buffer math; golden tests for resampler output; RT-audit green during playback.** | 4–6 days |
| **M2** | **Transport (lock-free command channel)** | Main thread can `play`/`pause`/`seek`/`set_rate`/`set_gain` deck 0 while CoreAudio is playing, via a `ringbuf` SPSC queue drained at the start of every render block. UI reads deck position/playing/at-end via per-deck atomic snapshot (`AtomicU64` of `f64` bits, Relaxed). RT-audit: 100k blocks alloc-free **including** drain of pre-staged commands. CLI demo: `dub play <file> --realtime --pause-at 1.0 --resume-at 2.0 --seek-at 3.0=4.0` produces an audibly correct pause/resume/seek with snapshot-correct end state. | 3–4 days |
| **M2.1** | **RT discipline + soak harness** | rt-audit green under stress; 1-hour playback with no xruns at 64-sample buffer; **soak test harness in CI runs nightly**; **first parser fuzz target wired up** (ID3 reader). Folded as a milestone-internal gate before M3, not a user-visible milestone. | 3–5 days |
| **M3** | **Format coverage + hot track loading** | Loads MP3/FLAC/AIFF/M4A in addition to WAV (everything decoded fully into RAM per §4.4 — no streaming). `Command::DeckLoad(Arc<Track>)` allows changing decks live; old `Arc<Track>` is returned to the main thread via a trash channel and freed off the audio thread. CLI demo: `dub play <A> --hot-swap-at WALL=<B>` audibly swaps A→B mid-playback. Sample-accurate seek across all formats (already works since everything is in memory). | 4–6 days |
| **M3.5** | **De-click envelope + tail-fade + offline analyzer** | Two complementary primitives sharing one precomputed `sin²` envelope (2 ms × engine SR): (a) **transport-change declick** — an equal-power crossfade between pre- and post-mutation state on every track load, seek, and play/pause; (b) **tail-fade** — a multiplicative envelope applied as the playhead approaches a track's natural end so walking off the last sample doesn't step to 0. Both are gated by a `track_len ≥ 2 × envelope_length` threshold so synthetic short tracks aren't obliterated. Back-to-back transport changes routed via a single-slot `pending_disposal` + `AtomicU64` overflow counter; old `Arc<Track>`s never drop on the audio thread. New `dub analyze <wav>` subcommand reports peak/RMS/DC, clipping, and max per-sample first-difference, flagging any `|s[i] − s[i-1]|` above a configurable threshold (default 0.05) — replaces subjective listening with mathematical click detection. Offline `dub play -o` now supports the same scheduled transport events as realtime, so any scenario can be rendered deterministically and audited end-to-end. | 1–2 days |
| **M4** | **Two decks + debug mixer** | Both engine decks (`DECK_COUNT = 2`) drivable end-to-end through the CLI: `dub play <A> <B>` loads independent tracks onto deck A and deck B, both summed by the engine's existing additive deck loop into one stereo bus. Debug internal mixer adds a single `master_gain` field on `Engine` (M4 addition) plus the existing per-deck `set_gain`, applied multiplicatively after deck summing — pass-through when `master_gain == 1.0` to avoid the per-block multiply on the common case. New `Command::SetMasterGain` and `EngineHandle::set_master_gain` so the master is mutable mid-playback through the same lock-free SPSC channel as transport. CLI gains `--deck-b-*` mirrors of every transport flag (`--deck-b-rate`, `--deck-b-gain`, `--deck-b-pause-at`, `--deck-b-resume-at`, `--deck-b-seek-at`, `--deck-b-hot-swap-at`) plus `--master-gain G` and `--master-gain-at WALL=G`; bare flags target deck A for backward-compat with single-deck usage. `ScheduledEvent` carries a per-event `deck` index so each scheduled event addresses the right deck; engine-wide events (master gain) carry no deck. **External-mixer 4-channel routing is intentionally deferred** to M5/M6 where it's needed by the timecode hardware (SL3, Audio 6) — v1's debug mixer sums to one stereo output for now. CLI demo: `dub play <A> <B> --master-gain-at 1.0=0.6 --hot-swap-at 1.5=<C> --deck-b-pause-at 2.0 --deck-b-resume-at 3.0 -o out.wav && dub analyze out.wav` reports CLEAN with `max delta ≤ 0.026` (well under the 0.05 click threshold). Realtime path verified audibly. RT-audit extended to alternate command/load traffic across both decks plus periodic master-gain churn — 100k blocks alloc-free under `assert_no_alloc`. | 2–3 days |
| **M5** | **Timecode v1 (Serato)** | Serato CV02 controls deck position. Scope visible. Stickiness works. Broken into five sub-milestones below. | 1–2 weeks |
| **M5.1** | **Timecode decoder, offline (clean-room)** | New `dub-timecode` crate decoding Serato CV02 from stereo audio in **relative mode only**. Algorithm: treat `s = L + jR` as a complex analytic signal; compute the coherent block sum of `s_n · conj(s_{n-1})`; per-block instantaneous frequency = `arg(sum) / (2π·Δt)`; rate = `f_inst / carrier_hz` (signed — negative = reverse); position integrates rate × block-seconds. Confidence = `\|sum\| / Σ\|s\|²` (1.0 = pure carrier, 0.0 = noise). RT-safe (alloc-free under `assert_no_alloc`). Fully unit-testable on synthetic stereo quadrature signals — no hardware required. Bitstream/absolute decode deferred to M6. **Clean-room implementation** from xwax/Mixxx algorithm description; no xwax code copied. CLI: `dub decode-timecode <wav>` reads recorded timecode and reports rate / position / amplitude / confidence per window with a LOCKED/PARTIAL/POOR verdict; `--synthetic` runs a built-in 1.0× → 0.5× → -1.0× → silence scenario for sanity-checking without a turntable. | 3–5 days |
| **M5.2** | **Audio input plumbing** | `dub-audio` gets an `AudioInput` primitive mirroring `AudioOutput`: HAL input AudioUnit, ringbuf-buffered handoff to a consumer thread. CLI: `dub capture` (writes input to WAV) and `dub levels` (live meter). Verified on default mic input first, SL3 input pair second. | 2–3 days |
| **M5.3** | **Live timecode → deck (first scratch)** | Wire `AudioInput` → `dub-timecode::Decoder` → engine deck in **relative mode**: per-block decoded rate is applied to the deck via `set_rate`; lift detection runs three layers — (a) **amplitude gate** (`DEFAULT_AMPLITUDE_THRESHOLD = 0.01` RMS) overrides confidence whenever the carrier is dead, since handling/rumble noise on a lifted cartridge can produce moderate confidence at near-zero RMS, (b) **two-edge confidence hysteresis** (engage `0.8`, disengage `0.5`) for clean scratch-transient handling, (c) **sticky-block window** (4 blocks ≈ 21 ms @ 256-frame / 48 kHz) for dust-tick immunity. Three iterations on the SL3 drove the design: the first single-threshold gate chattered on lift; the second confidence-only hysteresis treated lift as a "lukewarm scratch transient" and burst-played the track while the needle was up; the amplitude gate closes that hole. The state machine is factored into a pure `step_policy(DecodeOutput) → Intent` on top of `drive(...)` (which sources data from the ringbuf), so each pathology has a dedicated regression test. The decoder consumes the input ringbuf directly on the audio thread inside `Engine::render` — no extra thread, no extra channel — so the only added latency on top of M5.2's input ring is one `Decoder::process` call per render block (~µs). New public engine surface: `Engine::attach_timecode_input(deck_idx, HeapCons<f32>, TimecodeInputConfig)`, `Engine::detach_timecode_input`, `Engine::timecode_last_output(deck_idx)` for UI observability. New `dub_audio::AudioInput::take_consumer()` lets the consumer end of the IOProc → consumer ringbuf move into the engine while the `AudioInput` itself stays on the main thread for shutdown. **`AudioOutput` now also force-aligns the output device's nominal SR to engine SR** (same gauntlet as `AudioInput`) — first SL3 validation surfaced an 8 % pitch drift when output was at 44.1 kHz and engine at 48 kHz because the CoreAudio HAL DefaultOutput unit does not reliably SRC across that boundary. Position drift correction (re-syncing deck position to decoded position over wall time) is intentionally deferred — relative-mode in v1 lets position evolve via integration of rate, which is what platter motion already encodes. **rt-audit extended** with a 10k-block timecode-driven render path under `assert_no_alloc`, verifying the entire Decoder + transport-update path is heap-free on the audio thread. CLI: `dub timecode-deck <track.wav> --input-channels N,M [--device NAME] [--duration SECS] [--confidence T] [--disengage-threshold T] [--sticky-blocks N] [--amplitude-threshold T]`. **Demo criterion: scratch a record on Deck A, hear Deck A's loaded track react with sub-buffer-size latency, see deck mute cleanly on stylus lift with no track audio leakage, see direction reversal on backspin.** This is the milestone where Dub becomes a DJ app. | 2–3 days |
| **M5.4** | **Calibration + scope** | Split into two delivered sub-milestones because the scope is independently valuable and lands a refactor that calibration also needs. **M5.4.1 — TUI scope** (`dub scope`) opens the input device, runs the same `LiftPolicy` as `dub timecode-deck`, and renders a ratatui inspector: Lissajous of input `(L, R)` (the carrier should trace a clean circle; lift collapses it to a noisy blob), `[LOCKED]` / `[LIFT]` engagement badge, gauges for confidence and amplitude (color-coded against current thresholds), rate readout with a centered slider in `[-2×, +2×]`, sticky countdown bar showing the policy's `consecutive_below` counter walking toward disengage, and a row of live thresholds. Arrow keys mutate engage / disengage / amplitude *in place* so users can find sane defaults for their cartridge against their actual signal — calibration sandbox that M5.4.2 persists. Block size pinned to 256 frames so the scope's policy decisions match `timecode-deck` 1:1. **Refactor:** `step_policy` and the engagement state were factored out of `TimecodeInput` into a public `LiftPolicy { engage, disengage, sticky, amplitude, engaged, consecutive_below, last_locked_rate }` with a `step(DecodeOutput) -> LiftIntent` method; `TimecodeInput` now embeds it and delegates. Three callers — engine playback, `dub scope`, and `dub calibrate` (M5.4.2) — share exactly the same lift behavior because they share the code path. New CLI: `dub scope [--device NAME] [--input-channels N,M] [--engage T] [--disengage T] [--sticky N] [--amplitude T] [--format serato-cv02] [--duration SECS]`. New deps in `dub-cli` only: `ratatui` 0.30, `crossterm` 0.29 (engine and audio crates untouched). **M5.4.2 — Calibration UX (`dub calibrate`)** measures typical engage / disengage / amplitude levels for the user's specific cartridge + preamp + interface and stores them per-device as a JSON artifact in `~/Library/Application Support/Dub/calibration/`. `dub timecode-deck` auto-loads matching calibration on start (CLI flags still override); `dub scope` thresholds also default to it once stored. | scope: 1 day; calibration: 1–2 days |
| **M5.5** | **External-mixer 4-channel output routing** | Map deck N → physical output channels `2N+1, 2N+2`. Required by SL3 / Audio 6 ins-outs in the typical scratch-DJ topology (deck → physical mixer's line input). The internal debug mixer (M4) keeps working as a `--internal-mixer` flag for development on devices without ≥ 4 outputs. | 2–3 days |
| **M6** | **Timecode v2 (Traktor)** | Traktor MK2 also works. 33+45 RPM, A+B side, abs/rel modes, calibration UI. | 1 week |
| **M7** | **Thru Mode** | Per-deck input routing, Direct + Processed, auto-mode-switching on FX engage. Audio 6 / SL3 inputs working. | 1 week |
| **M8** | **Auto-BPM on Thru** | aubio-rs integrated; live BPM detection on Thru input with confidence indicator. | 4–6 days |
| **M9** | **Live waveform capture (Thru)** | Multi-resolution peak waveform of Thru input rendered live; in-memory only. | 4–6 days |
| **M10** | **Waveform UI** | Metal-rendered overview + zoomed waveform, beatgrid overlay, 60 fps tracking during scratch. Same renderer used by Thru waveforms. | 1–2 weeks |
| **M11** | **Library import: Serato** | Import Serato library, browse it, load tracks, beatgrids appear. | 1 week |
| **M12** | **Library import: rest** | Traktor + rekordbox + iTunes + Lexicon. | 1–2 weeks |
| **M13** | **Looping** | Manual + auto-loop, halve/double, behaves correctly under timecode. | 4–6 days |
| **M14** | **Key Lock + auto-bypass** | Rubber Band integrated, on/off per deck, scratch-aware auto-bypass per §6.1.1. | 1 week |
| **M15** | **Smart FX: Echo-Out** | Tap-and-hold echo-out works on both decks (incl. Thru: Processed). | 4–5 days |
| **M16** | **Smart FX: Dub Siren** | Synth + delay + reverb, keyboard controllable. | 3–4 days |
| **M17** | **Sampler + Quick Scratch + Instant Doubles** | All three trigger systems work per §7. | 4–6 days |
| **M18** | **Polish + Alpha** | Calibration UX, preferences, key remapping, dark-mode polish, performance pass. **Alpha release per §2.2.8: invite ~3–5 trusted DJs for real-gig testing.** Manual rig checklist (§2.2.10) executed end-to-end. | 2–3 weeks |
| **M19** | **Beta** | Public opt-in beta on GitHub Releases. Feature-frozen for v1.0. **Hotfix discipline active: any crash → patch within 24h.** Soak test logs publicly viewable. | 2–4 weeks (gated by gig-time accumulation) |
| **M20** | **v1.0 Stable Release** | All §2.2.6 SLOs met (100 cumulative gig-hours zero-crash, zero xruns in soak, zero fuzz crashes, manual rig checklist signed off). DMG on GitHub Releases (unsigned dev build acceptable per current decision), README, docs site stub, demo video. | 3–5 days once SLOs met |

**Aggregate:** ~ 16–22 weeks of focused work for v1.0, including beta-gated promotion. The Beta → Stable gap is **deliberately variable**: we ship Stable when the SLOs are met, not on a schedule. This is the load-bearing protection against the "DJ on stage" failure mode.

### 12.1 v1.1 (post-launch follow-up, ~6–8 weeks after v1.0 stable)

| # | Name | Demo criterion |
|---|---|---|
| **M21** | **Fingerprint recognition** | Chromaprint integrated; first play of a real record captures fingerprint; second play recognizes within 5–10 s and loads saved waveform. |
| **M22** | **Persistent waveform learning** | Captured Thru waveforms persist to library DB; rendered immediately on recognized records. |
| **M23** | **Code signing + notarization** | Apple Developer ID acquired; notarized DMG; auto-update mechanism. |
| **M24** | **Beatgrid editor** | Full grid editing UX (drag downbeat, nudge BPM, halve/double, taps). |
| **M25** | **Opt-in crash reporting** | Sentry (or similar) integration with explicit user toggle, redaction of file paths, per §2.2.7. |

---

## 13. Risks & open questions

### 13.1 Technical risks

| Risk | Severity | Mitigation |
|---|---|---|
| Timecode quality on cheap interfaces | High | Test matrix from day one (SL3, Audio 6, generic class-compliant); document supported interfaces. We have both reference rigs in-house. |
| Rubber Band CPU at 2 decks + key lock + active playback | Medium | Profile early (M14). Have a lower-quality fallback flag (`R3` engine off, use `Faster` engine). Scratch-aware auto-bypass (§6.1.1) reduces total Rubber Band load substantially during real DJ use. |
| Auto-BPM accuracy on dub / minimal genres (sparse beats, half-time feels) | Medium | Tunable hint per Thru deck: "expected BPM range" picker (60–110 / 120–150 / etc.) feeding aubio's tempo prior. Genre-aware presets for common cases. |
| Chromaprint robustness to turntable pitch drift / mixer EQ (v1.1) | Medium | Validate during v1.1 with real-world test corpus. Fall back to Shazam-style constellation hashing if Chromaprint underperforms. |
| Thru: Processed latency perceived as "feel different" by sensitive scratch DJs | Low–Medium | Default to Thru: Direct; auto-switch to Processed only when FX engaged. Document the trade-off. |
| rekordbox DB6 format changes | Medium | Always offer XML-export path as fallback. |
| CoreAudio aggregate device weirdness | Medium | Document recommended interface configs. SL3 and Audio 6 both don't need aggregation. |
| Notarization / code-signing setup | Low | Defer to v1.1 (M22). v1.0 ships unsigned-with-instructions per current decision. |
| GPL incompatibility with future commercial plans | Medium | Explicit decision: GPL for now, revisit at v2. Rubber Band commercial license = ~£600 one-time when/if needed. |
| SL3 discontinued by Serato | Low | Class-compliant on macOS, works fine. We test against it but recommend the Audio 6 (or successors) as the reference modern interface in our docs. |

### 13.2 Open questions (to resolve during development)

1. **Sample bundling decision** — defer until UX is testable.
2. **Dub-siren dedicated FX-bus output** — cheap to add; decide during M16.
3. **Auto-gain default**: LUFS-I (musically correct) or peak (predictable for scratch DJs)? — A/B test with users in beta.
4. **Beatgrid editor UX** — minimal in v1, full pass in v1.1 (M24).
5. **Funding model for sustainability post-v1** — out of scope for PRD; revisit before v1 release.
6. **Brand identity** (logo, marks) — not in v1 PRD scope.
7. **Alpha tester recruitment** — need 3–5 trusted DJs willing to run pre-release on real gigs. Author's network presumably covers this; revisit at M17.
8. **CI runner for nightly soak/fuzz** — GitHub Actions has limits; may need a self-hosted runner (e.g., a spare Mac mini) once nightly soak exceeds free CI minutes. Decide at M2.

### 13.3 Items explicitly deferred to v2

- Phase support (full subsystem, including SDK access and integration)
- Hot cues
- Recording (master out + per-deck)
- Windows port
- HID controller ecosystem
- Key detection
- Stems (probably never)

---

## 14. Acceptance criteria for v1.0

Dub v1.0 ships when **all** of the following hold on a DMG installed on a clean macOS 14+ machine:

1. Scratch DJ can plug a class-compliant 4-in/4-out USB interface (test rig: SL3 or Audio 6), route timecode inputs to In 1/2 and 3/4, route outputs to their hardware mixer, and have both decks under timecode control with **< 5 ms latency at 64-sample buffer, < 10 ms total timecode-to-audio response.**
2. Both Serato CV02 and Traktor MK2 vinyl supported.
3. Either deck can be set to **Thru: Direct** (real record routes through the interface, zero added latency) or **Thru: Processed** (real record routes through engine FX, ~1.3 ms added latency).
4. **Auto-BPM detects tempo of a real record played in Thru mode within 15 seconds, with a confidence indicator that distinguishes "tentative" from "locked".**
5. **Live waveform of a Thru-mode real record is rendered as the record plays, at 60 fps, with no glitches.** (Persistence and recognition land in v1.1.)
6. Echo-Out and Dub Siren can be applied to a Thru: Processed deck (i.e. FX work on real records).
7. User can import their existing Serato / Traktor / rekordbox / iTunes / Lexicon library and play tracks with imported beatgrids. Auto-detect grids fall back when source has none.
8. Looping (manual + auto, halve/double) works correctly under timecode.
9. **Key Lock works on both decks; engages and disengages automatically based on playback rate per §6.1.1; user hears no glitches during scratching with Key Lock on.**
10. Echo-Out, Dub Siren, Sampler (4 slots), Quick Scratch (4 slots, hotkey fast-load), Instant Doubles all work per §6 / §7.
11. UI is keyboard-navigable end-to-end. No performance interaction requires the mouse ("No Mouse DJ Ever").
12. Zero xruns in a 60-minute scratch session at 64-sample buffer on M2 Air.
13. README + first-run experience documents how to set up a typical rig (turntables → interface → mixer → speakers) and a Thru-mode rig (real record → interface → engine → mixer).
14. **All §2.2.6 reliability SLOs met**: zero crashes in 100 cumulative beta-gig-hours; zero xruns in soak; zero RT-thread allocations; zero fuzz crashes in last 7 days; no benchmark regressions; manual rig checklist (§2.2.10) signed off.

---

## 15. Out of scope for v1 (reaffirmed)

- Internal mixer mode (user-facing)
- Mouse-driven mixing of any kind ("No Mouse DJ Ever")
- Hot cues
- Recording
- Streaming services
- Phase
- HID controllers
- Audio fingerprint recognition / persistent waveform learning (planned v1.1)
- Code signing & notarization (planned v1.1; v1.0 ships as unsigned dev DMG)
- Auto-update mechanism (planned v1.1)
- Stems / AI
- Video / visuals
- Cloud
- Mobile
- Windows
- Mac App Store
- Localizations beyond English

---

*End of document. v0.1 draft. Next step: scaffold workspace per §10.1 once PRD is approved.*
