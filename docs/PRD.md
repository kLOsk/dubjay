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
- **No Mouse DJ for performance gestures.** A *performance gesture* — pitch, scratch, crossfade, EQ, gain, cueing live to the audience — never goes through the mouse. Those live on the turntable + external mixer + keyboard, always. The mouse **is** allowed (and welcome) for: configuration in Preferences; library navigation and search; loading tracks onto decks; **track-position navigation and cue-point location** (single click on the overview to jump the playhead, vinyl-style drag-scratch on the zoomed waveform — see §6.1); and the explicit **transport / recovery controls** — Play, Pause, Restart — used for dirty-needle recovery (§6.1.2) and casual pre-performance playback (§6.1.3). The forbidden list is short and precise: no software crossfader, no software EQ, no software cue/preview channel, no software pitch fader. **Mouse-driven vinyl-style scratching is allowed but only as a cue-locating affordance**, not as a routed-to-FOH performance gesture: the rate is driven by the cursor's velocity (mouse-still means silence, just like a stylus on a stationary platter), it never auto-plays from the click, and the audience never hears it because the DJ's external mixer is the only thing routed to the FOH. The DJ uses it to find the *exact* downbeat of an intro or the start of a kick before the platter takes over. Everything else is on the table. See §6 for the positive list, §5.3 / §6.6 / §15 for the negative list.
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
| **Track Preparation Mode** | Only the built-in soundcard is detected (no multi-channel interface) | Single deck, **horizontal** waveform full-width, library prominent | Auditioning tracks, fixing beatgrids, prepping cues — work the DJ does in advance of a gig, on the couch with no rig attached. **v1.0 ships the shell only** (load + play + horizontal waveform); the actual prep tooling (beatgrid editor, hot-cue prep) is v1.x — see [§12 M10.8 row](#12-milestones) and [SHIPPED §M10.8](SHIPPED.md#m108). |

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
- **Track time display** — shown in the deck header. Live read of the engine's deck-rate-aware playhead, formatted `MM:SS`. **Performance / Timecode mode** shows *only* the remaining time as `-MM:SS` — the two-deck split is space-tight in the header and "how long until I have to mix" is the only number the DJ touches mid-set (PRD §1 "every screen pixel earns its keep"). **Prep / Track-Preparation mode** shows both elapsed (`MM:SS`) and remaining (`-MM:SS`) because the single-deck rehearsal surface has the real estate and elapsed time is useful for hot-cue placement. Total length is omitted in both modes — duration + the displayed value gives total trivially.
- Auto **gain trim** based on track loudness (LUFS-I or peak normalization, user choice)
- **Mouse-allowed transport surfaces** (per the §1 mouse rule and §6.1.2 / §6.1.3 below):
  - **Click-to-jump on the overview waveform** seeks the deck to the absolute track position under the cursor. Single click only — the overview is a "where am I in the whole track" map, not a fine-positioning tool, so it intentionally does not support drag-scrub. Transport is left alone: a paused deck stays paused at the new position, a playing deck keeps playing from the new position. Works in both Performance and Prep mode. In two-deck Timecode the seek lands instantly and the timecode driver re-locks on the next confident sample.
  - **Click + drag on the zoomed waveform** for **vinyl-style scratch-cueing**. The cursor's *velocity* drives the deck's playback rate every block. Drag direction per orientation: in **vertical** mode the future region of the waveform sits below the playhead (§9.6), and dragging *down* plays the deck forward at the cursor's speed — this matches the "drag toward the future" intuition that DJs already use on vertical-waveform software (Serato, rekordbox, Engine). In **horizontal** mode the convention is the mirror of the visual scroll instead: forward playback scrolls the waveform leftward through the playhead, so dragging *left* plays the deck forward, like grabbing the platter and pushing it forward. Drag faster and the deck plays sped-up; drag the opposite way and it plays in reverse; hold the cursor still and the playhead freezes silently, exactly like a stylus on a stationary record. There is no auto-play on click — pressing without moving produces silence. In Timecode mode the shell engages Panic Play for the duration of the drag so the timecode driver doesn't fight the cursor-driven rate; on release Panic Play cancels and the pre-scratch transport (play / pause) is restored. The DJ uses this surface to find the *exact* downbeat of an intro or the leading edge of a kick before passing the deck to the platter. Works in both Performance and Prep mode.
  - The §1 carve-out is mechanical: this *is* a mouse-driven scratch (continuous rate modulation), explicitly allowed for cueing because the audience never hears it — the DJ's external mixer is the only routing to FOH, and a cueing scratch lives on the cue channel / headphones. The DJ would never use the mouse to scratch during a live mix; the platter is on the deck for that. The PRD's earlier ban on mouse-DJ-as-performance still holds — what's new is the recognition that *off-air* mouse scratching is the right cueing tool for a wide-track waveform.
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

**Opt-out for the load-into-playing guard (M10.5r):** Preferences exposes an `Allow loading onto a playing deck (Performance mode)` toggle, default off. When on, drops / Space-loads onto a playing deck succeed silently — the new track replaces the old one mid-play. The toggle exists for the rehearsal-style workflow where a single DJ is bouncing between two decks and wants to drop the next track without lifting the needle first. The default stays "refuse + red flash" because that matches the on-stage muscle memory of every other DVS app and prevents accidental cue-track loss. **Prep mode always allows the load** regardless of the toggle — Prep is a single-deck shell where the "currently playing in front of an audience" concern doesn't apply.

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
- **Performance / Timecode mode:** `-MM:SS` remaining only. Right-aligned. Audience-facing "30 seconds to mix" cue (§6.1).
- **Prep mode:** `MM:SS` elapsed (left) · `-MM:SS` remaining (right). Both shown because the single-deck rehearsal surface has the screen room and elapsed time is the natural anchor for hot-cue placement.
- **Thru mode (no track loaded):** the row is omitted entirely — no canonical playhead concept when timecode is driving the rate.

Header height is fixed at ≈ 92 px regardless of which rows have content. Empty rows preserve their height to avoid layout reflow when source mode changes mid-set.

### 9.6 Waveform rendering

- GPU-rendered via Metal (`MTKView`).
- Two views per deck: **overview** (whole track, thin vertical strip on the deck's outside edge) + **zoomed** (≈ 4 bars, wide vertical strip on the deck's inside edge).
- **Playhead is fixed at 25 % from the top of the zoomed column**; during forward playback the waveform scrolls *upward* through the playhead at `1 / (engine_rate)` pixels per sample (future rises from the bottom, plays into the playhead, slides upward into the past above). During reverse playback the waveform scrolls *downward* — direction follows engine rate sign, never inferred or corrected. The user sees the future below the playhead, the past above. The overview's playhead marker tracks the same position as a horizontal hairline.
- **Empty-groove rendering at track edges.** Both the past region (above the playhead) and the future region (below the playhead) always draw their full pixel extent, even when the playhead is at the very start or very end of the track. Slots in the renderer's peak-chunk ring buffer beyond the loaded `peaksLen` range are guaranteed zero — the ring is zero-initialised at construction and on every `reset()`, and the bounded `ingestNewChunks` writer only ever fills `[0, peaksLen)` — so the shader reads "silence" peaks for any column that addresses a global chunk index `< 0` or `≥ peaksLen` and the trapezoid collapses to zero height. The visual result is the **lead-in / lead-out groove of a real record**: a flat dark band of "empty room" above the track at `t = 0`, and a flat dark band below the track at `t = duration`, with the bars themselves remaining at their natural pixel-per-chunk cadence. The dark band carries a 1-px white-at-22 %-opacity zero-crossing line through its centre (the amplitude=0 axis) so it reads as "needle is on the platter, just no audio" rather than pure black; the same line runs through the bars region too, but is largely hidden by the bar geometry which is centred on it. Without this the regions used to *collapse* as the playhead approached an edge (e.g. at `t = 0` the past region shrunk to zero width and the bars "warped" against the strip edge), which felt wrong against the rest of the renderer's fixed-axis design. The empty-groove path is gated on the ring having enough zero-init headroom past `peaksLen` to keep both regions clear of loaded data; tracks long enough to violate that (≈ > 25 min at the broadband 64-sample chunk cadence) fall back to the pre-existing collapse-at-edges behaviour rather than risk a column wrapping into real chunks.
- **Playhead can be pushed past the track edges.** The renderer drives its `playheadChunk` arithmetic off `PositionInfo.playhead_secs_unclamped` (FFI v14, M10.5t) rather than `elapsed_secs`. The unclamped field carries the raw `position_frames / sample_rate` from the audio thread, which the deck already lets walk past `[0, frames)` while emitting silence (matching a real platter being scratched off the end of its run-out groove). The renderer's ring-offset math is signed-Int64 with Euclidean modulo, so a negative or post-`peaksLen` global chunk index wraps cleanly into the zero-padded tail of the ring and the past / future regions render silence in the right places. Time-display consumers (deck header, track overview bracket) keep using the *clamped* `elapsed_secs` so a hard scratch past the end doesn't make the running counter run negative on screen.
- **Click-to-jump on the overview column** (per §6.1) seeks the deck to that absolute position. Single click only — the overview does not support drag-scrub or scrub-with-audio. Transport state is left alone (paused stays paused at the new position; playing keeps playing from it). Works in both Performance and Prep mode. Both the click-to-fraction maths and the playhead bracket position are computed on the **same chunk grid the bars are laid out on** (`peaksLen × chunkDurationSecs` seconds) rather than through the deck's reported `durationSecs`. This is the same M10.5n principle applied to the overview: peaks are cadenced in track frames, all seek and playhead maths must stay on that grid, otherwise a sub-millisecond mismatch between `track.frames() / track_sr` and `peaksLen × chunkDurationSecs` accumulates into a visible "which bar represents the current audio" misalignment by the end of the file. The overview also reserves a small dark padding region (8 pt) at each end of its time axis so the first and last bars don't kiss the strip edges — without it the bars read as a solid block and feel "warped" against the strip corners. Click positions inside the padding clamp to the nearest bar.
- **Drag-to-scratch on the zoomed column** (per §6.1) is the cue-locating surface — a vinyl-style rate-driven scratch. The mouse cursor's *velocity* (pixels per real-second, converted to audio-seconds per real-second) drives the deck's playback rate every block; cursor-still means rate-zero means silence, exactly like a stylus on a stationary platter. There is no auto-play on click and no implicit seek-to-cursor: the cursor maps to motion, not to position. In Timecode mode the shell engages Panic Play for the duration of the drag so the timecode driver doesn't fight the cursor-driven rate; on release Panic Play cancels and the pre-scratch transport (play / pause) is restored. Allowed in both modes — see §1 for the cueing-affordance carve-out. **Rate derivation is per-event, not polled** (M10.5t rework): each `onChanged` from the SwiftUI gesture overlay computes `instantRate = Δoffset / Δrealtime`, low-passes it with an EMA (α = 0.35), and writes the result through `setDeckRate`. A 60 Hz watchdog timer runs alongside *only* to ramp the deck rate toward zero when no event has fired for ≥ 25 ms (cursor held still). The earlier polled implementation snapshotted the running offset on a fixed 60 Hz clock, which beat against the 60–120 Hz cursor-event stream and surfaced as audible "jumping" on a steady drag — that is the bug the per-event path fixes.
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

The full archaeology of the M10.5h–p shader ladder that was rolled back to produce this baseline lives in [`docs/SHIPPED.md` §M10.5h → §M10.5p](SHIPPED.md#m105hp) and [§M10.8](SHIPPED.md#m108). The Rust-side `OnsetDecimator`, `BeatGrid`, and `FilteredDecimator` data primitives remain in place as dormant building blocks for future additive consumers.

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

#### 9.6.2 Future additive waveform layers — parked

The M10.8 Serato-parity baseline (§9.6.0) is the line below which all future waveform work must remain *additive and reversible*. Three concrete ideas have been scoped against the target user (scratch / urban DJ on timecode, no programmed hot cues, performance-distance glance only) and explicitly **parked for later**. None are committed to a milestone; each must be re-evaluated when picked up.

1. **Phrase landmarks (lead candidate).** Off-line analyzer (new `dub-segment` crate or `dub-bpm` feature) detects structural boundaries — drop, breakdown, verse-in, outro — from discontinuities in the spectral-flux ODF that `dub-bpm` already produces. Result: small chevrons painted *in the existing reserved gutter beside the waveform*, green for energy-up, red for energy-down. Conservative confidence threshold so the failure mode is "no chevron" rather than "wrong chevron." Cached in the M10.5j sidecar. Differentiator: **no commercial DJ app surfaces phrase boundaries on the playing waveform**, and this fills the cue-point role that scratch DJs on timecode otherwise can't program. Cost: ~3–4 days incl. corpus validation against hip-hop / reggae tracks before exposing the feature.

2. **Ghost waveform of the other deck (cheapest win).** A second low-opacity (≈ 0.15) monochrome render of the *other* deck's `PeakBuffer` painted *behind* the current deck's coloured waveform, aligned by playhead. Makes inter-deck transient alignment visually obvious — the qualitative complement to M10.7's numeric Phase-Drift Trail. Zero new analysis (reuses existing peak data). One additional Metal draw call per deck; default-off Preferences toggle. Cost: ~0.5–1 day. The natural pairing with phrase landmarks.

3. **Vocal-presence overlay (parked pending §15 ruling).** Heuristic over the existing `dub-spectral` 8-band data (strong stable mid-band harmonics + low onset density inside that band) flags vocal-heavy sections; rendered as a dotted-line texture or local desaturation across the waveform's mid stripe. Useful for hip-hop / reggae blending discipline ("don't put two rappers over each other"). **Boundary case with §15 stems / AI separation** — the proposal is a visual annotation derived from spectral statistics with no separated audio buffer ever produced, but explicit sign-off needed before any code lands. Cost: ~2.5 days if approved.

Guardrail: each of these is a *layer*, not a rewrite. They land behind a Preferences toggle (default off until corpus-validated), they don't touch the M10.8 baseline shader's per-column colour mapping, and they ship one at a time so any regression bisects cleanly back to the M10.8 commit.

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

Each milestone has a **demo criterion** — a single sentence describing what the user can observably do at the end. **Shipped milestones (M0 → M10.8) are summarized below; full design history and rationale live in [`docs/SHIPPED.md`](SHIPPED.md). M10.5h–p shader exploration was rolled back in the M10.8 baseline freeze — see [§9.6.0](#960-waveform-baseline-freeze-m108-cleanup) for the guardrail.**

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
| **M7.5** | **BPM engine + offline analysis** | ✅ shipped — pure-Rust `dub-bpm` crate (spectral-flux ODF + harmonic-summed autocorrelation), `BpmEstimator` streaming core + `analyze_bpm` offline driver, `Track::bpm: Option<f64>`. Aubio parked as a future opt-in feature backend. → [SHIPPED §M7.5](SHIPPED.md#m75). | 2–3 days est, 1 day actual |
| **M8** | **Auto-BPM on Thru — streaming driver** | ✅ shipped — `BpmTracker` (estimator + hysteresis state machine + throttled tempo search) + `BpmStream` analysis thread + lifecycle; `ThruSource::with_bpm_tee` alloc-free audio-thread tap; `searching → tentative → locked` transitions surfaced via SPSC ring. → [SHIPPED §M8](SHIPPED.md#m8). | shipped |
| **M8.1** | **BPM octave fix (log-band ODF + windowed-energy picker)** | ✅ shipped — algorithmic point-release fixing the M8-era hip-hop 2× regression and the 128/64 BPM streaming-mode oscillation. Public `BpmRange` + `--bpm-range MIN,MAX` escape hatch for irreducibly-ambiguous genres. → [SHIPPED §M8.1](SHIPPED.md#m81). | 1–2 days est, 1 day actual |
| **M9** | **Live waveform capture (Thru)** | ✅ shipped — new `dub-peaks` crate; off-RT decimator thread producing `PeakChunk { min, max, rms }` (`#[repr(C)]` 12 bytes); `PeakBuffer` with O(new) `extend_chunks` renderer fast path; `PeakStream` lifecycle; ThruSource shared mono-downmix for BPM + peaks taps. → [SHIPPED §M9](SHIPPED.md#m9). | 4–6 days est, 1 day actual |
| **M0.5** | **Apple shell + smoke screen** | ✅ shipped — XcodeGen-generated `Dub.xcodeproj` + AppKit `@main` + SwiftUI smoke screen + UniFFI 0.28 proc-macros + universal `DubCore.xcframework` + one-shot `scripts/bootstrap.sh`. Local sign-to-run only; distribution signing is a post-M20 milestone. → [SHIPPED §M0.5](SHIPPED.md#m05). | 3 days |
| **M9.5** | **dub-spectral extraction + 8-band peak capture** | ✅ shipped — **9.5a** lifted the shared FFT pipeline into the new `dub-spectral` crate (`OnsetDetector` is a thin shell over `SpectralFrameStream`, byte-identical to M8.1). **9.5b** extended `dub-peaks` with `BandPeakChunk` (`#[repr(C)]` 32 bytes, 8 bands) + `BandDecimator` consuming the existing shared mono-downmix. Audio-thread cost zero. → [SHIPPED §M9.5](SHIPPED.md#m95). | 4 days |
| **M10** | **First waveform on screen** | ✅ shipped — UniFFI `DubEngine` surface (M10-A) + live Metal-rendered broadband waveform (M10-B) inside the Apple shell. → [SHIPPED §M10-A](SHIPPED.md#m10a), [§M10-B](SHIPPED.md#m10b). | 5 days |
| **M10.1** | **Multi-colour rendering** | ✅ shipped — vertex shader looks up `BandPeakChunk` per broadband instance; fragment shader mixes 8 bands into RGB with loudness compensation + broadband-RMS luminance. `FFI_VERSION` 3. → [SHIPPED §M10.1](SHIPPED.md#m101). | 3 days |
| **M10.2** | **Waveform polish — first wave** | ✅ shipped (3 of 7 plan bullets) — deck B wired identically, three palette presets, honest silence + clipping. `FFI_VERSION` 4. → [SHIPPED §M10.2](SHIPPED.md#m102). | 3 days |
| **M10.2 remainder** | **Polish — re-homed, then rolled back in M10.8** | Retired. Onset glow / beat-aware saturation / 9-band split / mip pyramids were each re-homed onto the M10.5h–p shader ladder. The ladder was wholesale rolled back in M10.8 in favour of the Serato-parity baseline. → [SHIPPED §M10.2 remainder](SHIPPED.md#m102-remainder), [§M10.8](SHIPPED.md#m108). | — (retired) |
| **M10.3** | **Performance shell** | ✅ shipped — real Performance View (status strip, deck headers, Metal waveform, FX/library placeholders), dev toolbar behind ⌘, Preferences, `DubLayout` tokens as single source of truth. → [SHIPPED §M10.3](SHIPPED.md#m103). | 3 days |
| **M10.4** | **Vertical waveform + symmetric two-pane layout** | ✅ shipped — Metal renderer rotated to vertical per §9.1; symmetric two-deck layout; status strip gains live battery + wall-clock per §9.3. → [SHIPPED §M10.4](SHIPPED.md#m104). | 1–2 days |
| **M10.5** | **File playback dev loop** | ✅ shipped — M10.5a (FFI: `start_engine`, `load_track`, `play/pause/seek`, `position`, `compute_offline_peaks`, `FFI_VERSION` 5) + M10.5b (Apple shell: auto-detect lifecycle, single-pass renderer, drag-and-drop, slim FS browser, master-deck tracking, Space-key load per §6.4, `FFI_VERSION` 7). → [SHIPPED §M10.5](SHIPPED.md#m105). | 4–5 days |
| **M10.5c** | **Track Overview + horizontal-orientation shader** | ✅ shipped — per-deck thin vertical overview strip with playhead bracket + File-mode click-to-jump (§9.6.1); `orientation: u32` uniform plumbed end-to-end so M10.8 Prep mode reuses the same shader. → [SHIPPED §M10.5c](SHIPPED.md#m105c). | 2 days |
| **M10.5d** | **Background load (decode + peaks off-thread)** | ✅ shipped — `load_track` split into three phases so decode + peaks compute mutex-free; Swift `loadTrack` becomes `async`; concurrent-load guards + optimistic UI. No FFI bump. → [SHIPPED §M10.5d](SHIPPED.md#m105d). | 0.5 day |
| **M10.5e** | **Waveform polish — compression + past-region dim + brighter floor** | ✅ shipped — soft amplitude compression, past-region dim, brighter luminance floor, zero-crossing hairline overlay. No FFI changes. → [SHIPPED §M10.5e](SHIPPED.md#m105e). | 0.5 day |
| **M10.5f** | **Waveform 2× zoom-in** | ✅ shipped — one-line `chunksPerPixel` 4.0 → 2.0; click-scrub gesture stays calibrated via shared helper. → [SHIPPED §M10.5f](SHIPPED.md#m105f). | 0.1 day |
| **M10.5g** | **Anti-alias + temporal smoothing** | ✅ shipped — 4× MSAA enabled end-to-end; vertex stage convolves min/max/rms with a `[1, 2, 1] / 4` Gaussian across ±1 neighbour chunks (honest-state flags still read the raw centre chunk). Survives into M10.8. → [SHIPPED §M10.5g](SHIPPED.md#m105g). | 0.5 day |
| **M10.5h–p** | **Shader exploration ladder — shipped, then rolled back in M10.8** | The renderer's shader stack accumulated an HDR + bloom + ACES tonemap pipeline, an `OnsetDecimator`-driven brightness layer, a kick-emphasis tint, a time-domain `FilteredPeakChunk` ring, a `WaveformTuning` runtime knob panel, and a `djLandmarks` palette — each individually shipped. Side-by-side dogfooding against Serato exposed that the stack was not converging on a DJ-effective waveform. The entire post-processing ladder was deleted from the runtime in M10.8 in favour of a single-pass Serato-parity shader (see [§9.6.0 Waveform baseline freeze](#960-waveform-baseline-freeze-m108-cleanup)). Full design archaeology in [SHIPPED §M10.5h → §M10.5p](SHIPPED.md#m105hp). Rust-side `OnsetDecimator`, `BeatGrid`, and `FilteredDecimator` remain as dormant data primitives for future additive use. | — (retired) |
| **M10.5n** | **Playhead-vs-audio drift root-cause fix** | ✅ shipped (survives M10.8) — diagnosed integer-rounded chunk-index drift (0.49 %/chunk ≈ 1.17 s by end of a 4-min track on cross-SR tracks); fix uses `peakChunkDurationSecs: Double` directly so `playheadChunk = floor(elapsed_secs / peakChunkDurationSecs)`. Manual `avOffsetMs` slider removed. → [SHIPPED §M10.5n](SHIPPED.md#m105n). | 0.5 day |
| **M10.5j** | **On-disk waveform sidecar cache** | *Planned.* New `dub-cache` library + versioned on-disk format keyed by `sha-256(path‖size‖mtime)` so a re-load is ~1 ms instead of ~150 ms. → [SHIPPED §M10.5j](SHIPPED.md#m105hp) for the full plan-of-record. | 1.5–2 days |
| **M10.5k** | **Mip pyramid in `dub-peaks`** | *Planned.* Five mip levels in `OfflinePeaks` so zoom changes animate smoothly and `TrackOverviewView` reads mip-4 directly. Pairs with M10.5j sidecar. → [SHIPPED §M10.5k](SHIPPED.md#m105hp). | 1 day (after j) |
| **M10.5m(b)** | **9-band sub-bass split** | *Deferred to M11.* Bumps `dub-spectral::NUM_BANDS` 8 → 9 by splitting the lowest log-band into sub-bass (30–60 Hz) and kick-band (60–200 Hz); breaks the M8.1 BPM fixtures and the `BandPeakChunk` wire format. Parked until M11 lands DJ-curated content to validate against. → [SHIPPED §M10.5m(b)](SHIPPED.md#m105hp). | 1.5–2 days |
| **M10.5p-grid** | **Beat-grid v2 — own milestone** | *Deferred.* Tempo-drift tracking, downbeat detection, manual phase correction, library sidecar persistence, and a streaming Thru-mode variant. Re-scoped out of M10.5p when the Stage 1 overlay's static phase proved insufficient for tempo-drifting material. → [SHIPPED §M10.5p-grid](SHIPPED.md#m105hp). | 2–3 days (when scheduled) |
| **M10.6** | **Mouse transport + position navigation + Repeat** | ✅ shipped a–d; M10.6e Repeat outstanding. **M10.6a** Casual Play UI + zoomed click-scrub (Prep-only per §6.1). **M10.6b** Panic Play engine + FFI (`PanicPlayState`, `LiftPolicy::force_disengaged`, panic-aware `drive_timecode_inputs` branching, `FFI_VERSION` 8). **M10.6c** Panic Play UI + Timecode overview un-gate. **M10.6d** Transport-cluster redesign as Serato-style INT/ABS toggle; `cancel_panic_play` hands authority back to the timecode driver instead of pausing. **M10.6e** LFSR run-out auto-trigger (Repeat, §5.4.2) outstanding. → [SHIPPED §M10.6](SHIPPED.md#m106). | 3 days (a–d shipped, e remaining) |
| **M10.7** | **Phase-Drift Trail** | ✅ shipped — new `dub-match` crate (cross-correlation of both decks' `dub-bpm` ODFs off-RT, 30 Hz `MatchSample` ring); `apple/Dub/Performance/PhaseDriftView.swift` Metal-rendered centre-gutter strip with `Δ BPM` / `Δ ms` overlays; grid-agnostic per §9.4. `FFI_VERSION` 9. Single mode only — no numeric-only Preferences variant in v1. → [SHIPPED §M10.7](SHIPPED.md#m107). | 5 days |
| **M10.8** | **Track Preparation Mode shell + Serato-parity waveform baseline freeze** | ✅ shipped — Prep-mode auto-detection + alternate root view (`apple/Dub/Prep/PrepView.swift`) with horizontal full-width zoomed waveform + overview band stacked above; load + play / pause only (no editor / hot-cue prep / gain UI — those are v1.x per §3). Concurrently, the M10.5h–p shader ladder is **rolled back wholesale** in favour of a single-pass Serato-parity shader per [§9.6.0](#960-waveform-baseline-freeze-m108-cleanup); `WaveformTuning` / `WaveformTuningPanel` deleted, HDR / bloom / ACES post-processing deleted, palettes collapsed to `.serato`. Future waveform work is **additive and reversible** relative to this baseline. → [SHIPPED §M10.8](SHIPPED.md#m108). | 2 days (Prep shell) + 1–2 days (freeze) |
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
- Additive waveform layers parked in [§9.6.2](#962-future-additive-waveform-layers-parked) — phrase landmarks, ghost waveform of the other deck, vocal-presence overlay. None scheduled; each must be re-evaluated against the M10.8 baseline guardrail (§9.6.0) when picked up.

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
