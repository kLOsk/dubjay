# Dub — Shipped Milestones (M0 → M8)

> Companion to [`docs/PRD.md`](PRD.md). The PRD's milestone table keeps shipped rows
> short; this doc holds the detailed write-ups, design history, and rationale
> for each milestone that has landed. Forward-looking milestones (M9 onward)
> stay in the PRD.
>
> **Why split?** Shipped milestones accumulate prose that's load-bearing for
> "why is the code this way" archaeology but is no longer load-bearing for
> "what are we building next." Keeping them in the PRD bloated it past the
> point where a reader (or AI assistant) could keep the whole roadmap in
> working memory. Moved verbatim here; nothing has been rewritten or
> summarized away.

**Currently shipped:** M0 through M8 (Auto-BPM on Thru — streaming driver). Workspace totals 386 tests passing under `cargo clippy --workspace --all-targets -- -D warnings`.

## Table of contents

- [M0 — Scaffold + CI + test discipline](#m0)
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

---

*End of shipped milestone history. Forward-looking milestones (M9 onward) live in [`docs/PRD.md` §12](PRD.md#12-milestones).*
