# Dub — Shipped Milestones (M0 → M10.8)

> Companion to [`docs/PRD.md`](PRD.md). The PRD's milestone table keeps shipped rows
> short; this doc holds the detailed write-ups, design history, and rationale
> for each milestone that has landed. Forward-looking milestones (M11 onward)
> stay in the PRD.
>
> **Why split?** Shipped milestones accumulate prose that's load-bearing for
> "why is the code this way" archaeology but is no longer load-bearing for
> "what are we building next." Keeping them in the PRD bloated it past the
> point where a reader (or AI assistant) could keep the whole roadmap in
> working memory. Moved verbatim here; nothing has been rewritten or
> summarized away.

**Currently shipped:** M0 → M9 (engine, two-deck, timecode, Thru, BPM, peaks), M0.5 (Apple shell), M9.5 (`dub-spectral` + 8-band capture), M10/M10.1/M10.2 (FFI + Metal renderer + first multi-colour waveform), M10.3 (Performance shell + design tokens), M10.4 (vertical waveform + symmetric two-pane layout), M10.5 (file playback dev loop) including sub-milestones M10.5a–g (FFI + Apple shell + background load + initial polish + zoom + anti-alias), the M10.5h–p shader exploration ladder (HDR / bloom / onset / kick / DJ-landmark experiments — *all rolled back in the M10.8 baseline freeze*, see [§M10.8](#m108)), M10.5c (Track Overview + horizontal-orientation shader), M10.5n (playhead-vs-audio drift root-cause fix), M10.6a–d (Casual Play UI, Panic Play engine + FFI + UI + transport-cluster redesign), M10.7 (Phase-Drift Trail), and M10.8 (Track Preparation Mode shell + Serato-parity waveform baseline freeze). Workspace passes `cargo clippy --workspace --all-targets -- -D warnings` and the full `cargo test --workspace` suite. The Apple project builds end-to-end via `./scripts/bootstrap.sh && xcodebuild build -scheme Dub`.

## Table of contents

- [M0 — Scaffold + CI + test discipline](#m0)
- [M0.5 — Apple shell + smoke screen](#m05)
- [M1 — First Sound](#m1)
- [M2 — Transport (lock-free command channel)](#m2)
- [M2.1 — RT discipline + soak harness](#m21)
- [M3 — Format coverage + hot track loading](#m3)
- [M3.5 — De-click envelope + tail-fade + offline analyzer](#m35)
- [M4 — Two decks + debug mixer](#m4)
- [M5.1 — Timecode decoder, offline (clean-room)](#m51)
- [M5.2 — Audio input plumbing](#m52)
- [M5.3 — Live timecode → deck (first scratch)](#m53)
- [M5.4 — Calibration + scope (M5.4.1 + M5.4.2)](#m54)
- [M5.4.3 — Calibration speed (industry-parity)](#m543)
- [M5.4.4 — Per-deck calibration](#m544)
- [M5.4.5 — Late-binding decks + non-blocking calibration](#m545)
- [M5.4.6 — Always-fresh calibration (gut the fingerprint probe)](#m546)
- [M5.5.1 — Engine routing primitive](#m551)
- [M5.5.2 — External-mixer 4-channel output routing](#m552)
- [M5.6 — Two-deck timecode](#m56)
- [M6 — Timecode v2 (Traktor MK1 + MK2)](#m6)
- [M7 — Thru Mode (per-deck input routing)](#m7)
- [M7.5 — BPM engine + offline analysis](#m75)
- [M8 — Auto-BPM on Thru — streaming driver](#m8)
- [M8.1 — BPM octave fix (log-band ODF + windowed-energy picker)](#m81)
- [M9 — Live waveform capture (Thru)](#m9)
- [M9.5 — dub-spectral extraction + 8-band peak capture](#m95)
- [M10-A — `dub-ffi` `DubEngine` UniFFI surface](#m10a)
- [M10-B — Metal renderer + first live broadband waveform](#m10b)
- [M10.1 — Multi-colour fragment shader](#m101)
- [M10.2 — Polish: deck B, palette presets, honest silence/clipping](#m102)
- [M10.2 remainder — superseded by M10.5h–p, then rolled back in M10.8](#m102-remainder)
- [M10.3 — Performance shell](#m103)
- [M10.4 — Vertical waveform + symmetric two-pane layout](#m104)
- [M10.5 — File playback dev loop (M10.5a + M10.5b)](#m105)
- [M10.5c — Track Overview waveform + horizontal-orientation shader](#m105c)
- [M10.5d — Background load (decode + peaks off-thread)](#m105d)
- [M10.5e — Waveform polish (compression + past-region dim + brighter floor)](#m105e)
- [M10.5f — Waveform 2× zoom-in](#m105f)
- [M10.5g — Waveform anti-alias + temporal smoothing](#m105g)
- [M10.5h → M10.5p — Shader exploration ladder (rolled back in M10.8)](#m105hp)
- [M10.5n — Playhead-vs-audio drift root-cause fix (survives M10.8)](#m105n)
- [M10.6a–d — Mouse transport, Panic Play, transport-cluster redesign](#m106)
- [M10.7 — Phase-Drift Trail](#m107)
- [M10.8 — Track Preparation Mode shell + Serato-parity waveform baseline freeze](#m108)

---

<a id="m0"></a>
## M0 — Scaffold + CI + test discipline

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 2–3 days

`cargo nextest` passes, `clippy -D warnings` green, RT-audit harness runs on a no-op render, xcframework builds, blank SwiftUI app launches and prints "engine OK" from Rust. GitHub Actions CI configured per PRD §10.4. Branch protection on `main` enabled. First TDD-discipline test exists and runs.

---

<a id="m1"></a>
## M1 — First Sound

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 4–6 days

Single deck, internal mode, plays a WAV through CoreAudio at < 8 ms latency. Property tests for buffer math; golden tests for resampler output; RT-audit green during playback.

---

<a id="m2"></a>
## M2 — Transport (lock-free command channel)

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 3–4 days

Main thread can `play` / `pause` / `seek` / `set_rate` / `set_gain` deck 0 while CoreAudio is playing, via a `ringbuf` SPSC queue drained at the start of every render block. UI reads deck position / playing / at-end via per-deck atomic snapshot (`AtomicU64` of `f64` bits, Relaxed). RT-audit: 100k blocks alloc-free **including** drain of pre-staged commands. CLI demo: `dub play <file> --realtime --pause-at 1.0 --resume-at 2.0 --seek-at 3.0=4.0` produces an audibly correct pause/resume/seek with snapshot-correct end state.

---

<a id="m21"></a>
## M2.1 — RT discipline + soak harness

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 3–5 days

rt-audit green under stress; 1-hour playback with no xruns at 64-sample buffer; soak test harness in CI runs nightly; first parser fuzz target wired up (ID3 reader). Folded as a milestone-internal gate before M3, not a user-visible milestone.

---

<a id="m3"></a>
## M3 — Format coverage + hot track loading

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 4–6 days

Loads MP3 / FLAC / AIFF / M4A in addition to WAV (everything decoded fully into RAM per PRD §4.4 — no streaming). `Command::DeckLoad(Arc<Track>)` allows changing decks live; old `Arc<Track>` is returned to the main thread via a trash channel and freed off the audio thread. CLI demo: `dub play <A> --hot-swap-at WALL=<B>` audibly swaps A→B mid-playback. Sample-accurate seek across all formats (already works since everything is in memory).

---

<a id="m35"></a>
## M3.5 — De-click envelope + tail-fade + offline analyzer

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 1–2 days

Two complementary primitives sharing one precomputed `sin²` envelope (2 ms × engine SR):

1. **Transport-change declick** — an equal-power crossfade between pre- and post-mutation state on every track load, seek, and play/pause.
2. **Tail-fade** — a multiplicative envelope applied as the playhead approaches a track's natural end so walking off the last sample doesn't step to 0.

Both are gated by a `track_len ≥ 2 × envelope_length` threshold so synthetic short tracks aren't obliterated. Back-to-back transport changes routed via a single-slot `pending_disposal` + `AtomicU64` overflow counter; old `Arc<Track>`s never drop on the audio thread.

New `dub analyze <wav>` subcommand reports peak/RMS/DC, clipping, and max per-sample first-difference, flagging any `|s[i] − s[i-1]|` above a configurable threshold (default 0.05) — replaces subjective listening with mathematical click detection. Offline `dub play -o` now supports the same scheduled transport events as realtime, so any scenario can be rendered deterministically and audited end-to-end.

---

<a id="m4"></a>
## M4 — Two decks + debug mixer

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 2–3 days

Both engine decks (`DECK_COUNT = 2`) drivable end-to-end through the CLI: `dub play <A> <B>` loads independent tracks onto deck A and deck B, both summed by the engine's existing additive deck loop into one stereo bus. Debug internal mixer adds a single `master_gain` field on `Engine` (M4 addition) plus the existing per-deck `set_gain`, applied multiplicatively after deck summing — pass-through when `master_gain == 1.0` to avoid the per-block multiply on the common case.

New `Command::SetMasterGain` and `EngineHandle::set_master_gain` so the master is mutable mid-playback through the same lock-free SPSC channel as transport. CLI gains `--deck-b-*` mirrors of every transport flag (`--deck-b-rate`, `--deck-b-gain`, `--deck-b-pause-at`, `--deck-b-resume-at`, `--deck-b-seek-at`, `--deck-b-hot-swap-at`) plus `--master-gain G` and `--master-gain-at WALL=G`; bare flags target deck A for backward-compat with single-deck usage. `ScheduledEvent` carries a per-event `deck` index so each scheduled event addresses the right deck; engine-wide events (master gain) carry no deck.

**External-mixer 4-channel routing is intentionally deferred** to M5/M6 where it's needed by the timecode hardware (SL3, Audio 6) — v1's debug mixer sums to one stereo output for now. CLI demo: `dub play <A> <B> --master-gain-at 1.0=0.6 --hot-swap-at 1.5=<C> --deck-b-pause-at 2.0 --deck-b-resume-at 3.0 -o out.wav && dub analyze out.wav` reports CLEAN with `max delta ≤ 0.026` (well under the 0.05 click threshold). Realtime path verified audibly. RT-audit extended to alternate command/load traffic across both decks plus periodic master-gain churn — 100k blocks alloc-free under `assert_no_alloc`.

---

<a id="m51"></a>
## M5.1 — Timecode decoder, offline (clean-room)

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 3–5 days

New `dub-timecode` crate decoding Serato CV02 from stereo audio in **relative mode only**. Algorithm: treat `s = L + jR` as a complex analytic signal; compute the coherent block sum of `s_n · conj(s_{n-1})`; per-block instantaneous frequency = `arg(sum) / (2π·Δt)`; rate = `f_inst / carrier_hz` (signed — negative = reverse); position integrates rate × block-seconds. Confidence = `|sum| / Σ|s|²` (1.0 = pure carrier, 0.0 = noise).

RT-safe (alloc-free under `assert_no_alloc`). Fully unit-testable on synthetic stereo quadrature signals — no hardware required. Bitstream/absolute decode deferred to M6. **Clean-room implementation** from xwax/Mixxx algorithm description; no xwax code copied.

CLI: `dub decode-timecode <wav>` reads recorded timecode and reports rate / position / amplitude / confidence per window with a LOCKED/PARTIAL/POOR verdict; `--synthetic` runs a built-in 1.0× → 0.5× → -1.0× → silence scenario for sanity-checking without a turntable.

---

<a id="m52"></a>
## M5.2 — Audio input plumbing

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 2–3 days

`dub-audio` gets an `AudioInput` primitive mirroring `AudioOutput`: HAL input AudioUnit, ringbuf-buffered handoff to a consumer thread. CLI: `dub capture` (writes input to WAV) and `dub levels` (live meter). Verified on default mic input first, SL3 input pair second.

See [`docs/ARCHITECTURE.md` → HAL input invariant](ARCHITECTURE.md#hal-input-invariant--sample-rate-match-m52) for the load-bearing sample-rate-match footgun this milestone closed.

---

<a id="m53"></a>
## M5.3 — Live timecode → deck (first scratch)

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 2–3 days

Wire `AudioInput` → `dub-timecode::Decoder` → engine deck in **relative mode**: per-block decoded rate is applied to the deck via `set_rate`; lift detection runs three layers — (a) **amplitude gate** (`DEFAULT_AMPLITUDE_THRESHOLD = 0.01` RMS) overrides confidence whenever the carrier is dead, since handling/rumble noise on a lifted cartridge can produce moderate confidence at near-zero RMS, (b) **two-edge confidence hysteresis** (engage `0.8`, disengage `0.5`) for clean scratch-transient handling, (c) **sticky-block window** (4 blocks ≈ 21 ms @ 256-frame / 48 kHz) for dust-tick immunity.

Three iterations on the SL3 drove the design: the first single-threshold gate chattered on lift; the second confidence-only hysteresis treated lift as a "lukewarm scratch transient" and burst-played the track while the needle was up; the amplitude gate closes that hole. The state machine is factored into a pure `step_policy(DecodeOutput) → Intent` on top of `drive(...)` (which sources data from the ringbuf), so each pathology has a dedicated regression test. The decoder consumes the input ringbuf directly on the audio thread inside `Engine::render` — no extra thread, no extra channel — so the only added latency on top of M5.2's input ring is one `Decoder::process` call per render block (~µs).

New public engine surface: `Engine::attach_timecode_input(deck_idx, HeapCons<f32>, TimecodeInputConfig)`, `Engine::detach_timecode_input`, `Engine::timecode_last_output(deck_idx)` for UI observability. New `dub_audio::AudioInput::take_consumer()` lets the consumer end of the IOProc → consumer ringbuf move into the engine while the `AudioInput` itself stays on the main thread for shutdown.

**`AudioOutput` now also force-aligns the output device's nominal SR to engine SR** (same gauntlet as `AudioInput`) — first SL3 validation surfaced an 8 % pitch drift when output was at 44.1 kHz and engine at 48 kHz because the CoreAudio HAL DefaultOutput unit does not reliably SRC across that boundary. Position drift correction (re-syncing deck position to decoded position over wall time) is intentionally deferred — relative-mode in v1 lets position evolve via integration of rate, which is what platter motion already encodes.

**rt-audit extended** with a 10k-block timecode-driven render path under `assert_no_alloc`, verifying the entire Decoder + transport-update path is heap-free on the audio thread. CLI: `dub timecode-deck <track.wav> --input-channels N,M [--device NAME] [--duration SECS] [--confidence T] [--disengage-threshold T] [--sticky-blocks N] [--amplitude-threshold T]`.

**Demo criterion:** scratch a record on Deck A, hear Deck A's loaded track react with sub-buffer-size latency, see deck mute cleanly on stylus lift with no track audio leakage, see direction reversal on backspin. *This is the milestone where Dub becomes a DJ app.*

---

<a id="m54"></a>
## M5.4 — Calibration + scope (M5.4.1 + M5.4.2)

**Status:** shipped &nbsp;·&nbsp; **Estimate:** scope 1 day; calibration 2 days

Split into two delivered sub-milestones because the scope is independently valuable and lands a refactor that calibration also needs.

### M5.4.1 — TUI scope (`dub scope`)

Opens the input device, runs the same `LiftPolicy` as `dub timecode-deck`, and renders a ratatui inspector: Lissajous of input `(L, R)` (the carrier should trace a clean circle; lift collapses it to a noisy blob), `[LOCKED]` / `[LIFT]` engagement badge, gauges for confidence and amplitude (color-coded against current thresholds), rate readout with a centered slider in `[-2×, +2×]`, sticky countdown bar showing the policy's `consecutive_below` counter walking toward disengage, and a row of live thresholds. Arrow keys mutate engage / disengage / amplitude *in place* so users can find sane defaults for their cartridge against their actual signal — calibration sandbox that M5.4.2 persists. Block size pinned to 256 frames so the scope's policy decisions match `timecode-deck` 1:1.

**Refactor:** `step_policy` and the engagement state were factored out of `TimecodeInput` into a public `LiftPolicy { engage, disengage, sticky, amplitude, engaged, consecutive_below, last_locked_rate }` with a `step(DecodeOutput) -> LiftIntent` method; `TimecodeInput` now embeds it and delegates. Three callers — engine playback, `dub scope`, and `dub calibrate` (M5.4.2) — share exactly the same lift behavior because they share the code path.

New CLI: `dub scope [--device NAME] [--input-channels N,M] [--engage T] [--disengage T] [--sticky N] [--amplitude T] [--format serato-cv02] [--duration SECS]`. New deps in `dub-cli` only: `ratatui` 0.30, `crossterm` 0.29 (engine and audio crates untouched).

### M5.4.2 — Calibration UX (`dub calibrate`)

Measures the user's specific *rig* (cartridge + preamp + cabling + soundcard) and persists derived thresholds + a rig fingerprint to `~/.dub/calibration/<device_key>_<format>.json`. Per-rig — not just per-soundcard — because a cartridge swap on the same SL3 changes the carrier amplitude by 50 %+ and would silently misfire a soundcard-only calibration.

Two zero-prompt phases: (1) *carrier* — auto-detects stable carrier (5 consecutive blocks: confidence ≥ 0.85, |rate − 1| < 0.10), captures 10 s; (2) *lift* — auto-detects lift (10 consecutive blocks: amp < 0.005), captures 5 s. From the percentile shapes (P5/P50/P95 of amplitude + confidence per phase) it derives `engage = carrier.conf_p5 - 0.03` (clamped 0.7–1.0), `amplitude = carrier.amp_p5 / 2`, keeps `disengage = 0.50` and `sticky = 4` from M5.3 defaults.

Stores a `RigFingerprint { carrier_amp_p50, carrier_amp_p95, carrier_conf_p50 }` — carrier-only on purpose; lift noise rises 10–100× in clubs vs. lab and would false-flag every venue change as "rig changed". `dub timecode-deck` startup loads the JSON, briefly probes the carrier (3 s) to validate the fingerprint at 30 % tolerance, and either uses the saved thresholds (match) or auto-recalibrates (mismatch — cartridge or preamp swap). `--recalibrate` forces fresh measurement; `--no-probe` skips fingerprint check; `--no-calibrate` falls back to M5.3 defaults. Per-knob CLI flags (`--confidence`, `--amplitude-threshold`, …) still override individual thresholds for partial overrides. SNR sanity check refuses to ship thresholds when carrier-to-lift SNR is below 10× (likely cartridge / cabling problem). Schema-versioned JSON includes the full P5/P50/P95 measurements so future formula changes (M5.4.3, M6) can re-derive thresholds without forcing a remeasurement.

> **Superseded:** M5.4.6 later gutted the load-from-disk + fingerprint-probe machinery. The JSON file is now a diagnostic artifact only; the runtime always recalibrates on startup. See [M5.4.6](#m546).

---

<a id="m543"></a>
## M5.4.3 — Calibration speed (industry-parity)

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 1 day (came in slightly ahead of the 1–2 day estimate; item 4 partial deferral kept scope tight)

M5.4.2 shipped *correct* per-rig calibration but at industry-trailing speed: live SL3 + Concorde validation showed ~25 s total wall time for the initial calibrate (10 s carrier capture + 5 s lift capture + two ≥ 5 s detection waits at `STABLE_BLOCKS = 5`), and the 3 s startup probe felt longer than 3 s in practice because of the same wait preamble.

**Goal:** match Traktor's "drop the needle, hit calibrate, you're done" feel. Achieved on shipping live SL3 + Concorde + Traktor MK1 vinyl: ≈ 3.5 s first-time calibration (was ~25 s), ≈ 1.7 s startup probe on a known rig (was ~5 s, claimed 3 s).

**What shipped:**

1. **Single-phase calibration** — eliminated the lift step entirely. Lift noise was always deliberately *not* on the fingerprint; M5.4.1 SL3 hand-tuning showed the carrier shape carries the threshold information (`amplitude = carrier_p5 * 0.5` matches the user's hand-found threshold within 1 % regardless of lift level), and lift was always the SNR safety net, never the signal source. New `MeasureOptions { two_phase: bool, .. }` opts struct routes through `measure_inline`; default is `two_phase = false`. Lift stats are persisted as `MeasurementStats::zero()` (`n_blocks == 0`) for schema compatibility — `derive_thresholds` recognizes the zero sentinel and skips the SNR check, so the JSON loads identically with single-phase or two-phase data downstream.
2. **Shorter carrier capture** — `DEFAULT_CARRIER_SECS` 10.0 → 3.0 s (≈ 564 blocks @ 256 frames @ 48 kHz). M5.4.1 + M5.4.4 captures show percentile convergence within < 1 % by ~ 2 s on a steady spin; 3 s leaves a small safety margin without user-visible cost.
3. **Faster detection threshold** — `STABLE_BLOCKS` 5 → 2 (≈ 11 ms detection wait at 256/48 kHz block size), with `CARRIER_DETECT_CONF` simultaneously tightened from 0.85 → 0.90 so 2 blocks is unambiguous. The user's deck-B SL3 carrier_conf_p5 ≈ 0.96 still passes comfortably; the rate gate (`|rate-1| < 0.10`, unchanged so ±10 % pitch fader keeps working) catches transient stylus motion because handling produces near-zero or wildly varying rate, never the unity rate of a steady spin.
4. **Startup probe accelerator (partial)** — `PROBE_SECS` 3.0 → 1.5 s; combined with the new `STABLE_BLOCKS = 2`, the effective probe-side wall time on a known rig drops to ≈ 1.7 s. The "run probe *concurrently* with timecode-deck spinup" half of the original M5.4.3 sketch was deferred to M5.4.5 because it requires the same architectural lift (mid-stream `attach_timecode_input`, parallel calibrators) that M5.4.5 builds for the takeover scenario; bundling it here would have either pre-built or duplicated that infrastructure.
5. **`--two-phase` opt-out** — `dub calibrate --two-phase` keeps the legacy M5.4.2 flow available for diagnostics (cartridge / preamp / cabling troubleshooting where the SNR safety net actually matters). Auto-calibration in `dub timecode-deck` always uses single-phase — the user explicitly opts into two-phase via the bare `dub calibrate --two-phase` invocation.

**Test surface (5 new, 273 workspace total, was 265 after M5.4.4 + 3 from M5.4.3 prep):** `derive_thresholds_skips_snr_when_lift_not_measured`, `derive_thresholds_still_rejects_low_snr_in_two_phase_mode`, `measurement_stats_zero_signals_unmeasured`, `parse_opts_default_is_single_phase`, `parse_opts_two_phase_flag_round_trips`, `parse_opts_default_carrier_secs_is_3`, `carrier_detect_constants_match_m543_targets` (pins all four tuned constants), `m543_probe_and_auto_calibration_constants_are_fast` (pins `PROBE_SECS = 1.5`, `AUTO_CARRIER_SECS = 3.0`). The constant-pinning tests are deliberate: any silent revert (e.g. someone bumps `STABLE_BLOCKS` back to 5 to "be safe") brings back the user-visible 25 s pain point, and we want a build-time failure rather than a quiet drift.

**Out of scope (deferred to M5.4.5):** concurrent probe-and-spinup.
**Out of scope (deferred indefinitely):** SNR-derived runtime ghost-noise warnings — single-phase loses the SNR floor at calibrate time, but the M5.4.5 + M10 runtime audio path can warn if observed lift amplitude exceeds the calibration's `amplitude` threshold for an extended window.

---

<a id="m544"></a>
## M5.4.4 — Per-deck calibration

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 1–2 days

M5.6 shipped two-deck timecode but probed pair 0 (deck A) only and silently reused those thresholds for deck B — correct for matched cartridges (the common case in vinyl-DJ rigs), silently wrong otherwise (ghost-noise on lift, premature disengage on light scratches, no error message). M5.4.4 makes calibration **per deck**: probe and store independently for each deck, fingerprint per deck, auto-recalibrate per deck on rig swap.

Calibration JSON keys by `(device, deck_index, format)` (path pattern `~/.dub/calibration/<device>_deck_<idx>_<format>.json`) instead of the M5.4.2 `(device, format)` pattern. The fingerprint-probe machinery runs once per deck sequentially in two-deck mode — deck A's full carrier+lift first, then deck B's — with per-deck status banners (`calibration A:`, `calibration B:`) so the user knows which side they're spinning/lifting at any moment.

**Backward compat:** deck 0 falls back to the legacy single-deck JSON (`<device>_<format>.json`, no deck infix) when the new per-deck file is missing, so existing user calibrations from M5.4.2 / M5.4.3 / M6 keep working without a migration step; the loader writes the calibration forward to the new path on the next save. Deck 1 has no legacy file (pre-M5.4.4 only stored deck A) so it always runs a fresh calibration on first M5.4.4 use, which is the correct behavior.

**API surface:** `Calibration::path_for(device, deck_index, format, dir)` (was 3-arg), `Calibration::path_for_legacy(device, format, dir)` (legacy fallback only), `Calibration::load_with_legacy_fallback(new, legacy)` (transparent migration), and `measure_inline(input, pair_idx, deck_index, format, ...)` / `probe_carrier(input, pair_idx, format, ...)` taking a pair index so they read from the right `read_into_pair(idx)` source in two-deck mode. The `pair_idx` (where to read on the AudioInput, M5.6 demuxing) is intentionally separate from `deck_index` (on-disk metadata) — `dub calibrate --input-channels 5,6 --deck 1` opens its own 2-channel input (so pair_idx=0 always there) but stamps the result as deck 1; `dub timecode-deck` two-deck mode uses pair_idx==deck_idx. Conflating them caused a bug during implementation that was fixed before merge.

**CLI:** `dub calibrate` gains `--deck 0|1` (default 0, rejects ≥ 2 because the engine has 2 decks today). `dub timecode-deck` runs `resolve_thresholds` once per deck in two-deck mode and prints two `timecode A:` / `timecode B:` lines instead of the M5.6-era shared-calibration single line. Test surface: 13 new dub-cli tests covering per-deck path keying, legacy-fallback load behavior (legacy used when new missing, new wins when both present, both-missing errors), `--deck` flag parse + validation (rejects 2/99/letters), deck-label mapping (0→A, 1→B), `deck_index` JSON round-trip, legacy JSON without `deck_index` field defaulting to deck 0 (`#[serde(default)]`). 265 workspace tests total (was 252 after M6).

**Explicitly out of scope** (deferred to v1.x or never): named cartridge profiles, profile libraries, cross-session "auto-load the right cartridge" UX. The earlier M5.4.4 design ("library of named profiles, fingerprint-match across them") was over-scoped — once M5.4.3 makes calibration ≤ 5 s, "always recalibrate on startup" is the simpler model with zero profile-management UX surface, and matches the user's mental model ("calibrate auto-runs on app start; if I swap a cartridge mid-set I press the calibrate button"). The "calibrate button" is part of M10 (UI); on the CLI today, `dub calibrate --deck 0` / `--deck 1` already serves that role.

**Known product gap deferred to M5.4.5** (not a polish item — the canonical DJ-takeover use case is structurally incompatible with M5.4.4's "calibrate both then start" model): when the incoming DVS DJ drops onto deck A while the previous DJ is still playing on deck B, deck B's record literally does not exist for calibration to run against. M5.4.5 makes each deck progress through `Unconfigured → Calibrating → Ready` independently — single-deck startup, mid-stream deck-add, audio plays during deck B's later calibration. Acceptable for the CLI dev tool today (no live use); blocking for any actual product release.

---

<a id="m545"></a>
## M5.4.5 — Late-binding decks + non-blocking calibration

**Status:** shipped &nbsp;·&nbsp; **Estimate:** product correctness, not polish

**Why this was a product gate, not a polish item:** the canonical DJ use case — DJ takeover — has the previous DJ still playing on deck B when the incoming DVS DJ drops onto deck A. The incoming DJ has *zero access* to deck B's record for the entire takeover window (which can be 5 minutes or 60 minutes). M5.4.4's "calibrate both decks then start audio" model was structurally incompatible with this: deck B's record does not exist *to be calibrated against* until the previous DJ leaves. A faster M5.4.3 calibrate doesn't help either — the issue isn't *how long* deck B's calibration takes, it's that deck B *has no record on it* at the moment audio must start.

**What shipped** (smaller than the original plan; it covers the takeover gate but defers a couple of nice-to-haves to follow-up):

1. **Engine surface — `EngineHandle::attach_timecode_input` (NEW).** The pre-M5.4.5 `Engine::attach_timecode_input(&mut self, …)` was synchronous and required `&mut Engine`, which means it could only be called *before* `AudioOutput::start_with_options` consumes the engine. M5.4.5 adds a parallel command-channel path: `EngineHandle::attach_timecode_input(idx, rx, cfg)` constructs `TimecodeInput::new` on the main thread (allocates), boxes it, and pushes a new `Command::AttachTimecodeInput { idx, input: Box<TimecodeInput> }` through the existing SPSC channel. The audio thread slots it into `engine.timecode_inputs[idx]`. If the slot was already filled (mid-stream re-cal), the displaced `Box<TimecodeInput>` is bounced back through a *second* trash channel (mirroring the `Arc<Track>` trash pattern from M3.1) for main-thread disposal — never dropped on the audio thread. New `EngineHandle::reclaim` drains both trash channels in one call. New `EngineHandle::timecode_trash_overflow_count` surfaces leak diagnostics. Pinned by 5 new engine tests (75 total, was 70).

2. **Calibrator API refactor.** `measure_inline` and the helpers `wait_for_stable_carrier` / `wait_for_lift` / `capture_phase` now take `&mut HeapCons<f32>` instead of `&mut AudioInput + pair_idx`. The exclusive borrow on `AudioInput` was what forced sequential calibration; now each calibrator owns its own consumer ring and two of them can run on two threads with no shared mutable state. The old `(device_name, sample_rate, deck_index, format)` metadata that used to be pulled off `AudioInput` is bundled into a new `MeasurementInputs` struct that the caller fills once and hands by reference (or moves) to each calibrator. New `MeasureOptions::detect_timeout_secs: Option<f64>` (was `f64`); `dub timecode-deck` startup passes `None` so the deck-B calibrator can wait indefinitely for the takeover window, while `dub calibrate` keeps the legacy 30 s timeout for the "user forgot the needle" safety net.

3. **`dub timecode-deck` flow.** Reordered: take both consumers out of `AudioInput`, build `Engine::new_with_handle`, load tracks (decks default to paused), `AudioOutput::start_with_options(engine, …)` immediately — both decks render silence into the output bus. Then spawn one `std::thread::spawn` worker per declared deck. Each worker owns its `HeapCons<f32>` + a `MeasurementInputs` bundle + an optional save path; on completion it sends `(deck_idx, Result<(HeapCons<f32>, Calibration)>)` back to main via an `mpsc` channel. Main interleaves stats-print (500 ms tick) with `rx.try_recv` polling at the same tick — as each calibrator finishes, main applies CLI overrides, builds a `TimecodeInputConfig`, and calls `handle.attach_timecode_input(idx, consumer, cfg)`. That deck goes live mid-stream; the other deck's calibrator keeps waiting independently.

   **Why detached `thread::spawn` and not `thread::scope`:** scope's auto-join would block forever at scope-exit if a calibrator is still waiting for a never-appearing carrier (process Ctrl-C window). Detached threads are cleaned up by the OS at process termination — acceptable for a CLI tool with `--duration` + Ctrl-C as the exit paths.

4. **Drain step deletion.** M5.4.4's `drain_input_pair` between sequential calibration and engine attach is gone — parallel calibrators consume their rings continuously, so there's no idle-pair stale-audio buildup to flush. The IOProc still pushes ~10 ms during the worker→main→engine handover; the existing 4 s ringbuffer absorbs that without effect.

5. **Output-now-decks-later semantics.** Decks loaded but paused before output start; `AudioOutput::start_with_options` brings up the device immediately, both decks render silence into routed output channels until their `TimecodeInput` is attached. The user sees a working audio chain (output device alive, no clicks) while calibrators work. The lift policy starts in "lifted" state on attach so the deck stays muted until the user drops the needle and the carrier locks.

**Takeover use case (the actual product gate, validated):** incoming DJ launches `dub timecode-deck a.wav b.wav --input-channels 3,4 --deck-b-input-channels 5,6 --format serato-cv02`. Both calibrator threads start. AudioOutput is up immediately. Deck A's calibrator detects the carrier the moment the DJ drops a needle on A; that deck attaches mid-stream and audio plays. Deck B's calibrator is sitting in `wait_for_stable_carrier` with `detect_timeout_secs = None` — could be 60 minutes. When the previous DJ finally vacates and the incoming DJ drops a record on B, deck B's carrier appears, the calibrator wakes up, completes, attaches. Deck A audio is uninterrupted across the entire window. **No hot-keys needed for this** — passive-wait on the calibrator side absorbs the takeover window naturally.

**Deferred to follow-up M5.4.5+:**

- **(a)** Hot-key `B` for mid-stream re-attach when `--deck-b-input-channels` *wasn't* declared at startup (e.g., DJ launches single-deck and later decides to add deck B). Engine surface is ready (replace-and-trash on `AttachTimecodeInput` works, `Box<TimecodeInput>` trash channel exists), but the CLI plumbing for crossterm hot-keys + dynamic `AudioInput` reconfiguration is its own piece of work.
- **(b)** Mid-set re-calibration via hot-key (cartridge swap during a set). Same engine surface; same deferral reason.
- **(c)** `--sequential-calibrate` debug flag. The original PRD entry called for this as a M5.4.4 fallback; not implemented because the parallel path is the only path now and there's no observed regression to fall back from. Add if needed.
- **(d) Follow-up landed alongside M5.4.5 live validation: `--duration` is now optional, default = run until Ctrl-C.** The old 60 s default (`DEFAULT_RUN_SECS`) was a holdover from M5.3's "validation run" mindset, where calibration was a few seconds and a 60 s wall-clock test was a natural shape. With M5.4.5's takeover scenario explicitly in scope — deck B's calibrator may legitimately wait 5–60 minutes for the previous DJ to vacate — a hard wall-clock exit would silently *drop the deck B calibration window*: the calibrator is still sitting in `wait_for_stable_carrier`, then the process exits, then a record drops on B and nothing happens. `Opts::duration_secs` is now `Option<f64>`; main-loop becomes `while opts.duration_secs.is_none_or(|d| start.elapsed() < d)`. `--duration N` is preserved for scripted / CI smoke tests (this is what unit tests of `parse_opts` pin). Startup banner adapts: `Some` → "running for N s — drop the needle and play", `None` → "running until Ctrl-C — drop the needle and play".

**Acceptance (validated live):**

1. Single-deck `dub timecode-deck a.wav --input-channels 3,4` works end-to-end as before (no deck B), audio plays on A after calibrator finishes.
2. Two-deck startup with both decks spinning shows parallel calibration banners and audio starts on whichever deck calibrates first.
3. Takeover scenario — only deck A spinning at startup — audio plays on A while deck B's calibrator banner shows it still waiting; minutes later when deck B is spun, audio appears on B without any audible disturbance to A.

---

<a id="m546"></a>
## M5.4.6 — Always-fresh calibration (gut the fingerprint probe)

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 0.5 day

Realised on the back of M5.4.3's speedup that the entire save+probe machinery (M5.4.2 → M5.4.5) was solving a problem the production audience (touring DJs) doesn't have. The save+probe model assumes the user calibrates once and reuses across sessions; the probe pays for itself when the rig is unchanged because reusing thresholds beats remeasuring. **For touring DJs every venue brings a different turntable + cartridge: the fingerprint *always* mismatches, the probe *always* burns ~1.7 s confirming what we already know, and the auto-recalibration runs anyway.**

Net cost on the production path was probe (~1.7 s) + recalibrate (~3.5 s) = ~5.2 s — *worse* than a no-cache always-fresh model that pays only the ~3.5 s calibrate. Net cost for the bedroom DJ (one fixed rig) is ~1.7 s either way (probe+match vs. fresh calibrate). **Decision: gut the probe, always recalibrate on startup.**

**Runtime change:** `dub timecode-deck`'s `resolve_thresholds` collapses to "if `--no-calibrate` → M5.3 defaults; else run a fresh single-phase M5.4.3 calibration; save the result as a diagnostic artifact". No load-from-disk, no fingerprint comparison, no auto-recalibrate-on-mismatch path, no legacy-format migration, no stale-age warning.

**What stays:** `Calibration` JSON schema is unchanged for forward+backward compat with M5.4.2 … M5.4.5 files (existing JSONs still parse fine, future analysis tooling still has the percentile data). `RigFingerprint` stays as a struct field — written at calibration time as a record of "what did this rig look like" — but the comparison code (`matches`, `max_relative_delta`, `within_relative`, `relative_delta`, `DEFAULT_FINGERPRINT_TOLERANCE`) is gone. `Calibration::path_for(device, deck_idx, format, dir)` stays for the diagnostic save. `Calibration::load` stays `pub` (with `#[allow(dead_code)]` on the binary path) for tests + future `dub inspect-calibration` tooling. `dub calibrate` is unchanged — still writes a per-deck JSON for inspection.

**What goes away:** `RigFingerprint::matches / max_relative_delta / within_relative / relative_delta`, `DEFAULT_FINGERPRINT_TOLERANCE`, `Calibration::path_for_legacy`, `Calibration::load_with_legacy_fallback`, `legacy_device_key`, `probe_carrier`, `legacy_calibration_path_for`, `calibration_age_days`, `STALE_CALIBRATION_DAYS`, `PROBE_SECS`, `PROBE_DETECT_TIMEOUT_SECS`, the `time::OffsetDateTime` / `Rfc3339` imports they fed, plus the CLI flags `--recalibrate` and `--no-probe`. `--no-calibrate` survives because "fall back to M5.3 defaults" remains a useful no-hardware testing path.

**CLI breakage:** `--recalibrate` and `--no-probe` are now rejected as unknown flags (caught by `parse_opts`'s leftover-flag check) so anyone with a copy-pasted old invocation gets a clear error instead of a silently-ignored flag. Pinned by `parse_opts_rejects_dropped_recalibrate_flag` and `parse_opts_rejects_dropped_no_probe_flag`.

**Test surface delta:** −14 (deleted: 6 fingerprint matching, 1 legacy_device_key, 1 path_for_legacy, 3 load_with_legacy_fallback, 3 calibration_age_days, 1 legacy_calibration_path_omits_deck_infix, 1 parse_opts_recalibrate_flag, 1 parse_opts_recalibrate_and_no_calibrate_conflict) + 3 (added: parse_opts_no_calibrate_flag, parse_opts_rejects_dropped_recalibrate_flag, parse_opts_rejects_dropped_no_probe_flag). Workspace total 259 (was 273).

**What this does NOT change:**

- **(a)** M5.4.5's late-binding-decks design — that's about *availability* (deck B's record doesn't exist yet during a takeover), orthogonal to the save model. M5.4.5 still needs to land.
- **(b)** Per-knob CLI overrides (`--confidence`, `--amplitude-threshold`, …) — they apply on top of the fresh measurement just like before.
- **(c)** `dub scope` — already runs in-place threshold tuning, never touched the JSON.
- **(d)** Per-deck calibration (M5.4.4) — `dub calibrate --deck N` still writes `<device>_deck_N_<format>.json`, just nothing reads it back automatically.

**Why this isn't a regression on bedroom-DJ UX:** repeated bedroom sessions on the same rig now pay 3.5 s × 1 calibrator (per-deck) instead of 1.7 s probe + (occasionally) 3.5 s recalibrate. On the cold-start path the bedroom user gives up ~1.8 s and gains a much simpler mental model ("the app calibrates on every start, period") and never gets surprised by a stale calibration silently used.

**Acceptance:** `dub timecode-deck a.wav --input-channels 3,4` runs a fresh calibration (≤ 5 s wall time per deck) on every invocation and writes the JSON; no probe phase appears in the output. `dub calibrate --deck 0` writes the same JSON shape, manually. `--recalibrate` / `--no-probe` rejected with "unknown flag".

---

<a id="m551"></a>
## M5.5.1 — Engine routing primitive

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 0.5 day

New `Engine::render_routed(rt, out, num_channels, &[Option<u32>; DECK_COUNT])` writes each deck's stereo pair into a configurable channel pair of an N-channel interleaved output buffer. `Deck::render_into(rt, out, sr, stride, offset)` is the strided variant of `render`; `render` becomes a thin wrapper at `(stride=2, offset=0)`. Two decks routing to the same `Some(c)` sum (= M4 internal mixer); non-overlapping `Some` values isolate (= M5.5 external mixer).

`Engine::render` becomes `render_routed(out, 2, INTERNAL_MIXER_ROUTING)` so all M0–M5 callers stay byte-identical (verified by an explicit `render_routed_internal_mixer_matches_render` regression test). `routing[i] == None` skips a deck entirely — its transport state does NOT advance — pinned by tests; muting goes through `Deck::set_gain(0.0)` instead, which keeps the transport ticking. Master gain applies once across the whole multi-channel buffer at the end (zero × g == zero so unrouted channels stay zero). RT-safe (alloc-free, verified under `assert_no_alloc`). Pure-engine work, no hardware required.

---

<a id="m552"></a>
## M5.5.2 — External-mixer 4-channel output routing

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 2–3 days

M5.5.1's primitive plumbed all the way to CoreAudio. New `dub_audio::OutputOptions { channels, buffer_frames, sample_rate, channel_map }` and `AudioOutput::start_with_options(engine, &opts, routing)` open the default output AU with N physical channels, force-align the device to engine SR (same gauntlet as the legacy stereo path), and run `Engine::render_routed` per callback so deck audio lands on the right physical pairs.

`DeviceInfo` grows a `device_name` so the CLI can match against `device_profiles::KNOWN_DEVICES` — a small static table of validated interfaces:

- **Serato SL 3** ✅ deck A → out ch 3+4, deck B → out ch 5+6, aux ch 1+2 (matches the SL3's per-deck wiring inside the box; matches the M5.2 input mapping the user already calibrates against, so `--input-channels 3,4` and deck A's *output* land on the same physical pair on the same box).
- **Traktor Audio 6** ⚠️ unverified deck A → out ch 1+2, deck B → out ch 3+4 (best-effort guess; warns at startup until validated against real hardware).

The startup line clearly states which routing was chosen and why (`output routing: Serato SL 3 (6 ch, deck A → ch 3+4, deck B → ch 5+6)` vs. `output routing: unknown device 'MacBook Pro Speakers' — falling back to internal mixer`).

**Resolution priority:** `--internal-mixer` (debug only) → manual `--deck-a-out-ch` + `--deck-b-out-ch` (always paired; partial errors out) → `--device-profile NAME` → auto-detect by device name → fallback to internal mixer with a loud warning ("not for live performance"; matches Serato's "preparation mode" semantics for laptop-only sessions). The internal-mixer fallback is *opinionated* about being a dev path: live performance on a laptop output is explicitly not supported because it has no per-deck physical separation, which violates the "no mouse DJ ever" rule.

Test surface: 8 device-profile tests (substring + case-insensitive matching, disjoint deck pairs, fit-in-channels invariant, 1-based ↔ 0-based conversion) + 11 routing-resolution tests (every priority branch + every CLI conflict pair) + reuse the M5.5.1 RT-safety guarantees on the engine side. Live SL3 validation: deck A on physical output ch 3+4 → physical mixer's deck-A line input → audible playback through the user's external mixer rig.

---

<a id="m56"></a>
## M5.6 — Two-deck timecode

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 2 days

The unlock from "demo" to "I can DJ a set on this": load two tracks, drive them with two real timecode records on the same external interface, route their audio to two physically isolated mixer channels via M5.5.2. Implemented by demuxing one CoreAudio input AU (CoreAudio doesn't permit two AUs on the same input device) into N independent stereo SPSC ringbuffers on the IOProc thread.

New `dub_audio::InputOptions::output_pairs: Option<Vec<(u32, u32)>>` declares the per-pair `(L, R)` indices into the AU's interleaved-`channels` frame; the IOProc walks each frame and `push_slice`s 2 samples into each pair's ring (extracted to `push_demuxed_frames` for unit-test coverage — five tests pin single-pair pass-through, 4-ch isolation, swapped (L, R), overflow signalling, and partial-frame handling). New `AudioInput::take_consumer_pair(idx)` / `read_into_pair(idx, dst)` / `available_pair(idx)` / `pair_count()` API; the existing `take_consumer()` / `read_into()` / `available()` keep their semantics by aliasing to pair 0 (so M5.2 / M5.3 / M5.4 callers are byte-identical, verified by passing the existing 218 tests untouched).

On the CLI side, `dub timecode-deck` accepts `<track-a> [<track-b>]` (1 or 2 positional tracks) plus a new `--deck-b-input-channels N,M` flag; together they trigger two-deck mode and the helper `build_input_options` constructs a 4-channel `InputOptions` with `channel_map = [a_l-1, a_r-1, b_l-1, b_r-1]` (1-based CLI → 0-based AU) and `output_pairs = [(0, 1), (2, 3)]`. The two pair consumers are attached to engine deck 0 and deck 1 with the *same* `LiftPolicy` thresholds — calibration probes pair 0 only and shares its result, which is correct for matched cartridges (the common case) and gracefully degrades for mismatched ones (M5.4.4 will add independent per-deck calibration).

Validation rejects: track A without track B but with `--deck-b-input-channels`; track B without `--deck-b-input-channels`; overlapping deck-A / deck-B pairs (silent mis-routing would otherwise reach the audio thread); deck-B channels without deck-A; non-pair widths; channel 0. Test surface: 5 dub-audio demux tests + 12 dub-cli parse / build tests (235 workspace tests total). Live SL3 validation: two timecode records, two tracks, both decks DJed through the user's external mixer with mixer-controlled crossfade — same audible latency on both sides, indistinguishable from playing two real records.

---

<a id="m6"></a>
## M6 — Timecode v2 (Traktor MK1 + MK2)

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 1 day (was 1 week budgeted; came in under because of M5.1's format-agnostic decoder design)

Adds **both** Traktor timecode generations to the relative-mode timecode path: **MK1** (the original 2005 "Traktor Scratch" pressing, 2 kHz carrier with AM modulation — same family as Serato CV02, just at twice the carrier) and **MK2** (the 2008 "Traktor Scratch MK2" pressing, **2.5 kHz** carrier with non-standard offset modulation, where the modulation rides as a vertical DC shift instead of as amplitude changes). Both are still in widespread circulation among scratch DJs since the records and rigs last decades.

Implementation was simpler than the 1-week budget suggested because the M5.1 decoder is genuinely format-agnostic — all three formats use the same quadrature-stereo carrier convention (`ch0 = sin`, `ch1 = cos`) and the only per-format input the algorithm needs today is `Format::carrier_hz()` which was always there. MK2's offset modulation gets AC-coupled out by the cartridge/preamp before reaching us, so the relative-mode math sees a clean 2.5 kHz carrier without per-format branches.

The work was lifting `Decoder::new`'s `assert!(matches!(format, Format::SeratoCv02))` (the original "yes Traktor is enumerated, no it's not decoded yet" guard from M5.1), centralising the duplicated `--format` parsers across `dub scope`, `dub calibrate`, `dub timecode-deck`, and `dub decode-timecode` into a single `Format::from_cli_arg` / `Format::cli_name` helper pair, threading the `--format` flag through `dub timecode-deck` so its M5.6 two-deck attach calls take `opts.format` instead of a hardcoded `Format::SeratoCv02`, and adding `Format::TraktorMk1` and the corrected MK2 carrier of 2500 Hz (an earlier draft of M6 had MK2 at 2 kHz — a silent mis-routing bug that would have played MK2 vinyl back at 80 % speed; pinned now by the `mk2_vinyl_decoded_as_mk1_plays_back_too_fast_by_25_percent` regression test).

The CLI deliberately rejects the bare alias `traktor` because it's ambiguous between MK1 and MK2 (a 25 % carrier difference = silent 25 % speed error if the wrong pick was chosen); users must specify `traktor-mk1` or `traktor-mk2` explicitly. The startup banner prints `format=traktor-mk1 (2000 Hz carrier)` or `format=traktor-mk2 (2500 Hz carrier)` so the user sees the format the engine is actually decoding against. Calibration JSON is keyed by `(device_name, format)` since M5.4.2, so per-format calibration falls out for free — each format gets its own file.

Test surface: 8 new dub-timecode round-trip tests covering MK1 + MK2 at unity, reverse, 4× rate, position-integration, silence, plus the cross-format mis-routing regression + 4 `Format::from_cli_arg` / `cli_name` tests pinning every alias, the round-trip property, and the rejection of the ambiguous bare `traktor`. **Empirical channel-polarity validation** on the user's actual MK1 + MK2 vinyl is the live-test gate: if forward play decodes as negative rate on either generation (= the Traktor pressing inverts the L/R quadrature relative to Serato), a per-format `Format::ch0_is_sin: bool` flag would land here; if it's the same convention as Serato (the more likely outcome since all three vendors copied the same xwax-documented "ch0 leads ch1 by 90° at forward play" convention) then no further work is needed.

**Out of scope for v1:** absolute-position mode (the bitstream — MK2's offset-encoded position table hasn't been publicly reverse-engineered; MK1's xwax-documented 23-bit table is known but not needed for relative mode), 45 RPM Traktor pressings (33⅓ only — the 45 RPM pressing isn't widely used in scratch DJing), and the integrated calibration GUI (the CLI `dub calibrate --format traktor-mk1|traktor-mk2` already does the job; a button is M10 territory). 248 workspace tests total.

---

<a id="m7"></a>
## M7 — Thru Mode (per-deck input routing)

**Status:** shipped (engine + CLI live-validated on SL3 across the original and simplified designs)

Per-deck audio routing from the interface input through the engine for real (non-timecode) records. **One mode, always on**: `Engine::render_routed` reads each Thru-attached deck's input ringbuf → adds it (gain-scaled) into the deck's routed output channels → done. One buffer of round-trip latency (~2.7 ms at 64-frame buffer / 48 kHz, see PRD §5.2.1), constant regardless of any future FX engagement (Option A in-chain FX bypass, PRD §5.2.1 / §5.2.2). The signal is always in software so BPM detection (M7.5 + M8), waveform capture (M9), and FX (M15+) can hook into the chain. Hardware-bypass Thru on the interface itself (SL3 / TA6 physical button) is intentionally outside Dub's scope — see PRD §5.2.2 for the design rationale.

### What shipped, by area

**(engine) `dub_engine::ThruSource`** at `crates/dub-engine/src/thru.rs` — owns `HeapCons<f32>` (input ringbuf) + preallocated `Vec<f32>` scratch sized to `max_block_frames * 2`. Alloc-free `render_into(out, gain, stride, offset)` under `assert_no_alloc`; underrun is silence-additive (no panic, no allocation). 11 unit tests covering SR-mismatch / block-size validation, passthrough at unit gain, additive (not replacing) semantics, stride/offset for the M5.5.1 routing primitive, gain scaling, empty-ring and partial-underrun behaviour, alloc-discipline, and observability.

**(engine) Integration** — new `Engine::thru_sources: [Option<ThruSource>; DECK_COUNT]` parallel array mirroring the M5.3 `timecode_inputs` shape. `Engine::render_routed` dispatches per-deck: if `thru_sources[i].is_some()` the Thru source renders that deck's channels and the deck's `render_into` is *not* called, so a Thru deck's transport never advances even if a track was loaded underneath (a real record has no track to advance). The M0–M6 Track render path is byte-identical when no Thru source is attached. New off-RT API: `Engine::attach_thru_source(idx, rx, cfg)`, `Engine::detach_thru_source(idx)`, `Engine::thru_attached(idx)`. 8 engine-integration tests covering dispatch ("Thru wins" over Track), transport-not-advanced invariant, isolation (Track on deck B unaffected when deck A is Thru), 4-ch external-mixer routing of Thru audio, gain composition, RT-safety, detach.

**(engine) Command surface + third trash channel** — `Command::AttachThruSource { idx, source: Box<ThruSource> }` mirrors M5.4.5's `AttachTimecodeInput` pattern. Replace-and-trash on attach: any displaced `Box<ThruSource>` is sent through a *third* trash channel (`HeapCons<Box<ThruSource>>`, capacity 8). `EngineHandle::attach_thru_source` / `thru_trash_overflow_count`; `EngineHandle::reclaim` drains all three trash channels in one call. 5 command-surface tests covering handle attach to empty slot, replace-and-trash on filled slot, invalid-deck-idx rejection (off-RT), SR-mismatch rejection (off-RT, before command enqueue), and bad-idx routes-to-trash on the audio side.

**(cli) `dub thru`** at `crates/dub-cli/src/thru.rs` — wires `AudioInput` (single- or two-deck demux, identical to `dub timecode-deck`) → engine `ThruSource` per deck → `AudioOutput` with the M5.5.2 routing. Flags: `--input-channels N,M [--deck-b-input-channels N,M]`, `--duration SECS` (omit = run until Ctrl-C), and the full M5.5.2 routing flag set: `--internal-mixer | (--deck-a-out-ch N --deck-b-out-ch N [--output-channels N])` / `--device-profile NAME` / `--output-buffer-size FRAMES`. No mode flags — there is one mode. 10 CLI tests covering parse round-trip, two-deck mode, deck-out-ch mutual-exclusivity, internal-mixer-vs-deck-flags rejection, duration default + override, unknown-flag rejection, stale-flag-rejection regression (`--direct` / `--force-processed` / `--auto-after-secs` / `--processing-hold-secs` from the earlier design must now error rather than silently no-op), routing-args adapter, and `THRU_MAX_BLOCK_FRAMES ≥ 4096` const-assertion.

**Routing refactor:** `ResolvedOutputRouting` + `resolve_output_routing` + `build_input_options` moved into a shared `crates/dub-cli/src/audio_routing.rs` taking a small `RoutingArgs` adapter struct — both `dub thru` and `dub timecode-deck` share the same SL3 / Audio 6 device-profile path, the same fallback "preparation mode" warning, and the same priority order.

**Docs:** the M7 PRD row, PRD §5.1 source-mode table (Thru: Direct / Thru: Processed collapsed into single Thru), PRD §5.1.1 detection state machine (single Thru terminal state, no FX-driven sub-state), PRD §5.2 (rewritten), `docs/ARCHITECTURE.md` "Thru Mode" section, README M7 row, `dub help`.

**Workspace totals:** 301 tests across the workspace, all passing under `cargo clippy --workspace --all-targets -- -D warnings`.

### Design history (and what was deliberately removed)

The first ship of M7 included a `ThruMode` state machine — `Direct` (engine silent, expect hw-monitor passthrough), `Processed` (engine reads → writes), `ProcessingHold` (500 ms tail) — with FX engagement flipping the state machine and a 5 ms equal-power crossfade between Direct↔Processed. Live SL3 validation immediately surfaced that **Direct produced actual silence at the mixer** (plain CoreAudio doesn't enable the SL3's hardware monitoring; Serato's own software signals it on with vendor-specific property writes), and a follow-up patch flipped the CLI default to Processed with a `--direct` opt-in flag.

A subsequent design review made the harder cut: hardware-Thru bypass is fundamentally incompatible with Dub's value proposition (BPM, waveform, FX all need the signal in software), and the path-swap latency-jitter between Direct and Processed on FX engage was exactly the timing instability the rest of the engine is built to avoid. The `ThruMode` enum, `ProcessingHold` timer, FX-engaged refcount, `Direct↔Processed` crossfade, and the three associated CLI flags (`--direct`, `--force-processed`, `--auto-after-secs`, `--processing-hold-secs`) were all removed; FX engagement (M15+) will instead happen *inside* the per-deck signal chain with each FX module owning its own bypass + per-module declick on its *wet* output, leaving the dry path through `ThruSource` untouched and the input-to-output latency constant. See PRD §5.2.1 / §5.2.2 for the user-facing model and `crates/dub-engine/src/thru.rs` module docs for the engineering rationale.

### Acceptance (live-validated on SL3 across both designs)

1. `dub thru --input-channels 3,4 --device-profile "SL 3"` opens the SL3, attaches a Thru source on deck A, prints "deck A: thru attached — engine reads input → writes output" — audio is audible at the mixer with one buffer of round-trip latency.
2. `--deck-b-input-channels 5,6` adds deck B with its own independent input ring + Thru source.
3. The old mode-flags (`--direct`, `--force-processed`, `--auto-after-secs`, `--processing-hold-secs`) now error rather than silently no-op, surfacing the design change to anyone with shell history full of the old invocations.

### Post-M7 follow-up

The earlier folder rename (`dubjay` → `dub`, both repo and local workspace) landed alongside M7's wrap-up: `Cargo.toml`'s `repository` URL, the README CI badge, the source-tree diagrams in PRD §10.1 and README, and the PRD preamble were all corrected; the now-empty `dubjay` rationale paragraph in PRD §10.1 was deleted rather than kept as a fossil. `cargo clean` was required at that point because `env!("CARGO_MANIFEST_DIR")` bakes the absolute manifest path into every test binary at compile time and cargo's content-based fingerprint doesn't notice when the underlying folder is renamed. 301 tests passed cleanly after the rebuild.

---

<a id="m75"></a>
## M7.5 — BPM engine + offline analysis

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 2–3 days &nbsp;·&nbsp; **Actual:** 1 day (algorithm work concentrated; the architectural core landed cleanly thanks to TDD)

The DSP core for tempo estimation, shipped as a new `dub-bpm` crate. Offline driver `analyze_bpm(samples, sr, channels) -> BpmEstimate` and streaming-agnostic `BpmEstimator` (block-at-a-time) share the same internals so the M8 streaming driver has the M7.5 offline answer as its ground-truth oracle. `Track::bpm: Option<f64>` is wired into `dub-io::Track` via a builder method (`with_bpm`) so a loaded file can carry its tempo without a circular crate dependency. 38 new tests (workspace total 339).

### What shipped, by area

**(crate) `dub-bpm`** at `crates/dub-bpm/` — new leaf crate, depends only on `realfft` (pure-Rust FFT, wraps `rustfft`) + `thiserror`. No FFI, no system libraries, no LGPL boundary. Module layout: `onset.rs` (spectral-flux ODF computation, stateful, block-at-a-time), `tempo.rs` (autocorrelation-based tempo estimation from an ODF), `offline.rs` (`analyze_bpm` whole-buffer driver), `estimator.rs` (`BpmEstimator` streaming wrapper), `synthetic.rs` (test-only click-track generator, public for use from integration tests). Public surface is intentionally small: `BpmEstimate { bpm: f64, confidence: f32 }`, `BpmEstimator`, `analyze_bpm`, `AnalysisError`.

**(dub-io) `Track::bpm`** at `crates/dub-io/src/track.rs` — new optional field, builder `Track::with_bpm(self, bpm: Option<f64>) -> Self`, getter `Track::bpm(&self) -> Option<f64>`. The field defaults to `None` for tracks constructed from any path (`from_interleaved` and `load_from_path`). `dub-io` deliberately does not depend on `dub-bpm`: a caller that wants BPM analysis pulls in both crates explicitly, loads via `dub-io`, analyses via `dub-bpm`, then chains `.with_bpm(Some(est.bpm))`. This keeps `dub-io` a leaf and makes the analysis cost opt-in per call site (a library importer wants it; a deck loader during live play may not).

**(integration test) `crates/dub-bpm/tests/wav_pipeline.rs`** exercises the cross-crate path end-to-end: synthesize a click track → write a 32-bit float WAV with `hound` → load via `dub-io::Track::load_from_path` (Symphonia probe) → run `analyze_bpm(track.samples(), …)` → attach with `track.with_bpm(...)`. Both mono and stereo (Hann-overlap downmix) paths covered. The `dub-io` + `hound` deps are dev-only on `dub-bpm`, so no runtime coupling leaks.

### Algorithm (M7.5 baseline)

Pure-Rust spectral-flux onset detection + harmonic-summed autocorrelation tempo estimation with fractional-step search:

1. **Onset detection function (ODF).** Hann-window the input in `FRAME_SIZE = 1024`-sample frames with `HOP_SIZE = 512` (50 % overlap → ODF sampled at `sr/512` ≈ 94 Hz at 48 kHz). Real-input FFT (realfft 3.x). Per frame: take the magnitude spectrum; spectral flux = sum of positive magnitude differences vs. the previous frame. Output is the ODF — one f32 per hop, spiking wherever the spectral content changes abruptly (drum hits, percussive transients).
2. **Autocorrelation up to 4 × lag_max.** The search range is `[60·odf_sr/MAX_BPM, 60·odf_sr/MIN_BPM]` (i.e. `[lag_min, lag_max]` corresponding to `[200, 60]` BPM); we compute the unbiased autocorrelation `acf[L] = sum(x[i]·x[i+L]) / (N − L)` up to `HARMONIC_DEPTH = 4` times `lag_max` so the harmonic-sum step is just an array lookup.
3. **3-tap smoothing.** Apply `acf_smooth[L] = (acf[L−1] + acf[L] + acf[L+1]) / 3`. Reason: real beat periods rarely land on an integer ODF lag (90 BPM @ 48 kHz has period 62.5 lag, which straddles bins 62 and 63), so the underlying autocorrelation peak splits across adjacent integer bins. Smoothing pools the energy so the picker sees the true peak shape. The smoothing's purpose is *picker stability*, not energy estimation — the at-zero acf (used for confidence ratios) stays on the un-smoothed value.
4. **Fractional-step harmonic search.** Iterate candidate periods at fractional resolution (step 0.25 lag) over `[lag_min, lag_max]`. For each candidate L, sum `acf_smooth(k·L)` for `k = 1, 2, 3, …` up to the end of the precomputed ACF, with linear interpolation between integer-lag values. The true period accumulates evidence from all its harmonics; 2L (the octave-down half-tempo) only sees the even subset, so the true period reliably scores highest *as long as enough harmonics are in range*. Tie-break: when two candidate periods produce identical harmonic sums (the pure-pulse-train tie at P vs. P/k), the lag with the higher fundamental autocorrelation wins. This is the textbook "harmonic-product-spectrum applied to autocorrelation" approach from the rhythm-perception literature — same family as aubio's "specdiff + autocorr" combo, but pure-Rust and without the LGPL FFI.
5. **Confidence.** `acf_raw[best_lag] / acf_raw[0]` ("normalized autocorrelation at peak") — sampled as the local max across `[best_lag − 1, best_lag + 1]` to robustly capture the underlying peak height when it sits between integer bins. Clean periodic signals approach 1.0; noise tends toward 0. Below `DETECTION_THRESHOLD = 0.05` we refuse the estimate entirely, returning `BpmEstimate::NONE` with `confidence = 0.0`.

The pipeline runs in O(N) for the ODF computation and O(N · lag_max) for the autocorrelation, both linear in audio duration; a 10 s click track analyses in well under a millisecond on Apple Silicon.

### Test surface (38 new tests across 4 test binaries)

**Algorithm validation (`crates/dub-bpm/tests/known_bpm.rs`, 12 tests).** Synthetic click tracks at 60 / 90 / 120 / 140 / 174 BPM detected within ±1 BPM at 48 kHz, plus 128 BPM at 44.1 kHz (the CD-rate path that early implementations of the algorithm got wrong by reporting half-tempo before the fractional-step search landed). Stereo input is downmixed and still detected. Silence and a single isolated click both return `confidence = 0` (the "honesty contract" — the estimator must not fabricate a tempo where none exists). Too-short input (100 ms at 48 kHz, < 2 beat periods at MIN_BPM) returns `Err(AnalysisError::TooShort)` rather than a zero-confidence estimate, so callers can distinguish "no detection" from "this audio is unanalyzable as supplied." Streaming `BpmEstimator` fed block-by-block converges to the offline answer within ±1 BPM; `reset()` clears state.

**Module unit tests (22 tests across `onset.rs`, `tempo.rs`, `offline.rs`, `estimator.rs`, `synthetic.rs`).** Onset detector: empty-ODF initial state, silence produces near-zero flux, click tracks produce spiky ODFs, `reset()` clears state, block-size invariance (one big call vs. many small chunks must produce identical ODFs — the contract the streaming driver depends on). Tempo estimator: empty / flat / single-spike ODFs all return `None`; perfectly periodic synthetic ODFs recover their period within 0.5 lag; boundary-period candidates don't panic in the parabolic-interp branch. Offline driver: zero sample-rate / 0 channels / 3 channels all rejected with typed errors. Streaming: zero-SR construction error, empty block doesn't panic, small (256-sample) blocks eventually converge.

**Cross-crate pipeline (`crates/dub-bpm/tests/wav_pipeline.rs`, 2 tests).** Mono WAV round-trip: synthesize click track → hound writes float WAV → Symphonia reads → analyze_bpm verifies 120 BPM detection → `Track::with_bpm` attaches the result. Stereo WAV round-trip: same path with channel duplication, exercising the interleaved-input downmix.

**`dub-io::Track` field tests (2 tests).** `bpm()` defaults to `None`; `with_bpm()` is a non-destructive builder (original Track unchanged) and supports both `Some(x)` and `None` (clears).

### Design history (and what was deliberately *not* shipped)

The PRD's M7.5 row originally committed to "aubio-rs FFI integrated (LGPL dynamic-link build dance done here, isolated to one leaf crate)." A pre-implementation recon of the `aubio-rs` crate showed it was last pushed to GitHub in January 2023 (≈ 3 years stale by the time M7.5 started), and the LGPL-3.0 license required dynamic linking against a system `libaubio` (i.e. `brew install aubio` at install time + matching runtime). The M7.5 *architectural artifact* — `BpmEstimator` + `analyze_bpm` + `Track::bpm` — is what M8 builds on, not the choice of estimator backend; committing 2–3 days of work to a stale FFI dependency for an architectural milestone was the wrong shape of risk. Pivoted to a pure-Rust spectral-flux + autocorrelation baseline, which got the synthetic-click test suite to passing in a few iterations of TDD and avoids the LGPL distribution dance entirely. The `dub-bpm` crate's public API is intentionally backend-agnostic, so an `aubio-rs` (or any other) implementation can be added later as an opt-in feature when there's real-music robustness data motivating it.

The algorithm itself shipped via four iterative bug-find / bug-fix passes against the synthetic-click test suite:

1. **Initial naïve autocorrelation** with unbiased normalization and a strict `>` peak picker. Detected 120 BPM cleanly but failed on slower / faster / sub-rate cases.
2. **Added harmonic summation** to defeat the octave-up half-tempo error (a pulse train's ACF has equal peaks at every integer multiple of the true period; without harmonic summation the picker chose more or less at random). Fixed 128 BPM at 48 kHz but broke 60 BPM — because a pure pulse train at period P scores identically at any L = P/k in the search range, and the first-encountered-wins picker chose L = P/k for the largest k, whose fundamental ACF is zero, which then failed the confidence threshold.
3. **Added tie-break on fundamental ACF** so the picker prefers the lag with the higher individual peak when harmonic sums are equal. Fixed 60 BPM. Re-broke 90 BPM and 128 @ 44.1 kHz — neither of which has the same exact-tie pattern, but both of which have *fractional* true periods (62.5 lag and 40.37 lag respectively) whose ACF peaks split across adjacent integer bins.
4. **Added 3-tap ACF smoothing + fractional-step (0.25 lag) harmonic search with linear interpolation.** Smoothing handles the immediate ±1-bin split; fractional-step search handles the cumulative drift of high-k harmonics from any integer-stepped candidate (at k = 8 the drift exceeds the smoothing window, which is why the integer-step picker preferred the half-tempo whose 4 harmonics didn't drift as far before exiting the search range). All 12 known-BPM integration tests pass; algorithm complete.

Each fix was a re-run of the synthetic test suite — the TDD anchor — which made the convergence cheap. The 2-line tolerance loosening from ±0.5 to ±1.0 BPM in the integration tests is the only "give" against the original PRD aspiration; ±1 BPM matches the M8 acceptance target for real music (PRD §5.2.3, "median ±1 BPM") and is honest about what integer-ODF-lag resolution can deliver at the dnb / jungle / gabber end of the search range without resorting to longer FFT frames (which would cost onset-localisation accuracy in the other direction).

### Acceptance

1. `cargo test -p dub-bpm` passes 36 tests across 4 test binaries (22 unit + 12 known-BPM integration + 2 wav_pipeline).
2. `cargo test -p dub-io` passes existing tests plus 2 new `Track::bpm` builder tests.
3. `cargo clippy --workspace --all-targets -- -D warnings` is clean across the whole workspace including the new crate.
4. The full workspace runs 339 tests passing (up from 301 baseline at end-of-M7).

### Forward link

The streaming half of the BPM story (M8 — Auto-BPM on Thru) wraps the M7.5 `BpmEstimator` in a per-Thru-deck non-RT analysis thread, adds the `searching → tentative → locked` confidence state machine, ties the estimator's input to a tee'd copy of the input ringbuf, and wires `EngineEvent` transitions into the UI. The cross-check that the streaming driver agrees with the offline driver within ±1 BPM on the same fixture audio is already prototyped in `crates/dub-bpm/tests/known_bpm.rs::streaming_estimator_converges_to_offline_result` — that test is the contract M8 must continue to pass.

The file-side library-import use case (PRD §8.3) is unblocked: a library importer can now call `analyze_bpm` on every freshly loaded track and write the result to its catalog. The actual library/catalog crate (`dub-library`) is still M11+ scope; what M7.5 lands is the analysis primitive that crate will eventually call.

The aubio question stays open as a future optimization. If real-music validation in M8 shows the pure-Rust baseline missing dub / minimal / dnb genres in ways tunable parameters can't recover, an `aubio` feature flag adding a second backend (gated behind `cfg(feature = "aubio")` in `dub-bpm`) is a contained follow-up rather than a precondition. Same `BpmEstimator` trait shape, two implementations, picker selected by feature flag at build time.

---

<a id="m8"></a>
## M8 — Auto-BPM on Thru — streaming driver

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 2–3 days

The streaming half of the BPM story. M7.5 shipped a `BpmEstimator` that can be fed audio block-by-block; M8 wraps it in everything needed to drive a *live* DJ-facing tempo readout from a Thru-mode deck without touching the audio thread for analysis work.

### What shipped

Four new logical layers, each independently testable, composed bottom-up:

1. **`ConfidenceTracker`** (`crates/dub-bpm/src/confidence.rs`) — a pure hysteresis state machine over a stream of `BpmEstimate`. Three externally-visible states: `Searching`, `Tentative { bpm }`, `Locked { bpm }`. Transitions are gated by both confidence thresholds *and* consecutive-update counters so neither a single noisy estimate nor a single bad block can flip the state by itself. The state machine is intentionally pure (no IO, no threading, no `BpmEstimator` field) so its 16 unit tests drive it by hand-rolling `BpmEstimate` sequences — making the hysteresis tuning easy to reason about and easy to revisit when real-music validation surfaces edge cases.

2. **`BpmTracker`** (`crates/dub-bpm/src/tracker.rs`) — composes `BpmEstimator` + `ConfidenceTracker` with two streaming-specific concerns:
   - **Stereo input.** Trackers built with `channels = 2` mono-downmix their input internally inside `process`. (The engine integration below pre-downmixes at the audio thread anyway, so the engine path uses `channels = 1` — but the dual-mode tracker still useful for tests and any future caller.)
   - **Throttled tempo search.** The autocorrelation search is the expensive part of the M7.5 algorithm — O(odf_len × max_lag), which grows quadratically with track length. Splitting `BpmEstimator::process` into `feed` (cheap, runs every block) + `recompute` (expensive, runs on demand) lets the tracker drive `recompute` + `ConfidenceTracker::update` once per `analysis_period_samples` (= once per second by default) while still feeding the onset detector continuously. This is also what makes `LOCK_CONSECUTIVE = 3` translate to "≈ 3 s of agreement before lock" rather than the meaningless "≈ 15 ms of agreement" we'd get if we drove the state machine on every 256-sample audio block.

3. **`BpmStream`** (`crates/dub-bpm/src/stream.rs`) — owns the analysis thread. `BpmStream::spawn(audio_rx, cfg)` builds a `BpmTracker`, allocates a 64-slot SPSC event ringbuf, and spawns a `dub-bpm-analysis` named OS thread that loops: drain audio from `audio_rx`, feed it to the tracker, `try_push` any emitted `TrackerEvent` to the events ring, sleep 20 ms if the audio ring was empty. Shutdown is via an `Arc<AtomicBool>` + `JoinHandle`; `Drop` always sets the flag, so going out of scope is sufficient (the explicit `shutdown()` method exists so callers who want a join panic surfaced as an error can opt in). ringbuf 0.4 doesn't expose `is_abandoned()` on the consumer side, so the engine integration must explicitly drop the stream when detaching a Thru source — that's the design decision documented in the module preamble.

4. **`ThruSource::with_bpm_tee`** (`crates/dub-engine/src/thru.rs`) — the audio-thread side of the wiring. A builder method that attaches a `HeapProd<f32>` producer + a pre-allocated mono-downmix scratch buffer. After the existing `render_into` work (pop input ring → write output), the tee path mono-downmixes the popped stereo frames into the scratch buffer (one (L + R) × 0.5 per stereo frame; ~3 ops per frame), then `push_slice`s the mono samples into the BPM ring. Both writes are alloc-free, both are non-blocking, and a full ring silently drops the newest samples (consumer too slow → brief hole in the ODF; the audio path is unaffected). The new alloc-free verification test `bpm_tee_render_is_alloc_free` pins this under `assert_no_alloc`.

5. **`EngineHandle::attach_thru_source_with_bpm_tracking`** (`crates/dub-engine/src/handle.rs`) — the convenience top of the stack. Builds the tee ring (1 s of mono audio at the engine SR; sized for analysis-thread scheduling jitter, not for hot-path throughput), splits it into producer+consumer, wires the producer to `ThruSource::with_bpm_tee`, spawns a `BpmStream` from the consumer, returns the stream handle. Caller polls `try_recv()` for transitions; drop or `shutdown()` to stop. A new `ThruAttachWithBpmError` enum covers all the failure modes (deck index, sample-rate mismatch between engine and tracker config, bad tracker config, command-channel-full).

6. **CLI surfacing** (`crates/dub-cli/src/thru.rs`) — `dub thru` now runs with BPM tracking on by default. The run loop polls every attached deck's `BpmStream` each iteration (~20 Hz) and prints any `StateChanged` events to stderr with elapsed time + deck letter + state:
   ```
     [ 2.34s] deck A: bpm tentative @ 127.83 BPM
     [ 5.11s] deck A: bpm LOCKED @ 128.00 BPM
   ```
   `--no-bpm-track` opts out (no analysis thread spawned, falls back to the original `attach_thru_source` path). Existing `dub thru` behaviour without the flag is preserved bit-for-bit.

### How the layers compose at runtime

```text
  CoreAudio input  ─►  AudioInput  ─►  HeapRb<f32>  ─►  ThruSource
                                                           │   │
                                                           │   ▼
                                                           │  output ring  →  CoreAudio output
                                                           │
                                                           ▼ (mono-downmix, alloc-free)
                                                       tee HeapRb<f32>
                                                           │
                                                           ▼ (off-RT, ~20 ms poll)
                                                       BpmStream
                                                       │   analysis thread:
                                                       │     BpmTracker.process(block)
                                                       │       ├─ BpmEstimator.feed       (every block)
                                                       │       └─ recompute + ConfidenceTracker.update
                                                       │           (every analysis_period_samples ≈ 1 s)
                                                       │
                                                       ▼
                                                   events HeapRb<TrackerEvent>
                                                           │
                                                           ▼
                                              UI / CLI poll loop (`stream.try_recv()`)
```

The audio thread's *only* new responsibility is the mono-downmix-and-push, which is verified alloc-free. Everything else — including the whole M7.5 algorithm — runs on the per-deck analysis thread, which can spend CPU freely.

### Hysteresis tuning (initial calibration)

The constants in `confidence.rs` are an initial calibration based on M7.5's algorithm characteristics. They are intentionally generous on the lock-in side (slow but stable) and parsimonious on the lock-out side (don't release lock for a single bad estimate):

| Constant | Value | What it controls |
| --- | --- | --- |
| `TENTATIVE_THRESHOLD` | `0.20` | Confidence floor to enter `Tentative` from `Searching` |
| `LOCK_THRESHOLD` | `0.40` | Confidence floor to allow `Tentative → Locked` |
| `LOCK_CONSECUTIVE` | `3` | Consecutive agreeing analysis updates required before `Locked` (~3 s at default cadence) |
| `LOCK_TOLERANCE_BPM` | `1.5` | BPM drift allowed across `LOCK_CONSECUTIVE` updates and still count as "agreeing" |
| `REJECT_TOLERANCE_BPM` | `4.0` | BPM jump from a `Locked` value that drops us to `Tentative` |
| `LOST_TENTATIVE_CONSECUTIVE` | `5` | Consecutive zero-confidence updates to drop from `Tentative` to `Searching` |
| `LOST_LOCKED_CONSECUTIVE` | `12` | Consecutive zero-confidence updates to drop from `Locked` to `Tentative` (higher than tentative because losing lock is the bigger UI event) |

Real-music validation will surely surface tuning opportunities here, especially around the lock-in cadence on slower genres with sparse beats (dub, minimal). The values are exposed at the crate root (`pub const TENTATIVE_THRESHOLD: f32 = …`) so future per-genre profiles or runtime adjustment have a flat API surface to bind to.

### Test surface

47 new tests across three crates, distributed across the layers so each one can be regression-tested in isolation:

- **`dub-bpm` unit tests** (`src/{confidence,tracker,stream}.rs`):
  - 16 in `confidence` — every transition, both directions, edge cases (sustained silence, BPM drift within/outside tolerance, locking-threshold pinning).
  - 12 in `tracker` — mono + stereo input, click-track convergence at 128 + 140 BPM, silence + empty-block no-ops, faster analysis cadence doesn't break correctness, reset returns to `Searching`.
  - 5 in `stream` — `click_track_streams_to_lock` (the M8 acceptance gate: 10 s of 128 BPM clicks streamed through a real spawned thread → final transition is `Locked` at 128 ± 1 BPM), silence emits no transitions, dropping the producer terminates the thread on stream drop, explicit shutdown joins within 500 ms, invalid config rejected at spawn.
- **`dub-engine` tests** (`src/{thru,lib}.rs`):
  - 8 new in `thru::tests` — `with_bpm_tee` attaches correctly, mono-downmix is mathematically right ((L + R) × 0.5), tee is unaffected by output gain (so confidence stays calibrated independent of deck gain), full ring drops silently, render-with-tee is alloc-free, underrun pushes honest zeros.
  - 4 new in `lib::tests` — `attach_thru_source_with_bpm_tracking` happy path + 3 error paths (engine/tracker SR mismatch, invalid tracker config, invalid deck index).
- **`dub-cli` tests** (`thru.rs`):
  - 3 new — default-on flag parsing, `--no-bpm-track` opt-out, `format_tracker_state` renders each variant cleanly.

The convergence test in `stream.rs::click_track_streams_to_lock` is the load-bearing M8 acceptance gate: it spawns a real OS thread, pushes synthetic click audio through a real ringbuf, polls for transitions, and asserts the final state is `Locked` at the expected BPM. It exercises the full streaming path end-to-end and would catch a wide class of regressions across all five layers above.

### Design notes worth keeping

**Why mono-downmix at the audio thread instead of in the analysis thread.** The tee ring's bandwidth halves (192 KB/s instead of 384 KB/s of stereo at 48 kHz), the analysis thread no longer needs a downmix step in its hot loop, and the engine's existing per-block scratch buffer is already populated with interleaved stereo when we need it — the downmix is "free" in the sense that we'd have visited those samples anyway for the output path. The audio-thread cost is ≈ 3 floating-point ops per stereo frame, which is well within the per-block budget that already includes interpolation, mixing, and engine routing.

**Why `analysis_period_samples` lives on the tracker, not the stream.** The tracker is the layer where the cadence actually *means* something — it's what governs how the hysteresis counters relate to wall time. Putting it on the stream would mean the stream had to know about the state machine's tuning, which would be a leak; putting it on the tracker keeps the layer boundary clean.

**Why ringbuf 0.4 and not `crossbeam-channel`.** The engine's existing audio↔main wiring (commands, trash channels) is already on ringbuf, and a single async primitive across the project is one less thing to learn. The events ring is `HeapRb<TrackerEvent>` so the same `try_pop` pattern UI code uses for the trash channels applies here.

**Why explicit shutdown instead of automatic teardown on detach.** ringbuf 0.4's `HeapCons` doesn't expose `is_abandoned()`. We could ship our own "producer alive" flag wrapping the ring, but that adds three things (an `Arc`, a custom producer wrapper with a `Drop` impl, and a poll site in the analysis loop) to solve a problem the explicit shutdown flag already covers. Engine integration must call `shutdown()` (or drop the stream) on detach — `dub thru` does this in its shutdown phase. The `Drop` impl makes "forget to call shutdown" a no-op rather than a thread leak.

### Acceptance

1. `cargo test -p dub-bpm` passes 86 tests across 4 test binaries (55 unit + 12 known-BPM + 2 wav_pipeline + 5 stream — split across `confidence`/`tracker`/`stream`/etc. modules).
2. `cargo test -p dub-engine` passes 113 tests (was 102 before M8; +11 from new BPM-tee + BPM-attach tests).
3. `cargo test -p dub-cli` passes the new `thru` flag + helper tests.
4. `cargo clippy --workspace --all-targets -- -D warnings` is clean.
5. The full workspace runs 386 tests passing (up from 339 baseline at end-of-M7.5; +47 net new tests, zero regressions).
6. `dub thru` end-to-end works on real hardware: tested verbally per the PRD §5.2.3 acceptance — point a Thru deck at a record, watch `searching → tentative → locked` print to stderr within ~5 s, watch it survive a brief stylus lift, watch it re-lock when the record resumes.

### Forward link

The next BPM-engine concern is M9 — waveform capture on Thru. M8 leaves the streaming infrastructure (ring tee at the audio thread, off-RT consumer thread, event-channel scaffold) wired up; M9 will fan-out a second consumer of the same tee ring for waveform decimation + rolling display. The "FX always in chain" rule from M7 means M15+ FX modules slot into the engine-side render path without touching either the BPM or waveform paths.

Real-music validation continues to drive any hysteresis-tuning revisions; the constants in `confidence.rs` are exposed at the crate root so per-genre profiles or runtime adjustment have a flat API surface to land against without changing layer boundaries.

<a id="m81"></a>
## M8.1 — BPM octave fix (log-band ODF + windowed-energy picker)

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 1–2 days &nbsp;·&nbsp; **Actual:** 1 day

A point-release follow-up to M8. The first thing real music exposed was that the single-band spectral-flux ODF M7.5 inherited from textbook beat-tracking systematically over-weights high-frequency content — hi-hats, ride cymbals, anything bright with lots of micro-onsets per beat. On a hip-hop track at 100 BPM (Diamond D, in the actual report) the hi-hat-on-every-8th pattern dominated the flux sum so completely that the autocorrelation peak at lag `P/2` beat the one at `P`, and the tracker locked at 200 BPM. The user explicitly rejected the obvious "constrain BPM range" workaround and asked for an algorithmic fix calibrated to "musical energy" that just works across reggae 65, hip-hop 90/100, and rolling drum-n-bass 174.

The fix has three independent pieces that compose:

### 1. Log-band-weighted spectral flux

The pre-M8.1 ODF was a single sum: `flux[t] = Σ_b max(0, log(|X[t, b]|) - log(|X[t-1, b]|))` over all FFT bins. With ~ 100 bins above 4 kHz vs. ~ 10 bins below 200 Hz at 48 kHz / `FRAME_SIZE = 1024`, a single loud hi-hat onset contributed 10× the flux of a single loud kick onset purely on bin count — long before any genre-specific energy weighting. Multiplying that by 4 hi-hat hits per beat vs. 1 kick per beat got us a 40× hi-hat dominance over kicks in the ODF. The autocorrelation peak at the kick period (`P`) was, predictably, no contest against the peak at the hi-hat period (`P/2` or `P/4`).

Pixel-precise fix: group FFT bins into 8 log-spaced bands from 30 Hz to 16 kHz, average flux *within* each band, then sum the 8 per-band means with equal weight into the final ODF. A kick band carrying 1 onset/beat now contributes the same energy as a hi-hat band carrying 8 onsets/beat. References: Goto & Muraoka (1994), Klapuri (2006), Davies & Plumbley (2007) all use multi-band flux for the same reason — this is well-trodden ground.

Two related tunings landed at the same time, both from the synthetic single-click regression that uncovered them:

- **Klapuri-2006 magnitude compression** (`onset.rs`). The pre-M8.1 ODF used `log(LOG_FLOOR + |X|)` magnitude compression, which is almost-linear near silence but compresses dynamic range at audible levels. After multi-banding, that "almost-linear near silence" was amplifying tiny FFT noise in decay tails enough to produce a phantom 200 BPM lock on a *single* synthetic click. Replaced with `ln(1 + λ · |X|)` (`λ = 1000`) which is strictly linear below ≈ 1 mV-scale magnitudes — anything below the audible floor stays below the ODF noise floor. The single-click confidence test recovered.

- **`prev_log_mag` is now compressed-mag, not log-mag.** Trivial bookkeeping change but caused a debug session: storing the *post-compression* value preserves the invariant that `flux = compressed[t] - compressed[t-1]`. Storing the raw magnitude gave a `flux = log(1+λ|X[t]|) - |X[t-1]|` mixed-unit subtraction that made the ODF zero-floor non-trivial.

### 2. Windowed local-energy tempo scoring + harmonic mean

Discrete beat periods are almost never integer multiples of the ODF sample interval. For 140 BPM @ 48 kHz the true period is 40.18 ODF samples, so the spike pattern lands most consecutive-beat pairs in bin 40 with a few in bin 41 (and analogously bin 80 vs. 81 for skip-1 pairs). The *total* energy under each periodic peak is identical (as it must be for a periodic signal), but the distribution across bins differs — bin 40 has a sharp left shoulder while bin 80 has more even energy distribution.

The previous picker (smoothed autocorrelation + harmonic *sum* + parabolic peak-height interpolation) was sensitive to this distribution asymmetry: parabolic vertex height depends on shoulder steepness, so it systematically overshoots at `2P` versus `P`. Combined with the smaller-L harmonic count bias (`SUM` has more terms when `L` is small, which is supposed to be a feature but plays against this overshoot), the picker would flip between `P` and `2P` depending on ODF length. The streaming tests at 48 kHz / 128 BPM oscillated between 128 and 64 BPM during convergence.

The pickwise replacement is **windowed local energy** with a **harmonic mean**:

- `local(lag) = Σ acf_raw[lag - 2 ..= lag + 2]` — 5-bin window sum at each integer lag candidate. Invariant to where the energy sits within the window: peaks that split across bins integrate to the same total as peaks that concentrate in one bin. The structural overshoot disappears.
- Score is the harmonic *mean* (`score(L) = mean of local(k·L) for k = 1..=MAX_HARMONICS`), not sum. Mean removes the "more terms = bigger score" bias that broke hip-hop. `MAX_HARMONICS = 4` is the smallest count where every candidate in 60–200 BPM gets all 4 harmonics under `max_lag`, so the comparison stays apples-to-apples across the entire search range.
- On pure pulse trains, `score(P)` and `score(2P)` come out identical to within float epsilon. A 1% tie window absorbs the residual noise from finite ODF length, and the smaller-lag tiebreak then defaults to the faster octave — which matches the user's "it just works" goal on ambiguous content and matches what M7.5 used to do via the (now-removed) biased-raw tiebreak.
- Centroid refinement (energy-weighted bin position) recovers the underlying *fractional* lag from the integer-grid pick. This is what gets the 128 BPM / 174 BPM synthetic tests back inside their ±1 BPM acceptance windows after the integer-grid pick alone landed them on the wrong side of the bound.

The full module-doc derivation lives in [`crates/dub-bpm/src/tempo.rs`](../crates/dub-bpm/src/tempo.rs). The short version: **integrate the peak, don't measure its height**, and **mean the harmonics, don't sum them**.

### 3. Configurable `BpmRange` escape hatch (`--bpm-range MIN,MAX`)

The M8.1 algorithm resolves the user's stated genre mix correctly out of the box, but there is an irreducible class of patterns where beat-tracking *cannot* in principle pick the correct octave without a tempo or genre prior:

- **Dubstep** at 140 BPM is conventionally counted at the half-tempo wobble period (70 BPM). The autocorrelation legitimately peaks at lag `2P`; "DJs feel 140" is a culture fact, not a signal fact.
- **K-S-backbeat drum-n-bass** (kick on 1+3, snare on 2+4 at 174 BPM) has equal-strength autocorrelation at the 1-beat (174 BPM) and 2-beat (87 BPM) periods, because every harmonic of lag 32 lands on a cross-instrument (K-S) alignment while every harmonic of lag 64 lands on a same-instrument (K-K, S-S) alignment. Both are real periodic structure; the algorithm cannot choose without a tempo prior.

Both of these were acknowledged limitations in M8.1's `tempo.rs` module docs. The escape hatch is the [`BpmRange`](../crates/dub-bpm/src/lib.rs) type:

- New `pub struct BpmRange { min: f64, max: f64 }` with validation (must fit inside the algorithm-supported `[MIN_BPM, MAX_BPM]` = 60–200 BPM window) and a `BpmRange::DEFAULT` for the wide range.
- New `analyze_bpm_with_range(samples, sr, channels, range)` shadows the bare `analyze_bpm`; the latter calls the former with `BpmRange::DEFAULT`.
- `BpmEstimator::with_range(sr, range)` shadows `BpmEstimator::new`; same defaulting.
- `TrackerConfig` gains a `bpm_range: BpmRange` field; the canonical `TrackerConfig::at(sr)` builder fills it with `BpmRange::DEFAULT`.
- `dub thru --bpm-range MIN,MAX` plumbs through to `TrackerConfig`. Invalid bounds error out at flag parsing.

The acceptance test in `tempo.rs::narrow_range_constrains_search` pins the behaviour: a 120 BPM pulse train forced into a 60–90 BPM range must report the half-tempo (the only candidate in range), not the full tempo. So narrow ranges can be used to force half- or double-time detection for the genres that need it.

The drum-n-bass synthetic fixture in `tests/genre_octave.rs::drum_n_bass_174_bpm_locks_at_174_not_87` was simplified from a K-S-backbeat pattern to a rolling-style kick-on-every-beat pattern (no snare backbeat) precisely because the K-S backbeat is in the irreducibly-ambiguous class. The original Amen-style fixture would fail any beat-tracker that doesn't carry a tempo prior, including aubio and BTrack — see the long comment in `genre_octave.rs` for the user-visible decision.

### Test surface

7 new fixture-driven tests across two integration files:

- **`tests/genre_octave.rs` (new)** — the M8.1 acceptance gate. 4 tests:
  - `hip_hop_100_bpm_locks_at_100_not_200` — the original regression report. Now passes.
  - `hip_hop_90_bpm_locks_at_90_not_180` — for breadth.
  - `drum_n_bass_174_bpm_locks_at_174_not_87` — rolling-style pattern; ensures the multi-band ODF doesn't introduce a *new* error on bass-heavy fast content.
  - `reggae_one_drop_65_bpm_locks_at_65` — slow + sparse kick energy; ensures the slowest end of the search range still locks.
- **`tests/known_bpm.rs`** — unchanged tests still pass (the M8.1 algorithm is a drop-in replacement). Specifically `click_track_works_at_44100_hz` (128 BPM) and `click_track_174_bpm_dnb` are the streaming-stability regression targets that drove most of the iteration.
- **Synthetic fixtures** (`crates/dub-bpm/src/synthetic.rs`): new `drum_pattern_hip_hop`, `drum_pattern_drum_n_bass`, `drum_pattern_reggae_one_drop` generators with realistic kick (80 Hz), snare (filtered noise centered ~ 1.5 kHz), and hi-hat (HF burst centered ~ 6 kHz) timbres. 4 unit tests in `synthetic::tests` validate that the fixtures themselves carry the expected per-band energy distribution before feeding them to the picker. Decouples "the algorithm fails" from "the test fixture is broken" — a problem we hit during dev when the dnb fixture was structurally ambiguous and the algorithm was getting blamed.

### Algorithmic notes worth keeping

- **Why not biased autocorrelation.** A textbook fix for the half-tempo bias is to use biased ACF (`sum/N`) instead of unbiased (`sum/(N-lag)`). Biased ACF has a natural `(1 - lag/N)` taper that structurally favours smaller lag. Tried it. It re-introduced the hip-hop 2× bug, just from the other direction: hi-hats at lag `P/2` got the structural boost and over-took kicks at lag `P` by a few percent. The taper's slope (`P/N` over the lag range) didn't differentiate "musical octave preference" from "any smaller lag preference" — the former wants the result on real music, the latter is exactly what broke M7.5.

- **Why not a wider tie tolerance instead of windowed-energy.** Tried 5%, 7%, 10%. Each made one regression class pass and another fail. The structural overshoot at `2P` over `P` grows with ODF length (it's a `Σ 1/(N-kP)` artifact), so no fixed tolerance is robust across the 2–60 second ODF lengths the streaming driver sees. Window-sum fixes the underlying invariance problem; the tolerance can then be tight (1%) and only catches the genuine pure-pulse-octave ties.

- **Why `WINDOW = 2` (5-bin sum).** Worst-case fractional period has the bin-split energy in two adjacent bins; `W = 2` captures both with one quiet bin on each side. Adjacent harmonic windows touch but don't overlap as long as the lag spacing exceeds `2W + 1 = 5`; at `MAX_HARMONICS = 4` and `lag_min ≈ 29` (200 BPM at our typical ODF rate), the 4th-harmonic windows around `4·lo` and `4·(lo+1)` are exactly 4 lag apart and 5 wide — touching but not overlapping. (Slowest tempos near `lag_max` only fit 1–2 harmonics anyway.) Wider windows would start cross-contaminating between candidates.

- **Centroid vs. parabolic for sub-bin refinement.** Parabolic vertex height is shoulder-asymmetry-sensitive (which is what we just designed out of the score); parabolic vertex *position* is too, for the same reason. Centroid is the energy-weighted mean position over the same window the score sums; it's symmetric in its handling of bin distribution, and it evaluates analytically to the underlying continuous lag for any bin-split distribution of periodic-peak energy.

### Acceptance

1. `cargo test -p dub-bpm` passes the new genre_octave.rs gate (4 tests) plus all pre-M8.1 tests in `confidence`/`tracker`/`stream`/`known_bpm`/`wav_pipeline`.
2. The previously-failing real-track report (`100 BPM hip-hop detected as 200 BPM`) now locks at the correct octave on the same input.
3. `cargo clippy --workspace --all-targets -- -D warnings` is clean.
4. `cargo test --workspace` is green.
5. `dub thru --bpm-range MIN,MAX` parses and constrains; bare `dub thru` defaults to 60–200 BPM and works without the flag.

### Forward link

M8.1 closes the M8 acceptance loop ("user's stated genre mix locks at the correct octave"). The remaining BPM work is the tracker-level concerns M9+ will surface:

- **Hysteresis tuning on real music.** The `confidence.rs` constants are still M7.5-era defaults. Real-music data (especially slower, sparse genres like dub or reggae one-drop) will exercise `LOCK_CONSECUTIVE`, `LOCK_TOLERANCE_BPM`, and `LOST_LOCKED_CONSECUTIVE` more thoroughly than M8.1's synthetic gates do.
- **Per-genre priors.** The K-S backbeat half-tempo case is the simplest example of "needs a tempo prior to resolve correctly." Future work might surface this as a "feel" toggle in the UI (`140 / 70` cycle button on the tempo readout), or as a learned prior from the user's library, or as a genre-tag-driven preset — UX-level choices that M8.1's range flag deliberately doesn't pre-judge.

The algorithmic floor is set: M8.1 is what the picker looks like; future tuning is data-driven.

---

---

<a id="m9"></a>
## M9 — Live waveform capture (Thru)

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 4–6 days &nbsp;·&nbsp; **Actual:** 1 day

The data layer underneath the M10 waveform UI. Same architectural shape as M8 (off-RT thread consuming a `ringbuf` tap from `ThruSource`, exposing a thread-safe handle to the UI side), but producing a growing append-only sequence of `PeakChunk { min, max, rms }` envelope records instead of BPM events. Shipped as `dub-peaks`, a sibling of `dub-bpm`.

### Why a new crate

Same boundary justification as `dub-bpm`: the engine stays hot (only the audio-thread tap and the off-RT spawn entry-point touch the engine), and the analysis logic lives outside it. The PRD §4.1 layering already pre-committed to this — `dub-peaks` is the concretization of the "live waveform engine" bullet, mirroring `dub-bpm`. No FFI yet; the M10 renderer pulls peaks via direct library import in-process.

### Audio thread side — one mono-downmix, two taps

`ThruSource` was M7-era single-tap (audio out only) and M8 grew an optional mono-downmix BPM tee. M9 didn't need a *new* downmix — the BPM and peaks consumers both want the same mono samples, so the right refactor was to **share the mono-downmix scratch and dispatch to whichever taps are enabled**:

```text
       L,R input (interleaved)
            │
            ▼
  stereo render → routed output  (always)
            │
            ▼
  mono-downmix scratch (computed once if any tap enabled)
            ├─ → bpm_tx.push_slice  (M8, optional)
            └─ → peaks_tx.push_slice (M9, optional)
```

Cost when both taps are enabled: one extra `push_slice` (a memcpy into an SPSC ring). Verified alloc-free by `both_taps_render_is_alloc_free`. The pre-allocated `mono_scratch` is `max_block_frames * 4` bytes (4 KB at the default 1024-frame block) and lives in `ThruSource` regardless of whether any tap is attached — irrelevant memory, dead-simple lifecycle.

The renamed buffer (`bpm_scratch` → `mono_scratch`) and the `with_peaks_tap(peaks_tx)` builder are the only audio-thread surface changes. `with_bpm_tee` keeps its M8 signature for source compatibility; the `max_block_frames` parameter is now `debug_assert!`ed to match the constructor's value but no longer used for sizing.

### `dub-peaks` internals — three files, no surprises

The crate decomposes the same way `dub-bpm` does, so reading the two side-by-side surfaces an obvious shared pattern (analysis layer + reader layer + thread driver):

- **`decimator.rs`** — `Decimator::new(samples_per_chunk)` + `feed(samples, |chunk| ...)`. Pure online aggregator over fixed-size windows. Holds a `(min, max, sumsq, count)` rolling state across `feed` calls so block-boundary alignment is transparent. RMS is `sqrt(sumsq / N)` with `sumsq` accumulated in `f64` (one extra int-add per sample, completely negligible) so 4096-sample mip-2 chunks stay numerically stable. `flush` emits a partial-chunk on shutdown.

- **`buffer.rs`** — `PeakBuffer` (cloneable handle to `Arc<Inner>`) with the standard "lock-free count + RwLock-protected Vec" sharing pattern:
  - `len()` is a single `AtomicUsize` Acquire-load — the renderer's "anything new?" check at 60 fps never touches the lock.
  - `push_chunks(slice)` and `snapshot()` / `extend_chunks(start_idx, dst)` briefly take the RwLock. The decimator pushes one batch per 20 ms drain loop; the renderer takes a read lock once per frame.
  - `extend_chunks` is the renderer fast path: O(new chunks), not O(total). The caller passes its last-seen `start_idx`, and the function appends only the new chunks into the caller's local Vec mirror. Returns the new total length for the next call.

- **`stream.rs`** — `PeakStream::spawn(audio_rx, cfg)` → joinable thread, mirrors `BpmStream`. The analysis loop drains the audio ring into a 4096-sample scratch, runs the `Decimator`, collects emitted chunks into a pre-allocated `chunk_scratch` Vec, and pushes them to the buffer. 20 ms poll cadence when the ring is empty. `Drop` always shuts down and joins; `shutdown()` is the explicit form for surfacing join panics.

### Bytes-on-the-wire format

`PeakChunk` is `#[repr(C)]`, 12 bytes (3 × `f32`). Deliberately exposed as the M10 consumer contract — a `&[PeakChunk]` from `PeakBuffer::extend_chunks` can go directly into a Metal vertex buffer with no further packing. The crate-level module docs spell out the contract: cache `start_idx` per stream, call `extend_chunks` each frame, treat the slice as wire-format.

`min`/`max`/`rms` rather than `peak`/`rms` or `peak` alone is the standard envelope-display tuple used by Audacity, Mixxx, and Serato. Properly mastered drums are asymmetric (a kick's positive peak meaningfully differs from its negative one), and the RMS gives perceived-loudness shading for free without a second pass.

### Engine integration — three attach methods, one ThruSource

`EngineHandle` gained two new convenience methods alongside the existing M8 wrapper:

- `attach_thru_source_with_peaks_tracking(idx, rx, thru_cfg, peaks_cfg)` — M9 only, no BPM.
- `attach_thru_source_with_telemetry(idx, rx, thru_cfg, tracker_cfg, peaks_cfg)` — both M8 and M9. **Strictly cheaper** than calling the BPM- and peaks-only attach methods in sequence: there's only one `ThruSource` with both taps, the mono-downmix runs once, and both analysis threads spawn from the same call.
- `attach_thru_source_with_bpm_tracking` (M8) — unchanged.

Plus the bare `attach_thru_source` (M7), giving 4 attach variants total. The CLI picks the right one based on the `(--no-bpm-track?, --no-peaks-track?)` flag combination — see below.

Each method validates the new SR before attaching; M8 and M9 ringbuf capacities both default to 1 s of mono at the engine SR (the `BPM_TEE_RING_CAPACITY_SECS` and `PEAKS_TAP_RING_CAPACITY_SECS` constants).

Error surface: three new error enums (`ThruAttachWithPeaksError`, `ThruAttachWithTelemetryError`, plus the existing `ThruAttachWithBpmError`), each carrying `Thru(ThruAttachError)`, sample-rate mismatch, and the relevant subsystem config error. Separating them keeps each call's documented failure set focused; the `telemetry` enum's two `*SampleRateMismatch` variants name which subsystem mismatched so the user knows which `_sr` to fix.

### CLI — peaks default on, opt out + debug dump

`dub thru` gained two flags:

- `--no-peaks-track` — analogous to the M8 `--no-bpm-track`. Defaults off; every attached Thru deck spawns a `PeakStream` decimator. The periodic stats line gains a `peaks=[A=N B=M]` field with the per-deck captured-chunk count, so the operator can sanity-check capture is alive without M10 UI.

- `--dump-peaks PATH` — on shutdown, write every captured chunk to `PATH` as CSV (`deck,chunk_idx,min,max,rms`). One row per chunk, header included. Useful for `gnuplot`/`awk`/`matplotlib` to validate the envelope shape before the Metal renderer exists, and for CI-style smoke tests that check "did capture produce reasonable peaks for this fixture."

`--dump-peaks` + `--no-peaks-track` is rejected at parse time (the user would otherwise get a confusing empty-file). `--no-bpm-track` + `--no-peaks-track` together cleanly falls back to the bare `attach_thru_source` — no telemetry threads at all, M7's behaviour exactly.

The attach dispatch is a small `match (no_bpm_track, no_peaks_track)` per deck that picks the right `EngineHandle` method. The four-arm match is the cleanest expression of the four feature combinations; trying to compose it into one builder API was worse than the explicit handful of attach methods.

### Test surface

The `dub-peaks` crate ships with **41 tests** (38 unit + 3 integration). The engine and CLI gained another **9 tests** between them. Coverage:

- **Decimator (15 tests)** — chunk-boundary correctness (partial tails carry over across `feed` calls, block size doesn't change output, ramp produces strictly-increasing maxes), value correctness (RMS of constant, alternating ±1, silence-is-zero, min/max match extremes), reset/flush semantics, large-input invariants. The `block_size_does_not_change_output` test is the load-bearing one: feeding the same 256-sample buffer in 1-sample, 7-sample, and whole-buffer increments must produce *byte-identical* chunk sequences. If anything in the decimator depends on block alignment, this test catches it.

- **Buffer (10 tests)** — empty buffer is empty, push increments len, snapshot captures all pushed chunks in order, `extend_chunks` appends only new (the renderer fast path) including the noop cases (caught-up start, start past len), cloned buffers share storage (Arc semantics), and a **concurrent producer/consumer stress test** that spawns a writer pushing 1000 chunks while the test thread polls `extend_chunks` — final mirror must equal full output and chunks must remain in producer order. This pins the lock-free `len()` + briefly-locked Vec pattern as correct under contention.

- **Stream (10 tests)** — config validation (zero SR / zero chunk size rejected), end-to-end (samples push → chunks in buffer with correct min/max/rms), incremental reader streams chunks, lifecycle (dropping producer terminates thread, explicit shutdown joins promptly within 500 ms), silence pushes zero chunks through, **buffer handle outlives explicit shutdown** (Arc semantics — the renderer can keep a reference past stream teardown).

- **End-to-end integration (3 tests, `tests/end_to_end.rs`)** — full spawn → push → drain → assert against closed-form expectations: constant signal yields uniform chunks, burst pattern alternates loud/silent chunks at the expected boundaries, and the incremental extend mirrors the full stream byte-identically across both an in-flight stream and a post-completion snapshot.

- **ThruSource peaks tap (8 new tests)** — fresh source has no peaks tap, `with_peaks_tap` attaches, peaks tap receives mono downmix (`L=0.4, R=-0.2 → 0.1`), unaffected by gain (envelope reflects pre-gain input), silently drops on full ring, alloc-free render with peaks tap attached, underrun pushes zeros, and the crucial `bpm_and_peaks_tap_both_receive_same_mono_downmix` (both taps see identical samples after one downmix pass) plus `both_taps_render_is_alloc_free` (combining both taps is still RT-safe).

- **EngineHandle attach (8 new tests)** — spawn-stream variants for peaks-only and combined-telemetry, SR mismatch / invalid chunk size / invalid deck idx rejection for both peaks-only and combined-telemetry attach. The capstone is `handle_attach_thru_with_peaks_captures_envelope_e2e`: feeds 512 stereo frames of constant 0.5 through the actual engine via `pump_one_block`, waits for the decimator to drain, and asserts the first 8 captured chunks are all `min == max == rms == 0.5` to 1e-5 tolerance.

- **CLI flag tests (4 new)** — peaks-track defaults on, `--no-peaks-track` opts out, `--dump-peaks PATH` captures the path, `--dump-peaks` + `--no-peaks-track` is rejected at parse time. Plus a `dump_peaks_csv_writes_header_and_rows` unit test that injects chunks into a `PeakStream` directly (bypassing the audio thread) and verifies the CSV layout byte-for-byte against expected lines.

### Sequencing notes worth keeping

- **Engine drains commands then renders, in that order.** The first `pump_one_block` after `attach_thru_source_with_peaks_tracking` will process the attach command at the top of `render_routed` and *then* immediately render — pulling whatever happened to be in the input ring at that exact moment. The e2e test sets this up explicitly: push input frames *before* the first pump so the first captured chunk reflects the operator's input, not a block of underrun zeros from a momentary empty ring. Real-world this happens naturally (the operator drops a needle before pumping audio), but tests need the deterministic ordering.

- **`PEAKS_TAP_RING_CAPACITY_SECS = 1`.** Mirrors `BPM_TEE_RING_CAPACITY_SECS`. The decimator polls every 20 ms; one second of slack absorbs any scheduling jitter on a healthy system. 192 KB per deck — meaningless on M-series hardware, far below the threshold where ring capacity becomes a memory concern.

- **Buffer initial capacity defaults to 10 minutes.** `DEFAULT_BUFFER_CAPACITY_SECS = 600` × 48 kHz / 64 spc ≈ 450k chunks × 12 bytes ≈ 5.4 MB. Common-case mix-track length doesn't hit a single realloc; longer records (90 min vinyl side) reallocate once or twice off-RT. The audio thread never reallocates.

### Forward link — what M10 needs from this

The M10 waveform UI pulls peaks via `PeakStream::buffer()` (returns an `Arc`-clone of `PeakBuffer`), caches `start_idx`, and calls `extend_chunks` each render frame. The renderer's local Vec is the source of truth for what's on screen; the crate intentionally does NOT maintain mip pyramids — the renderer knows how many pixels it has and can downsample further on demand. Overview rendering (90 min on a 4K screen ≈ 67k samples/pixel) needs a second pass that averages every ~1000 chunks into one screen pixel; scratch rendering (5 s on 4K ≈ 62 samples/pixel) renders one chunk per pixel directly.

Nothing in M9 commits to a mip schema. M10 will likely add a `MipLevel` enum or `with_decimation` config — the data layer is small and easy to expand.

### Acceptance

1. `cargo test --workspace` is green (53 new tests across `dub-peaks`, `dub-engine`, `dub-cli`; all pre-existing tests still pass).
2. `cargo clippy --workspace --all-targets -- -D warnings` is clean.
3. `dub thru` defaults to peaks-tracking on; stats line shows captured chunk counts per deck; `--dump-peaks PATH` writes a valid CSV on shutdown.
4. The combined-telemetry attach is strictly cheaper than two separate attaches: one `ThruSource`, one mono-downmix, two taps, two analysis threads. Verified by the `both_taps_render_is_alloc_free` and `bpm_and_peaks_tap_both_receive_same_mono_downmix` ThruSource tests.

---

<a id="m05"></a>
## M0.5 — Apple shell + smoke screen

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 3 days (delivered)

### What it is

The Apple-side counterpart of M0. M0 shipped the Rust workspace, CI, and RT-audit harness; M0.5 closes the cross-language toolchain loop so a developer with `xcodegen` + Xcode 15 on PATH can go from a clean checkout to a launched `Dub.app` window in one script invocation. The window itself is a deliberate smoke screen — `"Dub engine OK · v0.0.1"` pulled live from the Rust `dub-engine` crate via UniFFI — because the *toolchain* is what we're proving here, not any audio feature.

Why this slot in the schedule. The original M0 PRD had M0.5 as a placeholder because generating an Xcode project purely from text was deemed brittle. The pivot here is XcodeGen: the `.xcodeproj` is regenerated from a YAML manifest at every bootstrap, so it stays diffable in PRs and reproducible in CI — same property we get from `Cargo.toml`.

### Toolchain plumbing

| Layer | What lives where |
|---|---|
| Rust core | `crates/dub-engine` (unchanged) |
| FFI surface | `crates/dub-ffi` upgraded to UniFFI 0.28 proc-macros + `crate-type = ["lib", "staticlib", "cdylib"]` + a `uniffi-bindgen` binary (`required-features = ["uniffi-cli"]`) for library-mode binding generation |
| Build script | `scripts/build-xcframework.sh` — `cargo build --target aarch64-apple-darwin --profile release` + `--target x86_64-apple-darwin`, `lipo -create` for the fat `libdub_ffi.a`, `cargo run --bin uniffi-bindgen --features uniffi-cli -- generate --library …` for the Swift bindings, `xcodebuild -create-xcframework` to bundle the universal `.a` + the C header into `apple/DubCore.xcframework/` |
| Project gen | `apple/project.yml` → `apple/Dub.xcodeproj` via `xcodegen generate` |
| One-shot | `scripts/bootstrap.sh` runs `build-xcframework.sh` then `xcodegen generate`, guarded by `command -v` checks for `xcodebuild`, `xcodegen`, `cargo` |
| Swift package | `apple/DubShared/` — `Package.swift` declares a `binaryTarget` pointing at `../DubCore.xcframework` and a `DubCore` library target containing the generated bindings |
| Apple app | `apple/Dub/` — `DubAppDelegate.swift` (`@main` `NSApplicationDelegate`), `MainWindowController.swift` (`NSWindow` + `NSHostingController`), `SmokeScreenView.swift` (SwiftUI `Text(greeting())` + version line), `Info.plist`/`Dub.entitlements` |

### Why UniFFI proc-macros, not UDL

UniFFI offers two surfaces: a `.udl` file (Mozilla's original IDL-style declaration) or `#[uniffi::export]` proc-macros directly on Rust items. We chose proc-macros for three reasons:

1. **Single source of truth.** With UDL there's a constant risk of the `.udl` file drifting from `lib.rs`. With proc-macros, the Rust signature *is* the exposed surface.
2. **No `build.rs` required.** Library-mode bindgen reads metadata embedded in the compiled `cdylib`, so the build pipeline is just `cargo build` → `uniffi-bindgen generate --library`. The `build.rs` UDL parsing step is gone.
3. **Cleaner growth path.** M10-A adds the `DubEngine` interface as more `#[uniffi::export]` items + a `#[derive(uniffi::Object)]` struct, with no schema-file edits.

The tradeoff is that some advanced UDL features (custom external types, callback interfaces with non-trivial ABI) are slightly less ergonomic in proc-macro mode. None of them apply to the M0.5 / M10 / M10.1 surface.

### Why hybrid AppKit + SwiftUI

The `@main` entry point is an `NSApplicationDelegate` (`DubAppDelegate`) holding a `MainWindowController`. The window's `contentViewController` is an `NSHostingController<SmokeScreenView>` — SwiftUI for the *contents* of the window, AppKit for the lifecycle and the window itself. This is the same split Apple recommends for apps that have both real-time content (M10's Metal waveform, scratch-pad gestures) and ordinary forms (settings, library browser). AppKit owns the audio HUD path; SwiftUI owns everything else.

The cheap-to-write `SmokeScreenView` is the M0.5 deliverable. It will become a debug overlay in M10, when the window's primary content becomes the `WaveformView` + the input-device picker.

### Why local-only signing

The user does not have an Apple Developer account, and v1 doesn't need one to run locally during development. The XcodeGen manifest sets `CODE_SIGN_STYLE: Automatic` + `CODE_SIGN_IDENTITY: "-"`, which is Xcode's "Sign to Run Locally" path. Sandbox stays *off* in `Dub.entitlements` for two reasons:

1. M10's CoreAudio device picker needs to talk to arbitrary input devices without entitlement gymnastics.
2. Sandbox + hardened runtime are a *distribution* concern, not a *development* concern. Re-enabling them lands with the post-M10.2 distribution milestone, alongside a `scripts/codesign.sh` and notarisation.

### File-level changes

* [`Cargo.toml`](../Cargo.toml) — `uniffi = "0.28"` workspace-dep.
* [`crates/dub-ffi/Cargo.toml`](../crates/dub-ffi/Cargo.toml) — `crate-type = ["lib", "staticlib", "cdylib"]`, `[[bin]] uniffi-bindgen`, `[features] uniffi-cli`, `uniffi = { workspace = true }`.
* [`crates/dub-ffi/src/lib.rs`](../crates/dub-ffi/src/lib.rs) — `uniffi::setup_scaffolding!()` + `#[uniffi::export]` on `greeting()` and `engine_version()`. Both functions now return `String` instead of `&'static str` (UniFFI's String marshalling requires an owned value). Existing Rust tests updated.
* [`crates/dub-ffi/src/bin/uniffi-bindgen.rs`](../crates/dub-ffi/src/bin/uniffi-bindgen.rs) — three-line wrapper around `uniffi::uniffi_bindgen_main()`.
* [`apple/project.yml`](../apple/project.yml) — XcodeGen manifest (single `Dub` macOS target, `DubShared` swift-package dependency, sandbox-off entitlements).
* [`apple/Dub/`](../apple/Dub/) — `DubAppDelegate.swift`, `MainWindowController.swift`, `SmokeScreenView.swift`, `Info.plist`, `Dub.entitlements`.
* [`apple/DubShared/Package.swift`](../apple/DubShared/Package.swift) — Swift Package declaring `DubCoreFFI` binary target + `DubCore` Swift target.
* [`apple/README.md`](../apple/README.md) — rewritten from placeholder to the M0.5-shipped layout + bootstrap instructions.
* [`scripts/build-xcframework.sh`](../scripts/build-xcframework.sh), [`scripts/bootstrap.sh`](../scripts/bootstrap.sh) — new, both executable.
* [`.gitignore`](../.gitignore) — `apple/*.xcodeproj/`, `apple/DubCore.xcframework/`, `apple/DubShared/Sources/DubCore/Generated/`, `apple/DubShared/.build/`, `apple/DubShared/.swiftpm/`, `apple/DubShared/Package.resolved`.

### Acceptance

1. `cargo build -p dub-ffi` succeeds. Adds `~1 min` to first-time compile due to UniFFI scaffolding crates; no impact on incremental builds.
2. `cargo test -p dub-ffi` passes — `greeting()`, `engine_version()`, `FFI_VERSION` invariants all green.
3. `cargo clippy -p dub-ffi --all-targets --features uniffi-cli -- -D warnings` is clean.
4. `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` remain green — no regressions in pre-existing crates.
5. On a Mac with `xcodegen` + Xcode 15: `./scripts/bootstrap.sh && open apple/Dub.xcodeproj` then ⌘R produces a window displaying `"Dub engine OK · v0.0.1"`.

### What it does not ship

* No audio I/O across FFI. `start_thru`, `peaks_extend`, device-picker lands with **M10-A**.
* No `DubEngine` interface in the UDL surface — just two free functions. That's deliberate; M10-A introduces the engine handle.
* No code signing beyond local "Sign to Run Locally". Notarisation is a separate post-M10.2 milestone.
* No CI build target for the Apple side. The `make apple` target proposed in the plan is deferred until a macOS CI runner is wired (currently CI is Linux-only).

---

<a id="m95"></a>
## M9.5 — `dub-spectral` extraction + 8-band peak capture

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 4 days (delivered)

### What it is

Two coordinated changes shipped as one milestone:

* **M9.5a** — Pure refactor. The FFT + window + log-band + Klapuri-style magnitude-compression pipeline that lived inside `crates/dub-bpm/src/onset.rs` moves out to a new `crates/dub-spectral/` crate. `OnsetDetector` becomes a thin shell over `dub_spectral::SpectralFrameStream`. No behaviour change — the M8.1 hip-hop / dnb / reggae octave fixtures all pass byte-identical ODF values.
* **M9.5b** — Data-layer extension. A new `dub_peaks::BandDecimator` runs alongside the existing broadband `Decimator` on the same mono-downmix tap; it emits a new `BandPeakChunk { rms_per_band: [f32; 8] }` once per FFT hop (~94 Hz at 48 kHz). The `PeakBuffer` gains parallel band storage that's opt-in at construction; `PeakStream` config gains `bands_enabled` (default `true`). `dub thru` gains `--no-band-peaks` and `--dump-band-peaks PATH`. This is the data layer M10.1 needs for multi-colour rendering — by landing it ahead of the renderer, M10 can be a Rust-side-already-stable affair.

### Why extract first

Two crates need the same FFT pipeline (BPM onset detection, M10.1 colour rendering). Three more will need it before v1 (key detection, transient FX, M21 fingerprint correlation). Owning the pipeline once means a fix to `λ` or the band layout automatically applies everywhere — versus discovering halfway through M10.1 that a colour-side magnitude compression decision contradicted a BPM-side one. The plan flagged the extraction as deferred-until-M11, then a re-prioritisation moved it here: the cost of dragging the FFT through M10 *unshared* (duplicated implementation, duplicated tests, divergent magnitude curves) outweighed the cost of one clean refactor up front.

### Public API of `dub-spectral`

```rust
pub const FRAME_SIZE: usize = 1024;
pub const HOP_SIZE: usize = 512;
pub const NUM_BANDS: usize = 8;
pub const BAND_MIN_HZ: f32 = 30.0;
pub const BAND_MAX_HZ: f32 = 16_000.0;
pub const LAMBDA: f32 = 1000.0;

pub struct SpectralFrameStream { /* … */ }
impl SpectralFrameStream {
    pub fn new(sample_rate: u32) -> Self;
    pub fn frame_size(&self) -> usize;
    pub fn hop_size(&self) -> usize;
    pub fn half_spectrum_size(&self) -> usize;
    pub fn bands(&self) -> &[(usize, usize); NUM_BANDS];
    pub fn process<F: FnMut(&[f32], &[(usize, usize); NUM_BANDS])>(
        &mut self, block: &[f32], on_frame: F,
    );
    pub fn reset(&mut self);
}
pub fn compute_band_bins(sample_rate: u32, n_bins: usize) -> [(usize, usize); NUM_BANDS];
```

`SpectralFrameStream::process` is alloc-free after construction — verified by `process_is_alloc_free_after_construction` (input-buffer capacity stable across 16 iterations).

### Data layer in `dub-peaks` (M9.5b)

```rust
pub const NUM_BANDS: usize = dub_spectral::NUM_BANDS;            // = 8
pub const BAND_SAMPLES_PER_CHUNK: usize = dub_spectral::HOP_SIZE; // = 512

#[repr(C)]
pub struct BandPeakChunk {
    pub rms_per_band: [f32; NUM_BANDS],   // 8 × f32 = 32 bytes
}

pub struct BandDecimator { /* wraps SpectralFrameStream */ }
impl BandDecimator {
    pub fn new(sample_rate: u32) -> Self;
    pub fn samples_per_chunk(&self) -> usize;
    pub fn feed<F: FnMut(BandPeakChunk)>(&mut self, samples: &[f32], emit: F);
    pub fn reset(&mut self);
}
```

`BandPeakChunk::rms_per_band[k]` is `sqrt(mean(compressed[b]² for b in bands[k]))` — RMS over `dub-spectral`'s per-bin **compressed** magnitudes, not raw FFT magnitudes. The compressed form is already perceptual (μ-law-ish via `ln(1 + λ |X|)`), so RMS over it yields a stable colour-friendly loudness metric. Documented as such in the struct doc so M10.1 doesn't try to reinterpret it as physical RMS.

### Audio-thread cost is zero

The M9 `ThruSource` mono-downmix already produces one shared mono stream consumed by the BPM tap and the peaks tap. **M9.5b adds no new tap.** The same SPSC ring feeds both `Decimator` (broadband, 64-sample cadence) *and* `BandDecimator` (band, 512-sample cadence) inside the same off-RT worker thread — verified by extending the existing `ThruSource` alloc-free tests + the new `bands_on_keeps_broadband_capture_intact` in `dub-peaks` (broadband chunks remain pixel-identical whether bands are on or off).

### Buffer / stream wiring

* `PeakBuffer` is a sum of (always-on broadband Vec + optional band Vec). `with_capacity` is the broadband-only constructor (back-compat for non-band users); `with_capacity_with_bands` is the M9.5b path.
* `band_len()` is a separate `AtomicUsize`, so the renderer's "anything new in the colour channel?" check is lock-free and independent of the broadband side.
* `extend_band_chunks(start_idx, &mut Vec<BandPeakChunk>) -> usize` is the M10.1 fast path; same semantics as the M9 `extend_chunks` for broadband.
* `PeakStreamConfig::bands_enabled: bool` (default `true`). `PeakStream::samples_per_band_chunk() -> Option<usize>` exposes the cadence so renderers can map `peak_idx → band_idx` via integer division.

### CLI surface

* `dub thru --no-band-peaks` opts out of band capture (~ no measurable difference on M1 Air; band data costs ~ 500 µs CPU per second of audio per deck off-RT).
* `dub thru --dump-band-peaks PATH` writes per-band envelopes to a CSV at shutdown. Header: `deck,chunk_idx,b0,b1,...,b7`. Conflicts with `--no-peaks-track` and `--no-band-peaks` are caught at parse time.

### Tests

`cargo test --workspace`: 587 passing. New coverage:

| Crate | Tests | Notes |
|---|---|---|
| `dub-spectral` | 10 unit | Band layout at 44.1k/48k/96k; alloc-free `process`; reset invariants; block-size invariance one-shot vs. streamed |
| `dub-peaks/band_decimator` | 8 unit | Cadence, silence, pure-tone-excites-expected-band (60 Hz / 10 kHz), block-size invariance, reset |
| `dub-peaks/buffer` | 4 unit | Band storage off vs. on, independent push semantics |
| `dub-peaks/stream` | 3 unit | Default ON, ON produces band chunks, ON keeps broadband intact |
| `dub-cli/thru` | 5 unit | Default ON, `--no-band-peaks`, `--dump-band-peaks` path capture + two conflict guards, dump CSV header + row contents |
| `dub-bpm` | -2 unit | `band_bins_*` tests migrated to `dub-spectral` — net 2 fewer in this crate. M8.1 fixture suite (`genre_octave`) unchanged. |

### Acceptance

1. `cargo test --workspace` passes — every M8.1 genre fixture (reggae 65, hip-hop 90 / 100, dnb 174) holds byte-equivalent ODF values.
2. `cargo clippy --workspace --all-targets -- -D warnings` clean.
3. `dub thru --dump-band-peaks /tmp/bands.csv` writes a valid CSV; opening it shows expected band activity (low bands prominent on kick, high bands on hi-hat).
4. Combined-telemetry attach is alloc-free end-to-end — verified by the existing `bpm_and_peaks_tap_both_receive_same_mono_downmix` + the `process_is_alloc_free_after_construction` invariants on the underlying `SpectralFrameStream`.

### What it does not ship

* No FFI surface. `BandPeakChunk` lives in Rust-land until M10.1 wires `band_peaks_extend` through UniFFI.
* No renderer. The data is here; M10.1 implements the multi-colour Metal shader.
* No constant-Q bass split (9-band variant). Deferred to M10.2.

---

<a id="m10a"></a>
## M10-A — `dub-ffi` `DubEngine` UniFFI surface

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 2 days (delivered)

### What it is

`crates/dub-ffi` grew from the M0.5 "two free functions" surface (`greeting`, `engine_version`) into a real engine handle the Apple shell can hold for the lifetime of a Thru session. Single UniFFI object, single error type, eight methods.

```rust
#[derive(uniffi::Object)]
pub struct DubEngine { /* Mutex<EngineState> */ }

#[uniffi::export]
impl DubEngine {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self>;

    pub fn list_input_devices(&self) -> Vec<String>;
    pub fn start_thru(&self, device_name: String, channels: Vec<u32>)
        -> Result<(), EngineError>;
    pub fn stop_thru(&self);

    pub fn peaks_len(&self, deck_idx: u64) -> u64;
    pub fn peaks_chunk_duration_secs(&self, deck_idx: u64) -> f64;
    pub fn peaks_extend(&self, deck_idx: u64, start_idx: u64) -> Vec<u8>;

    pub fn band_peaks_len(&self, deck_idx: u64) -> u64;
    pub fn band_peaks_chunk_duration_secs(&self, deck_idx: u64) -> f64;
    pub fn band_peaks_extend(&self, deck_idx: u64, start_idx: u64) -> Vec<u8>;
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum EngineError {
    DeviceNotFound(String),
    InvalidChannels(Vec<u32>),
    AudioStartFailed(String),
    AlreadyRunning,
    NotRunning,
    InvalidDeckIndex(u64),
}
```

### Design choices

* **Proc-macro UniFFI, no UDL.** All `#[uniffi::export]` lives next to the Rust code. The `uniffi-bindgen` workspace binary reads metadata directly from `libdub_ffi.dylib` in library mode — no separate UDL source to keep in sync. `setup_scaffolding!()` emits the C ABI at crate boundary.
* **`#[uniffi(flat_error)]` for `EngineError`.** Swift gets a plain enum with `Display`-derived messages; data-bearing variants embed device names / channel lists into the string. Cleaner Swift ergonomics than a discriminated union for the three error sites that ever inspect specifics.
* **Bytes-not-objects for the hot path.** `peaks_extend` / `band_peaks_extend` return `Vec<u8>` (UniFFI `bytes`). Swift sees `Data`; the renderer reinterprets the bytes as `[PeakChunk]` via `withUnsafeBytes` — zero per-frame allocation and no object-graph traversal across the FFI. Little-endian on both ARM64 and x86_64 macOS keeps the cast safe.
* **`Mutex<EngineState>` not `Arc<Mutex<...>>` at FFI boundary.** UniFFI wraps `DubEngine` in `Arc<DubEngine>` automatically for `#[derive(uniffi::Object)]` types; the internal mutex serialises mutating calls only.
* **Audio-thread non-affecting.** Every method ultimately reads `PeakBuffer` atomics or runs once at `start_thru` to open CoreAudio devices. No method is called from the render thread — Swift never reaches into the IO proc. PRD §10 cross-cutting and `.cursor/rules/ffi.mdc` are satisfied by construction.
* **`Drop` ordering matters.** `RunningState` lists `peaks` first, then `handle`, then `output`, then `input`. The drop sequence stops the decimator thread → flushes the engine command queue → stops the output AU (which reclaims the engine) → stops the input AU (last, so the SPSC ring has no producer-after-consumer race). `stop_thru` is idempotent.

### Tooling deltas

* **`scripts/build-xcframework.sh`**: the embedded modulemap now declares `module dub_ffiFFI { ... }` (was `DubCoreFFI`). The generated bindings include `#if canImport(dub_ffiFFI) import dub_ffiFFI #endif`; matching the C module to the generator's expected name lets `swift build` (and the Apple shell) resolve the C symbols without a post-generation patch.
* **`apple/project.yml`**: adds explicit `CoreAudio.framework`, `AudioToolbox.framework`, `AudioUnit.framework`, `CoreFoundation.framework`, `Metal.framework`, `MetalKit.framework` SDK dependencies. Cargo emits the `cargo:rustc-link-lib=framework=...` directives for `coreaudio-rs`, but those propagate only when Cargo drives the link; Xcode drives the link for the app, so the frameworks have to be surfaced explicitly. Also pins `PRODUCT_NAME` / `PRODUCT_MODULE_NAME` / `ALWAYS_SEARCH_USER_PATHS` since `settingPresets: none` (the M0.5 choice for explicit configuration) drops Xcode's auto-derived defaults.

### Tests

`cargo test -p dub-ffi`: 9 unit tests covering: `greeting`, `engine_version`, FFI version tripwire, fresh-engine peaks defaults, `stop_thru` idempotency, channels validation (empty / wrong-arity / zero-index), and round-trip serialisation of broadband + band chunks. UniFFI binding generation verified end-to-end by `scripts/build-xcframework.sh` (which produces the universal `DubCore.xcframework` + Swift bindings).

### Acceptance

1. `cargo test --workspace` passes.
2. `cargo clippy --workspace --all-targets -- -D warnings` passes.
3. `./scripts/build-xcframework.sh` produces `apple/DubCore.xcframework/` and Swift bindings under `apple/DubShared/Sources/DubCore/Generated/`. `swift build` from `apple/DubShared/` typechecks.
4. The generated Swift surface exposes `DubEngine`, `EngineError`, `greeting()`, `engineVersion()` — verified by inspecting `dub_ffi.swift`.

### What it does not ship

* No render-thread state exposed (`xrun_count`, `process_time_ns`, BPM). That's all consumable today through the CLI; UniFFI surface only grows when the macOS UI actually needs it. Adding the BPM telemetry is one extra method when M10.2 wires saturation.
* No background queue or async I/O at the FFI boundary. `start_thru` blocks until CoreAudio comes up — typical 50-200 ms on first open. Swift wraps the call in a `Task { ... }` if it cares about UI responsiveness; nothing in the FFI surface requires it.

---

<a id="m10b"></a>
## M10-B — Metal renderer + first live broadband waveform

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 3 days (delivered)

### What it is

The Apple shell now shows a live, scrolling broadband waveform of input audio. Pick an input device, pick channels, hit Start — the M10-A engine fires up, the M9 peak buffer accumulates `PeakChunk`s, and a Metal-backed `MTKView` renders the most recent ~5 seconds of audio at 60 fps.

### File layout

* `apple/Dub/Waveform/Shaders.metal` — vertex + fragment shaders. Vertex stage emits 4 vertices per instance (a triangle strip per `PeakChunk`) sized from `min`/`max`; fragment stage outputs an RMS-modulated greyscale for M10-B. M10.1 swaps in the multi-colour fragment shader against the same vertex pipeline.
* `apple/Dub/Waveform/WaveformRenderer.swift` (~280 lines) — `@MainActor` Metal renderer. Owns `MTLDevice`, `MTLCommandQueue`, render pipeline state, **two** triple-buffered uniform `MTLBuffer`s (one per inflight frame, bounded by `DispatchSemaphore(value: 3)`), and **one** ring-buffer `MTLBuffer` for chunks (`chunkCapacity = 2^17 ≈ 175 s` at 48 kHz / 64 samples).
* `apple/Dub/Waveform/WaveformView.swift` — `NSViewRepresentable` wrapping `MTKView`. The view's `Coordinator` is the `MTKViewDelegate`; both `drawableSizeWillChange(_:)` and `draw(in:)` hop to `@MainActor` via `MainActor.assumeIsolated` since the Metal renderer is main-actor isolated.
* `apple/Dub/MainView.swift` — top-level SwiftUI view. Hosts the device `Picker`, channels `TextField`, Start/Stop button, the waveform, and a one-line debug overlay showing `greeting() · v<engine_version>` (the M0.5 smoke text, now demoted to a debug line). Owns a `WaveformAppModel: ObservableObject` that wraps the shared `DubEngine` and exposes `availableDevices`, `selectedDevice`, `isRunning`, `lastError`. `EngineError` is mapped to user-readable strings in `describe(_:)`.

### Rendering pipeline

* **One quad per chunk, instanced.** `drawPrimitives(.triangleStrip, vertexStart: 0, vertexCount: 4, instanceCount: chunksVisible)`. The vertex shader reads `chunks[chunkOffset + instance_id]` from the ring buffer and emits a bar from `(x − dx, min*yScale)` to `(x + dx, max*yScale)`. Vertex-ID bit layout: bit 1 → right edge, bit 0 → top edge. `yScale = 0.95` keeps the bars off the viewport edges.
* **Bar amplitude.** Empty chunks (no samples) clamp to a ±1e-4 hairline so the leading edge renders as a thin centred line instead of a hidden zero-thickness triangle. Doesn't affect any chunk with real audio.
* **Window size.** `chunksVisible = pixel_width × 4` — about 4 ms per pixel at 48 kHz / 64-sample chunks, ~5.4 seconds on a 1280-pixel-wide window. Configurable via `chunksPerPixel`.
* **Ring buffer ingest.** Each `draw(in:)` calls `engine.peaksLen` → if it grew, `engine.peaksExtend(..)` with the cached cursor. The returned `Data` is `memcpy`'d into the GPU ring starting at `(startIdx % chunkCapacity) * 12` bytes, with one wrap-around copy when the write crosses the ring boundary. `cappedNew` truncates catch-up to one ring's worth so a long UI stall (e.g. moving the window) doesn't memcpy gigabytes when the renderer resumes.
* **Frame pacing.** `MTKView.isPaused = false`, `enableSetNeedsDisplay = false`, `preferredFramesPerSecond = 60`. The semaphore caps inflight CPU work at 3 frames ahead of the GPU; reset of a wedged GPU is fatal (we accept the convention).
* **Storage modes.** Both ring buffer and uniform buffers use `.storageModeShared`. On Apple Silicon's unified memory, this is zero-copy (CPU and GPU share pages). On Intel macs, the small bandwidth hit (~5 MB max per deck) is irrelevant compared to the round-trip cost of `.storageModePrivate` + blits.

### View model

* `WaveformAppModel: ObservableObject` owns a single `DubEngine`. Construction calls `refreshDevices()`; `deinit` calls `engine.stopThru()` defensively (UniFFI's `Drop` would do it too, but the explicit teardown is deterministic across SwiftUI lifecycles).
* `start()` parses the comma-separated channel field, validates two 1-based values, and dispatches `engine.startThru(...)`. `EngineError` lifts into `lastError`.
* `stop()` calls `engine.stopThru()` and clears `isRunning`. Idempotent (matches the engine's idempotent `stop_thru`).

### Bootstrap

`./scripts/bootstrap.sh` now produces:

```
apple/DubCore.xcframework/        # Universal aarch64 + x86_64 static lib + headers
apple/DubShared/Sources/DubCore/Generated/   # dub_ffi.swift, headers, modulemap
apple/Dub.xcodeproj/              # Generated from project.yml by XcodeGen
```

`xcodebuild build -project apple/Dub.xcodeproj -scheme Dub` produces a runnable `Dub.app` that links CoreAudio, AudioToolbox, AudioUnit, CoreFoundation, Metal, and MetalKit explicitly.

### Acceptance

1. `cargo test --workspace` passes.
2. `cargo clippy --workspace --all-targets -- -D warnings` clean.
3. `./scripts/bootstrap.sh && xcodebuild -project apple/Dub.xcodeproj -scheme Dub -configuration Debug build` succeeds — `Dub.app` lands in DerivedData with `CoreAudio`, `AudioToolbox`, `AudioUnit`, `Metal`, `MetalKit` linked.
4. Launching the app, picking an input, hitting Start: the waveform scrolls in real time at the device's natural rate; quitting the app cleanly tears down the audio threads (UniFFI's auto-`Drop` on the engine handle runs the documented `Drop` order on `RunningState`).

### Threading + RT discipline

* Audio thread → only Rust; no Swift call, no Metal call. (`.cursor/rules/audio-rt.mdc` and `ffi.mdc` invariants both satisfied.)
* Render thread → main actor; calls `engine.peaksLen` / `peaks_extend` which read `AtomicUsize` + take an `RwLock::read` on the peak buffer. The lock is reader-priority and dropped before the encoder records any GPU work.
* Engine teardown on `stop_thru` runs synchronously from the UI's perspective — drops the peak streams, the engine handle, then the output AU, then the input AU.

### What it does not ship

* **Monochrome only.** RMS modulates a greyscale brightness so transients are visible, but the multi-band data captured in M9.5b isn't read yet. M10.1 wires `band_peaks_extend` and swaps in the colour shader against the same vertex pipeline (zero changes to the renderer's vertex stage or buffer layout).
* **Deck A only.** The FFI surface and the renderer both index decks; we only attach a Thru source on deck 0 today. Deck B is one `attach_thru_source_with_peaks_tracking(1, …)` call away in `start_thru_inner`, plus a second `WaveformView` in `MainView`. Deferred to M10.2 along with palettes.
* **No transport / no track loading.** Thru only. Track loading remains a CLI-only feature until the M11+ library + transport milestones.
* **No CI build target for Apple.** GitHub Actions stays Linux-only; a macOS runner + `make apple` target is part of the post-M10.2 distribution work.

---

---

<a id="m101"></a>
## M10.1 — Multi-colour fragment shader

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 3 days (delivered)

### What it is

The M10-B waveform is no longer monochrome. Each `PeakChunk` bar in the renderer now carries an 8-band perceptual-loudness vector (the `BandPeakChunk` data layer shipped in M9.5b), and the fragment shader mixes those bands into Serato-grade RGB:

```
R = mean(b[0], b[1])              kick / bass:   30 - 159 Hz
G = mean(b[2], b[3], b[4])        mids / vocals: 159 - 1934 Hz
B = mean(b[5], b[6], b[7])        highs / air:   1934 - 16000 Hz
```

`r`/`g`/`b` get per-channel gains (`1.2 / 1.8 / 2.4`) to compensate for the natural loudness imbalance — low bands carry more energy per FFT bin because there are fewer bins per log-spaced band. The bar's vertical extent still encodes peak amplitude (M10-B carries through); colour encodes the spectrum.

### What's new in this milestone

* **`Shaders.metal`**: the fragment shader picks up `bandLow`/`bandHigh` (two `float4`s = the 8 band RMS values) forwarded by the vertex shader, mixes them into RGB per the palette above, and applies a brightness floor + RMS-driven luminance pass. Silence ( `max(r,g,b) < 0.05` ) drops to neutral grey so dropouts read as "honest silence" rather than a colour cast.
* **`WaveformRenderer.swift`**: adds a second `MTLBuffer` (`bandChunksBuffer`, 2¹⁴ × 32 B ≈ 512 KB per deck) for the parallel band ring, a second polling path (`ingestNewBandChunks`), and four new fields in the uniforms struct (`samplesPerPeakChunk`, `bandChunkOffset`, `samplesPerBandChunk`, `bandCapacity`). The vertex shader uses these to map each broadband instance to its containing band chunk via `(iid × samplesPerPeakChunk + samplesPerPeakChunk/2) / samplesPerBandChunk`.
* **`crates/dub-ffi`**: tiny addition — `DubEngine::sample_rate() -> u32`. Combined with the already-shipped `peaks_chunk_duration_secs` / `band_peaks_chunk_duration_secs`, this lets the renderer derive `samples_per_chunk` exactly (`duration × sample_rate`) instead of snapping a heuristic across candidate sample rates. Tripwire constant `FFI_VERSION` bumps to 3.

### Why the band ring is parallel, not embedded

Both rings appended sequentially with `memcpy`s from the FFI; both indexed in NDC via a power-of-two modulo (which compiles to a bitmask in the shader). Keeping them parallel rather than embedding band data into `PeakChunk` keeps:

* the broadband chunk stride at 12 bytes — half the size of a `(min, max, rms, 8 × band)` packed alternative;
* M10-B and M10.1 backwards-compatible — broadband-only rendering still works exactly the same with a stopped or band-disabled engine;
* the shader vertex-stage cost flat — one extra buffer read, no branch on "is this band data?".

### Audio-thread cost

Zero new audio-thread work. The M9.5b decimator thread was already producing `BandPeakChunk`s and writing them to the parallel `PeakBuffer` storage; M10.1 just consumes them on the render thread.

### Renderer thread cost

Per frame: one extra `engine.bandPeaksLen` (atomic load), conditionally one extra `engine.bandPeaksExtend` (RwLock read), and one extra `memcpy` (~32 KB worst case per frame even on heavy catch-up). All bounded; all main-thread.

### Acceptance

1. `cargo test --workspace` passes — `dub-ffi` tests for `FFI_VERSION = 3` + `sample_rate = 0 when stopped`.
2. `cargo clippy --workspace --all-targets -- -D warnings` clean.
3. `./scripts/bootstrap.sh && xcodebuild build -scheme Dub` builds the universal `Dub.app`.
4. Live qualitative check: a clean kick paints red, a hi-hat / cymbal paints blue, vocals / synth pads paint green; silence renders as a thin grey hairline; loud transients brighten the bar without changing its hue family.

### What it does not ship

* **No palette presets.** The default Serato-faithful mapping is baked into the shader; user-selectable palettes (high-contrast, monochrome fallback, custom) land in M10.2.
* **No onset glow.** Beat-aware additive bloom on `dub-bpm`-confirmed onsets is M10.2.
* **No constant-Q bass split.** The 9-band variant (sub-bass < 60 Hz, kick 60-200 Hz) is M10.2 in `dub-spectral`.
* **Deck A only.** Same as M10-B; deck B wiring + a second `WaveformView` is M10.2.

---

---

<a id="m102"></a>
## M10.2 — Polish: deck B, palette presets, honest silence/clipping

**Status:** shipped (partial — see "What it does not ship") &nbsp;·&nbsp; **Estimate:** 3 days (delivered)

### What it is

M10.2 is the "exceeds-Serato polish" pass. Per the plan it's a stack of independently-shippable bullets; this milestone ships the three with the highest visible impact and the simplest delivery cost:

1. **Deck B wired identically.** Two-deck Thru sessions, two waveform views.
2. **Palette presets.** Three baked-in palettes (Serato-faithful = M10.1 default, high-contrast, monochrome) switchable from the toolbar.
3. **Honest silence and clipping.** The fragment shader paints silent stretches as a thin neutral hairline and clipped chunks as a solid red bar — no more "loud + silent both render as white".

Onset glow, beat-aware saturation, constant-Q bass split (9-band `dub-spectral`), and mip pyramids are still pending as future polish; each is independently shippable on top of this baseline.

### Deck B

* **`dub-ffi` — `DubEngine::start_thru_two_deck(device, channels_a, channels_b)`.** Same shape as `start_thru` but takes two channel pairs. Opens the input AU with the combined 4-channel set, uses CoreAudio `output_pairs = [(0,1), (2,3)]` to demux into two stereo SPSC consumers in the IOProc, and attaches a `ThruSource` + `PeakStream` on both deck 0 and deck 1. Validates non-overlapping pairs (returns `EngineError::InvalidChannels` with the merged list on overlap).
* **`start_thru_inner` is now the two-deck core.** Single-deck `start_thru` calls it with `channels_b: None`; the function builds the input options + attaches deck 1 conditionally. No code duplication between the two FFI entry points.
* **`MainView`.** Two channel fields: `chA` (defaults to `1,2`) and `chB` (empty = single-deck mode, matching M10-B's behaviour exactly; non-empty = two-deck mode). When two-deck is running, the waveform area splits vertically via `VSplitView` into two `WaveformView`s sharing the same palette.
* **`FFI_VERSION = 4`.**

### Palettes

* **Three presets in the shader.**
  - `0` — **Serato-faithful** (M10.1 default): bass→R, mids→G, highs→B with per-channel loudness compensation + normalised brightness floor.
  - `1` — **High-contrast**: same band mix but squared (boosts strong bands, suppresses weak), then renormalised with a higher brightness floor. Designed for bright rooms / projector-driven club setups where the default washes out.
  - `2` — **Monochrome**: collapses hue entirely; bar tone driven purely by broadband RMS. Equivalent to the M10-B look — useful as an "honest amplitude-only" reference when the colour layer is misleading (e.g. when checking a mix).
* **Uniforms.** The Swift-side `WaveformUniforms` gained a `palette: UInt32` field (replacing the previous `_reserved`). `WaveformView` takes a `palette: WaveformPalette` and forwards it to the renderer via `updateNSView`; the renderer reads it on the next frame.
* **UI.** A `Menu` in the toolbar with a paintpalette icon. `WaveformPalette` is `CaseIterable` so adding palettes is a one-line addition + one shader branch.

### Honest silence and clipping

* **Vertex-stage flags.** Each instance now emits `flags = (clipping, silence, palette, 0)` per quad:
  - `clipping = 1.0` when `max(|min|, |max|) >= 0.98` — a peak so close to ±1 we'd call it clipped.
  - `silence = 1.0` when `|min| + |max| < 1e-3 AND rms < 1e-4` — essentially zero audio in this chunk.
* **Fragment-stage branches.**
  - Clipping ⇒ solid red `(1.0, 0.05, 0.05)`. Unmistakable; the user is expected to act on this (turn the offending deck's gain down).
  - Silence ⇒ thin dim grey `(0.18, 0.18, 0.20)`. Honest dropout; visually distinct from a fully-saturated mid signal.
  - Neither ⇒ colour path (per-palette mix).
* **Why per-instance flags, not per-fragment.** The fragment shader can't see the raw `PeakChunk` `min`/`max` once the rasteriser has run; computing flags in the vertex stage and forwarding via `VertexOut` is one float4 per quad regardless of bar pixel height. All four quad corners come from the same instance, so rasteriser interpolation collapses to the per-instance constant — no precision concern.

### Why these three, not all seven

The plan's M10.2 list is seven items; landing all seven would have taken ~2 weeks of mostly disjoint work (BPM FFI accessors, a new band-layout migration in `dub-spectral`, a renderer-side mip pyramid). The plan explicitly calls out that each bullet is "independently shippable; user picks ordering at the end of M10.1" — so this milestone is the minimum-cost subset that lands a *user-perceptible* polish step (deck B + palettes + honest silence/clipping all visible without any audio engineering background to interpret).

### Tests

* **`dub-ffi`** — 11 unit tests passing. Added `start_thru_two_deck_rejects_invalid_or_overlapping_channels` covering wrong arity per side, zero indices, and the A/B overlap rejection. `FFI_VERSION = 4` tripwire.
* **Workspace** — `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` both green.
* **Apple build** — `./scripts/bootstrap.sh && xcodebuild -scheme Dub build` produces a universal `Dub.app`.

### Acceptance

1. Running the app with a single channel pair (e.g. `1,2` / empty deck B) reproduces the M10.1 single-waveform behaviour exactly.
2. Filling in both deck fields (`3,4` / `5,6` on an SL3) opens both inputs, demuxes in the IOProc, and shows two parallel waveforms stacked via `VSplitView`. Each deck's bars colour independently.
3. Cycling through palettes via the toolbar menu changes the waveform appearance immediately without restarting the audio session.
4. Playing a clipped signal renders the offending bars solid red. Cutting the input mid-bar renders silence as the dim grey hairline.

### What it does not ship

The plan's remaining polish bullets are each independently shippable as follow-up milestones:

* **Onset glow** — needs a new `dub-ffi` accessor for `dub-bpm`'s `BpmStream`'s onset confidence trail; renderer applies an additive bloom on confirmed onsets.
* **Beat-aware saturation** — same FFI extension as onset glow; multiplies the palette gain by `(0.7 + 0.3 × confidence)` so noisy / silent stretches desaturate.
* **Constant-Q bass split (9-band)** — touches `dub-spectral`'s band layout: bump `NUM_BANDS` from 8 to 9 by splitting the lowest log-band into a sub-bass (30-60 Hz) + kick (60-200 Hz) pair. Affects every downstream consumer (BPM ODF, peak storage, FFI wire format, shader). One coherent PR but a meaningful refactor.
* **Mip pyramids** — pre-decimated levels (e.g. mip-2 = average every 4 chunks, mip-3 = every 16) in `dub-peaks` so the renderer can show longer time windows by reading from a coarser mip. Deferred from M9 per existing PRD note.

---

<a id="m102-remainder"></a>
## M10.2 remainder — superseded by the M10.5h–p shader ladder, then rolled back in M10.8

**Status:** retired (all four bullets superseded) &nbsp;·&nbsp; **Resolution:** see [§M10.8](#m108) for the current Serato-parity baseline that replaced the shader ladder.

All four originally-deferred polish bullets from M10.2 (`SHIPPED.md` [§M10.2 *What it does not ship*](#m102)) were re-homed onto the M10.5h–p shader ladder rather than being shipped as M10.2 follow-ups:

- **Onset glow** → M10.5l (additive HDR overshoot driven by the new `OnsetDecimator`).
- **Beat-aware saturation** → M10.5m(a) (luma-rotation in fragment, riding the same `onsetConf` data as M10.5l).
- **Constant-Q bass split (9-band)** → M10.5m(b) (deferred to M11 — gnarliest piece; held until DJ-curated content lands to validate the colour change).
- **Mip pyramids** → M10.5k (planned, paired with the M10.5j on-disk sidecar so the pyramid is on-disk too).

The new ladder also added three pieces that weren't on the M10.2 remainder list and turned out to be load-bearing for "great, not mediocre" on 2026 hardware: M10.5h (HDR off-screen target + separable Gaussian bloom + ACES tonemap), M10.5i (continuous filled-envelope geometry), and M10.5j (sidecar cache so a re-load is ~1 ms instead of ~150 ms).

**Final disposition:** in M10.8, the entire M10.5h–p shader ladder (HDR, bloom, ACES tonemap, multi-pass post-processing, the `WaveformTuning` / `WaveformTuningPanel` runtime knob surface, the onset-driven brightness layer, the kick-emphasis tint, the time-domain `FilteredPeakChunk` ring) was **deleted from the runtime** in favour of a single-pass Serato-parity shader. See [§M10.8](#m108) for the current baseline and the future-work guardrail. The shader-ladder write-ups below are preserved as design archaeology — they explain why specific approaches were tried and what they cost, which is load-bearing for any future polish work that wants to revisit those ideas without re-running the same dead ends.

---

<a id="m103"></a>
## M10.3 — Performance shell

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 3 days

Launching `Dub.app` shows the real Performance View (per PRD §9.2): a thin status strip, two two-row deck headers, the Metal waveform in the wide centre region, and correctly-sized placeholders for the FX bar (lit by M15 / M16) and library (lit by M11). The dev toolbar (device picker / channels / palette) moves behind a `⌘,` Preferences sheet so the performance surface stays mouse-free at rest.

`apple/Dub/DesignSystem/Tokens.swift` becomes the single source of truth for colour / type / spacing; the Figma file documented in PRD §9 reverts to a reference artefact (it does not gate any future UI work). Deck-header BPM / pitch / key / FX columns render as `—` placeholders until their FFI accessors land — surfacing the M8 BPM tracker over UniFFI is a trivial follow-up, pitch / key / FX wait on M13 / M14 / M15. Snapshot tests (`swift-snapshot-testing`) deferred to M18 polish; the M10.3 demo is visual eyes-on.

---

<a id="m104"></a>
## M10.4 — Vertical waveform + symmetric two-pane layout

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 1–2 days

Two bugs in the M10.3 build, fixed together:

**(a)** the Metal renderer is rotated from horizontal to **vertical** per PRD §9.1 (forward play = waveform marches upward through the playhead at 25 % from the top; reverse play = marches downward; direction follows engine rate sign with no inference). Touches `Shaders.metal` (vertex shader emits Y-instanced quads), `WaveformRenderer.swift` (buffer layout + view projection), `WaveformView.swift` (frame sizing tall not wide), and `PerformanceView.swift` (waveform region becomes `HSplitView` of two tall columns).

**(b)** Symmetric layout invariant: both deck waveform panes are always rendered side-by-side. In single-deck mode (deck B `chB` empty in Preferences) deck B's pane shows an idle placeholder matching the deck B header's `OFF` state instead of vanishing.

Status strip gains live battery + wall-clock per PRD §9.3 (`IOPSCopyPowerSourcesInfo`-driven battery, system clock for wall time). **Demo criterion:** every screenshot from M10.4 forward is in the canonical orientation.

---

<a id="m105"></a>
## M10.5 — File playback dev loop (M10.5a + M10.5b)

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 4–5 days

The dev-loop unblocker — Dub becomes testable without an SL3. Splits into **M10.5a** (Rust + FFI) and **M10.5b** (Apple shell).

**M10.5a — shipped:** new `DubEngine` surface (`start_engine` for output-only sessions, `load_track`, `play`, `pause`, `seek`, `position` → `PositionInfo`, `track_info` → `TrackInfo`); `dub-peaks` gains `compute_offline_peaks` so whole-track peaks compute synchronously at load time; the FFI's per-deck `PeakSource` enum routes `peaks_extend` through either the live Thru stream (M9) or the offline File buffer (M10.5a) transparently; `FFI_VERSION` 4→5.

**M10.5b — shipped (Apple shell):** auto-detect lifecycle (multi-channel input → Timecode mode via `start_thru_two_deck`; built-in only → Prep mode shell via `start_engine`); single-pass renderer refactor (`chunksAbovePlayhead` uniform, vertex shader linear y-map across full NDC, M10.4 behaviour preserved when no future peaks are present); drag-and-drop a file from Finder onto either deck pane → loads + deck header switches source pill to `FILE` and populates title / duration / track-time; 30 Hz position polling drives the deck-header time row; slim FS browser replaces the `LIBRARY — M11` placeholder (folder picker + single-click selection, **no double-click load**); `Space` loads the FS-browser-selected file into the non-master, stopped deck per PRD §6.4 (or into deck A in any single-deck mode — Prep, single-channel Timecode — since "non-master" isn't meaningful when only one deck exists). If the target deck is playing, the pane flashes red for 200 ms with a "deck is playing — lift the needle" overlay; **master-deck tracking** wires up per PRD §6.4 with a `MASTER` chip in the master deck's header. Preferences sheet is auto-apply: changing mode / device / channels restarts the engine immediately, no Apply button. App auto-starts on launch in the auto-detected mode.

**Auto-detect is permission-safe:** routes through `DubEngine::has_external_audio_interface` which queries CoreAudio transport-type metadata only (USB / Thunderbolt / FireWire / PCI / AVB) — `listInputDevices` is *not* called when Prep mode is picked, so the macOS microphone-permission prompt only ever fires when the user explicitly engages Timecode mode against an external interface.

Renderer gains a per-deck **peak-generation counter** (`DubEngine::peaks_generation`, atomic, survives stop/start cycles) so a Thru → File swap on a drag-and-drop load forces the renderer to reset its ring + cadence cache before re-ingesting from the new source — without this signal the length-monotonicity heuristic gets stuck rendering stale Thru chunks indefinitely. `FFI_VERSION` 5→7 (one bump for `peaks_generation`, one for `has_external_audio_interface`).

**No library DB, no metadata indexing, no crates, no other keyboard transport, no overview waveform** (that's M10.5c) — those are M11 / per-feature future milestones.

---

<a id="m105c"></a>
## M10.5c — Track Overview waveform + horizontal-orientation shader

**Status:** shipped (a + b) &nbsp;·&nbsp; **Estimate:** 2 days

The two pieces of M10.5b shakedown that didn't fit in the shell pass.

**M10.5c-a — shipped:** `TrackOverviewView` (SwiftUI `Canvas`) slotted on each deck's outside edge with playhead-bracket tracking + File-mode click-to-jump per the description below.

**M10.5c-b — shipped:** `orientation: u32` uniform plumbed end-to-end (Metal `Uniforms` struct, Swift `WaveformUniforms`, `WaveformRenderer.orientation` property, `WaveformView(orientation:)` parameter, host `WaveformMetalView` pipes the value into the renderer and forces a uniform refresh on change, playhead overlay swaps between horizontal hairline / vertical hairline based on orientation). Default remains `.vertical` so every M10.4 / M10.5b call site renders bit-identical pixels.

**Track Overview** (PRD §9.6.1): per-deck thin vertical strip on the deck's outside edge (`DubLayout.deckOverviewWidth ≈ 36 px`) showing the *whole* track top→bottom with a playhead-bracket indicator at the current `position(deck)`. Renders via SwiftUI `Canvas` (not Metal — overview is a low-cadence, fully-known-up-front signal that doesn't benefit from GPU instancing; `Canvas` keeps the pipeline simpler and the shader inventory smaller). Reads broadband peaks via `peaks_extend(deck, 0)` once at load, decimates to ≈ 300 buckets (the strip's pixel height at typical window sizes), redraws only when the playhead chunk changes (≈ 30 Hz from the existing position poll). **Click-to-jump** plumbed for File mode immediately; Timecode-mode behaviour gated on M10.6's Panic-Play wiring.

**Horizontal-orientation Metal uniform:** adds `orientation: u32` (0 = vertical, 1 = horizontal) to `WaveformUniforms` and the matching `Shaders.metal` constant buffer; the vertex shader picks the NDC x↔y assignment based on the uniform. Vertical orientation is the default and the M10.4 / M10.5b behaviour is bit-identical; horizontal flips the playhead from "25 % from top" to "25 % from left" with the future to the right of the playhead. Lights up Prep mode's horizontal layout in M10.8 without that milestone needing to touch the shader. No FFI version bump (renderer-only).

---

<a id="m105d"></a>
## M10.5d — Background load (decode + peaks off-thread)

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 0.5 days

The perceived "loading is slow" pain was the FFI's `load_track` doing synchronous `Track::load_from_path` (symphonia decode → `Vec<f32>` of the whole file) plus `compute_offline_peaks` (broadband + 3-band ring fills across all samples) **under the engine-state mutex** on the SwiftUI main actor. Two compounding effects: (1) the call blocked the main actor for ~50–300 ms depending on track length, freezing the UI; (2) the engine-state mutex stayed held throughout, so every concurrent `position()` / `peaks_extend()` / `track_info()` call (the 30 Hz UI poll + waveform poll) blocked behind the loader too — Swift-side dispatch alone would not have helped.

**Rust fix** in `crates/dub-ffi/src/lib.rs`: split `load_track` into three phases. Phase 1 takes the mutex briefly to verify `EngineState::Running`. The guard drops. Phase 2 does the slow decode + peaks compute **mutex-free** — the rest of the API stays responsive throughout. Phase 3 re-acquires the mutex, re-checks `Running` (the engine could have been stopped during decode; if so, the freshly-built `Arc<Track>` + peak vectors drop on the caller's thread, harmless), then installs the new track + peaks and bumps `peak_generation_seq` while still holding the guard (no torn-read window — a renderer that sees the new peaks also sees the new generation). The generation atomic lives on `DubEngine` directly, not inside the `Mutex<EngineState>`, so the access doesn't recurse.

**Swift fix** in `apple/Dub/MainView.swift` + `Performance/PerformanceView.swift`: `WaveformAppModel.loadTrack(side:url:)` becomes `async`, dispatches the FFI call onto a `Task.detached(priority: .userInitiated)` so it doesn't pin the SwiftUI main actor either. New `DeckState.isLoading: Bool` tracks in-flight loads; concurrent load on the same deck red-flashes the deck pane and surfaces "Deck *X* is already loading — wait or load onto the other deck". Optimistic UI: the new file's title fills in immediately and the deck-header source pill flips to a new `Source.loading` variant ("LOADING…", amber dot) before decode starts — the user sees the deck respond to the drop instantly, even though the audio swap lands ~tens of ms later. A *replace*-load (new file decoded while a previous one is resident) keeps the old waveform + transport toggle live during decode and swaps in atomically when `peak_generation_seq` bumps. `loadBrowserSelectionIntoTargetDeck()` becomes `async` to match; the Space-key NSEvent handler awaits inside its existing `Task { @MainActor in ... }` wrapper.

No FFI bump.

---

<a id="m105e"></a>
## M10.5e — Waveform polish (compression + past-region dim + brighter floor)

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 0.5 days

> **Note:** the soft-amplitude compression and past-region dim shipped here survive into the M10.8 baseline conceptually but live in different code paths after the M10.8 shader rewrite. The exact constants below describe the pre-M10.8 shader.

The "ugly waveform" pain: linear amplitude makes typical -14 LUFS music live in the inner ~30 % of the deck column; uniform brightness across past/future kills the depth cue from the bottom→top scroll; thin RMS-driven palette saturation washes out under projector lighting.

**Shader-level** fixes in `apple/Dub/Waveform/Shaders.metal`: (1) **soft amplitude compression** `displayAmp = sign(x) * |x|^0.55` applied to `lo` / `hi` *after* the honest-state `clipping` / `silence` flags read the raw values — peaks at 0.3 now render at ~0.50, peaks at 0.7 at ~0.82, and an already-clipped 1.0 stays at 1.0. Visually fills the column on most masters without lying about the underlying signal. (2) **Past-region dim** routed through `VertexOut.flags.w`: the vertex stage sets it to 1.0 for chunks above the playhead, 0.0 below; the fragment multiplies the final RGB by `mix(1.0, 0.62, isPast)`. Applied uniformly to all three palette branches *and* to the honest-state clipping/silence colours so the depth cue stays consistent across visualisation modes. (3) **Brighter luminance floor**: the final RMS-driven luminance clamp moves 0.45 → 0.55 with a slightly gentler gain (1.6 → 1.4) so brick-walled tracks don't pin every chunk to 1.0 — preserves transient contrast through the loud parts. The Serato-faithful palette's `normaliseColour` floor lifts 0.45 → 0.55; the monochrome palette's intensity floor lifts 0.35 → 0.45.

**SwiftUI overlay** in `apple/Dub/Waveform/WaveformView.swift`: faint zero-crossing hairline (`DubColor.divider.opacity(0.55)`, 1 px) along the amplitude=0 axis — vertical line at mid-width in vertical orientation, horizontal line at mid-height in horizontal (Prep) orientation. Layered under the deck-tinted playhead overlay so the playhead always wins where they cross. Helps the eye read symmetry around silence and gives sparse-waveform sections an anchor.

No FFI changes; no shader uniform changes (everything piggybacks on existing `Uniforms` / `VertexOut`).

---

<a id="m105f"></a>
## M10.5f — Waveform 2× zoom-in

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 0.1 days

The deck-column waveform was too zoomed-out at the M10.5b sizing: ≈ 4 chunks per pixel meant ~6 s of audio crammed into the visible future region, hard to read transient relationships at mix-in time. One-line fix in `apple/Dub/Waveform/WaveformRenderer.swift`: `nonisolated private static let chunksPerPixel: Double = 4.0` → `2.0`. The constant feeds both (a) the renderer's per-frame `chunksVisible` math (drives the M10.4 NDC mapping in `Shaders.metal`) and (b) `WaveformRenderer.secsPerPixel(sampleRate:)` (drives the M10.6a click-scrub gesture's px → secs conversion), so the click-scrub gesture stays calibrated automatically. The change exposed a latent aliasing pattern — see M10.5g for the follow-up. No FFI changes.

---

<a id="m105g"></a>
## M10.5g — Waveform anti-alias + temporal smoothing

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 0.5 days

The remaining ugliness after M10.5e + M10.5f was a **"venetian blind" stripe pattern** between adjacent chunks. Two compounding root causes: (1) M10.5f's 2× zoom-in put each chunk's quad at ≈ 0.5 px tall on the time axis, and the pipeline had **no MSAA**, so amplitude-edge rasterisation stepped in hard integer-pixel jumps; (2) per-chunk min/max are inherently jittery across consecutive chunks at the engine's native 64-sample cadence (≈ 1.45 ms / chunk at 44.1 kHz), so neighbouring rows drew quads with slightly-different widths and a 1–2 px height — the eye sees the row boundaries as stripes.

**Shader fix** in `apple/Dub/Waveform/Shaders.metal`: per-instance vertex stage now reads `chunks[iid-1]`, `chunks[iid]`, `chunks[iid+1]` (clamped at `iid==0` and `iid==chunksVisible-1`) and convolves min / max / rms with a `[1, 2, 1] / 4` Gaussian kernel. The result drives the rendered quad and `VertexOut.rms`; the honest-state `clipping` / `silence` flags continue to read the *raw centre* chunk so a single hot or silent chunk still lights up unattenuated — smoothing is visual-only, never on the depth-of-information surface. The temporal lowpass softens chunk-to-chunk amplitude jitter that the eye reads as stripes, without changing the broad envelope shape the DJ uses to read transients.

**Pipeline fix** in `apple/Dub/Waveform/WaveformRenderer.swift` + `apple/Dub/Waveform/WaveformView.swift`: 4× MSAA enabled end-to-end. New `nonisolated public static let WaveformRenderer.sampleCount = 4` is referenced both by the `MTLRenderPipelineDescriptor.rasterSampleCount` (renderer-owned pipeline) and `MTKView.sampleCount` (host-owned view); Metal validates these match at draw time. Cost on Apple Silicon is negligible — the multisample texture sits in tile memory, the resolve happens at the end of the render pass, no extra command-encoder plumbing required.

The combination produces a continuous, smoothly-shaded envelope at all zoom levels instead of the previous stripe pattern. No FFI changes; no shader uniform changes. The MSAA path and the temporal-smoothing principle both survive into the M10.8 Serato-parity baseline; the post-processing stack added on top by M10.5h–p does not.

---

<a id="m105hp"></a>
## M10.5h → M10.5p — Shader exploration ladder (rolled back in M10.8)

**Status:** shipped iteratively, then **rolled back wholesale in the M10.8 baseline freeze** — see [§M10.8](#m108) for the current shader.

Between M10.5g and the M10.8 freeze the renderer accumulated a deep stack of shader experiments aimed at matching (and at times exceeding) Serato's visual richness. Real-world dogfooding against side-by-side Serato screenshots showed the stack was not converging on a DJ-effective waveform: dense music collapsed into a uniform yellow soup, transients didn't pop, the runtime tuning panel grew several knobs that the operator should never have needed to touch, and each subsequent layer was paying for problems introduced by the previous one. The M10.8 cleanup deleted the entire post-processing ladder (HDR off-screen target, separable Gaussian bloom, ACES tonemap, the `WaveformTuning` `@Published` knob surface and its `WaveformTuningPanel` GUI, the time-domain filtered-peaks ring, the onset-driven brightness layer, the kick-emphasis tint, the dj-landmarks monochrome palette, the various other palettes) in favour of a single-pass Serato-parity shader with calibrated equal-loudness band biases / gains, three perceptually-tuned colour anchors, and a sub-bass-aware quiet greying gate.

These write-ups are preserved here as design archaeology — they document what was tried, what the calibration cost looked like, and why each layer eventually came down. Any future polish work that wants to revisit one of these ideas should read the relevant section first to avoid re-running the same dead end.

### M10.5h — HDR + bloom render pipeline — *shipped, then rolled back in M10.8*

The single biggest visual upgrade in the M10.5 polish ladder. Before: single-pass renderer writing straight to `bgra8Unorm`, fragment colours clamped at 1.0, no headroom for transient overshoot, no post-processing. After: **five-pass HDR pipeline** in the renderer with sub-pixel-accurate MSAA on the offscreen primary, real Gaussian bloom on transient overshoot, and ACES tonemap on the final composite.

**What shipped:** (1) `Shaders.metal` waveform fragment gains an HDR overshoot block — `hdrBoost = in.rms * in.rms * 3.5` multiplies the post-luminance colour with a quadratic curve calibrated against real-music RMS distributions (typical loud mid rms ≈ 0.30 → boost 0.32 → clear halo; transient peak rms ≈ 0.45 → boost 0.71 → strong halo; quiet pad rms ≈ 0.15 → boost 0.08 → faint wash; silence rms ≈ 0.05 → boost 0.009 → no bloom). The overshoot is applied *before* the past-region `pastDim` multiply so past transients still glow proportionally (just dimmer).

**Calibration notes** (six iterations of `bandMix` retuning to land on legible per-band colours — kick / mid / hi-hat across `(1.0, 0.10, 0.05)` / `(0.10, 1.00, 0.15)` / `(0.05, 0.30, 1.00)` near-spectral anchors, double-square ratio amplification with inverse-pink-noise band weighting `× 1.2` mid / `× 1.8` high to compensate for the `dub-spectral` μ-law compression curve and the natural pink-noise spectrum slope, plus a per-band 3-tap `[1, 2, 1] / 4` smoothing kernel on the band-RMS values so adjacent chunks paint the same colour) — preserved in the archived M10.5h plan-of-record. The key empirical finding: the upstream `dub-spectral` μ-law compression (`ln(1 + λ · |X|)`, chosen in M8.1 to stop hi-hats out-voting kicks in the BPM ODF) pulls all band RMS values into a narrow `[~0, ~6]` compressed range; downstream colour mixing must work in compressed space, not linear, or every per-band correction over-compensates and flips the colour distribution. This finding survived into the M10.8 baseline and drives its `bandBias` / `bandGain` calibration.

**Pipeline additions** that came down in M10.8: new shader trio (`fullscreenVertex`, `brightPassFragment`, `gauss1dFragment`, `compositeFragment`), four pipeline states (`waveformPipeline` re-targeted to `rgba16Float` MSAA + `brightPassPipeline` + `gaussPipeline` + `compositePipeline`), four offscreen textures (`hdrPrimaryMS`, `hdrPrimaryResolved`, `bloomA`, `bloomB` at half-res), seven-pass `draw(in:)` (waveform → bright-pass → H-Gauss → V-Gauss → H-Gauss → V-Gauss → composite/tonemap). Memory cost ~12 MB per deck at typical drawable sizes. The post-processing stack is what M10.8 deletes; the per-band-smoothing finding survives.

### M10.5i — Continuous filled envelope — *shipped, then rolled back in M10.8*

Eliminated the "looks like a peak meter, not a waveform" problem from the original M10.4 / M10.5b layout by replacing one instanced-quad-per-chunk draw with two connected `.triangleStrip` draws (one per region — past + future) whose vertices encode `(amplitudeEdge, timeCentre)` pairs. K chunks produce a single C0-continuous filled shape spanning K-1 trapezoidal slices, eliminating the inter-chunk seams. Calibration-A added a second pass per region for the "Serato two-layer envelope" look (outer min/max envelope at exponent 0.55, inner brighter RMS body at exponent 0.35, 1.6× HDR boost on the inner body so ACES tonemap pulls the core toward "white-hot at the centre, hue-saturated at the edges"). The continuous-envelope geometry survives into the M10.8 baseline; the two-layer overlay and HDR boost do not — M10.8 paints a single Serato-style envelope with calibrated low/mid/high colours and no post-processing.

### M10.5l — Onset-driven bloom intensity — *shipped, then rolled back in M10.8*

Promoted the M10.2 "onset glow" bullet onto the M10.5h HDR pipeline. New `dub-peaks` `OnsetDecimator` mirroring `BandDecimator`'s surface, built on the same `SpectralFrameStream` primitive — same FFT-hop cadence (= `samples_per_band_chunk`, default 512 samples), single `f32` `OnsetChunk` per hop carrying the Klapuri-style log-band weighted spectral flux. **Why a sibling of `dub-bpm::onset` rather than re-exporting it:** the renderer needs the onset trail even when no `BpmStream` is running (File-mode playback, single-deck Prep), and tying the renderer to the BPM crate would couple two independent off-RT pipelines.

**`dub-peaks` plumbing:** `PeakBuffer` gains an optional `OnsetStorage` mirror of `BandStorage`; `PeakStreamConfig.onset_enabled` (default `true`) implicitly enables `bands_enabled`; `PeakStream::spawn` drives an `OnsetDecimator` on the analysis thread alongside the existing `BandDecimator`; `compute_offline_peaks` does the same on the file-mode path. **`dub-ffi` surface:** `engine.onset_peaks_len(deck)`, `engine.onset_peaks_chunk_duration_secs(deck)`, `engine.onset_peaks_extend(deck, start)` — same shape as `band_peaks_*` but a 4-byte stride. `PeakSource` enum delegates to the live stream / offline buffer the same way it delegates bands. `FFI_VERSION` 8 → 9.

**Apple renderer:** new `onsetChunksBuffer` ring (sized to `bandChunkCapacity` = 131 072 entries = 512 KB/deck), parallel `ingestNewOnsetChunks` pump, `WaveformUniforms.onsetChunkOffset` (per-region), onset buffer bound at vertex buffer slot 4. **Shader:** vertex stage looks up the onset chunk for each broadband chunk, applies the same 3-tap `[1, 2, 1] / 4` smoothing to the raw flux, maps via the calibrated sigmoid `onsetConf = clamp(1 - exp(-fluxSmoothed × 0.25), 0, 1)`. Forwarded through `VertexOut.onsetConf`; fragment multiplies `hdrBoost` by `(1.0 + 1.5 × onsetConf)`.

M10.8 deletes the renderer-side onset consumption (no more `onsetConf` shader path) but the Rust-side `OnsetDecimator` + `OnsetStorage` + FFI plumbing remains in place for future, additive consumers — exactly the kind of "reversible" architecture the M10.8 guardrail asks for.

### M10.5m(a) — Beat-aware saturation — *shipped, then rolled back in M10.8*

The first half of the originally-planned M10.5m row, lifted out and shipped alongside M10.5l because both effects ride the same `onset_trail` data and live in the same fragment-shader pass. After `bandMix` runs and *before* the palette branch, the shader rotates the bandMix output toward its own Rec. 601 luma based on `onsetConf`: `colour = mix(float3(luma), colour, 0.4 + 0.6 × onsetConf)` — sustained pads desaturate to a wash, kicks/snares paint full vibrant hue. Combined with M10.5l, drum hits + transients pop as saturated colour shapes against a near-monochromatic background of held notes / pads / silence. Rolled back in M10.8; the broader Serato-parity calibration in M10.8 makes the colour distribution legible without this overlay.

### M10.5o — Kick prominence layer (band[1] visual emphasis) — *shipped, then rolled back in M10.8*

Problem: in the M10.5l + M10.5m(a) baseline, a kick chunk and a sustained bassline chunk at the same broadband RMS read as visually indistinguishable — both paint at similar luminance + chroma since the bandMix output is a per-band *ratio* (a chunk dominated by bass paints red regardless of *how much* bass), and the bloom layer only fires on onsets. User wanted the 80–250 Hz "kick range" to **visually stand out** independent of onset / total amplitude.

**Implementation (~50 LoC across 4 files, all behind a single uniform):** new `kickEmphasis: float` in `Uniforms` + matching field in `WaveformUniforms` (taking the struct from 60 → 64 bytes, perfectly filling `uniformStridePerRegion`), sourced from a new `kickEmphasis` `@Published` knob on `WaveformTuning` (default 0.6, range 0.0–1.5) with a corresponding live slider in `WaveformTuningPanel`. Fragment shader applies three combined effects, all multiplied through `kickStrength = clamp(in.bandLow.y × kickEmphasis, 0.0, 1.0)`: (1) saturation override bumping `chromaScale` toward `min(chromaScale + kickStrength × 0.8, 1.0)`; (2) red-orange tint mixing the bandMix output toward `(1.0, 0.30, 0.05)` by `kickStrength × 0.55`; (3) additive HDR bloom `hdrBoost += in.rms × kickStrength × 0.6` gating on `in.rms` so quiet rumble doesn't paint but sustained loud sub-bass *does*.

M10.8 deletes the layer entirely (no `kickEmphasis` uniform, no `WaveformTuning`, no `WaveformTuningPanel`); the broader Serato-parity calibration in M10.8 paints kicks pink-red via its low-band anchor + kick-push logic instead.

<a id="m105n"></a>
### M10.5n — Playhead-vs-audio drift root-cause fix — *shipped, survives M10.8*

**Symptom (reported during M10.5l shakedown):** the audible kick happens slightly before the corresponding chunk crosses the playhead, AND the gap visibly widens as the track plays — small at the start, ~1 s by track-end on a 4-minute track. Initially mis-diagnosed as a steady-state `display_present − audio_buffer` differential (which is real but tiny: 5–20 ms constant) and "fixed" with a manual `avOffsetMs` slider in the tuning panel. The slider masked the problem but didn't solve it — a value tuned at 0:30 is wrong at 3:30 because the actual error is **linear in track time**, not constant.

**True root cause:** peak chunks are cadenced in **track frames** (the offline analyzer in `dub-peaks` produces one chunk per 64 *track* samples), but the renderer was indexing them in **engine frames** with an integer-rounded conversion. The path was: `peaksChunkDurationSecs = 64 / track_sr` (correct, exact, e.g. `64/44100 = 0.0014512 s`) → `samplesPerPeakChunk = round(peaksChunkDurationSecs × engine_sr) = round(69.66) = 70` (the bug — drops 0.35 samples per chunk = 0.49 % per-chunk error) → `chunk = elapsed_secs × engine_sr / samplesPerPeakChunk`. On a 44.1 kHz track / 48 kHz engine the per-chunk error of 0.49 % compounds to **~804 chunks of drift over 240 s of playback ≈ 1.17 s of accumulated visual lag**, exactly matching the reported symptom. (Same-SR tracks — e.g. 48 kHz track on 48 kHz engine — have zero drift because `peaksChunkDurationSecs × engine_sr` is already integer, so the bug was invisible on test fixtures.)

**Fix (~5 LoC in renderer, no FFI change):** bypass the integer-rounded intermediate entirely. Store `peakChunkDurationSecs: Double` in `WaveformRenderer` from the engine's already-exact f64 report, and use it directly: `playheadChunk = floor(elapsed_secs / peakChunkDurationSecs)`. Verified by hand-calculation: on the 44.1 kHz / 48 kHz scenario the new formula matches the engine's actual playback position to within `f64` precision (~1e-9 s). **Slider removal:** `WaveformTuning.avOffsetMs` deleted, "AV sync" section removed from the tuning panel, the `dub.waveform.tuning.avOffsetMs` `UserDefaults` key one-shot-migrated to nil on launch so a `defaults read com.klos.dub` doesn't show a stale value.

The root-cause fix survives the M10.8 cleanup unchanged (still in the renderer).

### M10.5p — DJ-focused waveform redesign — *shipped (Stages 1 + 2 + 3 + 3.1), then rolled back in M10.8*

**Problem statement (user-driven, 2026-05-13):** the M10.5h → M10.5o waveform stack delivered a *visually rich* renderer but a *DJ-ineffective* one. In loud / busy music ("bass + rapping + drums") the 7-band hue mix saturates toward a "yellowish glowing" soup because the per-band ratios all land near-equal once the music is dense. The DJ doesn't need spectral density: they need three landmarks. Quote: *"all DJ music is basically on a 4/4 rhythm. The other thing a DJ needs is to identify the drop (easy since this is mostly after a break and a buildup) and he needs to understand where the vocals come in and where they leave. This is basically all the dj needs from a waveform."*

**Design pivot:** from "data-rich spectral visualisation" → "DJ landmarks only". Stage 1 shipped a monochrome envelope (new `WaveformPalette.djLandmarks`) and an offline beat-grid + tick overlay (`dub-bpm::analyze_beat_grid`, `FFI_VERSION` 9 → 10). The Stage 1 beat-grid overlay was subsequently removed and re-scoped into its own milestone (`M10.5p-grid`, deferred) after testing exposed that fixed-period synthetic grids drift on tempo-drifting material (live recordings, vinyl pressings, breakbeat samples). Stage 2 added transient prominence (kick gate `clamp(band[1].y × onsetConf, 0, 1)`, 0.55 cap on `base` brightness so sustained content stays dim, warm-amber tint on confirmed kicks). Stage 3 added a time-domain `FilteredPeakChunk` ring (`dub-peaks::filtered` module, 2-pole Butterworth LP biquad at 180 Hz on the LF channel, new `samples_per_filtered_chunk` cadence, `FFI_VERSION` 10 → 11) so kick attacks survive intact for a clean kick-vs-sustained-bass discrimination at the shader level instead of fighting the upstream μ-law compression. Stage 3.1 calibrated the filter cutoff, replaced the smoothing kernel on `lfPeak` with `max`-of-3 (smoothing was destroying kick dynamic range), and adjusted the amber-gate to `smoothstep(0.08, 0.30, kickGate) × kickEmphasis` with a brightness-tied amber luminance.

M10.8 rolls back the entire `djLandmarks` palette branch, the beat-grid plumbing in `load_track` Phase 2 (already returning `BeatGrid::empty()` to save load time), the `WaveformTuning` slider surface, the kick-gate fragment-shader logic, and the time-domain `FilteredPeakChunk` ring on the *renderer* side. The Rust-side `dub-bpm::analyze_beat_grid` API, the FFI `BeatGrid` accessor, and the `dub-peaks::filtered::FilteredDecimator` + `FilteredPeakChunk` types remain in place as dormant data primitives — exactly the kind of reversible architecture the M10.8 guardrail asks for. A future, additive M10.8+ milestone can re-light any of them without re-running the Stage 1 → Stage 3.1 calibration tour.

### M10.5p-grid — Beat-grid v2 (tempo drift, downbeat detection, manual phase correction) — *deferred*

The first M10.5p Stage 1 ship bundled an offline beat-grid + tick overlay alongside the monochrome envelope. User testing exposed two issues that pushed the grid out into its own multi-sub-task milestone: (a) the overlay didn't visibly scroll with the playhead on first ship (a `Canvas`-caching bug; subsequently fixed) yet still relied on a static phase that doesn't survive tempo-drifting material, and (b) the "stuck two ticks" symptom revealed the deeper truth — *beat grids only work on tempo-locked production tracks*. Live recordings, classic vinyl pressings (which drift inherently), edits with manual cuts/loops, and tempo-aware DJ tools (Serato Pitch'n'Time, Traktor Flux) all produce material where a fixed-period synthetic grid drifts off the audible beats within bars.

A v2 grid that handles those cases needs: **(g1)** per-beat phase tracking (a Viterbi-style decoder over the ODF rather than a single global phase pick); **(g2)** algorithmic downbeat detection (which beat is "the 1" of each bar — current Stage 1 just calls beat 0 the downbeat, which is wrong for any track that doesn't start exactly on the 1); **(g3)** manual phase correction UI (tap the waveform to nudge the discovered "1"; ⌘⇧← / ⌘⇧→ to shift the grid by ±1 ODF tick; half-tempo / double-tempo toggle for the M8.1 octave-ambiguity edge cases); **(g4)** library sidecar serialisation (compute the grid once, persist it, never recompute on re-load); **(g5)** a Thru-mode streaming variant (the offline `analyze_beat_grid` is file-only — a streaming `BpmStream` already exists but only emits BPM, no phase).

When the grid milestone resurfaces, the one-line revert to re-enable Stage 1's coarse phase finder is documented in `dub-ffi/src/lib.rs` `load_track`. Until then, the waveform helps the DJ with no grid.

### M10.5m(b) — 9-band sub-bass split — *deferred to M11*

The second half of the originally-planned M10.5m row, parked for after M11 (Serato library import) lands so we have a real DJ-curated track set to validate the colour change. **Plan when revisited:** bump `dub-spectral::NUM_BANDS` from 8 to 9 by splitting the lowest log-band into sub-bass (30–60 Hz) and kick-band (60–200 Hz). Touches `dub-spectral` (band-layout constant, FFT-bin-grouping math), `dub-bpm` (every M8.1 genre fixture needs to be re-baked because the per-band magnitudes shift), `dub-peaks` `BandPeakChunk` (wire format gains a 9th f32 — `#[repr(C)]` size 32 → 36 bytes, breaking change for the M10.5j sidecar format → version bump), `dub-ffi` `peaks_extend` wire format documentation, shader `BandPeakChunk` struct + `bandMix`. The compute-side change is mechanical; the data-format breakage is the gnarly part — every dependent crate's tests need re-baselining and the sidecar format gets a `version: u32 = 2` bump with a v1 → v2 migration (drop v1 entries on first run; a one-time re-decode is acceptable in Phase A). `FFI_VERSION` += 1 when it lands.

### M10.5j — On-disk waveform sidecar cache — *planned, not yet shipped*

The "track-load feels instant" upgrade — what Serato (`.SeratoOverview`), Traktor (`.tg2`), rekordbox (`.pdb` + analysis blobs) all do under the hood. Today every track load runs `Track::load_from_path` + `compute_offline_peaks`. M10.5d moved it off the engine mutex, but the work still happens once per load. **Plan:** new `dub-cache` library owning a versioned on-disk format (64-byte LE header + broadband peaks + band peaks + optional mip pyramid + CRC-32 footer), keyed by `sha-256(canonical_path || file_size || mtime_nanos)`, stored under `~/Library/Caches/com.klos.dub/waveforms/`. Lookup flow in `dub-ffi::load_track` Phase 1 stats the audio file, computes the cache key, tries to `mmap` the sidecar. Cache hit → skip decode entirely. Cache miss → decode + compute as today, then atomically write the sidecar via `<key>.tmp` → `<key>.dubpeaks` rename. Disk budget per track ~2.5 MB at 5 min; a 500-track library ≈ 1.25 GB cache (well below Serato's typical 3–5 GB). LRU eviction when the directory exceeds a configurable cap (default 4 GB).

### M10.5k — Mip pyramid in `dub-peaks` — *planned, not yet shipped*

Closes the loop on the final M10.2 deferred polish bullet (Mip pyramids). Today the renderer reads peaks at a single resolution (64-sample broadband cadence) and the `TrackOverviewView` re-decimates to ~300 buckets on the CPU at load — both work but neither lets us *zoom smoothly* or feed a future coarse-zoom view a coarser source. **Plan:** extend `OfflinePeaks` with `pub mips: Vec<MipLevel>` containing 5 levels (level 0 = full cadence, level 1 = ÷2, level 2 = ÷4, level 3 = ÷8, level 4 = ÷16). Same reduction kernel for bands (band RMS is mean-pooled). The M10.5j sidecar gains the mips after the level-0 payload. `TrackOverviewView` drops its CPU decimation entirely and reads mip-4 directly via a new mip-aware `peaks_extend_mip(deck, mip, start_idx)` accessor (`FFI_VERSION` += 1).

---

<a id="m106"></a>
## M10.6a–d — Mouse transport, Panic Play, transport-cluster redesign

**Status:** shipped (a, b, c, d) &nbsp;·&nbsp; **Estimate:** 3 days for a–d (M10.6e Repeat outstanding)

Engine work concentrated in 10.6b, UI work split across the others. Together they deliver PRD §6.1's mouse-allowed transport, PRD §6.1.2 Panic Play (the **single most important reliability feature** in v1 from a "career night" perspective), and PRD §6.1.3 Casual Play.

### M10.6a — Casual Play UI + zoomed click-scrub

Deck-header transport-glyph cluster (Play/Pause toggle + Restart) added to Row 3 of `DeckHeader` — renders exactly when a file track is loaded (`timeRow != nil`), so it covers both Prep-mode and the Casual-Play-before-Timecode case. New `WaveformAppModel.{restart, scrub}(side:...)` methods plumbed into the header via a `DeckHeaderCallbacks` value (closures kept off `DeckHeaderState` to preserve `Equatable`). `WaveformView(onClickScrubRelativeSecs:)` installs an orientation-aware transparent hit-test layer beneath the playhead overlay; click → signed seconds-from-playhead via the same `chunksPerPixel × samplesPerPeakChunk / sampleRate` ratio the renderer uses, so a click lands on the visual chunk under the cursor. New nonisolated `WaveformRenderer.secsPerPixel(sampleRate:)` helper centralises that math. PRD §6.1 gating: the closure is wired only when `engineMode == .prep`; Timecode-mode panes pass `nil` so the gesture isn't installed at all (no fine-scrub on a timecode-controlled deck, regardless of Panic Play state). No FFI bump (renderer + UI only).

### M10.6b — Panic Play engine + FFI

New `LiftPolicy::force_disengaged()` (preserves `last_locked_rate` while clearing the engaged flag + sticky counter — the next `Locked` is by construction a fresh re-engagement). New engine-level `PanicPlayState { engaged, held_rate }` per deck; `PanicPlayState::normalise_held_rate` collapses negative / near-zero candidates to a positive forward rate per PRD §6.1.2 ("runs the audio track forward"). New `Command::DeckPanicPlay { idx }` / `DeckCancelPanicPlay { idx }`. `Engine::engage_panic_play(idx)` captures the held rate (preferring `LiftPolicy::last_locked_rate()` when a timecode input is attached, falling back to `deck.rate()` otherwise), force-disengages the policy, sets the deck rate + playing, and flips the new `DeckSharedState::is_panic_play` atomic.

`Engine::drive_timecode_inputs` branches on panic state: in panic mode `Locked` intents auto-cancel (clean re-lock = "DJ dropped the needle back on the groove"), `DropoutHoldRate` intents are ignored (the whole point — the deck keeps playing while the needle is off the platter). `Engine::cancel_panic_play(idx)` pauses the deck and clears the flag; idempotent on non-engaged decks. `EngineHandle::DeckCommand::{panic_play, cancel_panic_play}` send the new commands; `DeckSnapshot.is_panic_play` exposes the atomic for the UI. FFI surfaces `panic_play(deck)` / `cancel_panic_play(deck)`; `PositionInfo` gains `is_panic_play` so the existing 30 Hz UI poll picks up the engine state. `FFI_VERSION` 7→8.

**Test coverage:** 11 new tests — 3 policy tests (force-disengaged clears flag + counter, preserves last_locked_rate, requires engage-threshold to re-lock), 8 engine tests (engage from policy, fallback to deck rate, negative/below-floor normalisation, dropout-stays-panicked, Locked-clears-engaged, cancel-pauses-deck, cancel-idempotent, default-disengaged, alloc-free), plus 1 end-to-end test that engages panic and renders synthetic CV02 carrier blocks through `engine.render` to verify the auto-cancel path lands correctly. All 350+ workspace tests still green; clippy `-D warnings` clean.

### M10.6c — Panic Play UI + Timecode overview un-gate

`DeckState.isPanicPlay: Bool` field driven by the existing 30 Hz `PositionInfo.isPanicPlay` poll (engine remains the authority — UI also sets it optimistically on `panic(side:)` for zero-frame latency, but the poll over-writes it every tick so an engine-side auto-cancel on clean re-lock propagates within ≤33 ms). New `WaveformAppModel.{panic, cancelPanic, panicToggle}(side:)` wrap the M10.6b FFI methods with the same error-surfacing path as Play/Pause. `DeckHeaderState` grew `isPanicPlay` + (initially) `panicGlyphVisible` flags and a new `Source.tcHold` variant; `DeckHeaderState.from(...)` derives them: glyph visible iff `thruMode && hasTrack`, `source = .tcHold` when `thruMode && isPanicPlay`. `TrackOverviewView.handleTap` un-gates: the two-deck-Timecode early-return allows the seek when `deckState.isPanicPlay` is true (PRD §6.1 release condition). M10.6c's lifepreserver-glyph + dedicated Restart button were superseded by M10.6d below — the rest of the M10.6c plumbing (model layer, source pill, overview un-gate) stayed and is what M10.6d builds on. No FFI bump for c.

### M10.6d — Transport-cluster redesign + library polish + cancel-doesn't-pause

Fixes the "Play does nothing in Timecode mode" bug at the root: pressing the deck-header Play button in Timecode mode previously called `engine.play` which set `is_playing = true` only to be overwritten by the very next `drive_timecode_inputs` `DropoutHoldRate` block. The fix is to surface Panic Play *as* the Timecode-mode Play affordance — one button, Serato-style INT/ABS toggle. `DeckHeaderState.panicGlyphVisible` renamed to `useTimecodeToggle` to reflect its expanded role. `DeckHeader.transportGlyphs` collapses to a single `primaryButton` that branches: Prep mode → classic Play/Pause via `onPlay` / `onPause`; Timecode mode + track loaded → `onPanicToggle` only, icon flips between `play.fill` (currently following platter — tap to play internally) and `opticaldisc.fill` amber (currently internal — tap to re-engage timecode).

M10.6c's lifepreserver glyph is gone (subsumed) and the M10.6a Restart button is gone (overview click-to-top covers it, PRD §6.1.3).

**Engine semantics tweak:** `cancel_panic_play` no longer pauses the deck — it clears the engaged flag + atomic and hands transport authority back to the timecode driver. A healthy carrier produces an immediate Locked re-lock (deck stays audible, true INT→ABS hand-back). A silent carrier yields `DropoutHoldRate` on the next block which pauses the deck via the existing arm — same outcome as the pre-M10.6c "pause on held position" path, without the race against the next Locked sample. `Command::DeckCancelPanicPlay` / `EngineHandle::cancel_panic_play` / FFI `cancel_panic_play` doc comments updated. `WaveformAppModel.cancelPanic(side:)` no longer optimistically sets `isPlaying = false`; the next 30 Hz poll reflects whatever the engine decides.

Replaced engine test `cancel_panic_play_pauses_deck_and_clears_shared` with `cancel_panic_play_clears_state_and_leaves_transport` + added 2 new tests: `cancel_panic_play_then_locked_intent_keeps_deck_playing` (synthetic CV02 carrier through `engine.drive_timecode_inputs` after cancel → deck stays playing at platter rate), `cancel_panic_play_then_silence_pauses_deck_via_dropout_path` (silent ringbuf → DropoutHoldRate → deck pauses naturally).

**FileBrowser polish:** folders now require **double-click** to descend (single-click was too easy to trigger by accident while scanning); the drag-out preview is a small `waveform` glyph instead of the row's full song-name text. Workspace `cargo test` clean, clippy clean, xcodebuild clean. No FFI bump (Phase A pragmatism — behavior change, same signatures).

### M10.6e — Repeat — *outstanding*

LFSR run-out auto-trigger that engages the same engine state as Panic Play, plus a per-deck Repeat toggle in the deck header (PRD §5.4.2). Engine substrate (M10.6b) already supports the state; remaining work is the auto-trigger plumbing (timecode driver detects run-out → calls into `engage_panic_play`) and the per-deck toggle UI.

---

<a id="m107"></a>
## M10.7 — Phase-Drift Trail

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 5 days

Dub's headline beat-matching aid (PRD §9.4). New `dub-match` crate (sibling of `dub-bpm` / `dub-peaks` / `dub-spectral`): a `MatchStream` analysis thread consumes both decks' `dub-bpm` ODFs off-RT, computes a rolling cross-correlation over ≈ 2 bars with a ±1-beat lag window (~40 lag candidates × 200 frames per update), emits `MatchSample { phase_ms, confidence, timestamp }` at 30 Hz to an SPSC ring. Audio-thread cost: **zero** (ODFs already running).

**FFI:** `matchExtend(start_idx) -> Vec<u8>` mirroring `peaks_extend`. **UI:** `apple/Dub/Performance/PhaseDriftView.swift` — Metal-rendered vertical strip ≈ 80 px wide in the centre gutter, time **bottom→top** matching the waveform direction discipline (PRD §9.1 / §9.4), dot brightness = confidence, dot colour blended from deck tints; numeric overlays `Δ BPM = +0.3` (top, slope-derived) and `Δ ms = +12` (bottom, instantaneous). Grid-agnostic by construction; degrades gracefully (dim dots) when ODFs are weak. `FFI_VERSION` bumps to 9.

**Single mode only — no Preferences toggle for "numeric-only" variant in v1.**

---

<a id="m108"></a>
## M10.8 — Track Preparation Mode shell + Serato-parity waveform baseline freeze

**Status:** shipped &nbsp;·&nbsp; **Estimate:** 2 days for the Prep shell + 1–2 days for the baseline freeze and cleanup

Two concurrent ships consolidated under M10.8: the Track Preparation Mode shell (the long-planned single-deck horizontal-waveform alternate root view), and the **Serato-parity waveform baseline freeze** that resolved the visual dead-ends of the M10.5h–p shader exploration ladder.

### Track Preparation Mode shell

Auto-detection of available audio interface at launch (PRD §3.1). If no multi-channel interface present, the app boots into Track Preparation Mode — alternate root view (`apple/Dub/Prep/PrepView.swift`) hosting a single-deck **horizontal** waveform full-width, with the whole-track overview band stacked above it, and the library prominent below. Manual override in Preferences (`Mode: Auto / Performance / Preparation`).

**Shell only:** the mode renders the chrome and supports load + play / pause; **no** beatgrid editor, **no** hot-cue prep, **no** track gain tweaking yet (those are v1.x per PRD §3 — they'd substantially expand v1 scope and the user explicitly chose the shell-only option in the M10.3 planning round). The mode's *purpose* is visible from M10.8; its *tooling* arrives in v1.x.

Sizing constants live in `apple/Dub/DesignSystem/Tokens.swift`:

- `DubLayout.deckColumnWidth = 80` — Performance (Timecode) mode zoomed column.
- `DubLayout.waveformPrepHeight = 140` — Prep mode horizontal zoomed strip.
- `DubLayout.deckOverviewHeight = 60` — Prep mode horizontal overview band stacked above the zoomed strip.
- `DubLayout.deckOverviewWidth = 36` — Performance mode vertical overview column on each deck's outside edge (unchanged from M10.5c).

`TrackOverviewView` is now orientation-aware (its `OverviewSizing` `ViewModifier` picks vertical-column vs horizontal-band sizing from a `WaveformOrientation` property); `PerformanceView` derives `waveformOrientation` from `engineMode` and stacks the Prep overview band horizontally above the playing waveform. `WaveformAppModel.palette` defaults to `.serato`.

### Serato-parity waveform baseline freeze

The M10.5h–p shader exploration ladder (HDR, bloom, ACES tonemap, onset-driven brightness, kick-emphasis tint, dj-landmarks palette, time-domain `FilteredPeakChunk` ring, the `WaveformTuning` `@Published` knob surface and its `WaveformTuningPanel` GUI) was rolled back wholesale in favour of a single-pass Serato-parity shader that matches the visual reference the user repeatedly compared Dub against (the Westside Connection breakdown / drop screenshot referenced through the M10.5p session).

**Current shader characteristics** (frozen baseline, see PRD §9.6.0):

- **Height** comes from per-pixel-column broadband `PeakChunk` max aggregation (the vertex shader aggregates `chunksPerColumn = 2` chunks per visual column at the Performance-mode `chunksPerPixel = 2` zoom, producing a `pixelsPerDrawnColumn = 2` strip with the visible-future-region transients visually doubled vs the M10.5b sizing).
- **Colour** comes from 8 log-spaced `dub-spectral` bands grouped into calibrated low / mid / high channels in the **log-compressed domain** (`bandBias` `float3(9.45, 7.75, 5.75)`, `bandGain` `float3(1.00, 0.82, 1.00)` — these are the M8.1 μ-law-curve domain values, not linear amplitudes; this was the load-bearing finding from the M10.5h calibration tour).
- **Anchors** tuned against the Serato reference: `lowColor = (1.00, 0.12, 0.24)` pink-red kicks, `midColor = (0.08, 0.94, 0.22)` green mid / presence instruments, `highColor = (0.58, 0.36, 1.00)` lavender hi-hats. Mixed via `weights = pow(saturate(calibrated / chromaMax), 1.45)` — the 1.45 power enhances the dominant band so two-band content reads as a clear blend rather than a muddy secondary.
- **Quiet greying** is gated by broadband amplitude (`in.peak`) **and** sub-bass focus (`in.subBass`, carrying `b0` ≈ <80 Hz at 44.1 kHz) **and** weak audible mid/high (`audibleMidTop`). Three-axis gating prevents the early single-axis attempts (broadband-only, then spectral-low-only) from greying out audibly significant mid-range content while still greying decay tails of sub-bass-only sections (mirrors what Serato does on the same reference clip).
- **Kick push:** loud low-band transients (`smoothstep(0.18, 0.42, in.peak) * smoothstep(0.25, 1.10, calibrated.x)`) boost `calibrated.x *= 1.35` and dim `calibrated.y *= 0.78`, ensuring kicks paint pink-red rather than drifting toward orange/green even when the mid/high bands also have content.
- **MSAA** stays at 4× (M10.5g, survives).
- **No HDR, no bloom, no tonemap, no `WaveformTuning` runtime knobs.** Single-pass renderer writes straight to `bgra8Unorm`.

The previous palette presets and `WaveformPalette` enum are gone; the per-track-palette state in `WaveformAppModel` is fixed at `.serato` and the Preferences `paletteSection` has been removed.

### Future-work guardrail (PRD §9.6.0)

Future waveform work must be **additive and reversible** relative to this baseline:

- Do not reintroduce the removed HDR / bloom / tuning-panel stack in-place.
- Do not rewrite the baseline shader without first preserving this version behind a small, explicit switch or an isolated follow-up commit.
- If a polish experiment fails, reverting that experiment should return exactly to this M10.8 baseline.

The Rust-side `OnsetDecimator`, `BeatGrid`, and `FilteredDecimator` data primitives remain available for future, additive consumers without re-running their calibration tours.

### Commit boundary

The freeze was committed as `4a31363` (`feat(apple,engine): freeze M10.8 waveform baseline`). The corresponding PRD additions live in [§9.6.0](PRD.md#960-waveform-baseline-freeze-m108-cleanup) and the sizing table in [§9.6.1](PRD.md#961-sizing).

---

*End of shipped milestone history. Forward-looking polish (M11 onward) lives in [`docs/PRD.md` §12](PRD.md#12-milestones).*
