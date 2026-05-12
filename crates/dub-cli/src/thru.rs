//! `dub thru [flags]` — the M7 live wiring demo for Thru Mode.
//!
//! Wires:
//!
//! 1. [`dub_audio::AudioInput`] on the chosen input device (same channel-
//!    mapped demux as `dub timecode-deck`, e.g. SL3 deck A on
//!    `--input-channels 3,4`, deck B on `--deck-b-input-channels 5,6`),
//! 2. The IOProc → consumer ringbuf moved into the engine as a
//!    [`dub_engine::ThruSource`] via the [`dub_engine::EngineHandle`]
//!    command channel,
//! 3. [`dub_audio::AudioOutput`] running the engine on the CoreAudio
//!    render thread with the M5.5.2 routing (`audio_routing` shared
//!    module).
//!
//! Result: a real, non-timecode record on the platter is audible
//! through the engine at one buffer of round-trip latency (~2.7 ms
//! @ 64 frames / 48 kHz). The signal is *always* in software — that
//! is the entire point of Thru Mode in Dub. BPM detection (M8),
//! waveform capture (M9), and FX (M15+) all live in the software
//! path; sending audio around it via the interface's hardware Thru
//! button would defeat every one of those features. See the
//! `dub_engine::thru` module docs for the design rationale.
//!
//! What this is **not**: a UI, a mixer, an FX host, or production-
//! grade. It's the smallest possible "engine path is hot for a real
//! record" rig so we can validate M7 against a turntable before any
//! of those higher-level concerns land.
//!
//! ## Flags
//!
//! - `--device NAME` — input device name pattern (defaults to system
//!   default input).
//! - `--input-channels N,M` — deck A's stereo pair, 1-based. SL3 deck A
//!   = `3,4`. Defaults to `1,2`.
//! - `--deck-b-input-channels N,M` — deck B's stereo pair, 1-based.
//!   Omitted means single-deck (deck A only); deck B's slot stays
//!   unattached and silent.
//! - `--sr SR` — engine sample rate (defaults to input device's nominal
//!   SR). The engine and output device are aligned to this; no SRC.
//! - `--buffer-size FRAMES` — input AU buffer (defaults to device
//!   nominal).
//! - `--output-buffer-size FRAMES` — output AU buffer.
//! - `--duration SECS` — bounded runtime for scripted/CI tests; omitted
//!   means run until Ctrl-C (the live-DJ default).
//! - `--internal-mixer` / `--deck-a-out-ch N` / `--deck-b-out-ch M` /
//!   `--output-channels N` / `--device-profile NAME` — the shared
//!   M5.5.2 output routing (same semantics as `dub timecode-deck`,
//!   resolved via [`crate::audio_routing`]).
//! - `--no-bpm-track` — disable M8's live BPM analysis. Default is
//!   on: every attached Thru deck spawns a `dub_bpm::BpmStream`
//!   worker that prints `searching → tentative → locked` state
//!   transitions to stderr (with the detected BPM) as they occur.
//! - `--no-peaks-track` — disable M9's live waveform-peak capture.
//!   Default is on: every attached Thru deck spawns a
//!   `dub_peaks::PeakStream` decimator that accumulates a min/max/rms
//!   envelope of the deck's input. Periodic stats include the
//!   captured chunk count per deck.
//! - `--dump-peaks PATH` — on shutdown, write the captured peak
//!   chunks for every attached deck to `PATH` (CSV, columns
//!   `deck,chunk_idx,min,max,rms`). Useful for verifying capture
//!   end-to-end without the M10 waveform UI; plot with `gnuplot`,
//!   `awk + matplotlib`, or any tool that reads CSV.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

use crate::audio_routing::{build_input_options, resolve_output_routing, RoutingArgs};
use crate::input_cmds::{parse_input_args, InputArgs};
use dub_audio::AudioInput;
use dub_bpm::{BpmRange, BpmStream, TrackerConfig, TrackerEvent, TrackerState};
use dub_engine::{Engine, ThruInputConfig};
use dub_peaks::{PeakStream, PeakStreamConfig};

/// Upper bound on input frames per render block. Same value as
/// `dub timecode-deck` — covers CoreAudio's variable-buffer surprises
/// up to 4096 frames at 96 kHz / 1024-frame buffers.
const THRU_MAX_BLOCK_FRAMES: u32 = 4096;

/// CLI options for `dub thru`. Built on top of the shared
/// [`InputArgs`] so the `--input-channels`/`--device`/`--sr` flags
/// are identical to `dub levels`, `dub capture`, and
/// `dub timecode-deck`.
struct Opts {
    /// Deck A's input config (device, channels, SR, buffer). Built
    /// from the shared [`InputArgs`] parser.
    input: InputArgs,
    /// Deck B's input channels (1-based device-channel indices),
    /// e.g. `--deck-b-input-channels 5,6` for SL3 deck B. `None`
    /// keeps single-deck mode.
    deck_b_input_channels: Option<Vec<u32>>,
    /// Output AU buffer size hint (frames). Smaller = lower output
    /// latency. None means "device default".
    output_buffer_size: Option<u32>,
    /// Total run duration. None means "run until Ctrl-C" — the DJ
    /// default. Bounded only for scripted/CI runs.
    duration_secs: Option<f64>,
    /// `--internal-mixer` flag (debug). See [`RoutingArgs`].
    internal_mixer: bool,
    /// `--deck-a-out-ch` (1-based).
    deck_a_out_ch: Option<u32>,
    /// `--deck-b-out-ch` (1-based).
    deck_b_out_ch: Option<u32>,
    /// `--output-channels N` override.
    output_channels: Option<u32>,
    /// `--device-profile NAME` override.
    device_profile: Option<String>,
    /// `--no-bpm-track` flag (off by default; default is "track").
    /// When `true`, no [`BpmStream`] is spawned per Thru deck.
    no_bpm_track: bool,
    /// `--bpm-range MIN,MAX` override (inclusive, in BPM). `None`
    /// means "use the algorithm default 60–200". Used as the M9-
    /// scoped escape hatch for genres the M8.1 algorithm can't
    /// disambiguate without a prior (dubstep, K-S backbeat dnb).
    bpm_range: Option<BpmRange>,
    /// `--no-peaks-track` flag (off by default; default is "track").
    /// When `true`, no [`PeakStream`] is spawned per Thru deck.
    no_peaks_track: bool,
    /// `--dump-peaks PATH`. When `Some`, the per-deck peak buffers
    /// are written as CSV to `PATH` on shutdown. Used for M9 sanity
    /// checks before the M10 waveform UI exists.
    dump_peaks: Option<PathBuf>,
    /// `--no-band-peaks` flag (M9.5b). Default off — band capture is
    /// on by default since multicolour rendering is the headline
    /// feature. Setting this disables the [`BandDecimator`] inside
    /// each per-deck [`PeakStream`] to shave a tiny CPU slice off
    /// the off-RT worker when you only want broadband peaks.
    no_band_peaks: bool,
    /// `--dump-band-peaks PATH` (M9.5b). When `Some`, the per-deck
    /// per-band peak buffers are written as CSV to `PATH` on
    /// shutdown. One row per band chunk: `deck,chunk_idx,b0,..,b7`.
    /// Used to verify M9.5b colour data before M10.1's renderer
    /// exists.
    dump_band_peaks: Option<PathBuf>,
}

/// Entry point. Parses argv, validates, then drives the engine.
///
/// # Errors
/// Device-open failures, command-channel saturation during attach,
/// or routing/argument validation failures.
pub fn run(args: &[String]) -> Result<()> {
    let opts = parse_opts(args)?;
    run_with_opts(opts)
}

fn parse_opts(args: &[String]) -> Result<Opts> {
    let (input, leftover) = parse_input_args(args)?;
    let mut deck_b_input_channels: Option<Vec<u32>> = None;
    let mut output_buffer_size: Option<u32> = None;
    let mut internal_mixer = false;
    let mut deck_a_out_ch: Option<u32> = None;
    let mut deck_b_out_ch: Option<u32> = None;
    let mut output_channels: Option<u32> = None;
    let mut device_profile: Option<String> = None;
    let mut no_bpm_track = false;
    let mut bpm_range: Option<BpmRange> = None;
    let mut no_peaks_track = false;
    let mut dump_peaks: Option<PathBuf> = None;
    let mut no_band_peaks = false;
    let mut dump_band_peaks: Option<PathBuf> = None;
    let mut i = 0;
    let positional: Vec<String> = Vec::new();
    while i < leftover.len() {
        let raw = leftover[i].as_str();
        match raw {
            "--deck-b-input-channels" => {
                let v = leftover
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--deck-b-input-channels expects N,M"))?;
                let parsed: Result<Vec<u32>, _> =
                    v.split(',').map(|s| s.trim().parse::<u32>()).collect();
                let chans = parsed.context("--deck-b-input-channels values must be integers")?;
                if chans.contains(&0) {
                    return Err(anyhow!(
                        "--deck-b-input-channels uses 1-based indices (no 0); use 5,6 not 4,5"
                    ));
                }
                deck_b_input_channels = Some(chans);
                i += 2;
            }
            "--output-buffer-size" => {
                output_buffer_size = Some(
                    leftover
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--output-buffer-size expects an integer"))?
                        .parse()
                        .context("--output-buffer-size not an integer")?,
                );
                i += 2;
            }
            "--internal-mixer" => {
                internal_mixer = true;
                i += 1;
            }
            "--deck-a-out-ch" => {
                deck_a_out_ch = Some(
                    leftover
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--deck-a-out-ch expects an integer"))?
                        .parse()
                        .context("--deck-a-out-ch not an integer")?,
                );
                i += 2;
            }
            "--deck-b-out-ch" => {
                deck_b_out_ch = Some(
                    leftover
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--deck-b-out-ch expects an integer"))?
                        .parse()
                        .context("--deck-b-out-ch not an integer")?,
                );
                i += 2;
            }
            "--output-channels" => {
                output_channels = Some(
                    leftover
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--output-channels expects an integer"))?
                        .parse()
                        .context("--output-channels not an integer")?,
                );
                i += 2;
            }
            "--device-profile" => {
                device_profile = Some(
                    leftover
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--device-profile expects a name pattern"))?
                        .clone(),
                );
                i += 2;
            }
            "--no-bpm-track" => {
                no_bpm_track = true;
                i += 1;
            }
            "--bpm-range" => {
                let v = leftover
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--bpm-range expects MIN,MAX"))?;
                let parts: Vec<&str> = v.split(',').collect();
                if parts.len() != 2 {
                    return Err(anyhow!(
                        "--bpm-range expects two comma-separated values \
                         (e.g. --bpm-range 60,200); got {v:?}"
                    ));
                }
                let min: f64 = parts[0]
                    .trim()
                    .parse()
                    .context("--bpm-range MIN not a number")?;
                let max: f64 = parts[1]
                    .trim()
                    .parse()
                    .context("--bpm-range MAX not a number")?;
                let range =
                    BpmRange::new(min, max).map_err(|e| anyhow!("--bpm-range invalid: {e}"))?;
                bpm_range = Some(range);
                i += 2;
            }
            "--no-peaks-track" => {
                no_peaks_track = true;
                i += 1;
            }
            "--dump-peaks" => {
                let v = leftover
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--dump-peaks expects a path"))?;
                dump_peaks = Some(PathBuf::from(v));
                i += 2;
            }
            "--no-band-peaks" => {
                no_band_peaks = true;
                i += 1;
            }
            "--dump-band-peaks" => {
                let v = leftover
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--dump-band-peaks expects a path"))?;
                dump_band_peaks = Some(PathBuf::from(v));
                i += 2;
            }
            other => {
                return Err(anyhow!("unknown flag: {other}"));
            }
        }
    }
    if !positional.is_empty() {
        return Err(anyhow!("unexpected positional arg: {:?}", positional[0]));
    }

    if internal_mixer && (deck_a_out_ch.is_some() || deck_b_out_ch.is_some()) {
        return Err(anyhow!(
            "--internal-mixer and --deck-a-out-ch/--deck-b-out-ch are mutually exclusive"
        ));
    }
    if deck_a_out_ch.is_some() ^ deck_b_out_ch.is_some() {
        return Err(anyhow!(
            "--deck-a-out-ch and --deck-b-out-ch must both be specified (or neither)"
        ));
    }

    // `--duration` is consumed by `parse_input_args` into
    // `input.duration`; surface it on the Opts in `Option` form so the
    // main loop preserves "omit = unbounded" semantics.
    let duration_secs = input.duration;

    if dump_peaks.is_some() && no_peaks_track {
        return Err(anyhow!(
            "--dump-peaks requires peaks tracking; remove --no-peaks-track"
        ));
    }
    if dump_band_peaks.is_some() && (no_peaks_track || no_band_peaks) {
        return Err(anyhow!(
            "--dump-band-peaks requires band-peak tracking; \
             remove --no-peaks-track / --no-band-peaks"
        ));
    }

    Ok(Opts {
        input,
        deck_b_input_channels,
        output_buffer_size,
        duration_secs,
        internal_mixer,
        deck_a_out_ch,
        deck_b_out_ch,
        output_channels,
        device_profile,
        no_bpm_track,
        bpm_range,
        no_peaks_track,
        dump_peaks,
        no_band_peaks,
        dump_band_peaks,
    })
}

fn routing_args_from_opts(opts: &Opts) -> RoutingArgs {
    RoutingArgs {
        internal_mixer: opts.internal_mixer,
        deck_a_out_ch: opts.deck_a_out_ch,
        deck_b_out_ch: opts.deck_b_out_ch,
        output_channels: opts.output_channels,
        device_profile: opts.device_profile.clone(),
    }
}

fn run_with_opts(opts: Opts) -> Result<()> {
    let two_deck = opts.deck_b_input_channels.is_some();

    // 1. Open the input AU (same demux pattern as `dub timecode-deck`:
    //    `output_pairs = [(0,1), (2,3)]` in two-deck mode).
    let input_opts = build_input_options(&opts.input, opts.deck_b_input_channels.as_deref())?;
    let mut input =
        AudioInput::start_with_options(&input_opts).context("opening input device for thru")?;
    let input_sr = input.sample_rate();
    println!(
        "input:        device='{}' sr={input_sr} Hz channels={} buffer={} frames pairs={}",
        input.device_name(),
        input.channels(),
        input.buffer_frames(),
        input.pair_count(),
    );

    // 2. Engine + handle. The engine moves into AudioOutput so we
    //    *must* go through the handle for the per-deck attach.
    //    Engine SR := input SR (single-clock invariant).
    let device = dub_audio::query_default_output().context("querying default output")?;
    if (device.sample_rate - input_sr).abs() > 0.5 {
        println!(
            "note: output device currently at {} Hz, will be retuned to {input_sr} Hz \
             (engine SR) so playback runs on a single clock — no SRC.",
            device.sample_rate
        );
    }
    let engine_sr = input_sr;
    let engine_block = 256_usize;
    let (engine, mut handle) = Engine::new_with_handle(engine_sr, engine_block);
    println!(
        "engine:       sr={engine_sr} Hz block={engine_block} frames\n\
         output:       device sr={} Hz (target {engine_sr} Hz) buffer={} frames",
        device.sample_rate, device.buffer_frames,
    );

    // 3. Take input consumers — one per declared deck. CoreAudio's
    //    SPSC contract means we get them ahead of `AudioOutput::start`.
    let consumer_a = input
        .take_consumer_pair(0)
        .ok_or_else(|| anyhow!("AudioInput pair 0 consumer already taken"))?;
    let consumer_b = if two_deck {
        Some(
            input
                .take_consumer_pair(1)
                .ok_or_else(|| anyhow!("AudioInput pair 1 consumer already taken"))?,
        )
    } else {
        None
    };

    // 4. Resolve output routing (shared M5.5.2 path).
    let routing = resolve_output_routing(&device, &routing_args_from_opts(&opts))?;
    println!("{}", routing.describe());

    // 5. Move the engine onto the audio thread. Output stage starts
    //    producing audio immediately — both decks render silence
    //    (no Thru attached yet) until we send the attach commands
    //    in step (6).
    let output_opts = dub_audio::OutputOptions {
        channels: routing.channels,
        buffer_frames: opts.output_buffer_size,
        sample_rate: None,
        channel_map: None,
    };
    let output = dub_audio::AudioOutput::start_with_options(engine, &output_opts, routing.routing)
        .context("starting CoreAudio output for thru")?;
    let achieved = output.buffer_frames();
    let latency_ms = output.latency_seconds() * 1000.0;
    println!("output buffer: {achieved} frames -> {latency_ms:.2} ms one-way latency");
    println!();

    // 6. Attach Thru sources mid-stream via the handle. Each attach
    //    is a single SPSC push; the audio thread picks it up at the
    //    next block (~5-10 ms at 48 kHz / 256 frames).
    //
    //    When BPM tracking is enabled (the default), use the
    //    `with_bpm_tracking` variant — that spawns one analysis
    //    thread per deck and returns a `BpmStream` handle we poll
    //    in the run loop below. We hold the handles by deck index
    //    so the per-deck "BPM: 128.0 (locked)" prints can label
    //    themselves.
    let thru_cfg = ThruInputConfig {
        max_block_frames: THRU_MAX_BLOCK_FRAMES as usize,
        input_sample_rate: input_sr,
    };

    // Stream handles indexed by deck idx (0 = A, 1 = B). `None`
    // means "this analysis isn't tracking on this deck" — either
    // because the corresponding `--no-*-track` flag was set, or
    // because the deck has no input.
    let mut bpm_streams: [Option<BpmStream>; 2] = [None, None];
    let mut peaks_streams: [Option<PeakStream>; 2] = [None, None];

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let tracker_sr = engine_sr as u32;
    let tracker_cfg = TrackerConfig {
        sample_rate: tracker_sr,
        // Audio thread already mono-downmixed at the tee — see
        // `ThruSource::with_bpm_tee` — so the tracker treats its
        // input as mono.
        channels: 1,
        analysis_period_samples: tracker_sr,
        bpm_range: opts.bpm_range.unwrap_or(BpmRange::DEFAULT),
    };
    let peaks_cfg = PeakStreamConfig {
        bands_enabled: !opts.no_band_peaks,
        ..PeakStreamConfig::at(tracker_sr)
    };

    // Per-deck attach. The combination of (BPM on?, Peaks on?)
    // dictates which `EngineHandle::attach_thru_source*` variant we
    // call — `_with_telemetry` is strictly cheapest when both are
    // on (one ThruSource, one mono-downmix, two taps).
    let attach_deck = |handle: &mut dub_engine::EngineHandle,
                       idx: usize,
                       rx: ringbuf::HeapCons<f32>|
     -> Result<(Option<BpmStream>, Option<PeakStream>)> {
        match (opts.no_bpm_track, opts.no_peaks_track) {
            (true, true) => {
                handle
                    .attach_thru_source(idx, rx, thru_cfg)
                    .with_context(|| {
                        format!("attaching thru source on deck {}", deck_letter(idx as u8))
                    })?;
                Ok((None, None))
            }
            (false, true) => {
                let bpm = handle
                    .attach_thru_source_with_bpm_tracking(idx, rx, thru_cfg, tracker_cfg)
                    .with_context(|| {
                        format!("attaching thru + bpm on deck {}", deck_letter(idx as u8))
                    })?;
                Ok((Some(bpm), None))
            }
            (true, false) => {
                let peaks = handle
                    .attach_thru_source_with_peaks_tracking(idx, rx, thru_cfg, peaks_cfg)
                    .with_context(|| {
                        format!("attaching thru + peaks on deck {}", deck_letter(idx as u8))
                    })?;
                Ok((None, Some(peaks)))
            }
            (false, false) => {
                let (bpm, peaks) = handle
                    .attach_thru_source_with_telemetry(idx, rx, thru_cfg, tracker_cfg, peaks_cfg)
                    .with_context(|| {
                        format!(
                            "attaching thru + telemetry on deck {}",
                            deck_letter(idx as u8)
                        )
                    })?;
                Ok((Some(bpm), Some(peaks)))
            }
        }
    };

    let (b0, p0) = attach_deck(&mut handle, 0, consumer_a)?;
    bpm_streams[0] = b0;
    peaks_streams[0] = p0;
    if let Some(c_b) = consumer_b {
        let (b1, p1) = attach_deck(&mut handle, 1, c_b)?;
        bpm_streams[1] = b1;
        peaks_streams[1] = p1;
    }
    let attached_decks: Vec<u8> = if two_deck { vec![0, 1] } else { vec![0] };

    for &idx in &attached_decks {
        let bpm_tag = if bpm_streams[usize::from(idx)].is_some() {
            " · bpm on"
        } else {
            " · bpm off"
        };
        let peaks_tag = if peaks_streams[usize::from(idx)].is_some() {
            " · peaks on"
        } else {
            " · peaks off"
        };
        println!(
            "deck {}:       thru attached — engine reads input → writes output{bpm_tag}{peaks_tag}",
            deck_letter(idx)
        );
    }

    match opts.duration_secs {
        Some(d) => println!("running for {d:.1} s — drop the needle and play."),
        None => println!("running until Ctrl-C — drop the needle and play."),
    }
    println!();

    // 7. Main loop. ~20 Hz. Three jobs:
    //   (a) drain the trash channels so any displaced sources from
    //       future re-attach paths don't leak (no-op today),
    //   (b) print live per-deck stats every 500 ms,
    //   (c) poll per-deck BPM streams and emit any state
    //       transitions (Searching → Tentative → Locked, and back)
    //       to stderr as they happen.
    let start = Instant::now();
    let total = opts.duration_secs.map(Duration::from_secs_f64);
    let mut next_tick = start + Duration::from_millis(500);
    while total.is_none_or(|t| start.elapsed() < t) {
        let _ = handle.reclaim();

        for (idx, slot) in bpm_streams.iter_mut().enumerate() {
            let Some(stream) = slot.as_mut() else {
                continue;
            };
            while let Some(ev) = stream.try_recv() {
                #[allow(clippy::cast_possible_truncation)]
                let elapsed_s = start.elapsed().as_secs_f64();
                print_bpm_transition(elapsed_s, idx as u8, ev);
            }
        }

        let now = Instant::now();
        if now >= next_tick {
            print_stats(
                &output,
                &input,
                &attached_decks,
                &peaks_streams,
                start.elapsed(),
            );
            next_tick += Duration::from_millis(500);
        }
        thread::sleep(Duration::from_millis(50));
    }

    // 8. Shutdown. Stream handles must be dropped BEFORE the
    //    engine/handle/output so that the analysis threads see the
    //    audio side wind down cleanly. `BpmStream::shutdown` is
    //    explicit (instead of relying on `Drop`) so a join panic
    //    surfaces as an error rather than being silently swallowed.
    //
    //    If --dump-peaks was set, snapshot every per-deck buffer
    //    BEFORE shutting the streams down. (Shutdown joins the
    //    decimator thread, which is fine — the buffer is shared
    //    via Arc and lives independently of the thread — but doing
    //    it first preserves the clean ordering of "stop producing,
    //    then read".)
    if let Some(path) = opts.dump_peaks.as_ref() {
        dump_peaks_csv(path, &peaks_streams)
            .with_context(|| format!("writing peak dump to {}", path.display()))?;
    }
    if let Some(path) = opts.dump_band_peaks.as_ref() {
        dump_band_peaks_csv(path, &peaks_streams)
            .with_context(|| format!("writing band peak dump to {}", path.display()))?;
    }
    for slot in &mut bpm_streams {
        if let Some(stream) = slot.take() {
            stream.shutdown();
        }
    }
    for slot in &mut peaks_streams {
        if let Some(stream) = slot.take() {
            stream.shutdown();
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    let cb = output.callback_count();
    let in_cb = input.callback_count();
    let in_of = input.overflow_count();
    drop(output);
    let _ = handle.reclaim();
    println!();
    println!("done — {elapsed:.3} s wall");
    println!("  output callbacks: {cb}");
    println!("  input  callbacks: {in_cb} (overflow={in_of})");
    println!(
        "  thru trash overflow: {}",
        handle.thru_trash_overflow_count()
    );
    if cb == 0 {
        anyhow::bail!("CoreAudio output never fired a callback — device probably failed");
    }
    if in_cb == 0 {
        anyhow::bail!(
            "input device delivered no callbacks. SR mismatch or TCC permissions? \
             See `dub levels --input-channels {:?}` for a quick check.",
            opts.input.input_channels.as_deref().unwrap_or(&[1, 2])
        );
    }
    println!("OK");
    Ok(())
}

/// Pretty per-deck label. 0 → "A", 1 → "B".
fn deck_letter(idx: u8) -> char {
    char::from(b'A' + idx)
}

/// Format a tracker state for human-readable output. Compact and
/// terminal-friendly; the elapsed-time prefix is added by the caller.
fn format_tracker_state(state: TrackerState) -> String {
    match state {
        TrackerState::Searching => "searching".to_string(),
        TrackerState::Tentative { bpm } => format!("tentative @ {bpm:.2} BPM"),
        TrackerState::Locked { bpm } => format!("LOCKED @ {bpm:.2} BPM"),
    }
}

/// Emit a per-deck BPM transition line to stderr. Stderr (not
/// stdout) so the line never interleaves with the periodic stats
/// printer in confusing ways during piped capture.
fn print_bpm_transition(elapsed_s: f64, deck_idx: u8, ev: TrackerEvent) {
    match ev {
        TrackerEvent::StateChanged(state) => {
            eprintln!(
                "  [{:6.2}s] deck {}: bpm {}",
                elapsed_s,
                deck_letter(deck_idx),
                format_tracker_state(state),
            );
        }
    }
}

fn print_stats(
    output: &dub_audio::AudioOutput,
    input: &AudioInput,
    attached_decks: &[u8],
    peaks_streams: &[Option<PeakStream>; 2],
    elapsed: Duration,
) {
    let buf_ms = (f64::from(output.buffer_frames()) / f64::from(output.sample_rate())) * 1000.0;
    let cb = output.callback_count();
    let in_cb = input.callback_count();
    let in_of = input.overflow_count();
    #[allow(clippy::cast_precision_loss)]
    let avail_frames = (input.available() as f64) / 2.0;
    // Per-deck captured chunk counts, joined as "A=N B=M" only for
    // decks that have a peaks stream attached. Skipped entirely
    // when no deck has peaks tracking on (keeps the line clean for
    // `--no-peaks-track`).
    let peaks_summary: String = attached_decks
        .iter()
        .filter_map(|idx| {
            let stream = peaks_streams[usize::from(*idx)].as_ref()?;
            Some(format!("{}={}", deck_letter(*idx), stream.len()))
        })
        .collect::<Vec<_>>()
        .join(" ");
    let peaks_field = if peaks_summary.is_empty() {
        String::new()
    } else {
        format!(" peaks=[{peaks_summary}]")
    };
    // Per-deck band-chunk counts, joined as "A=N B=M". Skipped
    // entirely when no deck has band capture enabled (mirrors the
    // peaks_field rule above).
    let bands_summary: String = attached_decks
        .iter()
        .filter_map(|idx| {
            let stream = peaks_streams[usize::from(*idx)].as_ref()?;
            stream.samples_per_band_chunk()?;
            Some(format!("{}={}", deck_letter(*idx), stream.band_len()))
        })
        .collect::<Vec<_>>()
        .join(" ");
    let bands_field = if bands_summary.is_empty() {
        String::new()
    } else {
        format!(" bands=[{bands_summary}]")
    };
    eprintln!(
        "  [{:6.2}s] out_cb={cb} buf={buf_ms:.2}ms in_cb={in_cb} in_overflow={in_of} \
         in_buffered={avail_frames:.0} frames decks={:?}{peaks_field}{bands_field}",
        elapsed.as_secs_f64(),
        attached_decks
            .iter()
            .map(|i| deck_letter(*i))
            .collect::<Vec<_>>(),
    );
}

/// Write all captured peak chunks to `path` as CSV. One row per
/// chunk: `deck,chunk_idx,min,max,rms`. Header row included.
///
/// Decks without a `PeakStream` (because `--no-peaks-track` was set
/// or the deck has no input) are skipped silently — the dump
/// represents whatever was captured. An empty `PeakStream` (no
/// samples flowed) writes zero data rows but the header is still
/// emitted, so downstream tools see a valid file.
fn dump_peaks_csv(path: &std::path::Path, peaks_streams: &[Option<PeakStream>; 2]) -> Result<()> {
    let f = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut w = BufWriter::new(f);
    writeln!(w, "deck,chunk_idx,min,max,rms")?;
    for (idx, slot) in peaks_streams.iter().enumerate() {
        let Some(stream) = slot.as_ref() else {
            continue;
        };
        #[allow(clippy::cast_possible_truncation)]
        let letter = deck_letter(idx as u8);
        let snap = stream.buffer().snapshot();
        for (i, c) in snap.chunks.iter().enumerate() {
            writeln!(w, "{letter},{i},{},{},{}", c.min, c.max, c.rms)?;
        }
    }
    w.flush().context("flushing peak dump")?;
    Ok(())
}

/// Write all captured per-band peak chunks to `path` as CSV. One row
/// per band chunk: `deck,chunk_idx,b0,b1,b2,b3,b4,b5,b6,b7`. Header
/// row included.
///
/// Decks without a `PeakStream`, or with band capture disabled
/// (`--no-band-peaks`), are skipped silently — the dump represents
/// whatever was captured. An empty stream writes the header only,
/// which keeps downstream tools that expect a non-empty CSV from
/// failing on a fresh-attach corner case.
fn dump_band_peaks_csv(
    path: &std::path::Path,
    peaks_streams: &[Option<PeakStream>; 2],
) -> Result<()> {
    let f = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut w = BufWriter::new(f);
    // Header: deck, chunk_idx, then NUM_BANDS columns named b0..b<N-1>.
    write!(w, "deck,chunk_idx")?;
    for k in 0..dub_peaks::NUM_BANDS {
        write!(w, ",b{k}")?;
    }
    writeln!(w)?;
    for (idx, slot) in peaks_streams.iter().enumerate() {
        let Some(stream) = slot.as_ref() else {
            continue;
        };
        if stream.samples_per_band_chunk().is_none() {
            continue;
        }
        #[allow(clippy::cast_possible_truncation)]
        let letter = deck_letter(idx as u8);
        let snap = stream.buffer().band_snapshot();
        for (i, c) in snap.chunks.iter().enumerate() {
            write!(w, "{letter},{i}")?;
            for v in c.rms_per_band {
                write!(w, ",{v}")?;
            }
            writeln!(w)?;
        }
    }
    w.flush().context("flushing band peak dump")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parse_minimum_args_succeeds_and_defaults_to_unbounded_runtime() {
        let opts = parse_opts(&s(&["--input-channels", "3,4"])).unwrap();
        assert_eq!(opts.input.input_channels.as_deref(), Some(&[3, 4][..]));
        assert!(opts.duration_secs.is_none(), "default is unbounded");
        assert!(opts.deck_b_input_channels.is_none());
        assert!(!opts.internal_mixer);
    }

    #[test]
    fn parse_two_deck_mode() {
        let opts = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--deck-b-input-channels",
            "5,6",
        ]))
        .unwrap();
        assert_eq!(opts.deck_b_input_channels.as_deref(), Some(&[5, 6][..]));
    }

    #[test]
    fn parse_deck_b_input_zero_index_rejected() {
        let r = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--deck-b-input-channels",
            "0,1",
        ]));
        assert!(r.is_err(), "0 is not a valid 1-based index");
    }

    #[test]
    fn parse_partial_deck_out_ch_errors() {
        let r = parse_opts(&s(&["--input-channels", "3,4", "--deck-a-out-ch", "3"]));
        assert!(r.is_err(), "deck-a-out-ch without deck-b-out-ch must error");
    }

    #[test]
    fn parse_internal_mixer_with_deck_flags_errors() {
        let r = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--internal-mixer",
            "--deck-a-out-ch",
            "1",
            "--deck-b-out-ch",
            "3",
        ]));
        assert!(r.is_err(), "internal-mixer + deck flags must conflict");
    }

    #[test]
    fn parse_duration_preserves_option_shape() {
        let with = parse_opts(&s(&["--input-channels", "3,4", "--duration", "12.5"])).unwrap();
        assert_eq!(with.duration_secs, Some(12.5));
        let without = parse_opts(&s(&["--input-channels", "3,4"])).unwrap();
        assert!(without.duration_secs.is_none());
    }

    #[test]
    fn parse_unknown_flag_errors() {
        let r = parse_opts(&s(&["--input-channels", "3,4", "--bogus"]));
        assert!(r.is_err());
    }

    #[test]
    fn parse_unknown_flag_rejects_stale_mode_flags() {
        // The old mode flags (`--direct`, `--force-processed`,
        // `--auto-after-secs`, `--processing-hold-secs`) are gone in
        // the simplified Thru Mode design. Confirm they now error
        // rather than silently being accepted — protects users with
        // shell history full of the old flags from a silent no-op.
        for stale in [
            "--direct",
            "--force-processed",
            "--auto-after-secs",
            "--processing-hold-secs",
        ] {
            let r = parse_opts(&s(&["--input-channels", "3,4", stale, "5"]));
            assert!(r.is_err(), "stale flag {stale} must be rejected");
        }
    }

    #[test]
    fn routing_args_carry_all_routing_flags() {
        let opts = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--deck-a-out-ch",
            "3",
            "--deck-b-out-ch",
            "5",
            "--output-channels",
            "6",
            "--device-profile",
            "SL 3",
        ]))
        .unwrap();
        let ra = routing_args_from_opts(&opts);
        assert!(!ra.internal_mixer);
        assert_eq!(ra.deck_a_out_ch, Some(3));
        assert_eq!(ra.deck_b_out_ch, Some(5));
        assert_eq!(ra.output_channels, Some(6));
        assert_eq!(ra.device_profile.as_deref(), Some("SL 3"));
    }

    #[test]
    fn parse_engine_thru_max_block_frames_bounds_match_engine() {
        // The CLI passes `THRU_MAX_BLOCK_FRAMES` as
        // `ThruInputConfig::max_block_frames`. Pin that this constant
        // is at least the engine's debug-assertion-friendly ceiling
        // (CoreAudio variable buffers up to 4096 frames). const
        // assertion so a regression at edit-time stops the build.
        const _: () = assert!(THRU_MAX_BLOCK_FRAMES >= 4096);
    }

    #[test]
    fn parse_default_is_bpm_track_on() {
        let opts = parse_opts(&s(&["--input-channels", "3,4"])).unwrap();
        assert!(
            !opts.no_bpm_track,
            "BPM tracking should default ON (no_bpm_track = false)"
        );
    }

    #[test]
    fn parse_no_bpm_track_opts_out() {
        let opts = parse_opts(&s(&["--input-channels", "3,4", "--no-bpm-track"])).unwrap();
        assert!(opts.no_bpm_track);
    }

    #[test]
    fn parse_default_is_peaks_track_on() {
        let opts = parse_opts(&s(&["--input-channels", "3,4"])).unwrap();
        assert!(
            !opts.no_peaks_track,
            "peaks tracking should default ON (no_peaks_track = false)"
        );
    }

    #[test]
    fn parse_no_peaks_track_opts_out() {
        let opts = parse_opts(&s(&["--input-channels", "3,4", "--no-peaks-track"])).unwrap();
        assert!(opts.no_peaks_track);
    }

    #[test]
    fn parse_dump_peaks_captures_path() {
        let opts = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--dump-peaks",
            "/tmp/peaks.csv",
        ]))
        .unwrap();
        assert_eq!(
            opts.dump_peaks.as_deref(),
            Some(std::path::Path::new("/tmp/peaks.csv"))
        );
    }

    #[test]
    fn parse_dump_peaks_with_no_peaks_track_errors() {
        // --dump-peaks needs the data path to be live; rejecting
        // this at parse time saves the user a confusing empty-file
        // surprise.
        let r = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--no-peaks-track",
            "--dump-peaks",
            "/tmp/peaks.csv",
        ]));
        assert!(r.is_err(), "--dump-peaks + --no-peaks-track must conflict");
    }

    #[test]
    fn dump_peaks_csv_writes_header_and_rows() {
        // Synthesize a peak stream with a few chunks, write it out,
        // verify the CSV layout. No engine, no audio thread —
        // pure unit test of the dump formatter.
        use ringbuf::traits::Split;
        let rb = ringbuf::HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let mut streams: [Option<PeakStream>; 2] = [None, None];
        streams[0] = Some(PeakStream::spawn(rx, PeakStreamConfig::at(48_000)).expect("spawn"));
        // Inject 3 chunks directly via the shared buffer.
        let buf = streams[0].as_ref().unwrap().buffer();
        buf.push_chunks(&[
            dub_peaks::PeakChunk {
                min: -0.1,
                max: 0.2,
                rms: 0.15,
            },
            dub_peaks::PeakChunk {
                min: -0.3,
                max: 0.4,
                rms: 0.25,
            },
            dub_peaks::PeakChunk {
                min: -0.5,
                max: 0.6,
                rms: 0.35,
            },
        ]);

        let tmp = std::env::temp_dir().join(format!("dub-cli-peaks-{}.csv", std::process::id()));
        super::dump_peaks_csv(&tmp, &streams).expect("dump");

        let contents = std::fs::read_to_string(&tmp).expect("read");
        let mut lines = contents.lines();
        assert_eq!(lines.next(), Some("deck,chunk_idx,min,max,rms"));
        assert_eq!(lines.next(), Some("A,0,-0.1,0.2,0.15"));
        assert_eq!(lines.next(), Some("A,1,-0.3,0.4,0.25"));
        assert_eq!(lines.next(), Some("A,2,-0.5,0.6,0.35"));
        assert_eq!(lines.next(), None);
        let _ = std::fs::remove_file(&tmp);

        // Clean up the stream so the decimator thread exits.
        for slot in &mut streams {
            if let Some(s) = slot.take() {
                s.shutdown();
            }
        }
    }

    // -------- M9.5b band-peaks CLI surface -----------------------

    #[test]
    fn parse_default_is_band_peaks_on() {
        let opts = parse_opts(&s(&["--input-channels", "3,4"])).unwrap();
        assert!(
            !opts.no_band_peaks,
            "band peaks should default ON (no_band_peaks = false)"
        );
    }

    #[test]
    fn parse_no_band_peaks_opts_out() {
        let opts = parse_opts(&s(&["--input-channels", "3,4", "--no-band-peaks"])).unwrap();
        assert!(opts.no_band_peaks);
    }

    #[test]
    fn parse_dump_band_peaks_captures_path() {
        let opts = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--dump-band-peaks",
            "/tmp/bands.csv",
        ]))
        .unwrap();
        assert_eq!(
            opts.dump_band_peaks.as_deref(),
            Some(std::path::Path::new("/tmp/bands.csv"))
        );
    }

    #[test]
    fn parse_dump_band_peaks_conflicts_with_no_peaks_track() {
        let r = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--no-peaks-track",
            "--dump-band-peaks",
            "/tmp/bands.csv",
        ]));
        assert!(r.is_err(), "--dump-band-peaks needs peaks tracking");
    }

    #[test]
    fn parse_dump_band_peaks_conflicts_with_no_band_peaks() {
        let r = parse_opts(&s(&[
            "--input-channels",
            "3,4",
            "--no-band-peaks",
            "--dump-band-peaks",
            "/tmp/bands.csv",
        ]));
        assert!(r.is_err(), "--dump-band-peaks needs band capture");
    }

    #[test]
    fn dump_band_peaks_csv_writes_header_and_rows() {
        use ringbuf::traits::Split;
        let rb = ringbuf::HeapRb::<f32>::new(64);
        let (_tx, rx) = rb.split();
        let mut streams: [Option<PeakStream>; 2] = [None, None];
        streams[0] = Some(PeakStream::spawn(rx, PeakStreamConfig::at(48_000)).expect("spawn"));
        // Inject 2 band chunks via the shared buffer.
        let buf = streams[0].as_ref().unwrap().buffer();
        buf.push_band_chunks(&[
            dub_peaks::BandPeakChunk {
                rms_per_band: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8],
            },
            dub_peaks::BandPeakChunk {
                rms_per_band: [0.9, 1.0, 1.1, 1.2, 1.3, 1.4, 1.5, 1.6],
            },
        ]);

        let tmp = std::env::temp_dir().join(format!("dub-cli-bands-{}.csv", std::process::id()));
        super::dump_band_peaks_csv(&tmp, &streams).expect("dump");

        let contents = std::fs::read_to_string(&tmp).expect("read");
        let mut lines = contents.lines();
        assert_eq!(
            lines.next(),
            Some("deck,chunk_idx,b0,b1,b2,b3,b4,b5,b6,b7"),
            "header"
        );
        assert_eq!(lines.next(), Some("A,0,0.1,0.2,0.3,0.4,0.5,0.6,0.7,0.8"));
        assert_eq!(lines.next(), Some("A,1,0.9,1,1.1,1.2,1.3,1.4,1.5,1.6"));
        assert_eq!(lines.next(), None);
        let _ = std::fs::remove_file(&tmp);

        for slot in &mut streams {
            if let Some(s) = slot.take() {
                s.shutdown();
            }
        }
    }

    #[test]
    fn format_tracker_state_renders_each_variant() {
        assert_eq!(format_tracker_state(TrackerState::Searching), "searching");
        assert_eq!(
            format_tracker_state(TrackerState::Tentative { bpm: 128.0 }),
            "tentative @ 128.00 BPM"
        );
        assert_eq!(
            format_tracker_state(TrackerState::Locked { bpm: 174.567 }),
            "LOCKED @ 174.57 BPM"
        );
    }
}
