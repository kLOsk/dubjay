//! Headless smoke test, offline render harness, and real-time playback
//! driver for the Dub engine.
//!
//! Subcommands:
//!
//! - `smoke` — verify the engine constructs and renders a block of silence.
//! - `rt-audit` — render N blocks and print the wall-clock time + tick count.
//! - `version` — print engine + ffi version.
//! - `play <input> [--realtime] [-o <output>] [--rate R] [--gain G]
//!         [--sr ENGINE_SR] [--block-size N] [--duration SECS]` —
//!   load `<input>` and either:
//!
//!   - **offline** (default): render through the engine into `<output>`
//!     (default `<input>.dub.wav`). Deterministic, used by automated tests.
//!   - **realtime** (`--realtime`): play through the default audio output
//!     for up to the track's duration (or `--duration`).

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use dub_engine::{Engine, EngineHandle, RealtimeContext};
use dub_io::Track;

mod analyze;
mod decode_timecode;
mod input_cmds;
mod scope;
mod timecode_deck;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("smoke");

    let result = match cmd {
        "smoke" => smoke(),
        "rt-audit" => rt_audit(),
        "version" => version(),
        "play" => play(&args[2..]),
        "analyze" => analyze_cmd(&args[2..]),
        "decode-timecode" => decode_timecode_cmd(&args[2..]),
        "list-inputs" => input_cmds::list_inputs(),
        "levels" => input_cmds::levels(&args[2..]),
        "capture" => input_cmds::capture(&args[2..]),
        "timecode-deck" => timecode_deck::run(&args[2..]),
        "scope" => scope::run(&args[2..]),
        "measure-latency" => measure_latency(),
        "help" | "-h" | "--help" => {
            print_help();
            return ExitCode::SUCCESS;
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            print_help();
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    eprintln!("usage: dub <subcommand> [args]");
    eprintln!();
    eprintln!("subcommands:");
    eprintln!("  smoke             engine handshake + zero-render");
    eprintln!("  rt-audit          stress the render path under assert_no_alloc");
    eprintln!("  version           print versions");
    eprintln!("  measure-latency   query the default output device for SR + buffer + latency");
    eprintln!("  list-inputs       enumerate audio input devices (M5.2)");
    eprintln!("  levels            [--device NAME] [--channels N] [--buffer-size F]");
    eprintln!("                    [--input-channels N,M] [--sr SR] [--duration SECS]");
    eprintln!("                                                  live RMS meters per ch");
    eprintln!("  capture <wav>     [--device NAME] [--channels N] [--buffer-size F]");
    eprintln!("                    [--input-channels N,M] [--sr SR] [--duration SECS]");
    eprintln!("                                                  record input device to WAV");
    eprintln!("                    --input-channels uses 1-based indices: SL3 deck A = 3,4");
    eprintln!("  play <deck-a> [<deck-b>]");
    eprintln!("                    [--realtime] [-o <output>] [--sr SR] [--block-size N]");
    eprintln!("                    [--duration SECS] [--buffer-size FRAMES]");
    eprintln!("                    [--master-gain G] [--master-gain-at WALL=G]");
    eprintln!("                    [--rate R] [--gain G]                  (deck A defaults)");
    eprintln!("                    [--pause-at SECS] [--resume-at SECS]   (deck A)");
    eprintln!("                    [--seek-at WALL=POS_SECS]              (deck A)");
    eprintln!("                    [--hot-swap-at WALL=PATH]              (deck A)");
    eprintln!("                    [--deck-b-rate R] [--deck-b-gain G]");
    eprintln!("                    [--deck-b-pause-at SECS] [--deck-b-resume-at SECS]");
    eprintln!("                    [--deck-b-seek-at WALL=POS_SECS]");
    eprintln!("                    [--deck-b-hot-swap-at WALL=PATH]");
    eprintln!("  analyze <wav>     [--threshold DELTA]   sample-discontinuity auditor");
    eprintln!("  decode-timecode <wav>");
    eprintln!("                    [--window MS] [--head N]");
    eprintln!("                    [--synthetic]   offline timecode-vinyl decoder (M5.1)");
    eprintln!("  timecode-deck <track>");
    eprintln!("                    --input-channels N,M [--device NAME] [--sr SR]");
    eprintln!("                    [--duration SECS] [--confidence T]");
    eprintln!("                    [--disengage-threshold T] [--sticky-blocks N]");
    eprintln!("                    [--amplitude-threshold T] [--output-buffer-size FRAMES]");
    eprintln!("                                    live timecode \u{2192} deck-0 demo (M5.3)");
    eprintln!("  scope             [--device NAME] [--input-channels N,M] [--sr SR]");
    eprintln!("                    [--buffer-size F] [--duration SECS]");
    eprintln!("                    [--engage T] [--disengage T] [--sticky N]");
    eprintln!("                    [--amplitude T] [--format serato-cv02]");
    eprintln!("                                    live timecode scope (TUI) (M5.4.1)");
    eprintln!();
    eprintln!("  play (offline, default): render the engine output to a 32-bit float WAV.");
    eprintln!("  play --realtime:         play through the default macOS output device.");
    eprintln!();
    eprintln!("    Two-deck (M4): the second positional argument loads onto deck B (engine");
    eprintln!("    deck 1) and is summed with deck A through the debug internal mixer.");
    eprintln!("    --master-gain scales the summed bus; --master-gain-at WALL=G schedules");
    eprintln!("    a master-gain change. Per-deck transport flags use the --deck-b- prefix");
    eprintln!("    for deck B; bare flags target deck A for backward compat with single-deck");
    eprintln!("    usage.");
    eprintln!();
    eprintln!("    Scheduled events (--*-at) work in BOTH offline and realtime modes —");
    eprintln!("    offline uses a virtual wall-clock derived from rendered frames so results");
    eprintln!("    are fully deterministic and analyzable via `dub analyze`.");
    eprintln!();
    eprintln!("  analyze: read a 32-bit float WAV (e.g. `dub play -o ...`) and report");
    eprintln!("    peak/RMS/DC, clipping count, max per-sample first-difference per");
    eprintln!("    channel, and locations where |Δ| exceeds --threshold (default 0.05).");
    eprintln!("    Use this instead of subjective listening to verify de-click correctness.");
    eprintln!();
    eprintln!("  decode-timecode (M5.1): read a stereo WAV containing recorded Serato CV02");
    eprintln!("    timecode and report decoded rate / position / amplitude / confidence in");
    eprintln!("    discrete time slices. Verdict heuristic LOCKED / PARTIAL / POOR. With");
    eprintln!("    --synthetic and no input path, decodes a built-in test scenario instead");
    eprintln!("    (sanity-check the decoder math without a turntable).");
    eprintln!();
    eprintln!("  timecode-deck (M5.3): live wiring — open the input device, route timecode");
    eprintln!("    through the dub-timecode decoder into deck 0's transport, play the loaded");
    eprintln!("    track through the default output. Forward platter motion plays forward,");
    eprintln!("    scratching scratches, lifting the stylus mutes the deck. Lift is detected");
    eprintln!("    via three layers: (1) amplitude gate — RMS below --amplitude-threshold means");
    eprintln!("    carrier is dead regardless of confidence (catches lift's quiet-but-coherent");
    eprintln!("    handling-noise); (2) confidence hysteresis — engage at --confidence, lukewarm");
    eprintln!("    band down to --disengage-threshold rides scratch transients; (3) sticky");
    eprintln!("    window — --sticky-blocks consecutive below-floor blocks before muting.");
    eprintln!("    Output device SR is forced to engine SR so playback runs on a single clock");
    eprintln!("    — no SRC. SL3 deck A: --input-channels 3,4. Default duration 60 s; Ctrl-C");
    eprintln!("    to stop.");
    eprintln!();
    eprintln!("  scope (M5.4.1): live timecode-vinyl inspector. Opens the input device,");
    eprintln!("    decodes timecode in real time, and runs the same lift policy as");
    eprintln!("    timecode-deck (so what you see here is what you'd hear). The TUI shows:");
    eprintln!("      - Lissajous (X=L, Y=R): clean carrier traces a circle; lift collapses it.");
    eprintln!("      - rate, confidence (gauge), amplitude (gauge), position, sticky countdown.");
    eprintln!("      - Live thresholds, mutable in-place via arrow keys for tuning your rig.");
    eprintln!("    Key bindings: q/Esc quit, c clear lissajous, \u{2191}/\u{2193} engage,");
    eprintln!("    PgUp/PgDn disengage, \u{2190}/\u{2192} amplitude. SL3 deck A:");
    eprintln!("    --input-channels 3,4. Calibration UX in M5.4.2 will persist these.");
}

fn smoke() -> Result<()> {
    println!("Dub CLI smoke test");
    println!("  engine version: {}", dub_engine::VERSION);
    println!("  io version:     {}", dub_io::VERSION);
    println!("  ffi version:    {}", dub_ffi::FFI_VERSION);
    println!("  ffi greeting:   {}", dub_ffi::greeting());

    let mut engine = Engine::new(48_000.0, 64);
    let mut buffer = vec![1.0f32; 128];
    let mut rt = RealtimeContext::new();

    engine.render(&mut rt, &mut buffer);

    let nonzero = buffer.iter().filter(|s| **s != 0.0).count();
    if nonzero != 0 {
        anyhow::bail!("expected silent render, got {nonzero} non-zero samples");
    }

    println!("  rendered:       1 block, 64 frames stereo, all-zero output OK");
    println!("OK");
    Ok(())
}

fn rt_audit() -> Result<()> {
    const BLOCKS: u64 = 10_000;
    const SAMPLE_RATE: f32 = 48_000.0;
    const BLOCK_SIZE: usize = 64;

    println!("Dub CLI rt-audit");
    println!("  rendering {BLOCKS} blocks of {BLOCK_SIZE} stereo frames @ {SAMPLE_RATE} Hz");

    let mut engine = Engine::new(SAMPLE_RATE, BLOCK_SIZE);
    let mut buffer = vec![0.0f32; 2 * BLOCK_SIZE];
    let mut rt = RealtimeContext::new();

    let start = Instant::now();
    for _ in 0..BLOCKS {
        engine.render(&mut rt, &mut buffer);
    }
    let elapsed = start.elapsed();

    let total_seconds = (BLOCKS as f32 * BLOCK_SIZE as f32) / SAMPLE_RATE;
    let wall_seconds = elapsed.as_secs_f32();
    let realtime_factor = total_seconds / wall_seconds;

    println!("  ticks observed: {}", rt.ticks());
    println!("  rendered audio: {total_seconds:.3} s");
    println!("  wall time:      {wall_seconds:.6} s");
    println!("  realtime ×{realtime_factor:.0}");
    println!("OK");
    Ok(())
}

fn version() -> Result<()> {
    println!("dub-cli   {}", env!("CARGO_PKG_VERSION"));
    println!("dub-engine {}", dub_engine::VERSION);
    println!("dub-io    {}", dub_io::VERSION);
    println!("dub-ffi   {}", dub_ffi::FFI_VERSION);
    Ok(())
}

fn measure_latency() -> Result<()> {
    let info = dub_audio::query_default_output().context("querying default output")?;
    let latency_ms = f64::from(info.buffer_frames) / f64::from(info.sample_rate) * 1000.0;

    println!("default output device:");
    println!("  sample rate:      {} Hz", info.sample_rate);
    println!("  channels:         {}", info.channels);
    println!("  buffer (current): {} frames", info.buffer_frames);
    #[cfg(target_os = "macos")]
    println!(
        "  buffer (range):   {}-{} frames",
        info.buffer_frame_range.min, info.buffer_frame_range.max
    );
    println!("  latency:          {latency_ms:.2} ms (output buffer only)");

    // Echo what each common buffer size would mean at the device's SR.
    println!();
    println!("latency at common buffer sizes (this device's SR):");
    for &n in &[64u32, 128, 256, 512, 1024] {
        let ms = f64::from(n) / f64::from(info.sample_rate) * 1000.0;
        println!("  {n:>4} frames -> {ms:6.2} ms");
    }

    if latency_ms < 8.0 {
        println!("\nOK ({latency_ms:.2} ms < 8 ms PRD target)");
    } else {
        println!(
            "\nNOTE: current device buffer ({:.2} ms) exceeds the <8 ms PRD target.",
            latency_ms
        );
        println!("Try `dub play --realtime --buffer-size 256` to request a smaller buffer.");
    }
    Ok(())
}

/// Per-deck CLI options. Two of these live inside [`PlayOpts`] — one
/// for deck A (engine deck 0) and one for deck B (engine deck 1).
///
/// Backward-compat: the bare `--rate`, `--gain`, `--pause-at`,
/// `--resume-at`, `--seek-at`, `--hot-swap-at` flags target deck A
/// (matches the single-deck CLI shipped through M3.5). The deck-B
/// equivalents use the `--deck-b-*` prefix.
#[derive(Debug, Clone, Default)]
struct DeckOpts {
    input: Option<PathBuf>,
    rate: Option<f64>,
    gain: Option<f32>,
    pause_at: Option<f64>,
    resume_at: Option<f64>,
    seek_at: Option<(f64, f64)>,
    hot_swap_at: Option<(f64, PathBuf)>,
}

impl DeckOpts {
    fn rate_or(&self, default: f64) -> f64 {
        self.rate.unwrap_or(default)
    }
    fn gain_or(&self, default: f32) -> f32 {
        self.gain.unwrap_or(default)
    }
}

struct PlayOpts {
    deck_a: DeckOpts,
    deck_b: DeckOpts,
    output: Option<PathBuf>,
    engine_sr: Option<f32>,
    block_size: usize,
    realtime: bool,
    duration: Option<f64>,
    buffer_size: Option<u32>,
    /// Initial master gain on the debug internal mixer. M4 addition.
    master_gain: f32,
    /// `WALL=G` — schedule a master-gain change at `WALL` seconds.
    master_gain_at: Option<(f64, f32)>,
}

impl Default for PlayOpts {
    fn default() -> Self {
        Self {
            deck_a: DeckOpts::default(),
            deck_b: DeckOpts::default(),
            output: None,
            engine_sr: None,
            block_size: 64,
            realtime: false,
            duration: None,
            buffer_size: None,
            master_gain: 1.0,
            master_gain_at: None,
        }
    }
}

/// Read the next argument's string value, with a clearer error than
/// `args.get(i+1).ok_or_else(...)` repeated everywhere.
fn next_value<'a>(args: &'a [String], i: usize, flag: &str) -> Result<&'a str> {
    args.get(i + 1)
        .map(String::as_str)
        .ok_or_else(|| anyhow!("{flag} expects a value"))
}

/// Parse a `WALL=VAL` pair where VAL is parsed by `f`.
fn parse_wall_eq<T, F>(s: &str, flag: &str, f: F) -> Result<(f64, T)>
where
    F: FnOnce(&str) -> Result<T>,
{
    let (wall, val) = s
        .split_once('=')
        .ok_or_else(|| anyhow!("{flag} expects WALL=VALUE, got {s}"))?;
    let wall: f64 = wall
        .parse()
        .with_context(|| format!("{flag} WALL not a number"))?;
    let val = f(val)?;
    Ok((wall, val))
}

/// Apply a per-deck flag to the right [`DeckOpts`] based on whether the
/// flag was prefixed with `--deck-b-`. Returns `Ok(true)` if the flag
/// was recognized and consumed (caller should advance `i`); `Ok(false)`
/// if it's a non-deck flag the caller should fall through to handle.
fn parse_deck_flag(
    deck: &mut DeckOpts,
    args: &[String],
    i: usize,
    raw: &str,
    suffix: &str,
) -> Result<bool> {
    match suffix {
        "rate" => {
            let v = next_value(args, i, raw)?;
            deck.rate = Some(v.parse().with_context(|| format!("{raw} not a number"))?);
        }
        "gain" => {
            let v = next_value(args, i, raw)?;
            deck.gain = Some(v.parse().with_context(|| format!("{raw} not a number"))?);
        }
        "pause-at" => {
            let v = next_value(args, i, raw)?;
            deck.pause_at = Some(v.parse().with_context(|| format!("{raw} not a number"))?);
        }
        "resume-at" => {
            let v = next_value(args, i, raw)?;
            deck.resume_at = Some(v.parse().with_context(|| format!("{raw} not a number"))?);
        }
        "seek-at" => {
            let v = next_value(args, i, raw)?;
            let (wall, pos) = parse_wall_eq(v, raw, |s| {
                s.parse::<f64>()
                    .with_context(|| format!("{raw} POS not a number"))
            })?;
            deck.seek_at = Some((wall, pos));
        }
        "hot-swap-at" => {
            let v = next_value(args, i, raw)?;
            let (wall, path) = parse_wall_eq(v, raw, |s| Ok(PathBuf::from(s)))?;
            deck.hot_swap_at = Some((wall, path));
        }
        "input" => {
            let v = next_value(args, i, raw)?;
            deck.input = Some(PathBuf::from(v));
        }
        _ => return Ok(false),
    }
    Ok(true)
}

impl PlayOpts {
    fn parse(args: &[String]) -> Result<Self> {
        let mut opts = Self::default();
        let mut positional: Vec<&str> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let raw = args[i].as_str();
            // Deck-B namespaced flags first (otherwise --deck-b-rate
            // would match nothing and we'd fall through to "unknown").
            if let Some(suffix) = raw.strip_prefix("--deck-b-") {
                if parse_deck_flag(&mut opts.deck_b, args, i, raw, suffix)? {
                    i += 2;
                    continue;
                }
            }
            // Deck-A namespaced flags (explicit). Optional — bare flags
            // below also target deck A.
            if let Some(suffix) = raw.strip_prefix("--deck-a-") {
                if parse_deck_flag(&mut opts.deck_a, args, i, raw, suffix)? {
                    i += 2;
                    continue;
                }
            }
            // Bare flags — engine-wide AND deck-A backward-compat.
            match raw {
                "-o" | "--output" => {
                    opts.output = Some(PathBuf::from(next_value(args, i, raw)?));
                    i += 2;
                }
                "--sr" => {
                    opts.engine_sr = Some(
                        next_value(args, i, raw)?
                            .parse()
                            .context("--sr not a number")?,
                    );
                    i += 2;
                }
                "--block-size" => {
                    opts.block_size = next_value(args, i, raw)?
                        .parse()
                        .context("--block-size not a number")?;
                    i += 2;
                }
                "--realtime" | "--live" => {
                    opts.realtime = true;
                    i += 1;
                }
                "--duration" => {
                    opts.duration = Some(
                        next_value(args, i, raw)?
                            .parse()
                            .context("--duration not a number")?,
                    );
                    i += 2;
                }
                "--buffer-size" => {
                    opts.buffer_size = Some(
                        next_value(args, i, raw)?
                            .parse()
                            .context("--buffer-size not a number")?,
                    );
                    i += 2;
                }
                "--master-gain" => {
                    opts.master_gain = next_value(args, i, raw)?
                        .parse()
                        .context("--master-gain not a number")?;
                    i += 2;
                }
                "--master-gain-at" => {
                    let v = next_value(args, i, raw)?;
                    let (wall, gain) = parse_wall_eq(v, raw, |s| {
                        s.parse::<f32>().context("--master-gain-at G not a number")
                    })?;
                    opts.master_gain_at = Some((wall, gain));
                    i += 2;
                }
                // Bare --rate / --gain / --pause-at / etc. → deck A.
                bare if bare.starts_with("--")
                    && parse_deck_flag(
                        &mut opts.deck_a,
                        args,
                        i,
                        bare,
                        bare.trim_start_matches("--"),
                    )? =>
                {
                    i += 2;
                }
                s if s.starts_with('-') => {
                    return Err(anyhow!("unknown flag: {s}"));
                }
                _ => {
                    positional.push(raw);
                    i += 1;
                }
            }
        }
        // Up to two positional args: first → deck A input, second → deck B.
        if let Some(first) = positional.first() {
            if opts.deck_a.input.is_none() {
                opts.deck_a.input = Some(PathBuf::from(first));
            }
        }
        if let Some(second) = positional.get(1) {
            if opts.deck_b.input.is_none() {
                opts.deck_b.input = Some(PathBuf::from(second));
            }
        }
        if positional.len() > 2 {
            return Err(anyhow!(
                "too many positional args: {} (expected at most 2: <deck-a> [<deck-b>])",
                positional.len()
            ));
        }
        Ok(opts)
    }
}

fn default_output_path(input: &Path) -> PathBuf {
    let mut out = input.to_path_buf();
    let stem = input.file_stem().map_or_else(
        || std::ffi::OsString::from("dub"),
        std::ffi::OsStr::to_os_string,
    );
    let mut name = stem;
    name.push(".dub.wav");
    out.set_file_name(name);
    out
}

fn analyze_cmd(args: &[String]) -> Result<()> {
    let mut input: Option<PathBuf> = None;
    let mut threshold = analyze::DEFAULT_DELTA_THRESHOLD;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--threshold" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| anyhow!("--threshold expects a value"))?;
                threshold = v
                    .parse::<f32>()
                    .with_context(|| format!("--threshold {v}"))?;
            }
            other if other.starts_with("--") => {
                return Err(anyhow!("unknown analyze flag: {other}"));
            }
            other => {
                if input.is_some() {
                    return Err(anyhow!("analyze takes a single input WAV"));
                }
                input = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }
    let input = input.ok_or_else(|| anyhow!("usage: dub analyze <wav> [--threshold DELTA]"))?;
    analyze::run(&input, threshold)
}

fn decode_timecode_cmd(args: &[String]) -> Result<()> {
    let (input, synthetic, window_ms, max_lines) = decode_timecode::parse_args(args)?;
    decode_timecode::run(input.as_deref(), synthetic, window_ms, max_lines)
}

/// One loaded deck ready to be configured into the engine.
struct LoadedDeck {
    /// Engine deck index this maps to (0 for A, 1 for B).
    idx: usize,
    track: std::sync::Arc<Track>,
    rate: f64,
    gain: f32,
}

fn play(args: &[String]) -> Result<()> {
    let opts = PlayOpts::parse(args)?;
    if opts.deck_a.input.is_none() {
        return Err(anyhow!(
            "usage: dub play <deck-a> [<deck-b>] [--realtime] [-o <output>] ..."
        ));
    }

    // Load each deck's track off the audio thread.
    let mut decks: Vec<LoadedDeck> = Vec::new();
    for (idx, deck_opts, label) in [
        (0usize, &opts.deck_a, "deck A"),
        (1usize, &opts.deck_b, "deck B"),
    ] {
        let Some(input) = &deck_opts.input else {
            continue;
        };
        let track =
            Track::load_from_path(input).with_context(|| format!("loading {label} input"))?;
        println!(
            "{label} loaded: {} frames @ {} Hz, {} ch ({:.3} s)  [{}]",
            track.frames(),
            track.sample_rate(),
            track.channels(),
            track.duration_seconds(),
            input.display()
        );
        decks.push(LoadedDeck {
            idx,
            track: std::sync::Arc::new(track),
            rate: deck_opts.rate_or(1.0),
            gain: deck_opts.gain_or(1.0),
        });
    }

    if opts.realtime {
        play_realtime(decks, &opts)
    } else {
        play_offline(decks, &opts)
    }
}

/// Pre-load each deck with its track, set rate/gain/start-position, and
/// mark it playing. Used after both `Engine::new` and
/// `Engine::new_with_handle` so behavior is identical between offline
/// and realtime paths.
fn configure_decks(engine: &mut Engine, decks: &[LoadedDeck], master_gain: f32) {
    engine.set_master_gain(master_gain);
    for d in decks {
        let deck = engine.deck_mut(d.idx);
        deck.set_source(d.track.clone());
        deck.set_gain(d.gain);
        deck.set_rate(d.rate);
        deck.set_playing(true);
        if d.rate < 0.0 {
            #[allow(clippy::cast_precision_loss)]
            let last = (d.track.frames().saturating_sub(1)) as f64;
            deck.set_position_frames(last);
        }
    }
}

/// Build an engine + decks pre-loaded with `decks`, paired with an
/// [`EngineHandle`] for transport commands. Used by both the offline
/// renderer and the realtime CoreAudio player so the same scheduled
/// events apply through either path with bit-identical engine
/// behavior — only the wall-clock timing differs.
fn build_configured_engine_with_handle(
    decks: &[LoadedDeck],
    engine_sr: f32,
    block_size: usize,
    opts: &PlayOpts,
) -> (Engine, EngineHandle) {
    let (mut engine, handle) = Engine::new_with_handle(engine_sr, block_size);
    configure_decks(&mut engine, decks, opts.master_gain);
    (engine, handle)
}

fn play_offline(decks: Vec<LoadedDeck>, opts: &PlayOpts) -> Result<()> {
    let primary_input = opts
        .deck_a
        .input
        .as_deref()
        .or(opts.deck_b.input.as_deref())
        .ok_or_else(|| anyhow!("no input"))?;
    let output = opts
        .output
        .clone()
        .unwrap_or_else(|| default_output_path(primary_input));
    let engine_sr = opts.engine_sr.unwrap_or(48_000.0);
    let engine_sr_f = f64::from(engine_sr);

    println!("mode:         offline");
    println!("  output:     {}", output.display());
    println!("  engine SR:  {engine_sr} Hz");
    println!("  block size: {} frames", opts.block_size);
    println!("  master gain: {}", opts.master_gain);
    for d in &decks {
        let label = if d.idx == 0 { "deck A" } else { "deck B" };
        println!("  {label}: rate={} gain={}", d.rate, d.gain);
    }

    // Render duration: longest natural duration across loaded decks (each
    // deck is at its own track-SR with its own rate), or --duration if set.
    // M4 deviation from the single-deck CLI: we can't preserve "natural
    // length of THE track" because there are now two; deck A wins as a
    // backwards-compatible default unless --duration is specified.
    let primary_deck = decks.iter().find(|d| d.idx == 0).or_else(|| decks.first());
    let natural_secs = primary_deck.map_or(0.0, |d| {
        let track_sr = f64::from(d.track.sample_rate());
        let abs_rate = d.rate.abs().max(1e-12);
        #[allow(clippy::cast_precision_loss)]
        ((d.track.frames() as f64) / track_sr / abs_rate)
    });
    let play_secs = opts.duration.unwrap_or(natural_secs);

    // Use the same Engine/EngineHandle pattern as `play_realtime` so the
    // offline path is bit-deterministic against the same scheduled
    // events. This is essential for analyze-on-output workflows: running
    // a hot-swap scenario through `play_offline` then through
    // `dub analyze` should reveal any sample-discontinuity that the
    // realtime path produced, since the engine code path is identical.
    let (mut engine, mut handle) =
        build_configured_engine_with_handle(&decks, engine_sr, opts.block_size, opts);

    let schedule = build_transport_schedule(opts, play_secs, &decks)?;
    if !schedule.is_empty() {
        println!("  schedule:");
        for ev in &schedule {
            println!("    t={:.3}s {}", ev.wall_secs(), ev.describe());
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let total_output_frames = (play_secs * engine_sr_f).ceil() as u64;
    let total_blocks = total_output_frames.div_ceil(opts.block_size as u64);

    let spec = hound::WavSpec {
        channels: 2,
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        sample_rate: engine_sr.round() as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(&output, spec).context("opening output WAV")?;

    let mut buffer = vec![0.0f32; 2 * opts.block_size];
    let mut rt = RealtimeContext::new();
    let mut peak: f32 = 0.0;
    let mut rms_acc: f64 = 0.0;
    let mut n_samples: u64 = 0;
    let mut frames_rendered: u64 = 0;

    // Schedule iterator: peek ahead to fire events whose virtual
    // wall-clock has been crossed at the START of each block.
    let mut sched_iter = schedule.into_iter().peekable();

    let start = Instant::now();
    for _ in 0..total_blocks {
        #[allow(clippy::cast_precision_loss)]
        let virt_secs = (frames_rendered as f64) / engine_sr_f;
        while let Some(ev) = sched_iter.peek() {
            if ev.wall_secs() <= virt_secs {
                let ev = sched_iter.next().expect("peeked Some");
                let wall = ev.wall_secs();
                let label = ev.describe();
                let target_idx = ev.deck_idx();
                ev.fire(&mut handle)?;
                if let Some(idx) = target_idx {
                    let snap = handle.deck_state(idx).unwrap();
                    let dlabel = if idx == 0 { "A" } else { "B" };
                    println!(
                        "    @{wall:.3}s applied {label:<24} | deck {dlabel} pos={:.1}fr playing={}",
                        snap.position_frames, snap.is_playing
                    );
                } else {
                    println!("    @{wall:.3}s applied {label}");
                }
            } else {
                break;
            }
        }

        engine.render(&mut rt, &mut buffer);
        for sample in &buffer {
            writer.write_sample(*sample).context("writing sample")?;
            let abs = sample.abs();
            if abs > peak {
                peak = abs;
            }
            rms_acc += f64::from(*sample) * f64::from(*sample);
            n_samples += 1;
        }
        frames_rendered += opts.block_size as u64;
    }
    let elapsed = start.elapsed();
    writer.finalize().context("finalizing output WAV")?;

    let reclaimed = handle.reclaim();
    let overflow = handle.trash_overflow_count();

    #[allow(clippy::cast_precision_loss)]
    let rms = (rms_acc / (n_samples as f64).max(1.0)).sqrt();
    #[allow(clippy::cast_precision_loss)]
    let total_output_secs = (n_samples as f64 / 2.0) / engine_sr_f;
    let realtime_factor = total_output_secs / elapsed.as_secs_f64().max(1e-12);

    println!("  rendered:   {total_blocks} blocks, {n_samples} samples");
    println!("  output dur: {total_output_secs:.3} s");
    println!("  wall:       {:.3} ms", elapsed.as_secs_f64() * 1000.0);
    println!("  realtime ×{realtime_factor:.0}");
    println!("  peak:       {peak:.4} ({:.2} dBFS)", 20.0 * peak.log10());
    println!("  rms:        {rms:.4} ({:.2} dBFS)", 20.0 * rms.log10());
    if reclaimed > 0 || overflow > 0 {
        println!("  trash:        reclaimed={reclaimed} overflow={overflow}");
    }
    println!("OK");
    Ok(())
}

fn play_realtime(decks: Vec<LoadedDeck>, opts: &PlayOpts) -> Result<()> {
    use std::thread;
    use std::time::Duration;

    println!("mode:         realtime (CoreAudio default output)");

    // PRD §4.1.5: the engine matches the device, never the other way
    // around (no boundary resampling in v1). Query the device first so
    // the engine can be built at the right rate.
    let device = dub_audio::query_default_output().context("querying default audio output")?;
    let engine_sr = opts.engine_sr.unwrap_or(device.sample_rate);
    if (engine_sr - device.sample_rate).abs() > 0.5 {
        eprintln!(
            "warning: --sr {engine_sr} differs from device SR {} Hz; CoreAudio will SRC internally",
            device.sample_rate
        );
    }

    println!("  device SR:    {} Hz", device.sample_rate);
    println!("  engine SR:    {engine_sr} Hz");
    println!("  device buffer (current): {} frames", device.buffer_frames);
    if let Some(req) = opts.buffer_size {
        println!("  buffer (req): {req} frames");
    }
    println!("  master gain:  {}", opts.master_gain);
    for d in &decks {
        let label = if d.idx == 0 { "deck A" } else { "deck B" };
        println!("  {label}: rate={} gain={}", d.rate, d.gain);
    }

    let primary_deck = decks.iter().find(|d| d.idx == 0).or_else(|| decks.first());
    let natural_secs = primary_deck.map_or(0.0, |d| {
        let track_sr = f64::from(d.track.sample_rate());
        let abs_rate = d.rate.abs().max(1e-12);
        #[allow(clippy::cast_precision_loss)]
        ((d.track.frames() as f64) / track_sr / abs_rate)
    });
    let play_secs = opts.duration.unwrap_or(natural_secs);

    let (engine, mut handle) =
        build_configured_engine_with_handle(&decks, engine_sr, opts.block_size, opts);

    println!("  playing:      {play_secs:.3} s");

    // Build the transport schedule. Events are sorted by wall-clock time
    // and then fired in order from this (main) thread. Each fire is a
    // single ringbuf push; the audio thread observes the change at the
    // start of its next render block (≤ buffer-size latency later).
    let schedule = build_transport_schedule(opts, play_secs, &decks)?;
    if !schedule.is_empty() {
        println!("  schedule:");
        for ev in &schedule {
            println!("    t={:.3}s {}", ev.wall_secs(), ev.describe());
        }
    }

    let start = Instant::now();
    let output = dub_audio::AudioOutput::start_with_buffer_size(engine, opts.buffer_size)
        .context("starting CoreAudio output")?;
    let achieved = output.buffer_frames();
    let latency_ms = output.latency_seconds() * 1000.0;
    println!("  buffer (act): {achieved} frames -> {latency_ms:.2} ms one-way");

    // Sleep up to the first scheduled event, fire it, sleep to the next,
    // and so on. Every command is sent from this thread, never from the
    // audio thread.
    let mut last_wall = 0.0f64;
    for ev in schedule {
        let dt = (ev.wall_secs() - last_wall).max(0.0);
        thread::sleep(Duration::from_secs_f64(dt));
        let wall = ev.wall_secs();
        let label = ev.describe();
        let target_idx = ev.deck_idx();
        ev.fire(&mut handle)?;
        if let Some(idx) = target_idx {
            let snap = handle.deck_state(idx).unwrap();
            let dlabel = if idx == 0 { "A" } else { "B" };
            println!(
                "    @{wall:.3}s applied {label:<24} | deck {dlabel} pos={:.1}fr playing={}",
                snap.position_frames, snap.is_playing
            );
        } else {
            println!("    @{wall:.3}s applied {label}");
        }
        last_wall = wall;
    }
    let remaining = (play_secs - last_wall).max(0.0);
    thread::sleep(Duration::from_secs_f64(remaining));

    let elapsed = start.elapsed();
    let cb_count = output.callback_count();
    let snap_a = handle.deck_state(0).unwrap();
    let snap_b = handle.deck_state(1).unwrap();
    let reclaimed = handle.reclaim();
    let overflow = handle.trash_overflow_count();
    drop(output);

    println!(
        "  callbacks:    {cb_count} render calls in {:.3} s",
        elapsed.as_secs_f64()
    );
    println!(
        "  deck A final: pos={:.1}fr playing={} at_end={}",
        snap_a.position_frames, snap_a.is_playing, snap_a.at_end
    );
    println!(
        "  deck B final: pos={:.1}fr playing={} at_end={}",
        snap_b.position_frames, snap_b.is_playing, snap_b.at_end
    );
    if reclaimed > 0 || overflow > 0 {
        println!("  trash:        reclaimed={reclaimed} overflow={overflow}");
        if overflow > 0 {
            eprintln!(
                "warning: trash channel overflowed {overflow} times — old Arc<Track> leaked. \
                 The UI must call EngineHandle::reclaim() more frequently."
            );
        }
    }
    if cb_count == 0 {
        anyhow::bail!("CoreAudio fired zero render callbacks; device probably failed to start");
    }
    println!("OK");
    Ok(())
}

/// One transport event scheduled to fire from the main thread at a given
/// wall-clock offset since playback start.
///
/// Most variants name a target deck via `deck` (0 = A, 1 = B). The
/// engine-wide `SetMasterGain` variant has no deck since the debug
/// internal mixer's master applies to the whole summed bus.
///
/// `HotSwap` carries an `Arc<Track>`, so the enum itself is not `Copy`.
/// The list is consumed by value when fired so the Arc moves into the
/// load command without being cloned.
#[derive(Debug)]
enum ScheduledEvent {
    Pause {
        wall_secs: f64,
        deck: u8,
    },
    Resume {
        wall_secs: f64,
        deck: u8,
    },
    Seek {
        wall_secs: f64,
        deck: u8,
        pos_frames: f64,
    },
    HotSwap {
        wall_secs: f64,
        deck: u8,
        source: std::sync::Arc<Track>,
        path: PathBuf,
    },
    /// Engine-wide master gain change on the debug internal mixer.
    SetMasterGain {
        wall_secs: f64,
        gain: f32,
    },
}

impl ScheduledEvent {
    fn wall_secs(&self) -> f64 {
        match self {
            Self::Pause { wall_secs, .. }
            | Self::Resume { wall_secs, .. }
            | Self::Seek { wall_secs, .. }
            | Self::HotSwap { wall_secs, .. }
            | Self::SetMasterGain { wall_secs, .. } => *wall_secs,
        }
    }

    /// Index of the deck this event targets, or `None` for engine-wide
    /// events (e.g. master gain).
    fn deck_idx(&self) -> Option<usize> {
        match self {
            Self::Pause { deck, .. }
            | Self::Resume { deck, .. }
            | Self::Seek { deck, .. }
            | Self::HotSwap { deck, .. } => Some(*deck as usize),
            Self::SetMasterGain { .. } => None,
        }
    }

    fn describe(&self) -> String {
        let dlabel = |d: u8| if d == 0 { 'A' } else { 'B' };
        match self {
            Self::Pause { deck, .. } => format!("pause({})", dlabel(*deck)),
            Self::Resume { deck, .. } => format!("resume({})", dlabel(*deck)),
            Self::Seek {
                deck, pos_frames, ..
            } => format!("seek({},{:.0}fr)", dlabel(*deck), pos_frames),
            Self::HotSwap { deck, path, .. } => format!(
                "load({},{})",
                dlabel(*deck),
                path.file_name().and_then(|s| s.to_str()).unwrap_or("?")
            ),
            Self::SetMasterGain { gain, .. } => format!("master_gain({gain:.3})"),
        }
    }

    fn fire(self, handle: &mut EngineHandle) -> Result<()> {
        match self {
            Self::Pause { deck, .. } => handle.deck(deck as usize).pause()?,
            Self::Resume { deck, .. } => handle.deck(deck as usize).play()?,
            Self::Seek {
                deck, pos_frames, ..
            } => handle.deck(deck as usize).seek(pos_frames)?,
            Self::HotSwap { deck, source, .. } => handle
                .deck(deck as usize)
                .load(source)
                .map_err(|(e, _arc)| e)
                .context("hot-load command rejected")?,
            Self::SetMasterGain { gain, .. } => handle.set_master_gain(gain)?,
        }
        Ok(())
    }
}

/// Build the full schedule: per-deck transport events from each
/// [`DeckOpts`] plus engine-wide master-gain changes. Deck-B's
/// seek-at uses deck-B's track sample-rate to convert position
/// seconds → frames; same for deck A. Events are sorted by wall-clock
/// time before being returned so the realtime path can sleep between
/// them.
fn build_transport_schedule(
    opts: &PlayOpts,
    play_secs: f64,
    decks: &[LoadedDeck],
) -> Result<Vec<ScheduledEvent>> {
    let mut events: Vec<ScheduledEvent> = Vec::new();

    // Per-deck events. Walk both deck specs.
    for (deck_u8, dopts) in [(0u8, &opts.deck_a), (1u8, &opts.deck_b)] {
        let deck_track_sr = decks
            .iter()
            .find(|d| d.idx == deck_u8 as usize)
            .map_or(48_000.0, |d| f64::from(d.track.sample_rate()));

        if let Some(t) = dopts.pause_at {
            if t >= 0.0 && t <= play_secs {
                events.push(ScheduledEvent::Pause {
                    wall_secs: t,
                    deck: deck_u8,
                });
            }
        }
        if let Some(t) = dopts.resume_at {
            if t >= 0.0 && t <= play_secs {
                events.push(ScheduledEvent::Resume {
                    wall_secs: t,
                    deck: deck_u8,
                });
            }
        }
        if let Some((wall, pos_secs)) = dopts.seek_at {
            if wall >= 0.0 && wall <= play_secs {
                let pos_frames = pos_secs * deck_track_sr;
                events.push(ScheduledEvent::Seek {
                    wall_secs: wall,
                    deck: deck_u8,
                    pos_frames,
                });
            }
        }
        if let Some((wall, ref path)) = dopts.hot_swap_at {
            if wall >= 0.0 && wall <= play_secs {
                // Decode now (off the audio thread, on a not-yet-running
                // playback) so the wall-clock fire is just a ringbuf push.
                let track = Track::load_from_path(path)
                    .with_context(|| format!("decoding hot-swap source {}", path.display()))?;
                events.push(ScheduledEvent::HotSwap {
                    wall_secs: wall,
                    deck: deck_u8,
                    source: std::sync::Arc::new(track),
                    path: path.clone(),
                });
            }
        }
    }

    if let Some((wall, gain)) = opts.master_gain_at {
        if wall >= 0.0 && wall <= play_secs {
            events.push(ScheduledEvent::SetMasterGain {
                wall_secs: wall,
                gain,
            });
        }
    }

    events.sort_by(|a, b| {
        a.wall_secs()
            .partial_cmp(&b.wall_secs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(events)
}
