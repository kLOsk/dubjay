<div align="center">

# Dub

**A timecode-vinyl DJ application for scratch DJs and vinyl enthusiasts.**

*Mac-first. Rust-cored. GPLv3.*

[![CI](https://github.com/kLOsk/dubjay/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/kLOsk/dubjay/actions/workflows/ci.yml)
[![License: GPL v3](https://img.shields.io/badge/license-GPLv3-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-macOS-lightgrey?logo=apple)](#status)
[![Status](https://img.shields.io/badge/status-pre--alpha-red.svg)](#milestone-progress)

</div>

> Dub is the spiritual successor to Serato Scratch Live for the urban music scene
> (hip hop, reggae, dnb, dubstep, scratch). Two decks, external mixer, real
> records **through** the software, smart utility FX, fast sample throws.

This is **pre-alpha software**. There is no release. There is `main`.

---

## What's interesting about this project

- **Rust on the audio thread.** The engine runs in a CoreAudio HAL IOProc with
  zero allocations, zero locks, and zero syscalls. Verified in CI by the
  [`rt-audit`](tools/rt-audit) binary, which renders 100k blocks under
  [`assert_no_alloc`](https://crates.io/crates/assert_no_alloc) before any merge.
- **Clean-room timecode.** [`dub-timecode`](crates/dub-timecode) decodes Serato
  CV02 in relative mode — analytic-signal demod, signed rate, confidence
  estimate, all alloc-free. ([Architecture notes.](docs/ARCHITECTURE.md))
- **Lift policy hardened on real hardware.** The `LiftPolicy` state machine
  combines a three-layer defense — RMS amplitude gate, two-edge confidence
  hysteresis, sticky-block window — driven by SL3 + Serato CV02 testing.
  Each pathology has a dedicated regression test.
- **TUI inspector.** [`dub scope`](crates/dub-cli/src/scope.rs) is a ratatui
  Lissajous + gauges + live thresholds, sharing the same `LiftPolicy` as
  playback so calibration transfers 1:1.
- **No mouse on the audio path, ever.** UI = external mixer + controllers + the
  user's hands on real records. The mouse is for browsing tracks, period.

## Quickstart

Mac with Rust stable, an audio interface, and (optionally) a Serato CV02
control vinyl.

```bash
# Build everything in release mode.
make ci

# Smoke-test the engine.
./target/release/dub smoke

# Play a file through CoreAudio at the device's native sample rate.
./target/release/dub play --realtime path/to/track.mp3

# Live timecode → deck (M5.3). SL3 deck A is on input channels 3 & 4.
./target/release/dub timecode-deck path/to/track.mp3 --input-channels 3,4

# TUI inspector for tuning your rig against the live timecode signal
# (M5.4.1). Same LiftPolicy as `timecode-deck`, so what you see here is what
# you'd hear during playback.
./target/release/dub scope --input-channels 3,4

# One-shot per-rig calibration (M5.4.2). Drop the needle, lift the needle —
# the calibrator detects each phase, derives engage / amplitude thresholds,
# and stamps a rig fingerprint. Stored at ~/.dub/calibration/<device>_<format>.json.
./target/release/dub calibrate --input-channels 3,4

# After calibrate has run once, `timecode-deck` auto-loads it on startup
# and probes the carrier briefly to detect rig swaps (cartridge change,
# preamp change). Mismatch → automatic recalibration before playback.
./target/release/dub timecode-deck path/to/track.mp3 --input-channels 3,4
```

`dub scope` keys: `q`/`Esc` quit, `c` clear lissajous, `↑/↓` engage threshold,
`PgUp/PgDn` disengage, `←/→` amplitude, hold `Shift` for 10× steps.

## Milestone progress

Roadmap is detailed in [`docs/PRD.md`](docs/PRD.md). Bookmark order:

| Milestone | Status | Headline |
|---|---|---|
| **M0** | ✅ shipped | Workspace, RT-safety harness, license, AGENTS.md, CI |
| **M1** | ✅ shipped | First sound + bidirectional deck (forward/reverse, rate) |
| **M1.4** | ✅ shipped | Real-time CoreAudio output via Default Output AudioUnit |
| **M1.5** | ✅ shipped | Buffer-size control + latency measurement |
| **M2** | ✅ shipped | Transport via lock-free SPSC command channel |
| **M3** | ✅ shipped | Format coverage (mp3 / flac / m4a / aiff / aac / alac) + hot loading |
| **M3.5** | ✅ shipped | De-click envelope + tail-fade + offline analyzer |
| **M4** | ✅ shipped | Two decks + debug internal mixer + master gain |
| **M5.1** | ✅ shipped | Clean-room Serato CV02 decoder, relative mode |
| **M5.2** | ✅ shipped | Audio input plumbing (`AudioInput`, `dub capture`, `dub levels`) |
| **M5.3** | ✅ shipped | Live timecode → deck (first scratch). 3-layer lift policy. |
| **M5.4.1** | ✅ shipped | TUI scope + `LiftPolicy` refactor |
| **M5.4.2** | ✅ shipped | Per-rig calibration + fingerprint-based auto-detection |
| **M5.4.3** | 📋 planned | Calibration speed (≤5 s first-time, ≤1 s probe) — match Traktor |
| **M5.4.4** | 📋 planned | Independent per-deck calibration (probes + thresholds per deck, not just per soundcard) |
| **M5.5.1** | ✅ shipped | Engine routing primitive (`render_routed`, `OutputRouting`) |
| **M5.5.2** | ✅ shipped | External-mixer 4-channel output: SL3 (✅ verified) / Audio 6 (⚠️ unverified) profiles + manual override |
| **M5.6** | ✅ shipped | Two-deck timecode (single CoreAudio input AU, IOProc demux into per-deck SPSC rings) |
| **M6** | ◻ planned | Traktor MK2 timecode |
| **M7** | ◻ planned | Thru Mode (real records routed through Dub) |
| **M8** | ◻ planned | Auto-BPM on Thru |
| **M9** | ◻ planned | Live waveform capture (Thru) |
| **M10** | ◻ planned | Waveform UI (Metal, 60 fps during scratch) |

PRD §2.2.0 describes the reliability staging — pragmatism before users, rigor
before stable.

## Repo layout

```
dubjay/                              repo root (workspace)
├── Cargo.toml                       Rust workspace
├── crates/
│   ├── dub-engine/                  audio graph, transport, RT-safety, LiftPolicy
│   ├── dub-dsp/                     resamplers, filters, FX
│   ├── dub-stretch/                 Rubber Band FFI wrapper (placeholder)
│   ├── dub-io/                      symphonia-based decoders
│   ├── dub-timecode/                Serato CV02 decoder (clean-room, relative mode)
│   ├── dub-thru/                    Thru-mode pipeline + auto-detection (placeholder)
│   ├── dub-fingerprint/             Chromaprint FFI (v1.1, placeholder)
│   ├── dub-library/                 SQLite + library imports (placeholder)
│   ├── dub-controller/              HID/MIDI abstractions (placeholder)
│   ├── dub-ffi/                     UniFFI Swift bindings (placeholder)
│   ├── dub-audio/                   CoreAudio HAL input + output (M1.4, M5.2)
│   └── dub-cli/                     `dub` binary (smoke / play / capture /
│                                                 timecode-deck / scope / …)
├── apple/                           SwiftUI/AppKit shell (M0.5)
├── tools/
│   └── rt-audit/                    RT-thread allocation auditor
├── docs/                            PRD, architecture, ADRs
├── scripts/                         build helpers
├── .cursor/                         Cursor rules + hooks for AI-assisted dev
└── AGENTS.md                        always-loaded project context for AI
```

## Engineering tenets

These are anchored in [`docs/PRD.md` §2.2](docs/PRD.md) and enforced both
socially and in CI:

1. **No allocations on the audio thread.** Static buffers, lock-free SPSC
   ringbufs, `assert_no_alloc` in tests + a dedicated rt-audit binary.
2. **TDD on anything that touches a real audience.** Pre-alpha is permitted to
   move fast (PRD §2.2.0), but stable releases must demonstrate full coverage
   on every audio-path code path.
3. **The engine matches the device, never the other way around.** v1 does no
   boundary resampling; both input and output devices are forced to the engine's
   sample rate, or startup fails loudly. (See M5.3 SR-alignment notes in
   ARCHITECTURE.md.)
4. **DJs stand in front of audiences.** Stuttering, dropouts, sample-rate
   converter artifacts, and policy chatter are no-go bugs, not "polish later"
   bugs. The `dub analyze` and `dub scope` tools exist so we can verify
   correctness without subjective listening sessions.

## Common commands

```bash
make test          # cargo nextest run + clippy
make smoke         # run the CLI smoke test
make rt-audit      # run the RT-safety harness
make ci            # everything CI runs (fmt-check + clippy + test + rt-audit)
make clippy        # cargo clippy --workspace --all-targets -- -D warnings
make fmt           # cargo fmt
```

See the [Makefile](Makefile) for more targets.

## Hardware tested

These are validated end-to-end on real hardware as each milestone lands.

| Hardware | Used for | Status |
|---|---|---|
| Serato SL3 | Stereo input pair (deck A on ch 3+4) | ✅ M5.2, M5.3, M5.4.1, M5.4.2 |
| Serato CV02 control vinyl | Timecode source (relative mode) | ✅ M5.1, M5.3, M5.4.1, M5.4.2 |
| Native Instruments Audio 6 | Input device alternative | ◻ planned for M5.5 |
| Phase DJ | HID controller | ◻ planned for v1.1 |
| Rane mixers (any) | External mixer | ✅ M5.3 (line-out compatible) |

## License

GPLv3 — see [`LICENSE`](LICENSE).

This means: if you distribute a binary based on this code, you must release the
source under GPLv3 too. We chose GPL deliberately so that engine improvements
made by anyone in the community come back to the community.

## Contributing

This is currently a single-developer project. Contributions are welcome but
expect reviews to be opinionated about reliability and the No-Mouse-DJ-Ever
philosophy. Read [`docs/PRD.md`](docs/PRD.md) first; for engineering background,
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

Bugs and feature requests: open an issue. Patches: open a PR against `main`.
CI must be green; new audio-path code requires `assert_no_alloc` coverage and
ideally a `rt-audit` extension.
