# Dub — Product Requirements Document

> macOS app bundle id: `com.klos.dub`.

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
- **No Mouse DJ for performance gestures.** A *performance gesture* — pitch, scratch, crossfade, EQ, gain, cueing — never goes through the mouse. Those live on the turntable + external mixer + keyboard, always. The mouse **is** allowed (and welcome) for: configuration in Preferences; library navigation and search; loading tracks onto decks; **track-position navigation** (click on the overview waveform to jump the playhead, scrub the zoomed waveform in File mode); and the explicit **transport / recovery controls** — Play, Pause, Restart — used for dirty-needle recovery (§6.1.2) and casual pre-performance playback (§6.1.3). The forbidden list is short and precise: no software crossfader, no software EQ, no software cue/preview channel, no software pitch fader, no mouse-driven scratching of the waveform during Timecode mode. Everything else is on the table. See §6 for the positive list, §5.3 / §6.6 / §15 for the negative list.
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
6. **Production-grade audio ecosystem.** `coreaudio-rs`, `symphonia`, `rubato`, `ringbuf`, `assert_no_alloc`, `realfft`, `rustfft` — all mature, all maintained. (Aubio was the original M7.5 plan; pure-Rust took its place — see [`docs/SHIPPED.md#m75`](SHIPPED.md#m75).)
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
8. Switch a deck to Thru, drop a real record → audio passes through the engine (~2.7 ms one-way), waveform builds live, auto-BPM locks.
9. On the same Thru deck, engage Echo-Out → FX layers on top of the dry record; disengage → echo tail decays naturally over the next bar.
10. Auto-BPM on Thru locks within 15 s on a 4/4 hip-hop record, a reggae record, and a dnb record.
11. Unplug the audio interface mid-playback → engine stops cleanly, UI shows "interface lost", reconnect → playback resumes.
12. Run 60 minutes continuous use (any combination of features), close the app cleanly → no crash, no orphan processes.
13. Open the macOS sleep/wake cycle while Dub is running → audio resumes without artifacts on wake.
14. Run with a deliberately corrupted Serato library file → graceful error, app does not crash.

---

## 3. Platforms & roadmap

| Version | Platforms | Headline additions |
|---------|-----------|-------------------|
| **v1.0** | macOS (Apple Silicon + Intel) | Timecode vinyl, 2-deck, sampler, smart FX, library import, Phase-Drift Trail beat-match aid, Track Preparation Mode (shell) |
| **v1.x** | macOS | Polishing, controller/mapping support if requested by community, Track Preparation Mode prep tooling (beatgrid editor, hot cues if pulled forward) |
| **v2.0** | macOS + Windows | **Phase support**, hot cues, recording, Windows port (ASIO/WASAPI) |
| **v3.0** | macOS + Windows + iPadOS | iOS/iPadOS port (USB-C iPads), cloud library sync |

**v1 is macOS only.** No iOS, no Windows, no Phase. Time-to-first-release is the constraint.

### 3.1 Runtime modes

Dub has **two top-level runtime modes**, auto-selected at launch based on which audio interface is present. The user can override in Preferences.

| Mode | Triggered when | UI | Purpose |
|---|---|---|---|
| **Performance Mode** | A pro audio interface (≥ 4 in / 4 out) is detected | Two decks side-by-side, **vertical waveforms** scrolling bottom→top (PRD §9), Phase-Drift Trail in the centre gutter, FX bar, library | The live-DJ surface. The whole rest of this document, unless stated otherwise, describes Performance Mode. |
| **Track Preparation Mode** | Only the built-in soundcard is detected (no multi-channel interface) | Single deck, **horizontal** waveform full-width, library prominent | Auditioning tracks, fixing beatgrids, prepping cues — work the DJ does in advance of a gig, on the couch with no rig attached. **v1.0 ships the shell only** (load + play + horizontal waveform); the actual prep tooling (beatgrid editor, hot-cue prep) is v1.x — see [§12 M10.8](#m108-track-preparation-mode-shell). |

Both modes share the same engine, the same library, the same file format support, and the same tokens / colour palette — they differ only in the surface they present. Switching modes is a window-level re-mount (not an in-place reflow); the user perceives them as "two apps in one binary" rather than as a layout switch. This is intentional — neither mode should leak vocabulary into the other.

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
| **Thru** | Audio interface input on this deck's input pair routed *through* the engine to its output pair. **Always software-path**, never hardware-bypass: BPM detection, waveform capture, and FX all need the signal in software. One buffer of round-trip latency (~2.7 ms at 64-frame buffer / 48 kHz) — constant regardless of FX state. Hardware-Thru on the interface (SL3 / TA6 physical button) is outside Dub's scope because it routes audio around the software entirely. | **Yes — v1, automatic** |
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
              │TIMECODE │     │      THRU       │
              └─────────┘     └─────────────────┘
                              (FX modules engage/disengage
                               inside the chain; no
                               source-mode change)
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

Thru Mode is a **headline feature** of v1 and a key differentiator. The user plays a real (non-timecode) record on their turntable, the audio flows through the audio interface into Dub, and Dub treats it like any other source: BPM tracking, waveform capture, and FX all apply.

#### 5.2.1 Routing

- The user's real turntable plugs into the audio interface's input pair (e.g. SL3 inputs A or B, Audio 6 inputs 1/2 or 3/4).
- **Signal path:** input pair → engine bus → (FX chain, when modules are engaged) → output pair. The engine treats the live audio identically to file-decoded audio for everything downstream of the source. Constant one-buffer round-trip latency (~2.7 ms at 64 frames / 48 kHz) — independent of FX state.
- **No mode flip on FX engage/disengage.** FX modules sit inside the per-deck signal chain and own their own bypass semantics (each with its own per-module declick on engage/disengage of the FX's *wet* output; the *dry* path through the Thru source is never paused, never crossfaded, never re-timed). This gives the DJ a hardware-pedal hand-feel: the dry record is always present underneath, FX layer on top.

#### 5.2.2 Why we don't expose a "hardware Thru" mode

Some audio interfaces (SL3, TA6, …) ship a physical Thru button that routes the preamp output directly to the analog output, bypassing USB and the host entirely — zero latency, no software involvement. We do not integrate with that switch, and we do not try to expose a "Direct" software equivalent (engine-silent + driver-level monitoring), because both approaches are **incompatible with what Thru Mode is for in Dub**:

- BPM detection (§5.2.3) needs the signal in software.
- Live waveform capture (§5.2.4) needs the signal in software.
- FX (M15+) needs the signal in software.

If a DJ wants hardware-Thru zero latency on a specific record, they can press the interface's physical Thru button. Dub will see no input on that pair for the duration; the waveform stops growing, BPM goes "searching", FX have no signal to operate on. That's the correct behaviour given the trade-off the operator just made — it's not Dub's job to mirror it. The earlier design that had Dub auto-flip between "engine silent" and "engine processing" based on FX engagement is removed: the silent state was producing actual silence in practice (no host-side hw-monitor control exists on plain CoreAudio), and the path-swap latency-jitter between modes was exactly the timing instability the rest of the engine is built to avoid (latency that *changes* under the operator's hands is worse than latency that's constant and slightly higher).

#### 5.2.3 Auto-BPM on live audio

A Thru deck runs continuous tempo tracking on its input:

- Algorithm: pure-Rust **log-band spectral-flux ODF** (8 log-spaced bands from 30 Hz – 16 kHz, each summed equally into the final ODF) + **windowed local-energy autocorrelation** (5-bin sum at each integer-lag candidate) with **harmonic-mean** scoring over the first 4 multiples and centroid sub-bin refinement. Shipped in the `dub-bpm` crate; algorithm walk-through in [`docs/SHIPPED.md#m75`](SHIPPED.md#m75) (M7.5 baseline) and [`docs/SHIPPED.md#m81`](SHIPPED.md#m81) (M8.1 multi-band + windowed-energy overhaul that fixed the hip-hop 2× regression). Detects synthetic click tracks at 60–174 BPM within ±1 BPM *and* locks the user's stated genre mix at the correct octave (reggae 65, hip-hop 90/100, rolling dnb 174). The file-side fallback in §8.3 and the streaming wrapper below both build on the same `BpmEstimator` core. An `aubio-rs` backend remains available as a future opt-in feature flag for genres the M8.1 algorithm can't resolve without a tempo / genre prior (dubstep at 140 / 70, K-S-backbeat dnb at 174 / 87) — but those edges are also reachable today via the `BpmRange` escape hatch (`--bpm-range MIN,MAX` on `dub thru`, `analyze_bpm_with_range` for the offline driver). The LGPL FFI is not on any near-term critical path.
- Streaming wrapper (M8, shipped): `dub_bpm::BpmStream` spawns a non-RT analysis thread per Thru deck. The audio thread runs only a mono-downmix + SPSC push into a tee ring (alloc-free; verified by `assert_no_alloc` on the `render_into` path). The analysis thread reads from that ring, runs `BpmTracker` (= `BpmEstimator` + `ConfidenceTracker` hysteresis state machine + ~1 s throttle on the expensive autocorrelation search), and emits `TrackerEvent::StateChanged` transitions through a second SPSC ring the UI polls. The Thru `ThruSource` itself stays a pure passthrough (PRD §5.2.1). Engine ↔ tracker glue lives in `EngineHandle::attach_thru_source_with_bpm_tracking`. Full design + tuning rationale in [`docs/SHIPPED.md#m8`](SHIPPED.md#m8). The streaming-stability regressions M8 exposed at 128 BPM / 44.1 kHz and 140 BPM / 48 kHz are part of what motivated the M8.1 algorithm overhaul.
- Stabilization: ~10–15 s of music for a confident reading.
- UI: BPM display with a **confidence indicator** (3-state: searching / tentative / locked). Tentative readings shown italicized.
- The detected BPM feeds the FX (echo-out divisions, loop length references) so the user can apply tempo-synced FX to a real record.

#### 5.2.4 Live waveform capture (v1)

While a Thru deck is active, the engine accumulates a **peak waveform** of the input audio in real time, rendered live as the record plays. When the record finishes (or the user disengages Thru), the captured waveform is held in memory and optionally persisted (see §5.2.5).

**Shipped in M9** as the `dub-peaks` crate, the data-layer companion to M8's `dub-bpm`:

- **Audio thread** — the same mono-downmix scratch that feeds the M8 BPM tee also feeds a second `peaks_tap` SPSC ringbuf via one extra `push_slice`. Alloc-free; the ThruSource computes the downmix once and dispatches to whichever taps are enabled.
- **Off-RT decimator thread** — drains the peaks tap at 20 ms cadence into a `Decimator(samples_per_chunk=64)` that emits a `PeakChunk { min, max, rms }` per 64 input samples (≈ 1.33 ms / chunk at 48 kHz). Same architectural shape and lifecycle as `BpmStream`.
- **Shared buffer** — `PeakBuffer { len: AtomicUsize, chunks: RwLock<Vec<PeakChunk>> }`. The renderer's "anything new?" check is a lock-free Acquire-load on `len`; only when there's new data does it take a brief read lock to `extend_chunks(start_idx, &mut local_mirror)`. O(new chunks) per 60 fps frame, not O(total). Initial capacity defaults to 10 min of audio; growth beyond that reallocates off-RT (the audio thread never reallocates).
- **`PeakChunk` is `#[repr(C)]`** (12 bytes) — the M10 consumer contract is to take a `&[PeakChunk]` from `extend_chunks` and shove it directly into a Metal vertex buffer with no further packing.
- **Multi-resolution** — the M9 crate ships a single base mip level. Mip-up for the overview view (~67k samples/pixel at 90 min on a 4K screen) is one averaging pass in the renderer; the crate deliberately doesn't pre-build a mip pyramid because the renderer knows how many pixels it has and can downsample on demand.
- **CLI surface** — `dub thru` defaults to peaks-tracking on; the periodic stats line shows per-deck captured chunk counts. `--no-peaks-track` opts out; `--dump-peaks PATH` writes the captured buffer to a CSV file on shutdown (one row per chunk: `deck,chunk_idx,min,max,rms`) so the operator can plot the envelope before the M10 UI lands.

The architectural pre-commitment in §4.1 ("**Live waveform engine** — runs alongside Thru, accumulates multi-resolution peak data, rendered by Metal") is concretized exactly: `dub-peaks` is what M10's Metal renderer consumes. The M10 consumer contract is documented in `crates/dub-peaks/src/lib.rs` module docs.

See [`docs/SHIPPED.md#m9`](SHIPPED.md#m9) for the full design history and test surface.

#### 5.2.5 Audio fingerprint recognition (v1.1 — *not v1.0*)

The differentiating feature, planned for v1.1.

- As a Thru deck plays, the engine continuously computes a rolling **audio fingerprint** over a 5-second window. Algorithm: Chromaprint (LGPL, FFI) or a clean-room implementation if licensing forces it.
- Fingerprints are matched against a local database of records the user has played before.
- **First play** of a record: no match. Engine creates a new entry, captures the waveform, captures the auto-BPM, captures the fingerprint, persists everything to `library.sqlite` keyed by fingerprint hash. The user can optionally tag the entry with title/artist (or it stays anonymous).
- **Subsequent plays**: fingerprint matches within 5–10 s of needle-drop. Engine loads the saved waveform, beatgrid, and BPM. UI animates: waveform "fades in" as recognition completes, BPM stops searching and locks, beatgrid overlays appear. Effects that need beat-sync become available.
- **Robustness considerations**: pitch variation (turntable ±8 %), surface noise, mixer EQ, room sound. Chromaprint is designed for exactly this and handles ±10 % pitch reliably. Our fingerprint hashing is pitch-tolerant (use the Shazam-style constellation approach over Chromaprint's chroma if Chromaprint proves insufficient).

**v1.0 ships:** Thru routing (always software-path, FX in-chain), auto-BPM, **live waveform capture and rendering** (in-memory only — not persisted, no recognition). v1.0 already shows the user "this record I'm playing has BPM 92 and here's its waveform as it plays." That alone is unique.

**v1.1 adds:** persistence, fingerprinting, recognition, beatgrid storage. This is the magic.

#### 5.2.6 Constraints

- Thru Mode adds **one buffer of round-trip latency** (input + output), e.g. ~2.7 ms at 64 samples / 48 kHz. This is unavoidable physics. It is *constant* with respect to FX state (engaging FX does not change the input-to-output delay; FX modules add to the dry path, they don't replace it), so the DJ's hand→ear muscle memory stays calibrated across the whole set.
- The user must drop the needle near the start of a record for waveform capture to be meaningful. We do not "stitch" partial captures across plays in v1; that's a v1.x consideration.
- Auto-BPM cannot detect tempo on solo a-cappella or beat-less ambient sections. UI must communicate "no beat detected" honestly, not lie with a fake number.

### 5.3 External mixer mode (only mode in v1)

- **Required:** audio interface with ≥ 4 outputs (2 stereo pairs) AND ≥ 4 inputs if Thru mode is used.
- Per-deck output assignment: Deck A → Out 1/2, Deck B → Out 3/4 (configurable).
- Per-deck input assignment (for Thru and timecode): Deck A → In 1/2, Deck B → In 3/4 (configurable).
- **No software cue/preview channel.** Cueing is the hardware mixer's job.
- **No software crossfader.** External mixer's crossfader is the only crossfader.
- **No software EQ.** External mixer's EQ is the only EQ.
- **No mouse-driven mixing or performance gestures.** Per the §1 mouse rule — mouse never drives pitch / scratch / crossfade / EQ / gain / cue. Mouse-driven transport (Panic Play, Casual Play, position navigation) is allowed and lives in §6.1.
- **Smart FX (echo-out, dub siren) are inserted into the per-deck output bus pre-output.**

The internal debug mixer (§5.6) is **not** the same as a user-facing internal mixer mode. v1 ships only External Mixer mode in the UI; the debug mixer is dev-only.

### 5.4 Timecode subsystem

**Supported control records:**
- Serato Control Vinyl CV02 (1 kHz reference + LFSR position)
- Traktor MK2 Timecode

**Both supported in v1.** User selects which in preferences; auto-detect attempted on input.

**Required behaviors:**
- 33⅓ and 45 RPM detection (auto, with a manual Preferences override — see §5.4.1).
- **Relative mode only in v1.** Needle drops are ignored; only motion is tracked. Absolute mode is deferred — almost no scratch DJ uses absolute mode in practice. Skipping it cuts calibration UI complexity significantly.
- **Pitch range** as wide as the user's turntable (typically ±8 / ±16 / ±50 %)
- Slow-down to stop: tracks pitch through zero cleanly without click/glitch
- Backspin: tracks negative pitch with no audible artifact up to the resampler's limits
- **Drop-out detection (Stickiness)**: if signal quality degrades (dust, scratch, end of run-out groove), hold last known velocity for a grace window (default 250 ms), then engage internal playback at the last pitch until signal returns. See §6.1.2 (Panic Play) for the user-driven extension of this state.
- **Through groove handling**: detect run-out and hold position. See §5.4.2 (Repeat) for the user-driven extension that keeps audio playing forward instead of holding.
- **Calibration UI**: show signal scope, S/N ratio, RPM detection, pitch readout. Live calibration with vinyl spinning. **No A/B side toggle, no abs/rel mode selector** — relative mode is universal.
- **Tracking quality indicator (UI)**: a per-deck signal-health glyph in the deck header source pill (PRD §9) — green dot = clean lock, amber = degraded signal, red = no lock. Read off the M5.4.6 `LiftPolicy` confidence the engine already publishes. Note: it is **expected** for tracking to be red while cueing or scratching, identical to Serato's behaviour — the dot reports signal quality, not user intent.

**Algorithm:** port the xwax decoder (well-understood, ~2k lines C, BSD-licensed). Our port lives in `crates/dub-timecode/`. Both Serato and Traktor LFSR tables included.

**Absolute mode is deferred to v1.x or later** if user demand emerges. Most likely, never.

#### 5.4.1 RPM Preferences override

The auto-detect path handles 33⅓ and 45 RPM transparently in 99 % of cases. Edge cases (unusual pressings, calibration of an under-spec'd turntable) need a manual override. This lives in the Preferences sheet under "Timecode" — a per-deck `Auto / 33⅓ / 45` selector. The override is *not* a performance gesture; the DJ sets it once during sound-check and forgets it. It must not appear on the performance surface.

#### 5.4.2 Repeat (timecode run-out)

When the needle reaches the end of the timecode-encoded area of the control record, the LFSR signal disappears and Stickiness (§5.4) ordinarily engages, holding the playhead at the last known position. **Repeat** is the user-controlled alternative: when enabled (per-deck toggle; trigger surface TBD — see §5.5), the engine instead continues playing the audio track forward at the last-known velocity, decoupling track playback from the (now-absent) timecode entirely. Useful when the audio track is longer than the timecode record's playable area.

Engine-side this is the same state as Panic Play (§6.1.2): "decouple audio from timecode, run forward at last-known rate, re-engage on signal return." Repeat is the *auto-triggered* form (triggered by run-out), Panic Play the *user-triggered* form (button press). One state, two entry points; both exit on clean signal return.

#### 5.4.3 Reverse Input Control

A keyboard-only command (no UI button on the performance surface) that **swaps deck A's and deck B's timecode input pairs**. Two motivations:

1. **Booth wiring discovered late.** In a dark booth, the DJ can't always see which turntable is on which channel pair on their interface. They arrive, drop the needle, and the wrong virtual deck moves. Rather than re-cabling under stage lights, they hit one key and the mapping flips.
2. **One-turntable emergency.** A needle skips, a deck breaks, a cartridge dies mid-set. The DJ wants to continue with the surviving turntable controlling whichever virtual deck the next track is queued on. Reverse Input transfers control of the working turntable to the silent virtual deck without re-routing audio.

Implementation: the M5.4.5 late-binding-decks + per-deck input attach work already supports this; the command is a single re-attach pair on the trash channel. **Audio is never re-routed** — only the *control* (timecode → virtual deck) flips. Deck A still plays out of Out 1/2, Deck B still plays out of Out 3/4. This is critical: if we swapped audio routing too, the external mixer's channel assignments would become wrong mid-set.

Trigger surface TBD (see §5.5). Visual feedback: a brief "INPUT SWAPPED" toast in the Status Strip (PRD §9). No persistent indicator — the swap is the new default state until reversed.

### 5.5 Keyboard input

Keyboard is first-class for **non-performance** tasks (load, navigate, settings). Performance gestures — pitch, scratch, crossfade, EQ, gain, cue — live on the turntable and the user's external mixer, per the §1 mouse rule extended to the keyboard (the keyboard is not a substitute for a turntable).

v1.0 ships an **intentionally minimal** keymap. Performance keys (Quick Scratch, Sampler, Loops, Key Lock, Echo-Out, Zoom, etc.) get bindings *with* their feature milestone (PRD §12) — speculating an exhaustive keymap up front churns more than it helps.

**v1.0 confirmed bindings:**

| Key | Action | Milestone |
|---|---|---|
| `⌘,` | Open Preferences | M10.3 |
| `Space` | Load the **library selection** (highlighted row in the file browser, §9.7) into the **stopped, non-master deck**. If the non-master deck is currently playing, the deck pane flashes red with a "deck is playing — lift the needle" overlay; the user lifts the needle (or stops Casual Play) and tries again. See §6.4 Master deck. | M10.5 |

Every other key — performance and otherwise — is **TBD** and will be added to this table as its feature ships. The PRD does not commit to a binding before the feature exists, because we've learned that DJ keyboard muscle memory is heavily anchored to Serato / Traktor conventions and we want to choose deliberately, not preemptively.

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
- **Track time display** — elapsed and remaining, shown in the deck header. Live read of the engine's deck-rate-aware playhead, formatted `MM:SS.cc`. Standard audience-facing DJ utility (knowing "30 seconds left to mix" is foundational).
- Auto **gain trim** based on track loudness (LUFS-I or peak normalization, user choice)
- **Mouse-allowed transport surfaces** (per the §1 mouse rule and §6.1.2 / §6.1.3 below):
  - Click anywhere on the **overview waveform** column to jump the playhead to that absolute track position. Works in File mode always, and in Timecode mode only when the deck is in Panic Play (§6.1.2) — never during live timecode playback, where the needle is the position source.
  - Click on the **zoomed waveform** to fine-scrub position. **File mode only.** Disabled in Timecode mode regardless of Panic Play state (fine-scrub via mouse on a timecode-controlled deck would race with the needle and confuse the operator).
  - Click the deck header's **primary transport button** to drive Panic Play (Timecode mode, §6.1.2) or Play/Pause (Prep mode, §6.1.3). One button per deck — its role changes with engine mode. Keyboard equivalents in §5.5.

#### 6.1.2 Panic Play (dirty-needle recovery)

The most common stage failure for a Timecode-mode DJ is needle contamination — dust, a tiny scratch, accidental thumb on the cartridge — which interrupts the LFSR signal mid-song. Stickiness (§5.4) holds the playhead for 250 ms, but that's not enough time to clean a needle.

**Panic Play** is the user-driven extension, surfaced as the **deck-header primary transport button** in Timecode mode. The button is a Serato-style INT/ABS toggle:

- **Currently following the platter** → button shows ▶ `play.fill`. Tapping engages Panic Play (the deck disengages from timecode and runs internally at last-known velocity). The source pill flips to `TC · HOLD` with an amber dot, the button morphs to a vinyl glyph (`opticaldisc.fill`, amber tint).
- **Currently in Panic Play / internal mode** → button shows the vinyl glyph. Tapping cancels Panic Play (hands transport authority back to the timecode driver).

The transport state captures whatever rate the turntable was running just before, read from the most recent confident `LiftPolicy` velocity sample. While engaged the deck ignores `Locked` / `DropoutHoldRate` intents from the timecode driver — the cartridge can be lifted, cleaned, recalibrated and dropped back without the deck pausing.

The button also subsumes **Casual Play in Timecode mode**: when a track is loaded but neither timecode nor panic is active (the platter is silent, the needle isn't on yet), tapping ▶ engages Panic Play, which starts internal playback at unity (`PanicPlayState::normalise_held_rate` floors zero / negative held rates to `+1.0`). This is what "Play actually starts a Timecode-mode deck" maps to — bypassing the `DropoutHoldRate` arm that previously slammed `set_playing(false)` against any direct `engine.play` call.

Two ways out of Panic Play:

1. **Auto-resume.** When the engine sees a clean LFSR lock returning (carrier alive + confidence above the engage threshold), `Engine::drive_timecode_inputs` clears the panic flag and applies the new platter rate on the same block. The held playhead position is the new "zero" reference for the LFSR's relative motion (§5.4). The audience hears no interruption beyond a tiny `LiftPolicy` crossfade.
2. **Manual cancel** (the user tapping the vinyl button). The engine clears the engaged flag and **leaves deck transport alone**; the timecode driver decides what happens on the next block:
   - Healthy carrier present → driver re-locks, deck keeps playing at the platter rate (this is the INT→ABS hand-back).
   - Carrier silent / below threshold → driver's existing `DropoutHoldRate` arm pauses the deck on the held position (this matches the pre-M10.6c "engine pauses on held position" outcome, now produced by the natural dropout path instead of a manual `set_playing(false)` that would race the next Locked sample).

Panic Play is the **single most important reliability feature** in v1 from a "career night" perspective. PRD §2 reliability commitment is fulfilled here.

#### 6.1.3 Casual Play (pre-performance file-mode playback)

Before the actual set starts — sound-check, soundcrew dinner, opening DJ playing — the DJ may want to play music through the rig without engaging timecode at all. Maybe they want to hear how their gear sounds in the room; maybe they want to play a curated mixtape so the venue isn't silent.

**Casual Play** is the file-mode transport: drag a file onto a deck (or load via `Space` from the library, §6.4), tap the deck-header transport button (▶ `play.fill` ↔ ⏸ `pause.fill`), the track plays from the start at 1.0× rate. The deck header source pill shows `FILE`. **No pitch fader is exposed** — the user explicitly accepted "no mouse-driven pitch" in §1, and pitch in File mode is not a performance gesture. The deck plays at 1.0× the entire time; if the DJ wants pitch control they need to engage timecode. Restart / jump-to-zero lives on the Track Overview strip (§9.6.1): a click at the top of the overview seeks to 0:00 — one affordance, one place. No dedicated Restart glyph.

Casual Play is **not** sync-mixable from the keyboard — there is no "load and beat-match" workflow, no auto-crossfade, no countdown. A real mix requires the turntables. This mode exists for the *pre-set* use case only, and is intentionally limited so it doesn't grow into a controller-DJ surface.

When a Casual-Play track ends, the deck simply stops. No autoplay, no next-track logic — keeping the surface area small.

#### 6.1.4 Master deck (single-master semantics)

At any moment exactly one deck is the **master**. The master is the deck whose movement is currently authoritative for the rest of the surface — keyboard-load (§5.5) targets the *non*-master, the Phase-Drift Trail (§9.4) is colour-anchored to the master, the Status Strip shows the master's BPM, and future sync/quantise logic (v1.x) snaps to the master's beat phase.

**Derivation (engine, not user-controlled):**

1. If exactly one deck is playing, that deck is the master.
2. If both decks are playing, the master is the deck whose **transport last advanced** in the most recent UI frame. In Timecode mode "advanced" means the needle moved at a non-zero rate; in File mode (Casual Play) advancement is constant while the deck is playing, so a re-`play()` or `seek()` re-promotes that deck to master. The intent matches Traktor's deck-focus convention: whichever deck the DJ just touched is the one their next keyboard action targets.
3. If neither deck is playing, master is **sticky** — whichever was master last remains master. A freshly-launched session has no master until the first play.

The master is **not** chosen by mouse or by a focus ring. There is no `Tab` to cycle, no "click deck pane to focus." The platter (or in Casual Play, the deck's own play state) is the only authoring surface. This is a deliberate continuation of the §1 mouse rule — the deck the DJ is currently performing on tells the app what's the master; the DJ shouldn't have to tell the app twice.

**UI surface**: a single small **MASTER** chip in the master deck's header (top-right of the deck header), with the deck's BPM next to it. The non-master deck shows its BPM without the chip. No flashing, no animation — just presence vs absence.

**Load-into-non-master rule (M10.5):** pressing `Space` with a library row selected loads that file into the **non-master, stopped** deck. If the non-master deck is currently playing (rare, but possible if both decks were just playing and the DJ touched the other), the deck pane flashes red for 200 ms with a "deck is playing — lift the needle" overlay. The user lifts the needle (or pauses Casual Play), and the next `Space` succeeds. We do not auto-stop the deck — silently dropping a track on a deck that's mid-air is the kind of bug-prone helpfulness §2 rejects.

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

**Saved loop slots** (8 numbered, recallable from keyboard) — **deferred to v1.x**. The Serato workflow of "save loops to the track, recall during performance" is real but not load-bearing for v1; v1 ships ephemeral loops only. Library schema includes an empty `track_loops` table from M11 onward so v1.x can land the feature without a migration.

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

- **Hot cues** → **v2.** Confirmed in PRD planning: no v1 hot cues at all (not even a "single drop-cue per deck" lite version). The target user is a turntablist whose cueing is the needle; hot cues belong to the controller-DJ surface the v1 PRD does not address. v2 lands them in full alongside Phase support, the same release that opens up to controller-DJ workflows generally.
- **Saved loop slots** (8 numbered, recallable) → **v1.x.** v1 ships ephemeral loops only. M11 includes the empty `track_loops` table so v1.x lands without a schema migration.
- **Sampler expansion (4 → 6 slots, à la Serato SP-6)** → **v1.x** *if real-world use demands it.* v1 ships 4 slots, symmetric with the 4 Quick Scratch slots.
- **Track Preparation Mode tooling** (beatgrid editor, hot-cue prep, gain tweak UI) → **v1.x.** M10.8 ships the *mode shell* — load + play + horizontal waveform — but no editing surface. The mode is *visible* in v1; its *tools* arrive in v1.x.
- **Phase-Drift Trail "numeric-only" variant** (Preferences toggle to hide the dot trail and keep just the Δ BPM / Δ ms readouts) → **v1.x** *if real use suggests it.* v1 ships the single design and learns from how DJs actually use it.
- **Filesystem browser → full library** transition: v1.0's slim FS browser (M10.5) is intentionally minimal — folder navigation only, no metadata indexing, no crates. M11 lands the SQLite-backed library that replaces it.
- Recording → **v2**
- Streaming services (Tidal, Beatport, SoundCloud) → **v2+ or never**
- Phase → **v2**
- HID controllers → **v1.x or v2**
- Audio fingerprint recognition + persistent waveform learning → **v1.1** (post-launch)
- Apple Developer ID / notarization / auto-update → **v1.1**
- Stems / AI separation → **never**
- Video / OBS → **never**
- Software mixer / internal mixing mode (user-facing) → **never in v1/v2** (philosophy: external mixer is the product). v3 may reconsider for controller-only users.
- Mouse-driven **performance gestures** (pitch, scratch, crossfade, EQ, gain, cue) → **never** — per §1. Mouse-driven *transport* (panic-play, casual play, position navigation) → **explicitly in v1** (M10.6) and not in conflict with the philosophy.
- Cloud sync → **v3+**

---

## 7. Sampler, Quick Scratch & Instant Doubles

Dub has **three distinct sample/track-throw mechanisms** — each solves a different problem.

### 7.1 Sampler (one-shot, additive)

Classic DJ sampler. v1: **4 slots** (`A S D F`). The PRD considered matching Serato's SP-6 (6 slots), but rejected it: 4 keeps the keymap symmetric with the 4 Quick Scratch slots (`Q W E R`), and 4 has historically been enough for the target user's drop / siren / horn / vocal-stab workflows. Expansion to 6 stays on the table for v1.x if real-world use suggests it.

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
- **Fall back to auto-detect** when no grid exists. Algorithm: the M7.5 `dub-bpm::analyze_bpm` offline driver (shipped — pure-Rust spectral-flux + autocorrelation; see [`docs/SHIPPED.md#m75`](SHIPPED.md#m75)) feeds a grid placement step (anchor point + BPM, with downbeat detection from low-frequency emphasis). Same `BpmEstimator` core as the Thru streaming driver in §5.2.3 — one DSP implementation, two front-ends.
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
- **Waveforms front and center, vertical, side-by-side.** Two parallel vertical waveform columns — Deck A on the left, Deck B on the right — with time running **bottom → top**. This matches Serato Scratch Live's vertical-waveform mode and, more importantly, mirrors the platter's rotation: when the DJ pushes the record *forward*, the groove segment about to play moves *upward* under the needle, and the waveform must move the same way under the playhead so the on-screen motion never contradicts the hand. Horizontal-time waveforms (Traktor, default rekordbox, default Serato DJ) are explicitly not the model.
- **Playhead at 25 % from the top.** The user sees 25 % of what just played *above* the playhead and **75 % of what's coming up *below*** it. The waveform scrolls **upward** through the stationary playhead during forward playback — future rises from the bottom of the screen, passes through the playhead (where it is "now playing"), and continues upward into the played-region above, eventually sliding off the top. Reverse playback (manual rewind, backspin) inverts this: the waveform marches downward, future falls back into the playhead from above, freshly-recovered past pushes downward off the bottom. What's coming is more important than what's gone — most DJ apps (and even Serato's own vertical mode) put the playhead at center; this is wrong for the audience-facing DJ who needs to see *into the future* of the track. **Direction discipline:** the audio playhead's on-screen motion must match the hand's motion on the platter — forward platter rotation = waveform marches upward through the field; reverse (rewind) = waveform marches downward. The deviation from convention is the playhead's *position* (25 % top, not centre), not its *direction*.
- **Type-driven** — readable type at performance distance (1–2 m from the screen).
- **Color = function, not decoration.** Deck A and Deck B each have a single accent color; everything else is neutral.
- **No skeuomorphism, no jog wheel graphics, no fake CDJ overlays.** This is software, not a stage prop.

### 9.2 Layout (v1)

```
┌─ STATUS STRIP ──────────────────────────────────────────────────────┐
│ DUB · 48.0 kHz · LIVE · INPUT · TIMECODE  ····  CLOCK 21:47 · 🔋87% │
├─────────────────────────────────────────────────────────────────────┤
│ A · TIMECODE · The Test                     │ B · THRU · Live Capture│
│ A · 122.4 BPM · ±0.0 % · F♯m · FX —         │ B · 122.7 BPM · — · — │
│ A · 02:14.7 elapsed / 05:23.1 remaining     │ B ·  —                │
├──────┬──────────────────┬─────────┬─────────┬──────────────────┬────┤
│ ░░░░ │ ─past 25%────    │ │ phase │         │    ────past 25%─ │░░░░│
│ ░░░░ │ ▌▌▌▌▌▌▌ playhead │ │ drift │         │ playhead ▌▌▌▌▌▌▌ │░░░░│
│ ░░░░ │ ── ── ── ── ──   │ │trail │         │   ── ── ── ── ── │░░░░│
│ ░░░░ │ ── ── ── ── ──   │ │ ·  · │         │   ── ── ── ── ── │░░░░│
│ ░░░░ │ ─future 75%───   │ │ ·  · │         │   ─future 75%─── │░░░░│
│ ░░░░ │ ── ── ── ── ──   │ │  ·  ·│         │   ── ── ── ── ── │░░░░│
│ ░░░░ │ ── ── ── ── ──   │ │  ·  ·│         │   ── ── ── ── ── │░░░░│
│ over │   DECK A zoom    │ │ Δ BPM│         │   DECK B zoom    │over│
│ view │   (~4 bars)      │ │+0.3  │         │   (~4 bars)      │view│
│ A    │                  │ │ Δ ms │         │                  │ B  │
│ (thin│ forward play =   │ │ +12  │         │ forward play =   │thin│
│  full│ marches upward   │ │      │         │ marches upward   │full│
│ track│                  │ │ §9.4 │         │                  │track│
│      │                  │ │      │         │                  │    │
├──────┴──────────────────┴─┴──────┴─────────┴──────────────────┴────┤
│  FX bar (Deck A):   Echo-Out · Dub Siren · Quick Scratch · Sampler  │
│  FX bar (Deck B):   Echo-Out · Dub Siren · Quick Scratch · Sampler  │
├─────────────────────────────────────────────────────────────────────┤
│  Library / Browser                                                  │
│  [Source tree]  |  [Track list]                                     │
└─────────────────────────────────────────────────────────────────────┘
```

**Top:** the **Status Strip** (§9.3) — DUB wordmark, sample-rate, engine state, current source mode per deck (collapsed into the strip when no deck-specific info needs to be shown), wall clock, battery indicator. Persistent during the entire session.

**Below that:** **Deck headers** — three logical rows (identity / live stats / track time), two columns (deck A / deck B). Source pill, track title + artist, format, BPM, pitch, key, FX state, elapsed / remaining time. See §9.5 for the live-state column responsibilities.

**Centre:** **Two decks face each other symmetrically** — A on the left, B on the right. Each deck has a **thin overview column on its outside edge** (full track at a glance, click-jumpable per §6.1) and a **wide zoomed column on its inside edge** (~4 bars visible). Beatgrid is overlaid as faint horizontal lines on the zoomed column.

**Centre gutter:** the **Phase-Drift Trail** (§9.4) — Dub's beatmatching aid. A narrow vertical strip co-located between the two waveforms (where the DJ's eyes naturally focus during a mix). Replaces both Serato's Tempo Matching Display and Traktor's phase meter with a single grid-agnostic visualization. This is the only thing that lives in the centre gutter; it never houses controls.

The `▌` marks the playhead, fixed at 25 % from the **top** of each zoomed column. **Upcoming audio fills the lower 75 % of the column;** during forward play it rises through the playhead and slides off the top of the played-region above. Reverse playback (manual rewind, backspin) inverts this: the waveform marches downward, exactly mirroring the hand on the platter. This is the load-bearing UX commitment of the layout — on-screen motion direction must equal hand-on-platter motion direction at all times, because any contradiction between them costs the DJ a frame of cognitive translation that turntablist muscle memory cannot afford during scratch work.

The decks dominate vertical real estate intentionally. Scratch DJs spend the majority of a set staring at the waveform; everything else (FX, library) is subordinate.

### 9.3 Status Strip

A thin (≈ 28 px tall) row across the top of the window. Always visible, never interactive (clickable elements never live here — the status strip is *read-only by contract*; interaction lives in deck headers, the waveform region, and the Preferences sheet).

Left → right:

| Element | Source | Why |
|---|---|---|
| **DUB wordmark** | static | Identity. |
| **Engine state dot + sample-rate** | `DubEngine.sampleRate()` | Quick "is the engine running, at what rate" check during sound-check. Green dot + `48.0 kHz` when running; grey dot + `IDLE` when stopped. |
| **Source-mode summary** | per-deck source pill (PRD §5.1) | Optional condensed echo of the deck headers' source pills when the user needs an at-a-distance read. Hidden when both decks' source pills are already visible at their natural size. |
| **Spacer** | — | Pushes the right cluster to the edge. |
| **Wall clock** | system clock | Set-timing utility. "27 minutes until I need to be off stage." Format `HH:MM` (24-hour); user can switch to 12-hour in Preferences. No seconds — the DJ glancing at this should not be distracted by ticking digits. |
| **Battery indicator** | `IOPSCopyPowerSourcesInfo` | Critical safety: when running on battery, show `🔋 87 %` in dim amber; when battery drops below 20 %, the glyph turns red and pulses for ~3 s on transition. When plugged in, the glyph shows a power-plug variant and never alarms. **Critically: low battery never blocks or pauses playback — it warns only.** A DJ on stage cannot have the app refuse to play because the laptop is low; that's the audience's career-night moment, not a safe-default trigger. |

The Status Strip is intentionally bare. Hardware connection status, USB dropout indicator, CPU meter, and per-deck level meters are all **out of scope for v1** (level meters: PRD §9, decided in M10.3 round; CPU + USB dropout: M18 polish if at all).

### 9.4 Phase-Drift Trail (beatmatching aid)

The PRD's single most opinionated visual design decision. Replaces Serato's "Tempo Matching Display" (a row of peaks) + Traktor's phase meter (a needle/dial against a beatgrid) with one unified, grid-agnostic display.

#### 9.4.1 Problem statement

Beatmatching by ear is the bedrock skill of the target user. The visual aids most DJ apps ship have two persistent failure modes:

1. **Serato's peak rows** are a low-resolution position indicator: a 0.1 BPM mismatch takes many bars to visibly desync. By the time the DJ sees it, the tracks have already audibly drifted. The numeric BPM readout (e.g. `120.1` vs `120.0`) is consistently more useful in practice. The visual aid is *less* precise than the text it sits next to.

2. **Traktor's phase meter** trusts the inferred beatgrid. When the grid is even slightly wrong — and on real-world music with micro-timing, swing, or pre-analysis errors, it usually is — the meter lies. It says "in phase" while the kicks audibly clash. It's most likely to fail on the genres the target user plays the most: dub, reggae, dnb with shuffled snares, hip-hop with off-grid loops.

Both failure modes share a root cause: **trust in quantised abstractions (BPM numbers, inferred grid positions) instead of the actual audio.**

#### 9.4.2 Design

A vertical strip ≈ 80 px wide, occupying the centre gutter between the two zoomed waveforms (same height as the waveform region — typically 400–600 px depending on window size).

Time flows **bottom → top** matching the waveform direction discipline (§9.1): the bottom of the strip is "now," the top is "≈ 4 bars ago." A new sample is plotted at the bottom each ~33 ms (30 Hz); older samples advance upward, mirroring the waveform's motion through its playhead during forward playback.

**X-axis** (horizontal position across the strip) = current beat-phase offset between deck A and deck B, in milliseconds, centred at zero. Right of centre = deck B is *ahead* of deck A. Left = deck B is *behind* deck A. Default scale: ±60 ms — wider than the audible threshold (~10–15 ms), narrow enough that small mismatches are visible.

**Plotted at each sample:**
- A small dot at `(x = phase_offset_ms, y = time_index)`.
- Dot *brightness* = correlation confidence — a function of the cross-correlation peak height (PRD §9.4.4).
- Dot *colour* = a blend of the two decks' source-mode accent colours (deck A tint left of centre, deck B tint right, midline neutral).

**Reading the trail:**

| Pattern | Meaning |
|---|---|
| Dots stacked **vertically on the centre line** | Tempo and beat *both* matched. Hold position. |
| Dots stacked vertically, but **offset from centre** | Tempos matched but beats not aligned. Nudge the platter to centre the trail. |
| Dots forming a **diagonal slope** | Tempos *don't* match. The slope angle is proportional to BPM mismatch; the slope *direction* tells the DJ which way to move the pitch fader (slope towards the right → deck B is gaining → slow B down or speed A up). |
| **Dim / fading dots** | Low correlation — at least one deck has a sparse / a-cappella / ambient passage. The display is *honestly* signaling "I can't be sure right now." Do not trust the position; rely on your ear. |
| **Random-looking scatter** | Both decks have weak ODFs (no clear beats). Same honesty principle. |

Numeric overlays at the top and bottom of the strip:

- **Top:** `Δ BPM = +0.3` — tempo difference, slope-derived.
- **Bottom:** `Δ ms = +12` — instantaneous beat-phase offset.

Both update at the 30 Hz cadence. The numeric values are precise; the trail is the gestalt. **Together they're the answer to "where am I, where am I heading, and how confident is the display about it."**

#### 9.4.3 Why grid-agnostic

The cross-correlation operates on the **raw onset-detection function** (ODF) of each deck — the same log-band spectral-flux signal `dub-bpm` already computes (§5.2.3, M7.5 / M8.1, `crates/dub-bpm/src/onset.rs`). The ODF is what the audio *actually* does: peaks at kick / snare / clap onsets, troughs at sustains. No beatgrid is inferred; no downbeat is chosen. The cross-correlation finds the lag that best aligns the two ODFs over a rolling window of ≈ 2 bars.

Consequences:

- **A wrong beatgrid does not affect the display.** If the M11 importer pulls a Serato grid that's off by half a beat, the Phase-Drift Trail is unaffected. Beatgrid errors degrade other features (auto-loop length, FX division snapping), but not beatmatching.
- **Thru-mode (real record) is fully supported.** The M9-shipped ODF tap on the Thru source produces the same signal as a file source. The user can beatmatch a real record against a file with no special-casing.
- **Micro-timing / swing / drift is visible, not hidden.** When a drummer plays slightly ahead-of-beat in the chorus, the trail shifts slightly right *honestly* — the DJ can react.

#### 9.4.4 Implementation sketch

Lives in a new crate, **`dub-match`** (sibling of `dub-bpm` / `dub-peaks` / `dub-spectral`), per §10.

| Layer | Thread | Cost |
|---|---|---|
| ODF source | Audio thread (no change) | Already running — M8 `SpectralFrameStream` on each deck. Alloc-free verified. |
| **Cross-correlation** — for each new ODF frame, compute correlation of A's last ≈ 2 bars against B's last ≈ 2 bars across a lag window of ±1 beat (~500 ms either side at 120 BPM). | New off-RT analysis thread (`MatchStream`), one per pair of decks. | ~O(N×L) where N ≈ 200 frames, L ≈ 40 lag candidates. Order of microseconds per update. |
| Emit `MatchSample { phase_ms: f32, confidence: f32, timestamp: Instant }` at ~30 Hz to an SPSC ring. | Same off-RT thread. | Alloc-free; pre-allocated ring. |
| Render the trail. | Renderer thread (existing `WaveformView` infrastructure can be reused). | Pull the last N samples from the ring per frame; draw via Metal vertex instancing similar to `WaveformRenderer`. |

The `dub-bpm` ODF stream is already a public API; `dub-match` consumes it as a leaf. No reach-back into the audio thread.

#### 9.4.5 Trade-offs we accept

1. **Novel display, first-time interpretation needed.** DJs trained on Serato/Traktor will not read this at first glance. Mitigation: a `?` glyph next to the strip pops a one-line legend on hover (`make the trail straight, then centre it`).
2. **30 Hz update cadence** → ~33 ms minimum visible response time. Acceptable: human beat-matching reaction time is ~100 ms, audible beat-error threshold ~10–15 ms; the display is faster than the user can act on it.
3. **Centre-gutter real estate** (≈ 80 × 400 px) is reserved for this and nothing else. We considered placing it as a horizontal strip below the waveforms to free the gutter; the centre-gutter location wins because the DJ's eyes already converge there during a mix — putting the aid anywhere else costs an eye-saccade per check, which is the kind of friction Dub exists to eliminate.

A **numeric-only variant** (drop the dot trail, keep just `Δ BPM` + `Δ ms` overlays) is a future possible Preferences option but **not v1**. We ship one design first, learn from real use, then offer alternatives if needed.

### 9.5 Deck header (per deck)

Each deck header (top of each deck's column) is a three-row, two-column-aware strip:

**Row 1 — Identity.**
- Deck label (`DECK A` / `DECK B`) in the deck's accent tint.
- Source pill (`TIMECODE` / `TIMECODE · HOLD` / `THRU · LIVE` / `FILE` / `OFF`) with a dot whose colour encodes the tracking-quality indicator from §5.4: green = clean lock, amber = degraded, red = no lock / scratching / cueing (per Serato convention, red is *normal* during scratching).
- Track title + artist (live from file metadata or library, em-dash placeholder when none).
- Format chip (`FLAC · 44.1 kHz` etc.) — chip is hidden in Thru mode.

**Row 2 — Live stats.**
- `PITCH ±X.X %` — deck-rate as set by the turntable (from M5 timecode pipeline) or `±0.0 %` in File mode.
- `BPM XX.X` — track BPM, from `dub-bpm` tracker. Tentative readings shown italicized.
- `KEY` — `—` until v1.x key-detection ships (or M11-imported library metadata, whichever lands first).
- `FX` — active Smart FX state (Echo-Out / Dub Siren / off) — populated from M15 / M16 onward.

**Row 3 — Track time.**
- `MM:SS.cc elapsed / MM:SS.cc remaining` — live from the engine's deck-rate-aware playhead. In Thru mode this row is blank (no track concept).

Header height is fixed at ≈ 92 px regardless of which rows have content. Empty rows preserve their height to avoid layout reflow when source mode changes mid-set.

### 9.6 Waveform rendering

- GPU-rendered via Metal (`MTKView`).
- Two views per deck: **overview** (whole track, thin vertical strip on the deck's outside edge) + **zoomed** (≈ 4 bars, wide vertical strip on the deck's inside edge).
- **Playhead is fixed at 25 % from the top of the zoomed column**; during forward playback the waveform scrolls *upward* through the playhead at `1 / (engine_rate)` pixels per sample (future rises from the bottom, plays into the playhead, slides upward into the past above). During reverse playback the waveform scrolls *downward* — direction follows engine rate sign, never inferred or corrected. The user sees the future below the playhead, the past above. The overview's playhead marker tracks the same position as a horizontal hairline.
- **Click-to-jump on the overview column** (per §6.1) jumps the playhead to that absolute position. Allowed in File mode always; in Timecode mode allowed only while Panic Play is engaged.
- **Click-to-scrub on the zoomed column** is **File-mode only** (per §6.1). Disabled in Timecode mode regardless of Panic Play state.
- Beatgrid overlay drawn as **horizontal** lines across the zoomed column (white = downbeat, gray = beat).
- 60 fps minimum, 120 fps where supported (ProMotion).
- Timecode signal scope is also Metal-rendered (cheap — small circular buffer), shown as a small overlay on the deck (not a separate panel).
- During scratching, waveform tracks position with no visible lag relative to needle (i.e. ≤ 1 frame).
- `+` / `-` keyboard shortcuts zoom the focused deck's zoomed column in / out (Serato parity).

#### 9.6.0 Waveform baseline freeze (M10.8 cleanup)

The current Metal waveform shader is the **frozen Serato-parity baseline** for M10.8 dogfooding. It deliberately keeps the renderer simple and inspectable:

- height comes from per-pixel-column broadband `PeakChunk` max aggregation;
- colour comes from 8 log-spaced `dub-spectral` bands grouped into calibrated low / mid / high channels;
- low / mid / high anchors are tuned to the tested Serato references: pink-red kicks, green mid/presence instruments, lavender high hats, and dark neutral quiet sub-bass details;
- quiet greying is gated by broadband amplitude **and** sub-bass focus (`b0`, roughly <80 Hz at 44.1 kHz), not by "quiet" alone.

Future waveform work must be **additive and reversible** relative to this baseline. Do not reintroduce the removed HDR / bloom / tuning-panel stack or rewrite the baseline shader in-place without first preserving this version behind a small, explicit switch or an isolated follow-up commit. If a polish experiment fails, reverting that experiment should return exactly to this M10.8 baseline.

#### 9.6.1 Sizing

The zoomed column is **deliberately slim**. Scratch DJs need vertical *time-history* much more than they need horizontal *peak-detail* — a clean, narrow strip reads faster at performance distance than a wide one and leaves room for the overview, the deck-header chips, and the M10.7 Phase-Drift Trail in the centre gutter. Concretely, in the SwiftUI implementation:

| Surface | Dimension | Constant | Notes |
|---|---|---|---|
| **Zoomed column, Performance (Timecode) mode** | ≈ 80 px wide | `DubLayout.deckColumnWidth` | Slim Serato-parity strip after M10.8 waveform dogfooding; keeps kick transients readable while leaving room for overview, centre gutter, and info chips. |
| **Zoomed strip, Prep mode** | ≈ 140 px tall, full-width horizontal | `DubLayout.waveformPrepHeight` | Prep mode is single-deck and uses a horizontal scrolling playing waveform for screenshot/A-B judgement and track prep. |
| **Overview band, Prep mode** | ≈ 60 px tall, full-width horizontal | `DubLayout.deckOverviewHeight` | Whole-track waveform stacked above the zoomed Prep waveform. Same click-to-jump semantics as the vertical overview. |
| **Overview column** (M10.5c) | ≈ 36 px wide, full track top→bottom | `DubLayout.deckOverviewWidth` | Thin strip on the deck's outside edge. Shows the whole track at a glance with a playhead-bracket indicator at the current position. Click-to-jump per §6.1. |
| **Centre gutter** (M10.7) | ≈ 80 px wide | reserved | Phase-Drift Trail and nothing else. |

The **remaining horizontal space inside each deck pane** (window-half-width minus the zoomed column minus the overview column minus the centre-gutter share) is reserved for per-deck info chips that don't fit in the deck header — RPM toggle (33 / 45), key-lock indicator, beatgrid-offset readout, time-elapsed-vs-remaining secondary readout. Those are M10.x polish work and not specified individually here; the column-width discipline reserves the canvas they'll be drawn onto.

> **Why not just stretch the waveform to fill the column?**
> Two reasons. First, the waveform strip's *information density per pixel* peaks at the Serato-equivalent ≈ 140 px; past that, each additional horizontal pixel duplicates information already shown in the previous one (the peak data is one-dimensional in time — the *width* of a vertical strip is purely a visual amplitude axis). Second, a fat waveform crowds out everything else in the deck pane, leaving no room for the overview or the info chips; the slim discipline is what makes the rest of the layout possible.

### 9.7 Accessibility

- Full keyboard control (any feature reachable without a mouse — see §1 for the mouse-policy nuance).
- VoiceOver labels on all controls (best-effort in v1, full in v1.x).
- High-contrast mode (v1.x).

---

## 10. Tech stack

### 10.1 Workspace

```
dub/                                 # repo / workspace name
├── Cargo.toml                       # Rust workspace
├── crates/
│   ├── dub-engine/                  # Audio graph, transport, mixer, ThruSource, no_std-ish hot path
│   ├── dub-audio/                   # CoreAudio HAL input + output, ringbuf-buffered handoff
│   ├── dub-dsp/                     # rubato, biquads, dub-siren synth, echo-out
│   ├── dub-stretch/                 # Rubber Band FFI wrapper (separate crate for license clarity)
│   ├── dub-io/                      # symphonia-based decoders (everything in RAM, see §4.4)
│   ├── dub-timecode/                # Serato CV02 + Traktor MK1/MK2 decoder (clean-room)
│   ├── dub-thru/                    # Thru-mode source-detection classifier (§5.1.1)
│   ├── dub-bpm/                     # M7.5 — BpmEstimator (offline + streaming drivers, pure-Rust)
│   ├── dub-peaks/                   # M9 — off-RT decimator producing PeakChunk + BandPeakChunk for the renderer (M10)
│   ├── dub-spectral/                # M9.5 — shared FFT + log-band magnitude pipeline (consumed by dub-bpm + dub-peaks)
│   ├── dub-match/                   # M10.7 — Phase-Drift Trail (§9.4): cross-correlation of two decks' ODFs, off-RT
│   ├── dub-fingerprint/             # v1.1 — Chromaprint FFI + match index
│   ├── dub-library/                 # SQLite + import adapters
│   ├── dub-controller/              # HID/MIDI abstractions (placeholder in v1)
│   ├── dub-ffi/                     # UniFFI-generated bindings to Swift
│   └── dub-cli/                     # `dub` binary (smoke / play / capture / timecode-deck / scope / calibrate / thru)
├── apple/                           # M0.5 shipped — AppKit + SwiftUI shell
│   ├── project.yml                  # XcodeGen manifest (source of truth)
│   ├── Dub.xcodeproj                # generated, gitignored
│   ├── DubCore.xcframework/         # generated, gitignored — universal Rust static lib
│   ├── Dub/                         # @main AppKit lifecycle + SwiftUI views (bundle id: com.klos.dub)
│   │   ├── DubAppDelegate.swift     # NSApplicationDelegate lifecycle
│   │   ├── MainWindowController.swift # NSWindow holding an NSHostingController
│   │   ├── MainView.swift           # Top-level shell — hosts PerformanceView + Preferences sheet (M10.3)
│   │   ├── DesignSystem/Tokens.swift # Colour / type / spacing tokens — single source of truth (M10.3)
│   │   ├── Performance/             # PerformanceView, DeckHeader, StatusStrip, PhaseDriftView (M10.7), placeholders
│   │   ├── Preferences/             # PreferencesSheet (⌘,)
│   │   └── Waveform/                # Metal renderer + MTKView host (M10-B → M10.4 vertical rotation)
│   └── DubShared/                   # Swift Package wrapping DubCore.xcframework + bindings
├── scripts/                         # M0.5 shipped — Apple toolchain orchestration
│   ├── build-xcframework.sh         # cargo build (aarch64+x86_64) + lipo + xcodebuild -create-xcframework + UniFFI bindgen
│   ├── bootstrap.sh                 # one-shot: build-xcframework + xcodegen generate
│   ├── codesign.sh                  # v1.1 (placeholder)
│   └── notarize.sh                  # v1.1
├── tools/
│   └── rt-audit/                    # Static + runtime check: no alloc on audio thread
├── docs/
│   ├── PRD.md                       # ← this file
│   ├── SHIPPED.md                   # Full design history of M0 → M7 (extracted from this file)
│   ├── ARCHITECTURE.md              # How the crates fit together
│   └── LIBRARY-FORMATS.md           # Field notes on Serato / Traktor / rekordbox / iTunes / Lexicon parsing
└── README.md
```

**Notes on the tree:**

- **`dub-bpm`** (shipped in M7.5) hosts the BPM estimator. The current implementation is pure-Rust (spectral-flux ODF + harmonic-summed autocorrelation, see [`docs/SHIPPED.md#m75`](SHIPPED.md#m75)) so the crate has no system or FFI dependencies. If a future `aubio-rs` backend is added it will live behind a feature flag on this same crate, keeping any LGPL dynamic-link concern contained. Both the M7.5 offline driver (file-side fallback when imported metadata has no BPM, §8.3) and the M8 streaming driver (Thru-side live tracking, §5.2.3) build on the same `BpmEstimator` core.
- **`dub-thru`** is reserved for the **source-detection classifier** (§5.1.1's per-deck state machine that decides Timecode vs. Thru from a sliding window of input audio), not the Thru passthrough itself — that lives in `dub-engine` as `ThruSource`. The split exists because Thru *playback* is on the audio thread (and shipped in M7 in `dub-engine`), but Thru *detection* runs on a worker thread off-RT.
- **`apple/`** shipped with M0.5 — XcodeGen-managed `Dub.xcodeproj`, UniFFI-based `dub-ffi` (proc-macros, no UDL), and the AppKit-+-SwiftUI smoke screen. `scripts/build-xcframework.sh` + `scripts/bootstrap.sh` are the only entry points; everything else is gitignored. Distribution signing (a notarization-ready `codesign.sh`) is a separate post-M10.2 milestone.
- **ADRs** are not currently used. Significant design decisions live as commentary in `SHIPPED.md` and `ARCHITECTURE.md` instead; if ADRs prove valuable later they'll land as `docs/adr/`.

### 10.2 Key dependencies

| Crate | Purpose | License | Notes |
|---|---|---|---|
| `coreaudio-rs` | macOS audio I/O | MIT/Apache | Direct HAL access |
| `symphonia` | Decoding | MPL-2.0 | All formats incl. ALAC |
| `rubato` | Resampling | MIT | Sinc-based, FixedOut variant |
| `rubberband` (FFI) | Time-stretch / key lock | **GPLv3** | Forces whole project to GPL — accepted. |
| `aubio` (FFI) | Beat detection (fallback) + live tempo tracking on Thru — *not currently used* | **LGPL-3.0** | M7.5 shipped a pure-Rust baseline (see [`docs/SHIPPED.md#m75`](SHIPPED.md#m75)). Aubio is parked as a future opt-in feature backend on `dub-bpm`; if added it would be dynamically linked and confined to that single crate. |
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

Each milestone has a **demo criterion** — a single sentence describing what the user can observably do at the end. **Shipped milestones (M0 → M9) are summarized below; full design history and rationale live in [`docs/SHIPPED.md`](SHIPPED.md).**

| # | Name | Demo criterion | Estimate |
|---|---|---|---|
| **M0** | **Scaffold + CI + test discipline** | ✅ shipped — workspace, CI, RT-audit harness, branch protection, first TDD test. → [SHIPPED §M0](SHIPPED.md#m0). | 2–3 days |
| **M1** | **First Sound** | ✅ shipped — single-deck WAV playback through CoreAudio at < 8 ms latency, with property + golden + RT-audit tests. → [SHIPPED §M1](SHIPPED.md#m1). | 4–6 days |
| **M2** | **Transport (lock-free command channel)** | ✅ shipped — lock-free SPSC command channel for live `play` / `pause` / `seek` / `set_rate` / `set_gain` on deck 0; UI reads via atomic snapshots; RT-audit clean across 100k blocks + pre-staged drain. → [SHIPPED §M2](SHIPPED.md#m2). | 3–4 days |
| **M2.1** | **RT discipline + soak harness** | ✅ shipped — rt-audit under stress, 1-hour no-xrun playback at 64-sample buffer, nightly CI soak, first parser fuzz target (ID3). Milestone-internal gate. → [SHIPPED §M2.1](SHIPPED.md#m21). | 3–5 days |
| **M3** | **Format coverage + hot track loading** | ✅ shipped — MP3 / FLAC / AIFF / M4A / AAC / ALAC fully in RAM; live deck swap via `Command::DeckLoad(Arc<Track>)` + first trash channel for off-RT `Arc<Track>` disposal; sample-accurate seek across all formats. → [SHIPPED §M3](SHIPPED.md#m3). | 4–6 days |
| **M3.5** | **De-click envelope + tail-fade + offline analyzer** | ✅ shipped — 2 ms `sin²` envelope shared between transport-change declick and end-of-track tail-fade; `dub analyze` replaces subjective listening with mathematical click detection; offline `dub play -o` matches realtime for deterministic audit. → [SHIPPED §M3.5](SHIPPED.md#m35). | 1–2 days |
| **M4** | **Two decks + debug mixer** | ✅ shipped — both engine decks driveable via CLI; `Engine::master_gain` + `Command::SetMasterGain` for live master control; `--deck-b-*` mirrors of every transport flag; external-mixer routing deferred to M5.5. → [SHIPPED §M4](SHIPPED.md#m4). | 2–3 days |
| **M5** | **Timecode v1 (Serato)** | ✅ shipped (M5.1 → M5.6) — Serato CV02 controls deck position, scope inspector, stickiness, calibration, two-deck. See sub-milestones below. | 1–2 weeks |
| **M5.1** | **Timecode decoder, offline (clean-room)** | ✅ shipped — `dub-timecode` crate; analytic-signal demod of Serato CV02 in relative mode; signed rate + confidence; clean-room from xwax algorithm description; `dub decode-timecode` CLI. → [SHIPPED §M5.1](SHIPPED.md#m51). | 3–5 days |
| **M5.2** | **Audio input plumbing** | ✅ shipped — `dub-audio::AudioInput` HAL input AU + ringbuf consumer; `dub capture` / `dub levels`; SL3-verified. Closes the HAL sample-rate-match footgun (see [ARCHITECTURE §HAL invariant](ARCHITECTURE.md#hal-input-invariant--sample-rate-match-m52)). → [SHIPPED §M5.2](SHIPPED.md#m52). | 2–3 days |
| **M5.3** | **Live timecode → deck (first scratch)** | ✅ shipped — `AudioInput` → `Decoder` → deck on the audio thread; 3-layer `LiftPolicy` (amplitude gate + confidence hysteresis + sticky window) hardened across three SL3 iterations; output SR force-aligned to engine SR. **The milestone where Dub becomes a DJ app.** → [SHIPPED §M5.3](SHIPPED.md#m53). | 2–3 days |
| **M5.4** | **Calibration + scope** | ✅ shipped — `dub scope` ratatui inspector (M5.4.1) + `dub calibrate` per-rig threshold derivation (M5.4.2); `LiftPolicy` factored out so scope, calibrate, and playback share one code path. (M5.4.2's load-from-disk + fingerprint-probe machinery was later gutted in M5.4.6 — see below.) → [SHIPPED §M5.4](SHIPPED.md#m54). | scope: 1 day; calibration: 2 days |
| **M5.4.3** | **Calibration speed (industry-parity)** | ✅ shipped — single-phase calibration (lift step eliminated), shorter carrier capture, faster detection threshold, smaller startup probe. ≈ 3.5 s first-time vs. ~25 s pre-M5.4.3; matches the Traktor "drop, hit calibrate, done" feel. → [SHIPPED §M5.4.3](SHIPPED.md#m543). | 1 day |
| **M5.4.4** | **Per-deck calibration** | ✅ shipped — calibration is per-deck rather than shared across deck A/B; JSON keyed by `(device, deck_index, format)`; legacy single-deck fallback for the transition window. (Made obsolete in M5.4.6 — runtime no longer reads JSON.) → [SHIPPED §M5.4.4](SHIPPED.md#m544). | 1–2 days |
| **M5.4.5** | **Late-binding decks + non-blocking calibration** | ✅ shipped — `EngineHandle::attach_timecode_input` mid-stream attach via second trash channel; parallel calibrator workers, each owning its own ringbuf consumer; deck B's worker waits indefinitely for the takeover window. **Closes the DJ-takeover product gate.** → [SHIPPED §M5.4.5](SHIPPED.md#m545). | shipped |
| **M5.4.6** | **Always-fresh calibration (gut the fingerprint probe)** | ✅ shipped — gutted load-from-disk + fingerprint-probe (touring DJs see a new rig at every venue, so probe always mismatched and ~1.7 s was always burnt confirming what we already knew); JSON is now diagnostic-only, always-fresh on startup. → [SHIPPED §M5.4.6](SHIPPED.md#m546). | 0.5 day |
| **M5.5.1** | **Engine routing primitive** | ✅ shipped — `Engine::render_routed(rt, out, num_channels, &[Option<u32>; DECK_COUNT])` unifies internal-mixer summing (M4) and external-mixer isolation (M5.5.2) behind one strided render path. RT-safe; pure engine work. → [SHIPPED §M5.5.1](SHIPPED.md#m551). | 0.5 day |
| **M5.5.2** | **External-mixer 4-channel output routing** | ✅ shipped — M5.5.1 plumbed to CoreAudio; `OutputOptions` + `device_profiles::KNOWN_DEVICES` static table; **SL3** verified (deck A → ch 3+4, deck B → ch 5+6), Audio 6 unverified guess. Internal-mixer fallback opinionated as a dev path. → [SHIPPED §M5.5.2](SHIPPED.md#m552). | 2–3 days |
| **M5.6** | **Two-deck timecode** | ✅ shipped — single CoreAudio input AU demuxed in the IOProc into N stereo SPSC rings (`push_demuxed_frames`); both decks DJ'd live on one SL3, indistinguishable from two real records. → [SHIPPED §M5.6](SHIPPED.md#m56). | 2 days |
| **M6** | **Timecode v2 (Traktor MK1 + MK2)** | ✅ shipped — both Traktor generations through the format-agnostic M5.1 decoder; MK2 corrected from 2 kHz to 2.5 kHz (silent mis-routing bug avoided); bare `traktor` alias deliberately rejected as ambiguous. Came in well under budget. → [SHIPPED §M6](SHIPPED.md#m6). | 1 day |
| **M7** | **Thru Mode (per-deck input routing)** | ✅ shipped — per-deck `ThruSource` (single always-on software passthrough) integrated into `Engine::render_routed`; command-channel attach + third trash channel for `Box<ThruSource>`; `dub thru` CLI on the shared M5.5.2 routing path. Constant ~2.7 ms one-way latency, independent of future FX state (Option A in-chain bypass, §5.2.1 / §5.2.2). → [SHIPPED §M7](SHIPPED.md#m7). | shipped |
| **M7.5** | **BPM engine + offline analysis** | Shipped. New `dub-bpm` crate (pure-Rust spectral-flux ODF + harmonic-summed autocorrelation with fractional-step search), `BpmEstimator` streaming core + `analyze_bpm` offline driver, `Track::bpm: Option<f64>` wired in via builder so `dub-io` stays a leaf. 36 new tests in `dub-bpm` + 2 in `dub-io`, 339 workspace total. **Aubio was the original plan** but was deferred after a recon showed the `aubio-rs` crate had been stale since Jan 2023 and the LGPL dynamic-link path added install-time friction for an architectural milestone whose load-bearing artifact is the *API surface*, not the algorithm choice. Pure-Rust baseline passes synthetic click tracks at 60–174 BPM (and 128 @ 44.1 kHz) within ±1 BPM; aubio remains available as a future opt-in feature backend if real-music validation in M8 demands it. See [`docs/SHIPPED.md#m75`](SHIPPED.md#m75) for full algorithm, design history, and the four-pass bug-find loop that landed the harmonic-sum + smoothing + fractional-step combination. | 2–3 days est, 1 day actual |
| **M8** | **Auto-BPM on Thru — streaming driver** | Shipped. M7.5's `BpmEstimator` wrapped in a `BpmTracker` (estimator + hysteresis state machine + throttled tempo search) and a `BpmStream` (analysis thread + lifecycle). `ThruSource::with_bpm_tee` adds an audio-thread mono-downmix tap (alloc-free, drop-on-full) into a per-deck SPSC ring; the analysis thread reads from the ring off-RT, drives the tracker, and emits `searching → tentative → locked` transitions to a second ring the UI polls. New `EngineHandle::attach_thru_source_with_bpm_tracking` convenience method bundles the tee + thread spawn. `dub thru` prints transitions to stderr by default (`--no-bpm-track` opts out). Streaming convergence is verified by `crates/dub-bpm/src/stream.rs::click_track_streams_to_lock` end-to-end and by the in-tracker `streaming_estimator_converges_to_offline_result` cross-check from M7.5. 47 new tests, 386 workspace total. See [`docs/SHIPPED.md#m8`](SHIPPED.md#m8) for the full layered design + hysteresis tuning. |
| **M8.1** | **BPM octave fix (log-band ODF + windowed-energy picker)** | Shipped. Algorithmic point-release: replaced M7.5 single-band spectral flux with log-band-weighted flux (8 log bands, 30 Hz – 16 kHz, equal-weight sum), replaced harmonic-sum scoring with harmonic-mean over `MAX_HARMONICS = 4` multiples, replaced parabolic-vertex peak-height interpolation with windowed local-energy (5-bin sum, invariant to bin-split asymmetry), and added centroid sub-bin refinement. Fixes the M8-era hip-hop 2× regression (Diamond D at 100 BPM detected as 200 BPM) and the streaming-mode 128/64 BPM oscillation at 48 kHz, without re-introducing the half-tempo bias. New public `BpmRange` value type + `--bpm-range MIN,MAX` CLI flag on `dub thru` (and `analyze_bpm_with_range` for the offline driver) provide the escape hatch for irreducibly-ambiguous genres (dubstep 140 / 70, K-S-backbeat dnb 174 / 87). 7 new fixture-driven tests in `tests/genre_octave.rs`, new kick / snare / hi-hat drum-pattern synthetic generators in `synthetic.rs`. Workspace passes `cargo clippy --workspace --all-targets -- -D warnings` and full `cargo test --workspace`. See [`docs/SHIPPED.md#m81`](SHIPPED.md#m81) for the algorithm derivation. | 1–2 days est, 1 day actual |
| **M9** | **Live waveform capture (Thru)** | Shipped. New `dub-peaks` crate (sibling of `dub-bpm`): off-RT decimator thread that consumes a mono-downmixed audio tap from `ThruSource` and produces a growing append-only `Vec<PeakChunk { min, max, rms }>` behind a lock-free `len()` (`AtomicUsize`) plus `RwLock`-protected Vec. `Decimator` (pure online aggregator, RT-safe, alloc-free `feed`), `PeakBuffer` (shared reader with `extend_chunks(start_idx, &mut Vec)` renderer fast path — O(new chunks) per 60 fps frame, not O(total)), and `PeakStream` (joinable analysis thread mirroring `BpmStream` in shape and lifecycle). `ThruSource` refactored so the BPM tee and peaks tap share one mono-downmix pass — combining both telemetry consumers costs one extra `push_slice`, verified alloc-free. New `EngineHandle::attach_thru_source_with_peaks_tracking` + `attach_thru_source_with_telemetry` (BPM + peaks in one attach). `dub thru` gains `--no-peaks-track` (default off) and `--dump-peaks PATH` (CSV `deck,chunk_idx,min,max,rms` on shutdown) for sanity-checking capture before M10 lands; periodic stats line shows per-deck captured chunk counts. 53 new tests across `dub-peaks`, `dub-engine`, `dub-cli`. `PeakChunk` is `#[repr(C)]` 12-byte wire format — the M10 consumer contract is "cache `start_idx`, call `extend_chunks` per frame, slice goes straight into a Metal vertex buffer." See [`docs/SHIPPED.md#m9`](SHIPPED.md#m9) for the full data-layer design + M10 contract. | 4–6 days est, 1 day actual |
| **M0.5** | **Apple shell + smoke screen** | Shipped. XcodeGen-generated `apple/Dub.xcodeproj` (AppKit `@main` + SwiftUI `SmokeScreenView` inside an `NSHostingController`), `crates/dub-ffi` upgraded to UniFFI 0.28 proc-macros + `staticlib`+`cdylib`+ `uniffi-bindgen` binary, `scripts/build-xcframework.sh` building a universal (aarch64 + x86_64) `DubCore.xcframework` via `lipo` + `xcodebuild -create-xcframework`, `scripts/bootstrap.sh` orchestrating the full one-shot from a clean checkout. `DubShared/` Swift Package wraps the xcframework + UniFFI bindings. Window displays `"Dub engine OK · v<version>"` pulled live from Rust. Local "Sign to Run Locally" only — distribution signing is a separate post-M10.2 milestone. → [SHIPPED §M0.5](SHIPPED.md#m05). | 3 days |
| **M9.5** | **dub-spectral extraction + 8-band peak capture** | Shipped. **9.5a:** lifted the shared FFT pipeline (window, real-FFT, log-spaced band layout, `ln(1 + λ·|X|)` magnitude compression) out of `dub-bpm/onset.rs` into a new `dub-spectral` crate; `OnsetDetector` is now a thin shell over `SpectralFrameStream`, behaviour byte-identical (every M8.1 genre fixture passes unchanged). **9.5b:** extended `dub-peaks` with `BandPeakChunk { rms_per_band: [f32; 8] }` (`#[repr(C)]` 32-byte wire format) and `BandDecimator` consuming the same mono-downmix the existing `Decimator` already shares; `PeakStreamConfig::bands_enabled` (default on); CLI gained `--no-band-peaks` / `--dump-band-peaks PATH`. Audio thread cost is zero — no new tap, no new allocations. → [SHIPPED §M9.5](SHIPPED.md#m95). | 4 days |
| **M10** | **First waveform on screen** | Shipped. **M10-A:** extended `dub-ffi` from the M0.5 smoke surface to a real `DubEngine` UniFFI interface: `list_input_devices`, `start_thru(device, channels) throws EngineError`, `stop_thru`, `peaks_extend(deck, start_idx) -> Vec<u8>`, `peaks_len`, `peaks_chunk_duration_secs`, plus the parallel `band_peaks_*` trio so M10.1 lands without further FFI changes. `EngineError` is a flat enum with six variants (`DeviceNotFound`, `InvalidChannels`, `AudioStartFailed`, `AlreadyRunning`, `NotRunning`, `InvalidDeckIndex`). **M10-B:** Apple shell shows a live, scrolling broadband waveform driven by a Metal `MTKView` + `NSViewRepresentable`. Renderer keeps the chunk buffer as a power-of-two ring (`chunkCapacity = 2¹⁷`, ~175 s at 48 kHz / 64 samples), with triple-buffered uniforms gated by `DispatchSemaphore(value: 3)`. Vertex shader emits one instanced quad per chunk (`min`→`max` sized); fragment shader is RMS-modulated greyscale (M10.1 swaps in colour against the same vertex pipeline). `MainView` hosts the device picker + channels field + Start/Stop button + waveform + a debug overlay showing the M0.5 greeting. `apple/project.yml` now surfaces `CoreAudio`/`AudioToolbox`/`AudioUnit`/`Metal`/`MetalKit` SDK frameworks explicitly (Cargo's `cargo:rustc-link-lib` directives don't propagate when Xcode drives the link). `./scripts/bootstrap.sh && xcodebuild build -scheme Dub` produces a runnable universal `Dub.app`. → [SHIPPED §M10-A](SHIPPED.md#m10a), [SHIPPED §M10-B](SHIPPED.md#m10b). | 5 days |
| **M10.1** | **Multi-colour rendering** | Shipped. Vertex shader looks up the matching `BandPeakChunk` for each broadband instance (parallel-ring lookup via `samplesPerPeakChunk` / `samplesPerBandChunk` from new `DubEngine::sample_rate()` accessor); fragment shader mixes the 8 bands into RGB (`R = mean(b[0..2])`, `G = mean(b[2..5])`, `B = mean(b[5..8])`) with per-channel loudness compensation (`1.2 / 1.8 / 2.4`), a brightness floor for honest silence, and broadband-RMS luminance preserving the M10-B amplitude shape. Renderer gains a second `MTLBuffer` (~512 KB/deck) for the band ring. Audio-thread cost unchanged from M9.5b; renderer thread adds one atomic + one bounded memcpy per frame. `FFI_VERSION` bumps to 3. → [SHIPPED §M10.1](SHIPPED.md#m101). | 3 days |
| **M10.2** | **Waveform polish — first wave (shipped)** | Three of the seven plan bullets landed: **(1)** deck B wired identically via new `DubEngine::start_thru_two_deck(device, channels_a, channels_b)` (overlap-rejected; 4-channel input AU demuxed in the IOProc via `output_pairs=[(0,1),(2,3)]`; `VSplitView` stacks two `WaveformView`s when both pairs are configured), **(2)** three palette presets (Serato-faithful = M10.1 default, high-contrast = squared band mix, monochrome = RMS-only) with a paintpalette menu in the toolbar, **(3)** honest silence + clipping rendering — vertex shader emits per-instance flags, fragment shader paints clipped bars solid red and silent stretches as a thin neutral hairline. `FFI_VERSION` bumps to 4. → [SHIPPED §M10.2](SHIPPED.md#m102). | 3 days |
| **M10.2 remainder** | **Waveform polish — superseded by M10.5h–m** | All four originally-deferred bullets from this row (`SHIPPED.md` §M10.2 *What it does not ship*) have been re-homed onto the M10.5h–m HDR-bloom ladder below: **onset glow** → M10.5l (now riding the M10.5h HDR pipeline, true additive overshoot rather than SDR additive blending); **beat-aware saturation** → M10.5m first half (same `onset_trail` FFI accessor as M10.5l); **constant-Q bass split (9-band)** → M10.5m second half (still the gnarliest piece; deferred until M11 lands real DJ-curated content to validate the colour change against); **mip pyramids** → M10.5k (now writes through the M10.5j sidecar so the pyramid is on-disk too, not just an in-RAM compute per load). The new ladder also adds three pieces that weren't on the M10.2 remainder list and turn out to be load-bearing for "great, not mediocre" on 2026 hardware: M10.5h (HDR off-screen target + separable Gaussian bloom + ACES tonemap, the substrate that makes onset glow look like glow rather than colour saturation), M10.5i (continuous filled-envelope geometry, so the bloom lights up a smooth shape rather than a stack of quads), and M10.5j (sidecar cache so a re-load is ~1 ms instead of ~150 ms). | — (retired; see M10.5h–m) |
| **M10.3** | **Performance shell** | Launching `Dub.app` shows the real Performance View (per §9.2): a thin status strip, two two-row deck headers, the Metal waveform in the wide centre region, and correctly-sized placeholders for the FX bar (lit by M15 / M16) and library (lit by M11). The dev toolbar (device picker / channels / palette) moves behind a `⌘,` Preferences sheet so the performance surface stays mouse-free at rest. `apple/Dub/DesignSystem/Tokens.swift` becomes the single source of truth for colour / type / spacing; the Figma file documented in §9 reverts to a reference artefact (it does not gate any future UI work). Deck-header BPM / pitch / key / FX columns render as `—` placeholders until their FFI accessors land — surfacing the M8 BPM tracker over UniFFI is a trivial follow-up, pitch / key / FX wait on M13 / M14 / M15. Snapshot tests (`swift-snapshot-testing`) deferred to M18 polish; the M10.3 demo is visual eyes-on. | 3 days |
| **M10.4** | **Vertical waveform + symmetric two-pane layout** | Two bugs in the M10.3 build, fixed together. **(a)** the Metal renderer is rotated from horizontal to **vertical** per §9.1 (forward play = waveform marches upward through the playhead at 25 % from the top; reverse play = marches downward; direction follows engine rate sign with no inference). Touches `Shaders.metal` (vertex shader emits Y-instanced quads), `WaveformRenderer.swift` (buffer layout + view projection), `WaveformView.swift` (frame sizing tall not wide), and `PerformanceView.swift` (waveform region becomes `HSplitView` of two tall columns). **(b)** Symmetric layout invariant: both deck waveform panes are always rendered side-by-side. In single-deck mode (deck B chB empty in Preferences) deck B's pane shows an idle placeholder matching the deck B header's `OFF` state instead of vanishing. Status strip gains live battery + wall-clock per §9.3 (`IOPSCopyPowerSourcesInfo`-driven battery, system clock for wall time). **Demo criterion:** every screenshot from M10.4 forward is in the canonical orientation. | 1–2 days |
| **M10.5** | **File playback dev loop** | The dev-loop unblocker — Dub becomes testable without an SL3. Splits into M10.5a (Rust + FFI) and M10.5b (Apple shell). **M10.5a — shipped:** new `DubEngine` surface (`start_engine` for output-only sessions, `load_track`, `play`, `pause`, `seek`, `position` → `PositionInfo`, `track_info` → `TrackInfo`); `dub-peaks` gains `compute_offline_peaks` so whole-track peaks compute synchronously at load time; the FFI's per-deck `PeakSource` enum routes `peaks_extend` through either the live Thru stream (M9) or the offline File buffer (M10.5a) transparently; `FFI_VERSION` 4→5. **M10.5b — shipped (Apple shell):** auto-detect lifecycle (multi-channel input → Timecode mode via `start_thru_two_deck`; built-in only → Prep mode shell via `start_engine`); single-pass renderer refactor (`chunksAbovePlayhead` uniform, vertex shader linear y-map across full NDC, M10.4 behaviour preserved when no future peaks are present); drag-and-drop a file from Finder onto either deck pane → loads + deck header switches source pill to `FILE` and populates title / duration / track-time; 30 Hz position polling drives the deck-header time row; slim FS browser replaces the `LIBRARY — M11` placeholder (folder picker + single-click selection, **no double-click load**); `Space` loads the FS-browser-selected file into the non-master, stopped deck per §6.4 (or into deck A in any single-deck mode — Prep, single-channel Timecode — since "non-master" isn't meaningful when only one deck exists). If the target deck is playing, the pane flashes red for 200 ms with a "deck is playing — lift the needle" overlay; **master-deck tracking** wires up per §6.4 with a `MASTER` chip in the master deck's header. Preferences sheet is auto-apply: changing mode / device / channels restarts the engine immediately, no Apply button. App auto-starts on launch in the auto-detected mode. **Auto-detect is permission-safe**: it routes through `DubEngine::has_external_audio_interface` which queries CoreAudio transport-type metadata only (USB / Thunderbolt / FireWire / PCI / AVB) — `listInputDevices` is *not* called when Prep mode is picked, so the macOS microphone-permission prompt only ever fires when the user explicitly engages Timecode mode against an external interface. Renderer gains a per-deck **peak-generation counter** (`DubEngine::peaks_generation`, atomic, survives stop/start cycles) so a Thru → File swap on a drag-and-drop load forces the renderer to reset its ring + cadence cache before re-ingesting from the new source — without this signal the length-monotonicity heuristic gets stuck rendering stale Thru chunks indefinitely. `FFI_VERSION` 5→7 (one bump for `peaks_generation`, one for `has_external_audio_interface`). **No library DB, no metadata indexing, no crates, no other keyboard transport, no overview waveform** (that's M10.5c) — those are M11 / per-feature future milestones. | 4–5 days |
| **M10.5d** | **Background load (decode + peaks off-thread)** — *shipped* | The perceived "loading is slow" pain was the FFI's `load_track` doing synchronous `Track::load_from_path` (symphonia decode → `Vec<f32>` of the whole file) plus `compute_offline_peaks` (broadband + 3-band ring fills across all samples) **under the engine-state mutex** on the SwiftUI main actor. Two compounding effects: (1) the call blocked the main actor for ~50–300 ms depending on track length, freezing the UI; (2) the engine-state mutex stayed held throughout, so every concurrent `position()` / `peaks_extend()` / `track_info()` call (the 30 Hz UI poll + waveform poll) blocked behind the loader too — Swift-side dispatch alone would not have helped. **Rust fix** in `crates/dub-ffi/src/lib.rs`: split `load_track` into three phases. Phase 1 takes the mutex briefly to verify `EngineState::Running`. The guard drops. Phase 2 does the slow decode + peaks compute **mutex-free** — the rest of the API stays responsive throughout. Phase 3 re-acquires the mutex, re-checks `Running` (the engine could have been stopped during decode; if so, the freshly-built `Arc<Track>` + peak vectors drop on the caller's thread, harmless), then installs the new track + peaks and bumps `peak_generation_seq` while still holding the guard (no torn-read window — a renderer that sees the new peaks also sees the new generation). The generation atomic lives on `DubEngine` directly, not inside the `Mutex<EngineState>`, so the access doesn't recurse. **Swift fix** in `apple/Dub/MainView.swift` + `Performance/PerformanceView.swift`: `WaveformAppModel.loadTrack(side:url:)` becomes `async`, dispatches the FFI call onto a `Task.detached(priority: .userInitiated)` so it doesn't pin the SwiftUI main actor either. New `DeckState.isLoading: Bool` tracks in-flight loads; concurrent load on the same deck red-flashes the deck pane and surfaces "Deck *X* is already loading — wait or load onto the other deck". Optimistic UI: the new file's title fills in immediately and the deck-header source pill flips to a new `Source.loading` variant ("LOADING…", amber dot) before decode starts — the user sees the deck respond to the drop instantly, even though the audio swap lands ~tens of ms later. A *replace*-load (new file decoded while a previous one is resident) keeps the old waveform + transport toggle live during decode and swaps in atomically when `peak_generation_seq` bumps. `loadBrowserSelectionIntoTargetDeck()` becomes `async` to match; the Space-key NSEvent handler awaits inside its existing `Task { @MainActor in ... }` wrapper. No FFI bump. xcodebuild clean. | 0.5 days |
| **M10.5e** | **Waveform polish** — *shipped* | The "ugly waveform" pain: linear amplitude makes typical -14 LUFS music live in the inner ~30 % of the deck column; uniform brightness across past/future kills the depth cue from the bottom→top scroll; thin RMS-driven palette saturation washes out under projector lighting. **Shader-level** fixes in `apple/Dub/Waveform/Shaders.metal`: (1) **soft amplitude compression** `displayAmp = sign(x) * |x|^0.55` applied to `lo` / `hi` *after* the honest-state `clipping` / `silence` flags read the raw values — peaks at 0.3 now render at ~0.50, peaks at 0.7 at ~0.82, and an already-clipped 1.0 stays at 1.0. Visually fills the column on most masters without lying about the underlying signal. (2) **Past-region dim** routed through `VertexOut.flags.w`: the vertex stage sets it to 1.0 for chunks above the playhead, 0.0 below; the fragment multiplies the final RGB by `mix(1.0, 0.62, isPast)`. Applied uniformly to all three palette branches *and* to the honest-state clipping/silence colours so the depth cue stays consistent across visualisation modes. (3) **Brighter luminance floor**: the final RMS-driven luminance clamp moves 0.45 → 0.55 with a slightly gentler gain (1.6 → 1.4) so brick-walled tracks don't pin every chunk to 1.0 — preserves transient contrast through the loud parts. The Serato-faithful palette's `normaliseColour` floor lifts 0.45 → 0.55; the monochrome palette's intensity floor lifts 0.35 → 0.45. **SwiftUI overlay** in `apple/Dub/Waveform/WaveformView.swift`: faint zero-crossing hairline (`DubColor.divider.opacity(0.55)`, 1 px) along the amplitude=0 axis — vertical line at mid-width in vertical orientation, horizontal line at mid-height in horizontal (Prep) orientation. Layered under the deck-tinted playhead overlay so the playhead always wins where they cross. Helps the eye read symmetry around silence and gives sparse-waveform sections an anchor. No FFI changes; no shader uniform changes (everything piggybacks on existing `Uniforms` / `VertexOut`). xcodebuild clean. | 0.5 days |
| **M10.5f** | **Waveform 2× zoom-in** — *shipped* | The deck-column waveform was too zoomed-out at the M10.5b sizing: ≈ 4 chunks per pixel meant ~6 s of audio crammed into the visible future region, hard to read transient relationships at mix-in time. One-line fix in `apple/Dub/Waveform/WaveformRenderer.swift`: `nonisolated private static let chunksPerPixel: Double = 4.0` → `2.0`. The constant feeds both (a) the renderer's per-frame `chunksVisible` math (drives the M10.4 NDC mapping in `Shaders.metal`) and (b) `WaveformRenderer.secsPerPixel(sampleRate:)` (drives the M10.6a click-scrub gesture's px → secs conversion), so the click-scrub gesture stays calibrated automatically. The change exposed a latent aliasing pattern — see M10.5g for the follow-up. No FFI changes. xcodebuild clean. | 0.1 days |
| **M10.5g** | **Waveform anti-alias + temporal smoothing** — *shipped* | The remaining ugliness after M10.5e + M10.5f was a **"venetian blind" stripe pattern** between adjacent chunks. Two compounding root causes: (1) M10.5f's 2× zoom-in put each chunk's quad at ≈ 0.5 px tall on the time axis, and the pipeline had **no MSAA**, so amplitude-edge rasterisation stepped in hard integer-pixel jumps; (2) per-chunk min/max are inherently jittery across consecutive chunks at the engine's native 64-sample cadence (≈ 1.45 ms / chunk at 44.1 kHz), so neighbouring rows drew quads with slightly-different widths and a 1–2 px height — the eye sees the row boundaries as stripes. **Shader fix** in `apple/Dub/Waveform/Shaders.metal`: per-instance vertex stage now reads `chunks[iid-1]`, `chunks[iid]`, `chunks[iid+1]` (clamped at `iid==0` and `iid==chunksVisible-1`) and convolves min / max / rms with a [1, 2, 1] / 4 Gaussian kernel. The result drives the rendered quad and `VertexOut.rms`; the honest-state `clipping` / `silence` flags continue to read the *raw centre* chunk so a single hot or silent chunk still lights up unattenuated — smoothing is visual-only, never on the depth-of-information surface. The temporal lowpass softens chunk-to-chunk amplitude jitter that the eye reads as stripes, without changing the broad envelope shape the DJ uses to read transients. **Pipeline fix** in `apple/Dub/Waveform/WaveformRenderer.swift` + `apple/Dub/Waveform/WaveformView.swift`: 4× MSAA enabled end-to-end. New `nonisolated public static let WaveformRenderer.sampleCount = 4` is referenced both by the `MTLRenderPipelineDescriptor.rasterSampleCount` (renderer-owned pipeline) and `MTKView.sampleCount` (host-owned view); Metal validates these match at draw time. Cost on Apple Silicon is negligible — the multisample texture sits in tile memory, the resolve happens at the end of the render pass, no extra command-encoder plumbing required. The combination produces a continuous, smoothly-shaded envelope at all zoom levels instead of the previous stripe pattern. No FFI changes; no shader uniform changes. xcodebuild clean. | 0.5 days |
| **M10.5h** | **HDR + bloom render pipeline** — *shipped* | The single biggest visual upgrade in the M10.5 polish ladder. Before: single-pass renderer writing straight to `bgra8Unorm`, fragment colours clamped at 1.0, no headroom for transient overshoot, no post-processing. Mediocre by 2026 standards — Serato / rekordbox / djay-Pro all bloom transient peaks in HDR and tonemap on composite. After: **five-pass HDR pipeline** in the renderer with sub-pixel-accurate MSAA on the offscreen primary, real Gaussian bloom on transient overshoot, and ACES tonemap on the final composite. **What shipped:** (1) `Shaders.metal` waveform fragment gains an HDR overshoot block — `hdrBoost = in.rms * in.rms * 3.5` multiplies the post-luminance colour with a quadratic curve calibrated against real-music RMS distributions (typical loud mid rms ≈ 0.30 → boost 0.32 → clear halo; transient peak rms ≈ 0.45 → boost 0.71 → strong halo; quiet pad rms ≈ 0.15 → boost 0.08 → faint wash; silence rms ≈ 0.05 → boost 0.009 → no bloom). The overshoot is applied *before* the past-region `pastDim` multiply so past transients still glow proportionally (just dimmer). **Calibration notes:** (a) the first M10.5h cut used `max(rms - 0.55, 0) * 1.8` which only activated on synthetic/clipping-level material; the user smoke test showed zero visible bloom on a real DJ mix because broadband dub-peaks RMS for real DJ content lives in [0.1, 0.45], never reaching the 0.55 threshold. The quadratic above activates earlier and ramps softly — the visible halo on real music is now the calibration target, not the synthetic peak. (b) Second smoke test revealed bloom was visible but the *colour* was indistinguishable — kick / mid / hi-hat all glowed the same cream-grey. Root cause: the M10.1 `bandMix` was channel-balanced to produce `(0.5, 0.5, 0.5)` ≈ grey on balanced content, then `normaliseColour` preserved the greyness. Re-wrote `bandMix` to ratio-normalise the three RGB channels (rBass, rMid, rHigh on a simplex with rBass + rMid + rHigh = 1), **square** the ratios to amplify the dominant band, re-normalise, then map onto three vibrant anchor colours — kick red-orange `(1.00, 0.35, 0.10)`, mid green `(0.20, 0.95, 0.35)`, hi-hat cyan-blue `(0.25, 0.55, 1.00)`. A bass-dominant chunk now reads as warm red-orange instead of brown-grey; a hi-hat-dominant chunk reads as cool blue instead of off-white. (c) Third smoke test revealed the bass-dominant chunks were rendering **yellow** instead of red. Root cause: two compounding bugs in the (b) anchors / curve. (1) The "kick red-orange" anchor `(1.00, 0.35, 0.10)` is in fact warm orange — the 0.35 in the green channel means kick + mid sum has the form `(kₖ·1.0 + kₘ·0.20, kₖ·0.35 + kₘ·0.95, …)`; for a typical real-music chunk where the squared-ratio split is ~0.62 / 0.34 / 0.04 between bass / mid / high, the R and G channels both land near 0.58 → **yellow** by definition (equal R and G with low B is yellow on every monitor). (2) Single-square `pow(ratio, 2)` boost wasn't aggressive enough on real DJ ratios: a 0.45 / 0.35 / 0.20 input chunk became 0.62 / 0.38 / 0.13 after square + renorm — the runner-up (mid green) still contributes ~38% of the colour, far too much to read as "the kick won." Fix: (1) anchors purified to **near-spectral primaries** — kick `(1.00, 0.10, 0.05)` (almost pure red, kept the 0.05 / 0.10 warm tint so it doesn't look like an LED), mid `(0.10, 1.00, 0.15)` (near-pure green), hi-hat `(0.05, 0.30, 1.00)` (cool blue with a touch of cyan). The crossbleed budget per channel is now ≤ 0.30 so two simultaneous bands can no longer sum to a confused secondary. (2) **Square twice with a renormalise between** (effectively `pow(ratio, 4)` while keeping the simplex sum at 1) — a 0.45 / 0.35 / 0.20 input becomes 0.86 / 0.13 / 0.01, the dominant band wins by ~7×, the runner-up contributes ~13% so the perceived hue is unmistakably "the dominant band's colour." The chunk-by-chunk colour now changes legibly as the music's spectral balance shifts — a four-on-the-floor track shows red flashes on the kicks, green sustains on the chord beds, cyan stipple on the hi-hats. (d) Fourth smoke test revealed kicks and chord beds were now reading correctly, but **hi-hat sections never appeared blue anywhere on the track**. Root cause is a property of real music spectra, not the colour mapping: music follows a roughly **pink-noise envelope** (~-3 dB/octave roll-off), so even on chunks where the hi-hat is musically "the transient" — i.e. the moment the DJ's eye should track — the raw band-energy in the high band sits ~15–18 dB below the bass band because the bass guitar / sub is still ringing through off-beats. The high band literally never wins the double-squared ratio contest under raw-energy comparison. Fix: **inverse-pink-noise weighting** before the ratio normalise step. `bandMix` now multiplies the mid band by 2× and the high band by 5× before the simplex projection (the numbers picked by inverting the average band-energy slope across the M8.1 dub / hip-hop / techno fixture set so all three bands sit within ~3 dB of each other after weighting). The dominant-band-still-wins behaviour from calibration-3 is preserved because the double-square step lives downstream; on a kick-heavy chunk the bass still wins by 7× post-weighting, but on a hi-hat-prominent off-beat the high band can now actually take the win. Initial acceptance target: a typical drum-and-bass break shows red on the kick beats, blue stipple between the kicks (hi-hat alone), and green wash on melodic sustains — all three colours visible within a single 8-bar pattern. (e) Fifth smoke test of the calibration-4 weighting revealed the **entire track now read as blue** — overshot in the opposite direction. Root cause: I was treating the band values as if they were raw linear-amplitude RMS, but `dub-spectral` (the upstream FFT pipeline shared by `dub-bpm` and `dub-peaks`) already applies μ-law-style compression at the per-bin level: `ln(1 + 1000·|X|)`. The compression curve was *specifically* tuned in M8.1 to stop hi-hats out-voting kicks in the ODF — a kick bin at |X|≈1.0 compresses to ln(1001)≈6.9 and a hi-hat bin at |X|≈0.1 compresses to ln(101)≈4.6, collapsing a 20 dB linear gap into a 1.5× compressed gap. So by the time `bandMix` sees the per-band RMS values, the three bands are already within ~2× of each other in compressed space — my ×2 / ×5 linear-pink correction was double-compensating and flipping the contest to high-always-wins. Fix: dial the multipliers way down to **mid ×1.2 and high ×1.8** — just enough residual lift to overcome the (a) fewer-bins-in-the-bass-band asymmetry and (b) continuous-bassline-ringing asymmetry that the M8.1 spectral compression doesn't fully neutralise. Sanity-checked against three reference chunks in compressed space: a kick-dominant chunk (0.5/0.3/0.2 band RMS) yields 65% red after the full pipeline; a chord-bed-dominant chunk (0.3/0.4/0.2) yields 69% green; a hi-hat-dominant chunk (0.2/0.3/0.4) yields 94% blue. Initial acceptance of (e): red kicks, green sustains, **and** blue hi-hat off-beats all coexist within the same 8-bar pattern — the colours now actually reflect the dominant frequency band of each chunk instead of one band dominating the whole track. (f) Sixth smoke test revealed the colour palette was now *present* but *unstable*: "the same kick in different places looks different." Looking at a single kick blob (~70 broadband chunks tall in the deck column at 2 chunks-per-pixel), the chunks were painted in random patches of red / orange / pink / cyan rather than reading as one stable red transient. Root cause: the M10.5g 3-tap `[1, 2, 1] / 4` smoothing block in `waveformVertex` smooths min/max/rms across neighbour chunks, but the **band-RMS values that drive `bandMix` were read as a single point sample** (`BandPeakChunk b = bands[bandRing];` at line 317 of the M10.5h shader). Each band-RMS is an STFT-frame measurement with 1024-sample FFT × 512-sample hop, and frame-to-frame variance from (1) window phasing on transient placement, (2) quantised band-bin boundaries, and (3) the μ-law compression curve's non-linearity around the threshold makes adjacent band chunks differ by ~10–20% even on stationary content. The double-square in `bandMix` then amplifies that 10–20% input noise into ~40–80% output ratio swings — which paints visibly different colours. Fix: **mirror the M10.5g amplitude treatment onto the band values**. The vertex shader now reads `bandLocal-1, bandLocal, bandLocal+1` (clamped at zero on the left and at the rightmost-visible band-chunk index on the right so we never read stale data past the visible window) and convolves all 8 bands with the same `[1, 2, 1] / 4` Gaussian before forwarding to the fragment shader. At samplesPerBandChunk = 512 (default), the 3-tap kernel spans ~35 ms at 44.1 kHz — wider than a typical kick transient (~20 ms) so all chunks inside a single kick read the same hue, narrower than a 120 BPM kick-to-kick spacing (500 ms) so adjacent kicks never smear together. The honest-state `clipping` and `silence` flags continue to read the *raw centre* broadband chunk, never the smoothed bands, so a single hot or silent chunk still lights up unattenuated — smoothing is visual-only, never on the depth-of-information surface. **Final acceptance:** within one kick blob the colour is now stable across all chunks; across the track, kicks at different bars now read as the same warm-red even though their immediate spectral context differs, because the per-chunk noise component is smoothed out and only the deliberate musical-content differences remain. Plus a Rec. 601 luma-anchored **saturation boost** (`amount = 1.45`) added to `compositeFragment` post-ACES — every filmic tonemap desaturates highlights as a side-effect of the shoulder roll-off, which is correct for film but visually muddy when hue is encoding frequency-band information; the post-tonemap extrapolation compensates without changing the on-pixel luminance. (2) New shader trio: `fullscreenVertex` (big-triangle, zero-buffer, generates UV in `[0,1]²` per the standard post-process pattern), `brightPassFragment` (`max(c - 1.0, 0)`), `gauss1dFragment` (9-tap separable Gaussian, sigma ≈ 2 px on half-res; weights from the LearnOpenGL bloom reference matching GPU Gems 3 chapter 14 — applied **twice** in the renderer's draw loop so the stacked Gaussian has effective sigma √2× ≈ 2.83 px on half-res = ≈ 11 px halo radius in drawable space, the sweet-spot for "glowing peak" vs "smeared envelope"), `compositeFragment` (HDR + bloom × 1.5, ACES Narkowicz fit `(2.51x² + 0.03x) / (2.43x² + 0.59x + 0.14)` for the shoulder roll-off so a bright transient maps to "very bright but not blown-white" SDR instead of clipping at flat white). (3) `WaveformRenderer.swift` grew from one pipeline state to **four**: `waveformPipeline` (re-targeted from `bgra8Unorm` to `rgba16Float` MSAA), `brightPassPipeline` (`rgba16Float` single-sample), `gaussPipeline` (same — used twice per frame with different direction-uniform offsets), `compositePipeline` (back to `bgra8Unorm` for the final drawable write). New `MTLPixelFormat.rgba16Float` colour-attachment format constant + `bloomDownscale = 2` for the half-res bloom textures. (4) Four offscreen textures with lazy size-tracked allocation in `ensureOffscreenTargets(drawableSize:)`: `hdrPrimaryMS` (MSAA, drawable-sized, `.storageModePrivate`, render-target-only — written by waveform pass, MSAA-resolved at end-of-pass and discarded), `hdrPrimaryResolved` (single-sample, drawable-sized, `[.renderTarget, .shaderRead]` — read by bright-pass + composite), `bloomA` + `bloomB` (half-res ping-pong, `[.renderTarget, .shaderRead]`). All allocated `.private` for now; Apple Silicon `.memoryless` for the MSAA target is a follow-up optimisation (Intel macs don't support it, and the per-deck savings are ~10 MB at typical sizes — not yet worth the `device.hasUnifiedMemory` branch). (5) Seven-pass `draw(in:)`: Pass 1 waveform → `hdrPrimaryMS` with `storeAction = .multisampleResolve` into `hdrPrimaryResolved` (lets Apple Silicon keep MSAA samples tile-resident, never spilling to system RAM); Pass 2 bright-pass reads `hdrPrimaryResolved`, writes `bloomA`; Pass 3 horizontal Gaussian reads `bloomA`, writes `bloomB`; Pass 4 vertical Gaussian reads `bloomB`, writes `bloomA`; Passes 4b/4c re-run H/V on the already-blurred result for the √2× sigma stack; Pass 5 composite reads (`hdrPrimaryResolved`, `bloomA`), tonemaps, writes the MTKView drawable. All passes share one command buffer — Metal's automatic intra-buffer dependency tracking serialises them. The Gauss direction uniform lives in a 32-byte `MTLBuffer` populated once in init (horizontal `(1, 0)` at offset 0, vertical `(0, 1)` at offset 16; 16-byte stride is the conservative constant-buffer offset alignment across all Metal-capable Apple GPUs). The empty-waveform case (no chunks yet) still hits all five passes so the drawable clears between e.g. track-load and first chunk arrival, instead of holding a stale frame. (6) `WaveformView.swift` drops `mtkView.sampleCount = WaveformRenderer.sampleCount` — MSAA moved off the drawable, the MTKView allocates a normal single-sample drawable + we feed it composited SDR. **Memory cost**: ~12 MB per deck at typical 300×1080 px (10 MB MSAA + 2.5 MB resolved + 2× 0.6 MB bloom). **Acceptance**: a four-on-the-floor track shows a clearly-visible halo around confirmed-loud chunks (≈ 4–6 px outside the envelope) that swells on kicks and fades on sustains; ambient passages render exactly as M10.5g (no overshoot, no bloom, identical SDR colours). Workspace `cargo test --workspace` clean; xcodebuild clean; no new lints; no FFI changes. | 1.5 days |
| **M10.5i** | **Continuous filled envelope** — *shipped* | Eliminates the "looks like a peak meter, not a waveform" problem from M10.4 / M10.5b. **Before:** the renderer issued one `drawPrimitives(.triangleStrip, vertexCount: 4, instanceCount: chunksVisible)` call where each instance painted an independent quad — ~1500 stacked rectangles per frame stacking to form the envelope. The horizontal seams between adjacent chunk quads were perceptible at any zoom, especially after M10.5h's HDR + bloom made the brightness of each quad pop. **After:** the renderer issues two `drawPrimitives(.triangleStrip, vertexCount: 2 * chunksInRegion)` calls — one per region (past + future) — and each region renders as one **connected** triangle strip whose vertices encode `(amplitudeEdge, timeCentre)` pairs. Between any two adjacent chunks the strip's topology automatically fills the gap with two triangles forming a trapezoidal slice; K chunks produce a single C0-continuous filled shape spanning K-1 slices. **What shipped:** (1) `Shaders.metal` vertex stage refactored from instanced quads (4 verts × N instances) to per-region strip (2 verts × N chunks). The shader signature drops `instance_id` entirely and decodes per-vertex data from `vertex_id` alone: `chunkIdx = vid >> 1u`, `isMaxEdge = vid & 1u`. Same M10.5g `[1, 2, 1] / 4` smoothing kernel on min/max/rms preserved (still essential — without smoothing the strip's envelope shows single-chunk spikes between neighbours). Same M10.5h calibration-6 3-tap kernel on the band-RMS values preserved (essential for stable colours along the ribbon). The honest-state `clipping` / `silence` flags continue to read the *raw centre* chunk so a single hot or silent chunk still paints unattenuated — they sit per-vertex but are constant across the two vertices that share a chunk index, so rasteriser interpolation collapses to the same per-instance behaviour the M10.2 design assumed. (2) Per-region NDC mapping: the renderer signals which region each draw belongs to by setting `chunksAbovePlayhead = chunksVisible` for the past draw (>0) and `chunksAbovePlayhead = 0` for the future draw; the shader branches on `chunksAbovePlayhead > 0`. Past draw maps `chunkIdx ∈ [0, chunksVisible)` onto `y ∈ [+1.0, +0.5]` (oldest at top). Future draw maps onto `y ∈ [+0.5, -1.0]` (newest at bottom). The orientation-axis remap (M10.5c-b horizontal mode) lives downstream of the NDC mapping so Prep mode automatically gets the same continuous-envelope upgrade without a second code path. (3) The `± dTime` time-edge offset is gone — each vertex sits exactly at its chunk's time centre. The triangle-strip topology between adjacent chunks fills the inter-chunk gap *implicitly* via the two triangles that share the centre-aligned vertices on each side. (4) `WaveformRenderer.swift` packs **two** `WaveformUniforms` slots per inflight uniform buffer (stride = 64 bytes, rounded up from the natural 40 to the 32-byte constant-buffer alignment Metal guarantees across all Apple GPU families). Each frame fills both slots: slot 0 with the past region's `(chunkOffset, chunksVisible, chunksAbovePlayhead = chunksVisible, bandChunkOffset)` and slot 1 with the future region's `(chunkOffset, chunksVisible, chunksAbovePlayhead = 0, bandChunkOffset)`. The Pass-1 draw loop then binds the same buffer at offset 0 for the past draw and offset 64 for the future draw — Metal's `setVertexBuffer(_, offset:_, index:_)` is the canonical pattern for this. Each draw's `vertexCount = 2 * chunks` is computed independently; zero-chunk regions (Thru mode → no future, or track-just-started → no past) skip their draw call entirely. The chunks + bands buffers are bound once before the first regional draw and stay bound through both. (5) Bloom + tonemap chain (Pass 2–5) is unaffected — those passes read the rasterised HDR primary as a texture and don't care about the source topology. So the M10.5h bloom now lights up a **smooth** envelope instead of a stack of rectangles, which is the single biggest perceptual upgrade in this milestone: the halos look like glow around a real waveform rather than around the stacked top edges of 1500 individual peak-meter bars. **Subtle gotchas resolved:** (a) The per-vertex band-RMS lookup must read the *centre* chunk's band index (not an edge-vertex's) — the shader uses `chunkIdx * samplesPerPeakChunk + samplesPerPeakChunk/2` so both vertices of a chunk read the exact same band ring slot. Both vertices' interpolated outputs collapse to identical colours and the colour ribbon is C0-continuous along the strip. (b) Past + future stay separate strips because their NDC time ranges aren't contiguous (the playhead lives between them); a single combined strip would need either a degenerate-triangle bridge or a shader branch on `time > 0.5` — both more complex than two clean draws. (c) Edge-case `chunksVisible == 1`: a region with 1 chunk emits 2 vertices and renders 0 triangles (the strip needs at least 2 chunks to form a slice). Acceptable — a 1-chunk region is < 1 px tall at typical zooms and visually invisible anyway. **Initial acceptance:** zooming all the way in shows a single C0-continuous filled shape with no internal horizontal seams between chunks; per-chunk colour transitions still visible but they now read as a smooth colour gradient *along* the strip's length, not as quad boundaries. **Polish-A (calibration 7):** the first smoke test of M10.5i revealed a new visual problem the per-chunk-quad topology had been *hiding*: the min/max envelope **alone** produces visible "see-through" gaps at low-amplitude chunks (kick offsets, between-transient lulls) because the trapezoidal slice between adjacent chunks pinches to near-zero width when one chunk is much quieter than its neighbour. In the old M10.4 / M10.5b instanced-quad layout, every chunk painted its **full pixel row** in its band-mix colour (uniform per quad), so quiet chunks still showed as a thin coloured line — the eye read the envelope as "solid" even though the underlying amplitude was small. The continuous-strip topology exposes the *real* envelope contour, which is naturally pinched at quiet moments. This is mathematically what min/max envelope rendering looks like — and it's how Serato / Traktor / rekordbox handle this exact problem too: by rendering a **two-layer envelope** with an inner brighter RMS-amplitude "body" on top of the outer min/max contour. **Fix:** each region now renders TWICE in Pass 1 instead of once. (1) `Shaders.metal` `waveformVertex` gains a `fillMode` constant (vertex-buffer slot 3, 4-byte inline value set via `setVertexBytes`). When `fillMode == 0` the strip uses the smoothed `lo`/`hi` amplitudes and the M10.5e perceptual exponent of 0.55 — paints the outer envelope contour exactly as M10.5i ships. When `fillMode == 1`, the strip uses `±rms` (the same M10.5g 3-tap smoothed RMS already being computed for the M10.5h HDR-boost) as the amplitude edges and a more aggressive 0.35 perceptual exponent — paints a wider, brighter inner band. The 0.35 vs 0.55 choice is tuned so a near-silent 0.05 rms chunk compresses to 0.37 instead of 0.20 — almost 2× wider, enough to read as a continuous bright body across the whole envelope even on the quietest passages. (2) Fragment shader: a `discard_fragment()` fast-path when `isRmsFill && rms < 5e-3` so RMS-pass fragments over truly silent chunks fall through to the underlying envelope, never overwriting it with an opaque-coloured pixel. Final RMS-pass colour is multiplied by 1.6× post-`pastDim` to push the inner core well into HDR space — ACES tonemap's shoulder catches the overshoot and renders the core as "white-hot at the centre, hue-saturated at the edges" rather than a flat overlay. The 1.6× was calibrated against the M10.5h bloom chain: lower factors made the inner body indistinguishable from the outer envelope; higher factors blew the bloom into solid white blocks. (3) `WaveformRenderer.swift` Pass 1 draws 4× per frame instead of 2× — past envelope, past RMS, future envelope, future RMS (env-then-RMS ordering inside each region is what makes the inner core "win" the overlap; past-then-future ordering doesn't matter because the two regions don't overlap in NDC). `fillMode` is bound via `setVertexBytes` (Metal's canonical pattern for tiny per-draw constants — 4 bytes inline, no MTLBuffer, no per-frame allocation). The bloom + tonemap chain (Passes 2–5) consumes the combined HDR primary as before, so the bloom now lights up *both* the outer envelope and the inner core — exactly what Serato's "hot kick" look conveys: a saturated outer halo around a near-white core. **Calibration note (B from the user vote):** the user also asked for "thicken the strip" — `±dTime` overlap to make adjacent trapezoids overlap by a half-chunk in time. On closer inspection this turned out to have no geometric effect: the inter-chunk-centre distance is fixed at `1/K NDC = 0.5 px` regardless of where within the chunk the vertex sits, so moving vertices to chunk edges (or beyond) doesn't change the trapezoid height between consecutive chunks. The "thickening" intent maps cleanly onto the more-aggressive 0.35 amplitude exponent on the RMS pass — making the inner body visibly wider in the amplitude direction has the same perceptual effect (and the right one: the actual problem is amplitude-axis narrowness at quiet chunks, not time-axis spacing). **Final acceptance:** every chunk shows a visible warm-coloured inner band even when its outer envelope is near-zero; loud transients show the M10.5h bloom around a brighter near-white core; the overall envelope reads as a solid two-layer Serato-style waveform with no horizontal see-through gaps. xcodebuild clean; no new lints; no FFI changes. Per-frame Pass 1 cost ~2× because of the extra two draws (still pixel-bound by the rasteriser at ~280 × 1120 px per deck — fragment cost dominates, vertex doubling is in the noise). | 1.5 days |
| **M10.5l** | **Onset-driven bloom intensity** — *shipped* | Promotes the M10.2 "onset glow" bullet onto the M10.5h HDR pipeline. **What shipped:** new `dub-peaks` `OnsetDecimator` mirroring `BandDecimator`'s surface (`new` / `feed` / `reset`), built on the same `SpectralFrameStream` primitive — same FFT-hop cadence (= `samples_per_band_chunk`, default 512 samples), single `f32` `OnsetChunk` per hop carrying the Klapuri-style log-band weighted spectral flux. **Why a sibling of `dub-bpm::onset` rather than re-exporting it**: the renderer needs the onset trail even when no `BpmStream` is running (File-mode playback, single-deck Prep), and tying the renderer to the BPM crate would couple two independent off-RT pipelines. Each implementation is ~30 LoC of FFT-frame work; divergence would surface immediately in workspace tests. **`dub-peaks` plumbing**: `PeakBuffer` gains an optional `OnsetStorage` mirror of `BandStorage`; `PeakStreamConfig.onset_enabled` (default `true`) implicitly enables `bands_enabled`; `PeakStream::spawn` drives an `OnsetDecimator` on the analysis thread alongside the existing `BandDecimator`; `compute_offline_peaks` does the same on the file-mode path. **`dub-ffi` surface**: `engine.onset_peaks_len(deck)`, `engine.onset_peaks_chunk_duration_secs(deck)`, `engine.onset_peaks_extend(deck, start)` — same shape as `band_peaks_*` but a 4-byte stride. `PeakSource` enum delegates to the live stream / offline buffer the same way it delegates bands. `FFI_VERSION` 8 → 9. **Apple renderer**: new `onsetChunksBuffer` ring (sized to `bandChunkCapacity` = 131 072 entries = 512 KB/deck), parallel `ingestNewOnsetChunks` pump, `WaveformUniforms.onsetChunkOffset` (per-region), onset buffer bound at vertex buffer slot 4. **Shader**: vertex stage looks up the onset chunk for each broadband chunk via the same band-cadence math (`bandLocal = sampleCentre / samplesPerBandChunk`), applies the same 3-tap `[1, 2, 1] / 4` smoothing to the raw flux (suppresses single-hop ODF noise that would otherwise paint exactly one chunk hot → a 1-pixel-tall flicker), maps via the calibrated sigmoid `onsetConf = clamp(1 - exp(-fluxSmoothed × 0.25), 0, 1)` — saturates softly so silence ≈ 0, sustained tone ≈ 0, soft pad onset ≈ 0.4, mid-energy hit ≈ 0.78, kick/snare ≈ 0.95, loud crash ≈ 1.0. The smoothed `onsetConf` is forwarded through `VertexOut.onsetConf` (rasteriser-flat across both vertices of a chunk so onsets render as crisp halos, not interpolated smears). **Fragment stage**: HDR `hdrBoost` is multiplied by `(1.0 + 1.5 × onsetConf)` — confirmed kicks land at ~2.5× the base curve in HDR space, sustained content stays at the M10.5h baseline. The bright-pass + 2-iteration Gaussian chain then produces visibly brighter / wider halos on onsets without changing anything for non-onset chunks. Onset capture sits behind `PeakStreamConfig.onset_enabled` (default-on) so future Thru-with-no-spectral configs can opt out cleanly. **Acceptance** (visual): a four-on-the-floor pattern produces periodic bloom flashes synchronised with the kick; a melodic / ambient passage barely blooms (low RMS + low onset → multiplied near-zero). **Costs**: ~250 LoC Rust (decimator + buffer + stream + offline + FFI + tests), ~80 LoC Swift, ~30 LoC shader. | 1 day (after h + i) |
| **M10.5j** | **On-disk waveform sidecar cache** — *planned* | The "track-load feels instant" upgrade — what Serato (`.SeratoOverview`), Traktor (`.tg2`), rekordbox (`.pdb` + analysis blobs) all do under the hood. Today every track load runs `Track::load_from_path` (symphonia decode → `Vec<f32>` of the whole file) + `compute_offline_peaks` (broadband + 8-band ring fills across all samples). That's ~50–300 ms per track on a fast SSD, all of it CPU. M10.5d moved it off the engine mutex, but the work still happens once per load. **Plan:** new `dub-cache` library in `crates/dub-cache/` (or a `cache` module inside `dub-peaks`) owning a versioned on-disk format: 64-byte little-endian header `{ magic: b"DPK1", version: u32, sample_rate: u32, channels: u8, sample_count: u64, broadband_cadence: u32, band_cadence: u32, mip_count: u8, reserved: [u8; 22] }` followed by the broadband peaks (`PeakChunk` repr-C), band peaks (`BandPeakChunk` repr-C), and (when M10.5k lands) the mip pyramid as a stacked sequence of `(level_header, broadband_at_level, bands_at_level)`. Optional CRC-32 footer over the whole file (catches partial writes from a hard kill mid-write). **Cache path**: `~/Library/Caches/com.klos.dub/waveforms/<key>.dubpeaks` where `key = sha-256(canonical_path || file_size || mtime_nanos)`. The mtime/size in the key means a re-encoded file invalidates automatically without a stat check beyond `fs::metadata`. **Lookup flow** in `dub-ffi::load_track` Phase 1: stat the audio file (1 syscall), compute the cache key, try to `mmap` the sidecar (1 syscall on success, ~1 ms). Cache hit → skip decode entirely; the M10.5d Phase 2 work collapses to a memcpy of the pre-computed peaks into the engine's existing buffers. Cache miss → decode + compute as today, then atomically write the sidecar via a `<key>.tmp` → `<key>.dubpeaks` rename so a crash mid-write never produces a half-valid file. **Disk budget**: a 5-min track at 44.1 kHz with 64-sample cadence is `13.2M / 64 = 206k` peak chunks × `(3×4 + 8×4) = 44 bytes/chunk` = ~9 MB raw — way too big. **Real plan**: cadence stays at 64 samples for the broadband (matches the live Thru cadence so the live + offline visual stay identical), bands stay at 512 (the existing dub-peaks band cadence), and the sidecar stores the broadband at full cadence + bands at full cadence + 5 mip levels for the M10.5k pyramid. Net per-track sidecar: ~2.5 MB at 5 min ≈ 30 MB/h of library. A 500-track DJ library = ~1.25 GB of cache — fits within a typical Music folder analysis-data budget; well below Serato's typical 3–5 GB. Cache eviction follows LRU when the directory exceeds a configurable cap (default 4 GB; preferences-tunable). FFI surface: `DubEngine::warm_cache(path)` (preflight from the library browser when the user hovers a row), `DubEngine::clear_cache()`, `cache_stats() -> CacheStats { entries, bytes }`. **Acceptance**: a track loaded twice in a row hits the cache the second time — Activity Monitor shows zero symphonia CPU on the second load; the deck-header `LOADING…` pill flashes for <50 ms instead of ~150–300 ms. No correctness regression: byte-equality between cache-hit peaks and a forced cache-miss recompute for all M8.1 genre fixtures, asserted by a new `dub-cache::tests::roundtrip_byte_equal` test. `FFI_VERSION` bumps. | 1.5–2 days |
| **M10.5k** | **Mip pyramid in dub-peaks** — *planned* | Closes the loop on the final M10.2 deferred polish bullet (`SHIPPED.md` §M10.2 *What it does not ship: Mip pyramids*). Today the renderer reads peaks at a single resolution (64-sample broadband cadence) and the `TrackOverviewView` re-decimates to ~300 buckets on the CPU at load — both work but neither lets us *zoom smoothly* or feed the M10.5h bloom-chain a coarser source for far-out views. **Plan:** extend `OfflinePeaks` (in `crates/dub-peaks/`) with `pub struct MipLevel { broadband: Vec<PeakChunk>, bands: Vec<BandPeakChunk> }` and `pub mips: Vec<MipLevel>` containing 5 levels: level 0 = full cadence (existing), level 1 = ÷2 (each pair max-of-max, min-of-min, mean-rms), level 2 = ÷4, level 3 = ÷8, level 4 = ÷16. Same reduction kernel for bands (band RMS is mean-pooled). Computed during `compute_offline_peaks` in a single pass (each level is built from the previous, so total CPU is ~2× the level-0 cost — negligible vs. the symphonia decode). **Renderer integration:** `WaveformRenderer` gains an active-mip selector driven by the effective `chunksPerPixel` (each ×2 zoom-out beyond the level-0 threshold steps up one mip). For smooth transitions, the renderer can blend two adjacent mips in the shader via a `mipBlend: float` uniform (texture-LOD-style bilinear in time). **TrackOverview wins**: `TrackOverviewView` (in `Performance/TrackOverviewView.swift`) drops its CPU decimation entirely and reads mip-4 directly via the existing `peaks_extend` (a new mip-aware variant or an additional `peaks_extend_mip(deck, mip, start_idx)` accessor) — at mip-4 a 5-min track produces ~830 chunks, ideal for the overview strip's pixel height. **Sidecar storage**: the M10.5j sidecar gains the mips after the level-0 payload. **Disk impact**: levels 1–4 add `1/2 + 1/4 + 1/8 + 1/16` = ~0.94× the level-0 size. **Acceptance**: zooming the deck waveform via Preferences (or a future M10.6f gesture) animates smoothly across mip boundaries with no visible re-sampling step; the overview redraws ~30× cheaper than today (no per-load CPU decimation pass). Touches `dub-peaks`, `dub-ffi` (new accessor + sidecar bump → `FFI_VERSION` += 1), Swift renderer + overview. | 1 day (after j) |
| **M10.5m(a)** | **Beat-aware saturation** — *shipped* | The first half of the originally-planned M10.5m row, lifted out and shipped alongside M10.5l because both effects ride the same `onset_trail` data and live in the same fragment-shader pass. **What shipped (~10 LoC shader)**: after `bandMix` runs and *before* the palette branch, the shader rotates the bandMix output toward its own Rec. 601 luma based on `onsetConf`: `colour = mix(float3(luma), colour, 0.4 + 0.6 × onsetConf)`. Mix factor calibration: `conf = 0.0` (sustained pad, silence between hits) → 40 % chroma + 60 % luma → reads as a desaturated wash; `conf = 0.5` (mid-energy onset, soft hi-hat) → 70 % chroma → reads as a partly-coloured streak (gives the eye a depth cue without flattening quieter musical content into pure grey); `conf = 1.0` (kick / snare / vocal attack) → 100 % chroma → full vibrant hue. **Combined effect with M10.5l**: drum hits + transients pop as saturated colour shapes against a near-monochromatic background of held notes / pads / silence, *and* the same transients bloom brighter via the M10.5l HDR gate — the eye instinctively lands on "where's the kick / snare". This is the legibility upgrade the M10.5 polish ladder was designed to ship; the user's frustration prior to M10.5l + M10.5m(a) ("everything looks equally wide, I can't tell where the kick is") drove the priority. **What did NOT ship**: the sub-bass split (M10.5m(b), see below) — kept on the M11 roadmap because the data-format breakage requires DJ-curated content to validate the colour change against. | 0 days (folded into l) |
| **M10.5o** | **Kick prominence layer (band[1] visual emphasis)** — *shipped* | **Problem**: in the M10.5l + M10.5m(a) baseline, a kick chunk and a sustained bassline chunk at the same broadband RMS read as visually indistinguishable — both paint at similar luminance + chroma since the bandMix output is a per-band *ratio* (a chunk dominated by bass paints red regardless of *how much* bass), and the bloom layer only fires on onsets (M10.5l calib-2). User wanted the 80–250 Hz "kick range" to **visually stand out** independent of onset / total amplitude. **Two design options considered**: (A) shader-only re-weighting of band[1] via a single uniform — ~30 lines, no FFI change, ships today; (B) split `dub-spectral`'s lowest band into sub-bass + kick-band (`NUM_BANDS` 8 → 9), the original M10.5m(b) plan — ~2 days, breaks the BPM fixtures, bumps FFI_VERSION. **Shipped (A) first**, deferred (B) until A is proven insufficient. **Implementation (~50 LoC across 4 files, all behind a single uniform)**: new `kickEmphasis: float` in `Uniforms` (`Shaders.metal`) + matching field in `WaveformUniforms` (`WaveformRenderer.swift`, taking the struct from 60 → 64 bytes, perfectly filling `uniformStridePerRegion`), sourced from a new `kickEmphasis` `@Published` knob on `WaveformTuning` (default 0.6, range 0.0–1.5) with a corresponding live slider in `WaveformTuningPanel`. Fragment shader applies three combined effects, all multiplied through `kickStrength = clamp(in.bandLow.y × kickEmphasis, 0.0, 1.0)`: (1) **saturation override** — bumps `chromaScale = chromaFloor + chromaRange × onsetConf` toward `min(chromaScale + kickStrength × 0.8, 1.0)` so kick-heavy chunks keep full chroma even when `onsetConf` is low (long-decay 808s, kicks buried under busy snare patterns); (2) **red-orange tint** — mixes the bandMix output toward `(1.0, 0.30, 0.05)` by `kickStrength × 0.55`, so a kick-heavy chunk reads as kick-coloured even when bandMix pulled toward, say, mid-green by a simultaneous synth lead; (3) **additive HDR bloom** — `hdrBoost += in.rms × kickStrength × 0.6`, gating on `in.rms` so quiet rumble doesn't paint but sustained loud sub-bass *does* (filling the M10.5l onset-only bloom's blind spot for un-detected kicks). **Revert paths (three independent levels)**: (i) **runtime**: set `kickEmphasis = 0.0` in the live tuning panel — `kickStrength = 0` collapses all three effects to zero, output bit-identical to pre-M10.5o; (ii) **in-code**: change `WaveformTuning.defaultKickEmphasis` from 0.6 to 0.0; (iii) **git**: revert the single M10.5o commit covering `Shaders.metal`, `WaveformRenderer.swift`, `WaveformTuning.swift`, `WaveformTuningPanel.swift` (no FFI / no Rust / no engine changes — clean diff boundary). **Acceptance**: on a 4-on-the-floor reference track, kick chunks read as saturated orange streaks with prominent halos against the rest of the waveform, and the eye finds the kick pattern without conscious effort. Slider at 0.0 produces a pixel-identical match to pre-M10.5o builds. **If A is insufficient**: M10.5m(b) — the 9-band sub-bass split — gives band[1] a tighter ~60–200 Hz definition and unblocks crisper kick-vs-sub-bassline separation, but requires the BPM fixture rebake + FFI bump. | 0.25 day |
| **M10.5p-grid** | **Beat-grid v2 — own milestone (tempo drift, downbeat detection, manual phase correction)** — *deferred (re-scoped out of M10.5p)* | The first M10.5p Stage 1 ship bundled an offline beat-grid + tick overlay alongside the monochrome envelope. User testing exposed two issues that pushed the grid out into its own multi-sub-task milestone: (a) the overlay didn't visibly scroll with the playhead on first ship (an `Canvas`-caching bug; subsequently fixed) yet still relied on a static phase that doesn't survive tempo-drifting material, and (b) the "stuck two ticks" symptom revealed the deeper truth — *beat grids only work on tempo-locked production tracks*. Live recordings, classic vinyl pressings (which drift inherently), edits with manual cuts/loops, and tempo-aware DJ tools (Serato Pitch'n'Time, Traktor Flux) all produce material where a fixed-period synthetic grid drifts off the audible beats within bars. A v2 grid that handles those cases needs: **(g1)** per-beat phase tracking (a Viterbi-style decoder over the ODF rather than a single global phase pick); **(g2)** algorithmic downbeat detection (which beat is "the 1" of each bar — current Stage 1 just calls beat 0 the downbeat, which is wrong for any track that doesn't start exactly on the 1); **(g3)** manual phase correction UI (tap the waveform to nudge the discovered "1"; ⌘⇧← / ⌘⇧→ to shift the grid by ±1 ODF tick; half-tempo / double-tempo toggle for the M8.1 octave-ambiguity edge cases); **(g4)** library sidecar serialisation (compute the grid once, persist it, never recompute on re-load); **(g5)** a Thru-mode streaming variant (the offline `analyze_beat_grid` is file-only — a streaming `BpmStream` already exists but only emits BPM, no phase). All of these are non-trivial; bundling them under one row that also has to deliver Stage 1's monochrome envelope was the wrong scope. Stage 1's overlay is removed; the underlying foundation stays alive — `dub_bpm::analyze_beat_grid` + tests, the `BeatGrid` UniFFI Record + `DubEngine::beat_grid` accessor (currently returns `BeatGrid::empty()` because `load_track` skips the analysis pass for a ~100 ms / track load-time win). When the grid milestone resurfaces, the one-line revert to re-enable Stage 1's coarse phase finder is documented in `dub-ffi/src/lib.rs` `load_track`. Until then, the waveform helps the DJ with no grid: read M10.5p Stage 2's transient-prominence brightness modulation. | 2–3 days (when scheduled) |
| **M10.5p** | **DJ-focused waveform redesign — Stage 1 (monochrome envelope) + Stage 2 (transient prominence)** — *shipped (Stage 1 envelope kept; Stage 1 beat-grid overlay removed and re-scoped; Stage 2 onset-driven brightness + kick tint shipped on top)* | **Problem statement (user-driven, 2026-05-13)**: the M10.5h → M10.5o waveform stack delivered a *visually rich* renderer but a *DJ-ineffective* one. In loud / busy music ("bass + rapping + drums") the 7-band hue mix saturates toward a "yellowish glowing" soup because the per-band ratios all land near-equal once the music is dense — the colour layer adds noise rather than information. Saturation, kick emphasis, and onset bloom all *correctly* report what's there spectrally, but a DJ doesn't need spectral density: they need three landmarks. Quote: *"all DJ music is basically on a 4/4 rhythm. The other thing a DJ needs is to identify the drop (easy since this is mostly after a break and a buildup) and he needs to understand where the vocals come in and where they leave. This is basically all the dj needs from a waveform."* **Design pivot**: from "data-rich spectral visualisation" → "DJ landmarks only". Three semantic layers, each independently togglable: (1) **monochrome amplitude envelope** — read for *energy structure* (break / buildup / drop); no hue noise. (2) **4/4 beat grid ticks** — discrete markers at every beat, brighter every 4th (the "1"); read for *mix-in alignment*. (3) *(Stage 2)* **vocal-band indicator** — a thin coloured stripe gated on mid-band activity; read for *vocal in / out*. (4) *(Stage 3)* **drop / build markers** — derived from RMS envelope contour; read for *song structure*. **This row ships Stages 1 + 2's foundation (the beat-grid plumbing) in one go**; Stages 2 and 3 follow on separate rows. **What shipped (Stage 1, ~400 LoC across 7 files, 1 new module, FFI 9 → 10)**: (1) **`dub-bpm::beats`** — new module exposing `analyze_beat_grid(samples, sr, channels) -> Result<BeatGrid, AnalysisError>` and a public `BeatGrid { bpm, confidence, beats: Vec<f64>, beats_per_bar: u8 }`. Internally runs the existing `analyze_bpm_with_range` for tempo, then a fresh ODF pass + phase-search to align the synthetic grid against the audio's actual onsets. Phase search scans `phi ∈ [0, period_int)` ODF samples summing `Σ ODF[phi + i × P]`, picks the maximum, then parabolic-vertex-refines to sub-ODF-tick precision (an ODF tick at HOP_SIZE 512 / 48 kHz is 10.7 ms — without sub-tick refinement, beat ticks would visibly jitter against the audible kicks). Beat extrapolation walks backward from the discovered phase to capture any beat at or before `t = 0` (the spectral-flux algorithm is blind to a kick at sample 0 because `ODF[0]` has nothing to diff against; without back-walking, the rendered grid would lose its head beat). 4 unit tests cover the 120 BPM click track (beats at 0.5 s spacing), phase-offset recovery (0.25 s lead-in click track → first detected beat at ~0.25 s), silence input (zero-confidence return), and beat-monotonicity invariant. **What's deliberately NOT in v0**: per-bar downbeat detection (every 4th beat is the visual downbeat by convention — sufficient for the DJ-relevant 4/4 genres; a "tap to align the 1" UI affordance is M11+), tempo drift (real DJ-relevant tracks don't drift, so beats are spaced at the global BPM), and per-beat confidence (the per-beat output trusts the global BPM-estimator confidence). **Cost**: a 5-min track at 44.1 kHz / 120 BPM runs ~200 ms of CPU off the main thread inside `load_track`'s Phase 2 (decode + peaks already off-thread, beat analysis joins them). Two ODF passes — the BPM estimator's internal one plus a fresh one for phase search — could be folded into one with a small `analyze_bpm_with_range` refactor returning `(BpmEstimate, Vec<f32>)`; deferred because the duplicate pass is cheap and the simpler public surface is the right v0 trade. (2) **`dub-ffi` plumbing** — `BeatGrid` as a `uniffi::Record` mirror of the Rust struct (`bpm: f64`, `confidence: f32`, `beats: Vec<f64>`, `beats_per_bar: u32`); new field `beat_grid: BeatGrid` on the private `FilePeaks` struct alongside `broadband` / `bands` / `onset`; `load_track`'s off-mutex Phase 2 now also calls `analyze_beat_grid(track.samples(), track.sample_rate(), track.channels())` (a `TooShort` error from a sub-minimum-duration track yields `BeatGrid::empty()` rather than failing the load — short tracks are still loadable, they just don't paint ticks); new `DubEngine::beat_grid(deck_idx) -> BeatGrid` accessor returns the cached grid (empty for Thru-source decks — Thru-mode beat tracking needs streaming phase, M11.5 scope). `dub-ffi/Cargo.toml` adds `dub-bpm` as a dependency. `FFI_VERSION` 9 → 10. (3) **`apple/Dub/Waveform/WaveformView.swift`** — new private `BeatTickOverlay: View` slotted into the `WaveformView` ZStack between the zero-crossing line and the playhead hairline (so the playhead always wins where they cross). The overlay uses `TimelineView(.animation, minimumInterval: 1/60)` + a `Canvas` for rendering; each frame it (a) fetches the current `engine.beatGrid(deckIdx:)` (cheap — a `Vec<f64>` clone of a few KB; the cost of the FFI call at 60 Hz is ~300 KB/s memcpy, negligible vs. a memoised `@State` path's complexity), (b) reads the current `engine.position(deckIdx:).elapsedSecs`, (c) projects each beat onto pixel-space via `WaveformRenderer.secsPerPixel(sampleRate:)` — the same nonisolated helper the M10.6a click-scrub gesture uses — so ticks land on the exact chunk the shader paints them against, and (d) draws a 2-px-thick × 18-px-long warm-amber bar (`rgb(1.0, 0.62, 0.18)` at 55 % alpha) for each visible beat, extended to 28 px and 95 % alpha for every `beats_per_bar`-th beat (the downbeat). In **vertical orientation** (Performance mode) ticks anchor to the right edge of the pane and extend leftward — they live in the inter-deck gutter, not on top of the envelope, so they don't compete for the eye with the amplitude shape. In **horizontal orientation** (Prep mode) they anchor to the bottom edge and extend upward, same logic. Visibility window padded by ±2 s around the visible time range so ticks just off-screen don't pop in mid-tick on a zoom change. **Why a SwiftUI `Canvas` and not a Metal pass**: the tick set is small (a 5-min track at 120 BPM is 600 dots, of which ~6 are on-screen at default zoom). Re-encoding a Metal pass for 6 quads per frame is overkill, and the SwiftUI surface gives trivial revert / iteration on tick visuals (size, colour, downbeat emphasis) without shader recompiles. (4) **`apple/Dub/Waveform/WaveformRenderer.swift`** — new `WaveformPalette.djLandmarks = 3` case + matching `displayName: "DJ landmarks"`; the renderer's `palette` default flips from `.seratoFaithful` to `.djLandmarks`. `WaveformView(palette:)` default + `MainView`'s `@Published var palette` default flipped to match — the new look ships as the user-facing default, with the previous 7-band hue + bloom + kick-emphasis stack one preset away (`.seratoFaithful` in Preferences → Waveform palette). The Preferences-sheet `paletteSection` already iterates `WaveformPalette.allCases` so it picks up the new case automatically. (5) **`apple/Dub/Waveform/Shaders.metal`** — new fragment-stage branch at `palette == 3u` that early-returns a monochrome envelope: `intensity = clamp(0.12 + in.rms × 1.8, 0.12, 1.0); return float4(float3(intensity) × pastDim, 1.0)`. **Calibration vs. the M10.5e Monochrome palette (`palette == 2`)**: floor 0.12 (vs. 0.45) makes silence near-invisible — quiet sections read as visible *gaps* in the envelope rather than as dim-but-equal-thickness streaks, restoring the dynamic-range signal the eye needs to read for build / drop. RMS gain ×1.8 (vs. ×1.55) compensates so a typical loud chunk still hits ~1.0 by RMS ≈ 0.49. Same `pastDim` depth cue as every other palette branch. The early return bypasses the M10.5l onset bloom, the M10.5o kick emphasis, the M10.5m(a) saturation, and the ACES-tonemap post-saturation — all of those are colour-layer effects that don't apply to a monochrome envelope (and they'd compete with the beat ticks for attention if they did paint). **Net visual result**: a high-dynamic-range grey envelope (silence ≈ invisible, peak ≈ near-white) with crisp warm-amber ticks marching up the right edge, brighter at every "1". The eye reads the envelope's *shape* for energy structure and the *tick spacing / brightness pattern* for the 4/4 cadence — two orthogonal channels, no noise overlap. **Revert paths (four independent levels)**: (i) **runtime, no code**: Preferences → Waveform palette → "Serato-faithful" reverts to the M10.5o look; "Monochrome" reverts to the M10.5e palette; the beat-tick overlay continues to render on top of any palette (the overlay is layout-driven, not palette-driven — it's only hidden when `confidence == 0`, e.g. a sub-minimum-duration track). (ii) **in-code, palette default**: change `WaveformPalette = .djLandmarks` defaults in `WaveformRenderer.swift`, `WaveformView.swift`, and `MainView.swift` back to `.seratoFaithful`. (iii) **in-code, hide ticks**: remove the `BeatTickOverlay` ZStack entry from `WaveformView.body` (~3 lines). (iv) **git**: revert the single M10.5p commit covering `dub-bpm/src/beats.rs` (new file), `dub-bpm/src/lib.rs`, `dub-ffi/Cargo.toml`, `dub-ffi/src/lib.rs`, `apple/Dub/Waveform/{WaveformView,WaveformRenderer,Shaders.metal}` — clean diff boundary, no engine-runtime semantics change. **Stage 2 — transient prominence (shipped on top of Stage 1)**: user feedback on Stage 1's pure-RMS monochrome envelope: in a breakdown with rolling sub-bass at high RMS, the envelope rendered at the *same width and brightness* as a kick drop at the same RMS — the DJ couldn't distinguish the two visually. *"What we need is to distinguish the sub-bass from the kick. We don't want to remove info from the waveform — the DJ should still see what's there, also stuff that's not relevant for him, but the stuff that is relevant for him should be amplified."* Stage 2's fix uses the M10.5l `onsetConf` and the M10.5h `bandLow.y` (band index 1 ≈ 60–130 Hz — kick fundamental + body, narrower than band 0's 30–60 Hz sub-bass) already computed by the upstream pipeline, and folds both into the djLandmarks fragment branch (~10 lines, no FFI / Rust change, no new uniforms). The model: **width** still = RMS (sustained content is *visible*, not deleted); **brightness** = `base + onset pop` (the new axis); **hue** = warm amber when `onsetConf × band[1]` is high (the kick gate). Net taxonomy the eye reads: silence ≈ invisible (intensity 0.06); sustained quiet pad ≈ dim grey (0.10–0.20); **sustained rolling sub-bass** ≈ wide DIM grey (0.25–0.45, base brightness capped at 0.55 to never compete with kicks); snare/clap ≈ wide BRIGHT WHITE (0.70–1.0 from pop, no amber because band[1] is low); **kick** ≈ wide BRIGHT AMBER (0.70–1.0 from pop + 65 % amber tint); hi-hat ≈ narrow bright spike (small RMS × pop). **The 0.55 cap on `base` is the load-bearing constant** — without it a loud rolling bass at full RMS would paint the same intensity as a kick and we'd be back at Stage 1's original problem. With it, the dynamic range between "loudest sustained content" and "loudest transient" is 0.55 → 1.0 ≈ 2× brightness ratio; combined with the amber tint, kicks become *unmistakably distinct* from sub-bass within the same envelope width. Acceptance (validated against the user's breakdown→drop reference clip): the breakdown's rolling sub-bass renders as a *visibly dim* wide envelope; the drop's kicks render as bright amber bullets against the same wide envelope shape — both still legibly *there*, but the kick is what the eye lands on. **Stage 3 — time-domain filtered peaks (2026-05-13, user-driven, Mixxx-inspired)**: Stage 2's `bandLow.y × onsetConf` kick gate and the Stage 2 calib-1 triple-hard variant both failed to converge on a clean "grey envelope, amber kicks" taxonomy. Diagnosis: `bandLow.y` is a **μ-law-compressed STFT magnitude** from `dub-spectral`. Compression (the `ln(1 + λ · |X|)` curve, chosen in M8.1 to stop hi-hats out-voting kicks in the BPM ODF) by construction *pulls distinct amplitudes toward the same range*. A sustained 80 Hz bassline at raw FFT magnitude 0.4 and a kick at raw magnitude 0.9 both land around 0.6–0.8 after compression. The transient shape — the thing that distinguishes a kick from a bass roll in the time domain — is also smeared across the ~46 ms Hann window of the STFT. By the time `bandLow.y` reaches the shader, both axes of "kick vs. sustained bass" are gone, and no shader-side gate can recover what was thrown away upstream. **Mixxx reference** (<https://mixxx.org/news/2024-02-23-improved-waveforms/>): Mixxx's "Filtered" / "RGB" waveform types sidestep both problems by running the audio through **time-domain band-pass filters at load time** and storing per-pixel min/max of the *filtered* signal. A kick's filtered LF envelope survives intact as a sharp attack-decay spike; a sustained sub-bassline survives as a near-constant lower-amplitude envelope. The transient shape is preserved, the kick stands out by a clean ~2–3× ratio, and the renderer reads it directly. **Stage 3 implements this foundation**: new `dub_peaks::filtered` module exposing `FilteredDecimator` (a 2-pole Butterworth low-pass biquad at 250 Hz, RBJ cookbook coefficients, Direct-Form-I, alloc-free hot path) and the `FilteredPeakChunk` wire format (`#[repr(C)]`, 24 bytes, six `f32`s — `lf_min`, `lf_max`, `mf_min`, `mf_max`, `hf_min`, `hf_max`). v1 populates only the LF pair; MF / HF fields are reserved for follow-up Mixxx-style 3-band expansion so the wire format / FFI / shader layout never needs to break. **Filter design**: 2-pole Butterworth LP at 250 Hz, Q = 1/√2. Kick fundamentals sit at 50–80 Hz; the audible body (2nd–3rd harmonic) extends to ~250 Hz, fully in band. Snare fundamentals at 150–200 Hz leak in, but the bulk of snare energy (200 Hz–8 kHz body + buzz) is rejected — a snare's LF-channel peak amplitude is typically a third to a fifth of a kick at the same broadband RMS, enough separation when combined with `onsetConf`. Rejection: -12 dB at 500 Hz, -24 dB at 1 kHz, -36 dB at 4 kHz. Group delay at DC is ~3 ms (~144 samples at 48 kHz, ~2 chunks at the 64-sample broadband cadence — below the AV-sync threshold). **Empirical validation**: the unit test `synthetic_kick_pops_against_sustained_bass` in `filtered.rs` builds a 1-second waveform of sustained 80 Hz sine at amplitude 0.4 with a 30 ms decaying-sine kick at 60 Hz, peak 0.9, layered at t = 0.5 s. The test asserts `kick_lf_peak / pre_kick_lf_peak ≥ 1.8×` — the architectural separation promise; passes empirically at the chosen filter parameters. 12 unit tests total covering construction (zero-chunk/zero-SR panics), boundaries (partial chunk carryover, silence, flush, block-size invariance), filter correctness (DC unity, 80 Hz in-band, 2 kHz rejection), and the kick-vs-bass synthetic. **What shipped (~700 LoC across 4 files + 1 new module, FFI 10 → 11)**: (1) **`dub-peaks/src/filtered.rs`** — new module (NEW FILE, ~250 LoC excluding tests + ~250 LoC tests) implementing `Biquad` (Direct-Form-I, cookbook LP coefficients, `process` and `reset`) and `FilteredDecimator` (one biquad per band, currently LF only; min/max accumulator with `samples_per_chunk` cadence matching the broadband decimator; `feed` / `flush` / `reset` mirroring the existing `Decimator` API). (2) **`dub-peaks/src/lib.rs`** — `mod filtered`, `pub use filtered::FilteredDecimator`, and the `FilteredPeakChunk` type itself with full wire-format documentation (the type lives in `lib.rs` to stay alongside the other `_repr(C)_` chunks; the decimator lives in the `filtered` module). (3) **`dub-peaks/src/offline.rs`** — `OfflinePeaks` gains a `filtered: Vec<FilteredPeakChunk>` field and a `samples_per_filtered_chunk: usize` cadence field; `compute_offline_peaks` constructs a `FilteredDecimator::new(sample_rate, DEFAULT_SAMPLES_PER_CHUNK)` and feeds it alongside `bb` / `bd` / `od` in both mono and stereo paths; flushes at end-of-stream for parity with broadband. (4) **`dub-ffi/src/lib.rs`** — `FilteredPeakChunk` import from `dub_peaks`, `FilePeaks` struct gains `filtered: Vec<FilteredPeakChunk>` and `samples_per_filtered_chunk: u32` fields, `PeakSource` enum gains `filtered_len()` / `samples_per_filtered_chunk()` / `extend_filtered_from()` impls (Thru-mode `Live` returns 0 / `None` / no-op — v1 doesn't run a streaming filtered decimator), three new public FFI methods (`filtered_peaks_len`, `filtered_peaks_chunk_duration_secs`, `filtered_peaks_extend`), `filtered_peak_chunks_to_bytes` serialiser (24 bytes per chunk, six LE `f32`s in declaration order: `lf_min`, `lf_max`, `mf_min`, `mf_max`, `hf_min`, `hf_max`), `FFI_VERSION` 10 → 11 with bump-tripwire test updated. Round-trip serialiser test added. (5) **`apple/Dub/Waveform/WaveformRenderer.swift`** — `FilteredPeakChunkLayout` Swift mirror (24-byte struct, six `Float`s), `filteredChunksBuffer: MTLBuffer` (sized at `chunkCapacity × 24` ≈ 24 MB per deck of Apple-Silicon unified memory — matches the broadband ring's footprint), `lastSeenFilteredPeaksLen` + `totalFilteredChunksAppended` state, `ingestNewFilteredChunks()` method (mirrors `ingestNewChunks` byte-for-byte with the same ring-wrap memcpy logic; bails cleanly when `filtered_peaks_len == 0` for Thru-mode decks). The Swift binding gets the new buffer at vertex buffer slot 5. **Key architectural decision**: filtered chunks are cadenced at *exactly* the broadband chunk cadence (one filtered entry per 64-sample broadband chunk), so the renderer re-uses `chunkOffset` / `chunkCapacity` from the uniforms to index the filtered ring 1:1 against broadband — no new offset uniform fields, no `WaveformUniforms` layout churn. (6) **`apple/Dub/Waveform/Shaders.metal`** — new `FilteredPeakChunk` struct mirror, new vertex shader argument `constant FilteredPeakChunk* filtered [[buffer(5)]]`, vertex stage reads ±1 neighbour filtered chunks (same indexing math as broadband) and computes `lfPeak = max(|lf_min|, |lf_max|)` with the same `[1, 2, 1] / 4` smoothing kernel applied to broadband min/max/rms — visually aligns the filtered and broadband streams against the same chunk-edge jitter. `VertexOut` gains an `lfPeak: float` field. The fragment `palette == 3u` (djLandmarks) branch rewrites the kick gate from `pow(onsetConf, 3) × smoothstep(0.40, 0.70, bandLow.y)` to a simple `clamp(lfPeak × onsetConf, 0, 1)` — no power gymnastics, no smoothstep, no thresholds: the time-domain filter does the work the gate used to fight. Kick brightness boost stays (`intensity += kickGate × 0.30`); amber mix factor stays tied to `u.kickEmphasis` so the user can dial it live. **What we explicitly don't do (and why we beat Mixxx)**: Mixxx's RGB / Filtered waveforms ship the *honest* per-band envelope and stop there — the DJ reads kicks by scanning for spikes in the red channel. Dub's djLandmarks goes one step further: **the honest base IS the kick gate, but we then layer semantic emphasis on top** — brightness boost and warm-amber tint that draws the eye to the 4/4 pattern without obscuring the rest of the envelope. The base data is Mixxx-equivalent; the rendering layer is Dub-specific. **Calibration knobs**: `sigmoid k` + `sharpen exp` (control `onsetConf`, work in all palettes), `kick emphasis` (amber-tint factor, 0 = pure monochrome). The 3 Serato-palette-only knobs (`bloom gain`, `chroma floor`, `chroma range`) explicitly labelled as no-ops in djLandmarks via the tuning-panel hint text (panel grouped under "All palettes" / "Serato palettes only" section labels). **Revert paths**: (i) **runtime**: tuning panel → `kick emphasis = 0` reverts to pure monochrome landmarks. (ii) **runtime**: Preferences → Waveform palette → Serato-faithful reverts to the M10.5o look (bypasses the filtered ring entirely — `palette != 3u` doesn't read `in.lfPeak`). (iii) **in-code**: remove `out.lfPeak = lfPeak;` and the djLandmarks rewrite; the filtered ring continues to be computed and uploaded harmlessly (no shader reads it). (iv) **git**: revert the single Stage 3 commit covering `dub-peaks/src/filtered.rs` + the 5 touched files; clean diff boundary, no engine-runtime semantics change, FFI version goes back to 10. **Per-load CPU cost**: a 5-min 48 kHz mono track adds ~30 ms for the filter pass (one biquad evaluation per sample = ~14.4 M ops on a 5-min track). Negligible against the existing `compute_offline_peaks` cost of ~150 ms (broadband + bands + onset). **Per-deck RAM cost**: 24 MB filtered ring + ~7 MB offline `filtered` Vec for a 30-min track. Acceptable on Apple Silicon shared memory. **Future expansion (M10.5p Stage 3.1+)**: populate the MF / HF fields with a 2-pole HP at 250 Hz + LP at 4 kHz (MF) and HP at 4 kHz (HF) — this lights up a Mixxx-style 3-band RGB palette as a new `WaveformPalette.mixxxRGB` case without any FFI / shader-layout breakage. The vocal-band indicator (M10.5q) and drop/build markers (M10.5r) likewise will read from the same filtered ring instead of the μ-law'd band ring. **Stage 2 calib-1 (2026-05-13, user-driven)**: Stage 2 ship calib-0 painted amber across most loud sustained content. Diagnosis: the gate `kickGate = clamp(band[1].y × 1.5, 0, 1) × onsetConf` was *linear* in both factors, so sustained bass at `band[1] ≈ 0.5` (μ-law compression in `dub-spectral` pulls sustained content into the 0.4–0.6 range — it doesn't sit at "zero" like raw FFT bins would) combined with non-zero `onsetConf` (~0.1–0.3 on most loud chunks — real music's spectral flux is rarely zero) produced `kickGate ≈ 0.10–0.25`, mixed in at the hard-coded `× 0.65` factor as 6–18 % amber across the whole envelope. The intended *binary-ish* "kicks bullet, sustained dim grey" taxonomy degraded into a continuous "kinda amber, more amber, very amber" gradient. **Fix (triple-hard gate)**: (a) **`pow(onsetConf, 3.0)`** instead of raw onsetConf — exponential suppression pushes sustained flux into the noise floor (0.2 → 0.008, 0.5 → 0.125, 0.9 → 0.73). (b) **`smoothstep(0.40, 0.70, band[1].y)`** instead of `clamp(× 1.5)` — hard threshold replaces linear scale: nothing below 40 % band[1] energy passes, full pass above 70 %, smooth between. Sustained bass at ~0.5 gives bandHard ≈ 0.5; combined with a cubed onsetConf of 0.15 the product is ~0.002 — invisible. (c) **Kick-brightness boost**: confirmed kicks add `kickGate × 0.25` to intensity *on top of* the onset pop, so kicks read brighter than snares (which still paint bright white because they clear the onset gate but fail the band gate). (d) **Amber mix factor live-tunable**: the hard-coded `× 0.65` becomes `× u.kickEmphasis`, wiring the existing tuning-panel "kick emphasis" slider (which had been a no-op in djLandmarks since the Stage 1 early-return) to the amber-tint intensity. Default 0.6 → a confident kick paints ~43 % amber + ~57 % white; setting kickEmphasis = 0 reverts to pure monochrome landmarks (a runtime kill-switch for the colour layer). **Tuning panel honesty pass**: the panel's hint text now declares which palette each slider affects. `sigmoid k` + `sharpen exp` work in all palettes (they shape `onsetConf` in the vertex stage, upstream of every palette branch). `kick emphasis` works in all palettes (Stage 2 djLandmarks amber mix + Serato-faithful M10.5o kick layer). `bloom gain` / `chroma floor` / `chroma range` are explicitly labelled as Serato-palette-only — djLandmarks early-returns before the M10.5l onset-bloom and M10.5m(a) saturation stages, so those three sliders are no-ops in the default palette by design (revert via Preferences → Waveform palette → Serato-faithful to use them). **Net visual**: sustained content paints grey across the full envelope width (proportional to RMS, capped at 0.55 brightness); kicks paint as bright amber bullets visibly distinct from the surrounding grey both in luminance (~1.5× brighter) and hue (warm amber vs. neutral grey). The 4/4 pattern reads as amber dots marching past the playhead — the DJ-actionable landmark. **Stage 1 beat-grid + overlay — removed**: see the new `M10.5p-grid` row above. Per-load CPU drops by the ~100 ms that the offline `analyze_beat_grid` pass cost. **What's still pending (Stage 3+)**: M10.5q — vocal-band indicator (gate a thin coloured stripe on band[5–6] activity = ~600 Hz – 4 kHz, the vocal formant range; mute the stripe when the gate's below threshold so a strictly-instrumental break reads as a *gap* in the vocal stripe alongside its envelope gap, double-confirming the break). M10.5r — drop / build markers (look-ahead RMS envelope contour analysis — a sustained low-RMS plateau followed by a high-RMS step is a drop; a rising RMS slope into the plateau is a build; both rendered as overlay glyphs on the overview strip and the playing waveform). **Stage 3.1 — calibration (2026-05-13, user-driven, "is this how it's supposed to look")**: Stage 3.0 shipped a working filter pipeline + a working `lfPeak × onsetConf` gate, but the visual still painted as essentially pure grey with no readable amber on kicks. Three compounding problems, fixed independently. **(a) Filter cutoff too high**: 250 Hz left the snare fundamental (180–200 Hz) sitting in the flat passband, so the snare's `lfPeak` was ~90 % of a kick's at matched broadband peak — not the 3–5× separation the gate needs. Lowered to **180 Hz**, putting the snare fundamental at the -3 dB shoulder and the snare LF residual measurably below the kick's. New unit test `snare_lf_residual_stays_below_kick` asserts the ratio < 65 % at matched broadband amplitude — a regression tripwire for any future filter-tuning that erodes the separation. The kick body's 200 Hz harmonic loses ~12 dB but the kick fundamental at 50–80 Hz dominates per-chunk peak amplitude anyway, so kick lfPeak is essentially unchanged. **(b) `lfPeak` smoothing flattened kicks**: the vertex stage's `[1, 2, 1] / 4` averaging kernel across ±1 chunks (mirroring the broadband min/max/rms smoothing) turned a 1-chunk kick spike at 0.85 into ~0.45 by averaging it with quieter neighbours. Sustained sub-bass — already a slowly-modulating waveform with similar peak values across neighbouring chunks — was barely affected, so the *kick / bass dynamic range collapsed* through the smoothing pass. Replaced with **peak-of-3** (`max` across ±1 neighbours): kicks keep their full amplitude, sustained bass is essentially identical to the unsmoothed value (because neighbours already share its level), and the gate downstream gets a high-dynamic-range signal to threshold against. **(c) Linear gate + un-saturated amber + low default emphasis**: Stage 3.0 painted `amberMix = kickGate × kickEmphasis` (linear), with default `kickEmphasis = 0.6` and an amber colour that mixed with a near-white grey base. On a confident kick with `kickGate ≈ 0.35` (after the old smoothing) and `kickEmphasis 0.6`, the result was 21 % amber + 79 % near-white = a *peach* tint, not amber — visually indistinguishable from "warm grey" at the visual scale. Three coordinated fixes: **(c1)** **smoothstep threshold** — `kickAlpha = smoothstep(0.08, 0.30, kickGate) × kickEmphasis` replaces the linear scale; below kickGate 0.08 paints zero amber (sustained content with weak flux), above 0.30 paints full amber (confident kicks), smooth shoulder in between so borderline chunks don't pop in and out frame-to-frame. **(c2)** **gate-luminance-modulated amber** — the amber colour itself scales brightness with kickGate: `amberLum = 0.55 + kickGate × 0.45`, so soft kicks paint dimmer amber and hard kicks paint brighter amber, preserving amplitude info even when the grey base is fully overwritten. **(c3)** **default `kickEmphasis` 0.6 → 1.0** — the slider's semantic changed from "linear amber-mix factor" (where 0.6 was meaningful) to "kickAlpha clamp ceiling" (where the meaningful default needs to be 1.0 for full saturation on a confirmed kick). Range stays 0.0–1.5 so the user can dial down toward monochrome or push past 1.0 for an over-saturated look. **Costs**: 0 LoC added to the hot path (the filter cutoff is a `const`, the smoothing change is `max` vs. `+ * 0.25`, the gate change is `smoothstep` vs. `clamp`); +1 unit test (`snare_lf_residual_stays_below_kick`); workspace test + clippy + xcodebuild all clean. **Revert paths**: tuning panel `kick emphasis = 0` still reverts to pure monochrome (Stage 1 baseline); the four-level revert ladder from Stage 1 is unchanged. **Acceptance (Stage 1 + 2 combined)**: (a) the envelope reads as visibly *quieter* during a 4-bar break than during a chorus (Stage 1's dynamic-range floor at 0.06 + cap at 0.55 for sustained content makes breaks visible as near-gaps rather than dim streaks); (b) within a kick-heavy drop, individual kicks pop as bright amber bullets *brighter and warmer than* the surrounding sustained bass, regardless of whether the sub-bass is rolling underneath at the same RMS as the kick body (Stage 2's transient prominence); (c) the eye finds the kick pattern in <1 s on first look, with no beat grid required. **Costs**: workspace `cargo test` clean; clippy `-D warnings` clean; xcodebuild clean. Per-track-load CPU baseline is restored (no beat-grid pass). Per-frame renderer cost unchanged — Stage 2 reuses existing `VertexOut` fields (`onsetConf`, `bandLow.y`) that the M10.5l + M10.5h pipeline already computed; no new uniforms, no new buffers, no new draw passes. | 1 day |
| **M10.5n** | **Playhead-vs-audio drift — root-cause fix** — *shipped (the manual-slider half from the previous attempt was a band-aid and is reverted)* | **Symptom (reported during M10.5l shakedown, 2026-05-12 / 13)**: the audible kick happens slightly before the corresponding chunk crosses the playhead, AND the gap visibly widens as the track plays — small at the start, ~1 s by track-end on a 4-minute track. Initially mis-diagnosed as a steady-state `display_present − audio_buffer` differential (which is real but tiny: 5–20 ms constant) and "fixed" with a manual `avOffsetMs` slider in the tuning panel. The slider masked the problem but didn't solve it — a value tuned at 0:30 is wrong at 3:30 because the actual error is **linear in track time**, not constant. **True root cause**: peak chunks are cadenced in **track frames** (the offline analyzer in `dub-peaks` produces one chunk per 64 *track* samples), but the renderer was indexing them in **engine frames** with an integer-rounded conversion. The path was: `peaksChunkDurationSecs = 64 / track_sr` (correct, exact, e.g. `64/44100 = 0.0014512 s`) → `samplesPerPeakChunk = round(peaksChunkDurationSecs × engine_sr) = round(69.66) = 70` (the bug — drops 0.35 samples per chunk = 0.49% per-chunk error) → `chunk = elapsed_secs × engine_sr / samplesPerPeakChunk`. On a 44.1 kHz track / 48 kHz engine the per-chunk error of 0.49% compounds to **~804 chunks of drift over 240 s of playback ≈ 1.17 s of accumulated visual lag**, exactly matching the reported symptom. (Same-SR tracks — e.g. 48 kHz track on 48 kHz engine — have zero drift because `peaksChunkDurationSecs × engine_sr` is already integer, so the bug was invisible on test fixtures.) **Fix (~5 LoC in renderer, no FFI change)**: bypass the integer-rounded intermediate entirely. Store `peakChunkDurationSecs: Double` in `WaveformRenderer` from the engine's already-exact f64 report, and use it directly: `playheadChunk = floor(elapsed_secs / peakChunkDurationSecs)`. No engine-SR scaling step, no integer rounding, no per-chunk error. Verified by hand-calculation: on the 44.1 kHz / 48 kHz scenario the new formula gives `240 / 0.0014512 = 165,375` chunks at 4 min, matching the engine's actual playback position to within `f64` precision (~1e-9 s). **Slider removal**: `WaveformTuning.avOffsetMs` deleted, "AV sync" section removed from the tuning panel, the `dub.waveform.tuning.avOffsetMs` `UserDefaults` key one-shot-migrated to nil on launch so a `defaults read com.klos.dub` doesn't show a stale value. **What stays not-quite-perfect**: the shader-side band/onset cross-reference uniforms (`samplesPerPeakChunk` and `samplesPerBandChunk` passed to `Shaders.metal`) still use the rounded integer counts because the shader only uses them as a *ratio* against each other at small chunk indices (the visible region — a few thousand chunks max). The same 0.49% error there manifests as ≤ 1 band-chunk of misalignment across the entire visible strip, below perception. A proper fix would require new FFI methods returning the native track-frame cadences (e.g. `peaks_chunk_native_samples(deck) -> u32 = 64`) and bumping `FFI_VERSION` 9 → 10; deferred until a user-perceptible symptom in the band coloring motivates it. **Acceptance**: with a 4-on-the-floor reference track playing for 4 minutes, the kick at 0:30 and the kick at 3:30 both cross the playhead exactly when audible, with no visible drift between the two. Default behaviour (no slider) — the alignment is correct out of the box on every engine/track sample-rate combination. | 0.5 day |
| **M10.5m(b)** | **9-band sub-bass split** — *deferred to M11* | The second half of the originally-planned M10.5m row, parked for after M11 (Serato library import) lands so we have a real DJ-curated track set to validate the colour change. **Plan when revisited**: bump `dub-spectral::NUM_BANDS` from 8 to 9 by splitting the lowest log-band into sub-bass (30–60 Hz) and kick-band (60–200 Hz). The shader's `bandMix` red-channel definition splits: sub-bass paints **deep red / magenta**, kick-band paints **orange-red** — kicks now visibly read as a distinct colour from the bassline. Touches `dub-spectral` (band-layout constant, FFT-bin-grouping math), `dub-bpm` (every M8.1 genre fixture needs to be re-baked because the per-band magnitudes shift), `dub-peaks` `BandPeakChunk` (wire format gains a 9th f32 — `#[repr(C)]` size 32 → 36 bytes, breaking change for the M10.5j sidecar format → version bump), `dub-ffi` `peaks_extend` wire format documentation, shader `BandPeakChunk` struct + `bandMix`. The compute-side change is mechanical; the data-format breakage is the gnarly part — every dependent crate's tests need re-baselining and the sidecar format gets a `version: u32 = 2` bump with a v1 → v2 migration (drop v1 entries on first run; a one-time re-decode is acceptable in Phase A). Acceptance: a 909 kick reads visibly distinct in colour from a sustained sub-bassline at the same RMS, validated against real DJ-curated content imported via M11. `FFI_VERSION` += 1 when it lands. | 1.5–2 days |
| **M10.5c** | **Track Overview waveform + horizontal-orientation shader** | The two pieces of M10.5b shakedown that didn't fit in the shell pass. **M10.5c-a — shipped:** `TrackOverviewView` (SwiftUI `Canvas`) slotted on each deck's outside edge with playhead-bracket tracking + File-mode click-to-jump per the description below. **M10.5c-b — shipped:** `orientation: u32` uniform plumbed end-to-end (Metal `Uniforms` struct, Swift `WaveformUniforms`, `WaveformRenderer.orientation` property, `WaveformView(orientation:)` parameter, host `WaveformMetalView` pipes the value into the renderer and forces a uniform refresh on change, playhead overlay swaps between horizontal hairline / vertical hairline based on orientation). Default remains `.vertical` so every M10.4 / M10.5b call site renders bit-identical pixels. **Track Overview** (§9.6.1): per-deck thin vertical strip on the deck's outside edge (`DubLayout.deckOverviewWidth ≈ 36 px`) showing the *whole* track top→bottom with a playhead-bracket indicator at the current `position(deck)`. Renders via SwiftUI `Canvas` (not Metal — overview is a low-cadence, fully-known-up-front signal that doesn't benefit from GPU instancing; `Canvas` keeps the pipeline simpler and the shader inventory smaller). Reads broadband peaks via `peaks_extend(deck, 0)` once at load, decimates to ≈ 300 buckets (the strip's pixel height at typical window sizes), redraws only when the playhead chunk changes (≈ 30 Hz from the existing position poll). **Click-to-jump** plumbed for File mode immediately; Timecode-mode behaviour gated on M10.6's Panic-Play wiring. **Horizontal-orientation Metal uniform**: adds `orientation: u32` (0 = vertical, 1 = horizontal) to `WaveformUniforms` and the matching `Shaders.metal` constant buffer; the vertex shader picks the NDC x↔y assignment based on the uniform. Vertical orientation is the default and the M10.4 / M10.5b behaviour is bit-identical; horizontal flips the playhead from "25 % from top" to "25 % from left" with the future to the right of the playhead. Lights up Prep mode's horizontal layout in M10.8 without that milestone needing to touch the shader. No FFI version bump (renderer-only). | 2 days |
| **M10.6** | **Mouse transport + position navigation + Repeat** | Split into M10.6a–d (engine work concentrated in 10.6b, UI work split across the others). **M10.6a — shipped (Casual Play UI + zoomed click-scrub).** Deck-header transport-glyph cluster (Play/Pause toggle + Restart) added to Row 3 of `DeckHeader` — renders exactly when a file track is loaded (`timeRow != nil`), so it covers both Prep-mode and the Casual-Play-before-Timecode case. New `WaveformAppModel.{restart, scrub}(side:...)` methods plumbed into the header via a `DeckHeaderCallbacks` value (closures kept off `DeckHeaderState` to preserve `Equatable`). `WaveformView(onClickScrubRelativeSecs:)` installs an orientation-aware transparent hit-test layer beneath the playhead overlay; click → signed seconds-from-playhead via the same `chunksPerPixel × samplesPerPeakChunk / sampleRate` ratio the renderer uses, so a click lands on the visual chunk under the cursor. New nonisolated `WaveformRenderer.secsPerPixel(sampleRate:)` helper centralises that math. PRD §6.1 gating: the closure is wired only when `engineMode == .prep`; Timecode-mode panes pass `nil` so the gesture isn't installed at all (no fine-scrub on a timecode-controlled deck, regardless of Panic Play state). No FFI bump (renderer + UI only). **M10.6b — shipped (Panic Play engine + FFI).** New `LiftPolicy::force_disengaged()` (preserves `last_locked_rate` while clearing the engaged flag + sticky counter — the next `Locked` is by construction a fresh re-engagement). New engine-level `PanicPlayState { engaged, held_rate }` per deck; `PanicPlayState::normalise_held_rate` collapses negative / near-zero candidates to a positive forward rate per PRD §6.1.2 ("runs the audio track forward"). New `Command::DeckPanicPlay { idx }` / `DeckCancelPanicPlay { idx }`. `Engine::engage_panic_play(idx)` captures the held rate (preferring `LiftPolicy::last_locked_rate()` when a timecode input is attached, falling back to `deck.rate()` otherwise), force-disengages the policy, sets the deck rate + playing, and flips the new `DeckSharedState::is_panic_play` atomic. `Engine::drive_timecode_inputs` branches on panic state: in panic mode `Locked` intents auto-cancel (clean re-lock = "DJ dropped the needle back on the groove"), `DropoutHoldRate` intents are ignored (the whole point — the deck keeps playing while the needle is off the platter). `Engine::cancel_panic_play(idx)` pauses the deck and clears the flag; idempotent on non-engaged decks. `EngineHandle::DeckCommand::{panic_play, cancel_panic_play}` send the new commands; `DeckSnapshot.is_panic_play` exposes the atomic for the UI. FFI surfaces `panic_play(deck)` / `cancel_panic_play(deck)`; `PositionInfo` gains `is_panic_play` so the existing 30 Hz UI poll picks up the engine state. `FFI_VERSION` 7→8. **Test coverage:** 11 new tests — 3 policy tests (force-disengaged clears flag + counter, preserves last_locked_rate, requires engage-threshold to re-lock), 8 engine tests (engage from policy, fallback to deck rate, negative/below-floor normalisation, dropout-stays-panicked, Locked-clears-engaged, cancel-pauses-deck, cancel-idempotent, default-disengaged, alloc-free), plus 1 end-to-end test that engages panic and renders synthetic CV02 carrier blocks through `engine.render` to verify the auto-cancel path lands correctly. All 350+ workspace tests still green; clippy `-D warnings` clean. **M10.6c — shipped (Panic Play UI + Timecode overview un-gate).** `DeckState.isPanicPlay: Bool` field driven by the existing 30 Hz `PositionInfo.isPanicPlay` poll (engine remains the authority — UI also sets it optimistically on `panic(side:)` for zero-frame latency, but the poll over-writes it every tick so an engine-side auto-cancel on clean re-lock propagates within ≤33 ms). New `WaveformAppModel.{panic, cancelPanic, panicToggle}(side:)` wrap the M10.6b FFI methods with the same error-surfacing path as Play/Pause. `DeckHeaderState` grew `isPanicPlay` + (initially) `panicGlyphVisible` flags and a new `Source.tcHold` variant; `DeckHeaderState.from(...)` derives them: glyph visible iff `thruMode && hasTrack`, `source = .tcHold` when `thruMode && isPanicPlay`. `TrackOverviewView.handleTap` un-gates: the two-deck-Timecode early-return allows the seek when `deckState.isPanicPlay` is true (§6.1 release condition). M10.6c's lifepreserver-glyph + dedicated Restart button were superseded by M10.6d below — the rest of the M10.6c plumbing (model layer, source pill, overview un-gate) stayed and is what M10.6d builds on. No FFI bump for c. **M10.6d — shipped (transport-cluster redesign + library polish + cancel-doesn't-pause).** Fixes the "Play does nothing in Timecode mode" bug at the root: pressing the deck-header Play button in Timecode mode previously called `engine.play` which set `is_playing = true` only to be overwritten by the very next `drive_timecode_inputs` `DropoutHoldRate` block. The fix is to surface Panic Play *as* the Timecode-mode Play affordance — one button, Serato-style INT/ABS toggle. `DeckHeaderState.panicGlyphVisible` renamed to `useTimecodeToggle` to reflect its expanded role. `DeckHeader.transportGlyphs` collapses to a single `primaryButton` that branches: Prep mode → classic Play/Pause via `onPlay` / `onPause`; Timecode mode + track loaded → `onPanicToggle` only, icon flips between `play.fill` (currently following platter — tap to play internally) and `opticaldisc.fill` amber (currently internal — tap to re-engage timecode). M10.6c's lifepreserver glyph is gone (subsumed) and the M10.6a Restart button is gone (overview click-to-top covers it, §6.1.3). Engine semantics tweak: `cancel_panic_play` no longer pauses the deck — it clears the engaged flag + atomic and hands transport authority back to the timecode driver. A healthy carrier produces an immediate Locked re-lock (deck stays audible, true INT→ABS hand-back). A silent carrier yields `DropoutHoldRate` on the next block which pauses the deck via the existing arm — same outcome as the pre-M10.6c "pause on held position" path, without the race against the next Locked sample. `Command::DeckCancelPanicPlay` / `EngineHandle::cancel_panic_play` / FFI `cancel_panic_play` doc comments updated. `WaveformAppModel.cancelPanic(side:)` no longer optimistically sets `isPlaying = false`; the next 30 Hz poll reflects whatever the engine decides. Replaced engine test `cancel_panic_play_pauses_deck_and_clears_shared` with `cancel_panic_play_clears_state_and_leaves_transport` + added 2 new tests: `cancel_panic_play_then_locked_intent_keeps_deck_playing` (synthetic CV02 carrier through `engine.drive_timecode_inputs` after cancel → deck stays playing at platter rate), `cancel_panic_play_then_silence_pauses_deck_via_dropout_path` (silent ringbuf → DropoutHoldRate → deck pauses naturally). FileBrowser polish: folders now require **double-click** to descend (single-click was too easy to trigger by accident while scanning); the drag-out preview is a small `waveform` glyph instead of the row's full song-name text. Workspace `cargo test` clean, clippy clean, xcodebuild clean. No FFI bump (Phase A pragmatism — behavior change, same signatures). **M10.6e — Repeat.** LFSR run-out auto-trigger that engages the same engine state as Panic Play, plus a per-deck Repeat toggle in the deck header (§5.4.2). | 3 days (a–d shipped, e remaining) |
| **M10.7** | **Phase-Drift Trail** | Dub's headline beat-matching aid (§9.4). New `dub-match` crate (sibling of `dub-bpm` / `dub-peaks` / `dub-spectral`): a `MatchStream` analysis thread consumes both decks' `dub-bpm` ODFs off-RT, computes a rolling cross-correlation over ≈ 2 bars with a ±1-beat lag window (~40 lag candidates × 200 frames per update), emits `MatchSample { phase_ms, confidence, timestamp }` at 30 Hz to an SPSC ring. Audio-thread cost: **zero** (ODFs already running). FFI: `matchExtend(start_idx) -> Vec<u8>` mirroring `peaks_extend`. UI: `apple/Dub/Performance/PhaseDriftView.swift` — Metal-rendered vertical strip ≈ 80 px wide in the centre gutter, time **bottom→top** matching the waveform direction discipline (§9.1 / §9.4), dot brightness = confidence, dot colour blended from deck tints; numeric overlays `Δ BPM = +0.3` (top, slope-derived) and `Δ ms = +12` (bottom, instantaneous). Grid-agnostic by construction; degrades gracefully (dim dots) when ODFs are weak. `FFI_VERSION` bumps to 9. **Single mode only — no Preferences toggle for "numeric-only" variant in v1.** | 5 days |
| **M10.8** | **Track Preparation Mode shell** | Auto-detection of available audio interface at launch (§3.1). If no multi-channel interface present, the app boots into Track Preparation Mode — alternate root view (`apple/Dub/Prep/PrepView.swift`) hosting a single-deck **horizontal** waveform full-width, the library prominent below. Manual override in Preferences (`Mode: Auto / Performance / Preparation`). **Shell only:** the mode renders the chrome and supports load + play / pause; **no** beatgrid editor, **no** hot-cue prep, **no** track gain tweaking yet (those are v1.x per §3 — they'd substantially expand v1 scope and the user explicitly chose option (a) shell-only in the M10.3 planning round). The mode's *purpose* is visible from M10.8; its *tooling* arrives in v1.x. | 2 days |
| **M11** | **Library import: Serato** | Import Serato library, browse it, load tracks, beatgrids appear. | 1 week |
| **M12** | **Library import: rest** | Traktor + rekordbox + iTunes + Lexicon. | 1–2 weeks |
| **M13** | **Looping** | Manual + auto-loop, halve/double, behaves correctly under timecode. | 4–6 days |
| **M14** | **Key Lock + auto-bypass** | Rubber Band integrated, on/off per deck, scratch-aware auto-bypass per §6.1.1. | 1 week |
| **M15** | **Smart FX: Echo-Out** | Tap-and-hold echo-out works on both decks (incl. on a Thru deck — FX modules live inside the per-deck chain with per-module declick, see §5.2.1). | 4–5 days |
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
| Auto-BPM accuracy on dub / minimal genres (sparse beats, half-time feels) | Medium → Low | **First-line:** M7.5's offline driver lets us evaluate the BPM engine against a fixture corpus of target genres on the bench (`cargo test`) before risking it on live audio. M8.1's log-band ODF + windowed-energy picker resolves the user's stated genre mix (reggae 65, hip-hop 90/100, rolling dnb 174) at the correct octave without any genre hints; see [`docs/SHIPPED.md#m81`](SHIPPED.md#m81). **Second-line (already shipped):** `BpmRange` escape hatch (`dub thru --bpm-range MIN,MAX`, `analyze_bpm_with_range(samples, sr, ch, range)`) constrains the search to a user-chosen window for the irreducibly-ambiguous genres (dubstep 140 / 70, K-S-backbeat dnb 174 / 87) the algorithm cannot resolve without a prior. **Third-line (future):** real-music validation can still motivate an `aubio-rs` feature backend on `dub-bpm` if a class of tracks falls outside both the algorithmic gate and the range-flag escape hatch — but the M8.1 architecture has reduced this risk from the "blocking" level we started at. |
| Chromaprint robustness to turntable pitch drift / mixer EQ (v1.1) | Medium | Validate during v1.1 with real-world test corpus. Fall back to Shazam-style constellation hashing if Chromaprint underperforms. |
| Thru latency perceived as "feel different" by sensitive scratch DJs | Low–Medium | Hold latency below the ~5 ms scratch-imperceptibility threshold (PRD §6.1) with a 64-frame buffer / 48 kHz path. Keep it *constant* across FX state (Option A in-chain FX bypass, §5.2.1 / §5.2.2) so the DJ internalises one timing relationship for the whole set instead of one per FX engage. Document the trade-off; if hardware Thru is required, the operator uses the interface's physical button (which trades away BPM/waveform/FX for zero latency). |
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
8. **CI runner for nightly soak/fuzz** — GitHub Actions has limits; may need a self-hosted runner (e.g., a spare Mac mini) once nightly soak exceeds free CI minutes. Defer the decision until soak is actually running into the limit (Phase B, M18 onward); through Phase A the per-PR pipeline fits comfortably.

### 13.3 Items explicitly deferred to v2

- Phase support (full subsystem, including SDK access and integration)
- Hot cues (entire system — no v1 lite version)
- Recording (master out + per-deck)
- Windows port
- HID controller ecosystem
- Key detection
- Stems (probably never)

### 13.4 Items explicitly deferred to v1.x

- Saved loop slots (M11 schema includes empty `track_loops` table for forward-compat)
- Sampler expansion 4 → 6 slots (if real use demands)
- Phase-Drift Trail numeric-only Preferences variant
- Track Preparation Mode prep tooling (beatgrid editor, hot-cue prep, gain UI)

---

## 14. Acceptance criteria for v1.0

Dub v1.0 ships when **all** of the following hold on a DMG installed on a clean macOS 14+ machine:

1. Scratch DJ can plug a class-compliant 4-in/4-out USB interface (test rig: SL3 or Audio 6), route timecode inputs to In 1/2 and 3/4, route outputs to their hardware mixer, and have both decks under timecode control with **< 5 ms latency at 64-sample buffer, < 10 ms total timecode-to-audio response.**
2. Both Serato CV02 and Traktor MK2 vinyl supported.
3. Either deck can be set to **Thru** (real record routes through the engine, ~2.7 ms one-way latency, FX-capable). Hardware bypass on the interface itself is outside Dub's scope — see §5.2.2.
4. **Auto-BPM detects tempo of a real record played in Thru mode within 15 seconds, with a confidence indicator that distinguishes "tentative" from "locked".**
5. **Live waveform of a Thru-mode real record is rendered as the record plays, at 60 fps, with no glitches.** (Persistence and recognition land in v1.1.)
6. Echo-Out and Dub Siren can be applied to a Thru deck (i.e. FX work on real records); engaging FX does not change the deck's input-to-output latency.
7. User can import their existing Serato / Traktor / rekordbox / iTunes / Lexicon library and play tracks with imported beatgrids. Auto-detect grids fall back when source has none.
8. Looping (manual + auto, halve/double) works correctly under timecode.
9. **Key Lock works on both decks; engages and disengages automatically based on playback rate per §6.1.1; user hears no glitches during scratching with Key Lock on.**
10. Echo-Out, Dub Siren, Sampler (4 slots), Quick Scratch (4 slots, hotkey fast-load), Instant Doubles all work per §6 / §7.
11. UI is keyboard-navigable end-to-end. **No performance gesture** (pitch / scratch / crossfade / EQ / gain / cue) requires the mouse — per §1's refined mouse rule. Mouse-driven *transport* (Panic Play, Casual Play, position navigation per §6.1) is in v1 and *not* in conflict with the philosophy.
12. **Panic Play (§6.1.2)** recovers from a needle dirt event without audible interruption: keystroke transitions the deck from timecode-driven to last-known-velocity playback, audience hears no glitch, automatic resume on clean LFSR return verified in a manual rig test.
13. **Phase-Drift Trail (§9.4)** renders in the centre gutter, updates at 30 Hz with ≤ 1 frame of stutter at 60 fps UI, correctly slopes when tempos differ and centres when in phase. Verified on canned-mix fixtures (`crates/dub-match/tests/`).
14. **Track Preparation Mode shell** (M10.8) auto-boots when no multi-channel interface is connected; can load + play a file from the library at horizontal-waveform resolution.
15. Zero xruns in a 60-minute scratch session at 64-sample buffer on M2 Air.
16. README + first-run experience documents how to set up a typical rig (turntables → interface → mixer → speakers) and a Thru-mode rig (real record → interface → engine → mixer).
17. **All §2.2.6 reliability SLOs met**: zero crashes in 100 cumulative beta-gig-hours; zero xruns in soak; zero RT-thread allocations; zero fuzz crashes in last 7 days; no benchmark regressions; manual rig checklist (§2.2.10) signed off.

---

## 15. Out of scope for v1 (reaffirmed)

- Internal mixer mode (user-facing)
- Mouse-driven **performance gestures** (pitch / scratch / crossfade / EQ / gain / cue). Mouse-driven transport (Panic Play, Casual Play, position navigation) is **in scope** — see §1 for the rule, §6.1 for the surface.
- Hot cues (entirely deferred to v2 — no v1 "lite" version)
- Saved loop slots (deferred to v1.x — v1 ships ephemeral loops only)
- Sampler beyond 4 slots (v1 is 4; expansion to Serato-parity 6 deferred to v1.x if real use demands it)
- Track Preparation Mode editing tooling — beatgrid editor, hot-cue prep, gain tweak (v1 ships the *shell*; tools land in v1.x)
- Phase-Drift Trail numeric-only Preferences variant (single design in v1; alternative ships in v1.x if needed)
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
