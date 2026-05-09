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

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("smoke");

    let result = match cmd {
        "smoke" => smoke(),
        "rt-audit" => rt_audit(),
        "version" => version(),
        "play" => play(&args[2..]),
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
    eprintln!("  play <input>      [--realtime] [-o <output>] [--rate R] [--gain G]");
    eprintln!("                    [--sr SR] [--block-size N] [--duration SECS]");
    eprintln!("                    [--buffer-size FRAMES]");
    eprintln!("                    [--pause-at SECS] [--resume-at SECS]");
    eprintln!("                    [--seek-at WALL=POS_SECS]");
    eprintln!();
    eprintln!("  play (offline, default): render to a 32-bit float WAV through the engine.");
    eprintln!("  play --realtime:         play through the default macOS output device.");
    eprintln!("    --pause-at, --resume-at, --seek-at drive the engine's lock-free");
    eprintln!("    command channel (M2 transport) from the main thread while audio runs.");
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

#[derive(Debug)]
struct PlayOpts {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    rate: f64,
    gain: f32,
    engine_sr: Option<f32>,
    block_size: usize,
    realtime: bool,
    duration: Option<f64>,
    buffer_size: Option<u32>,
    /// Wall-clock seconds (since playback start) at which to send a
    /// pause command. M2 demo for the lock-free command channel.
    pause_at: Option<f64>,
    /// Wall-clock seconds at which to send a play (resume) command.
    resume_at: Option<f64>,
    /// `WALL=POS` — at `WALL` wall-clock seconds, seek deck 0 to `POS`
    /// track-seconds. M2 demo of the seek command.
    seek_at: Option<(f64, f64)>,
}

impl Default for PlayOpts {
    fn default() -> Self {
        Self {
            input: None,
            output: None,
            rate: 1.0,
            gain: 1.0,
            engine_sr: None,
            block_size: 64,
            realtime: false,
            duration: None,
            buffer_size: None,
            pause_at: None,
            resume_at: None,
            seek_at: None,
        }
    }
}

impl PlayOpts {
    fn parse(args: &[String]) -> Result<Self> {
        let mut opts = Self::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-o" | "--output" => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--output expects a value"))?;
                    opts.output = Some(PathBuf::from(v));
                    i += 2;
                }
                "--rate" => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--rate expects a value"))?;
                    opts.rate = v.parse().context("--rate not a number")?;
                    i += 2;
                }
                "--gain" => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--gain expects a value"))?;
                    opts.gain = v.parse().context("--gain not a number")?;
                    i += 2;
                }
                "--sr" => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--sr expects a value"))?;
                    opts.engine_sr = Some(v.parse().context("--sr not a number")?);
                    i += 2;
                }
                "--block-size" => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--block-size expects a value"))?;
                    opts.block_size = v.parse().context("--block-size not a number")?;
                    i += 2;
                }
                "--realtime" | "--live" => {
                    opts.realtime = true;
                    i += 1;
                }
                "--duration" => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--duration expects seconds"))?;
                    opts.duration = Some(v.parse().context("--duration not a number")?);
                    i += 2;
                }
                "--buffer-size" => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--buffer-size expects frames"))?;
                    opts.buffer_size = Some(v.parse().context("--buffer-size not an integer")?);
                    i += 2;
                }
                "--pause-at" => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--pause-at expects seconds"))?;
                    opts.pause_at = Some(v.parse().context("--pause-at not a number")?);
                    i += 2;
                }
                "--resume-at" => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--resume-at expects seconds"))?;
                    opts.resume_at = Some(v.parse().context("--resume-at not a number")?);
                    i += 2;
                }
                "--seek-at" => {
                    let v = args
                        .get(i + 1)
                        .ok_or_else(|| anyhow!("--seek-at expects WALL=POS in seconds"))?;
                    let (wall, pos) = v
                        .split_once('=')
                        .ok_or_else(|| anyhow!("--seek-at expects WALL=POS, got {v}"))?;
                    let wall: f64 = wall.parse().context("--seek-at WALL not a number")?;
                    let pos: f64 = pos.parse().context("--seek-at POS not a number")?;
                    opts.seek_at = Some((wall, pos));
                    i += 2;
                }
                s if s.starts_with('-') => {
                    return Err(anyhow!("unknown flag: {s}"));
                }
                _ => {
                    if opts.input.is_none() {
                        opts.input = Some(PathBuf::from(&args[i]));
                    } else {
                        return Err(anyhow!("unexpected positional arg: {}", args[i]));
                    }
                    i += 1;
                }
            }
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

fn play(args: &[String]) -> Result<()> {
    let opts = PlayOpts::parse(args)?;
    let input = opts
        .input
        .clone()
        .ok_or_else(|| anyhow!("usage: dub play <input> [--realtime] [-o <output>] ..."))?;

    let track = Track::load_from_path(&input).context("loading input")?;
    println!(
        "track loaded: {} frames @ {} Hz, {} ch ({:.3} s)",
        track.frames(),
        track.sample_rate(),
        track.channels(),
        track.duration_seconds()
    );

    if opts.realtime {
        play_realtime(track, &opts)
    } else {
        play_offline(&input, track, &opts)
    }
}

/// Pre-load deck 0 with `track`, set rate/gain/start-position, and
/// mark it playing. Used after both `Engine::new` and
/// `Engine::new_with_handle` so behavior is identical between offline
/// and realtime paths.
fn configure_deck0(engine: &mut Engine, track: &std::sync::Arc<Track>, opts: &PlayOpts) {
    engine.deck_mut(0).set_source(track.clone());
    engine.deck_mut(0).set_gain(opts.gain);
    engine.deck_mut(0).set_rate(opts.rate);
    engine.deck_mut(0).set_playing(true);
    if opts.rate < 0.0 {
        #[allow(clippy::cast_precision_loss)]
        let last = (track.frames().saturating_sub(1)) as f64;
        engine.deck_mut(0).set_position_frames(last);
    }
}

/// Build an engine + deck-0 pre-loaded with `track`, with the requested
/// rate, gain, and starting position (end-of-track if rate is negative).
/// Offline variant — no command channel, single-threaded.
fn build_configured_engine(
    track: Track,
    engine_sr: f32,
    block_size: usize,
    opts: &PlayOpts,
) -> (Engine, std::sync::Arc<Track>) {
    let track = std::sync::Arc::new(track);
    let mut engine = Engine::new(engine_sr, block_size);
    configure_deck0(&mut engine, &track, opts);
    (engine, track)
}

/// Realtime variant — paired with an [`EngineHandle`] for cross-thread
/// transport commands.
fn build_configured_engine_with_handle(
    track: Track,
    engine_sr: f32,
    block_size: usize,
    opts: &PlayOpts,
) -> (Engine, EngineHandle, std::sync::Arc<Track>) {
    let track = std::sync::Arc::new(track);
    let (mut engine, handle) = Engine::new_with_handle(engine_sr, block_size);
    configure_deck0(&mut engine, &track, opts);
    (engine, handle, track)
}

fn play_offline(input: &Path, track: Track, opts: &PlayOpts) -> Result<()> {
    let output = opts
        .output
        .clone()
        .unwrap_or_else(|| default_output_path(input));
    let engine_sr = opts.engine_sr.unwrap_or(48_000.0);

    println!("mode:         offline");
    println!("  output:     {}", output.display());
    println!("  engine SR:  {engine_sr} Hz");
    println!("  block size: {} frames", opts.block_size);
    println!("  rate:       {}", opts.rate);
    println!("  gain:       {}", opts.gain);

    let track_sr = f64::from(track.sample_rate());
    let track_frames = track.frames();
    let (mut engine, _track_arc) = build_configured_engine(track, engine_sr, opts.block_size, opts);

    let abs_rate = opts.rate.abs().max(1e-12);
    let engine_sr_f = f64::from(engine_sr);
    #[allow(clippy::cast_precision_loss)]
    let track_frames_f = track_frames as f64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let total_output_frames = (track_frames_f * (engine_sr_f / track_sr) / abs_rate).ceil() as u64;
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

    let start = Instant::now();
    for _ in 0..total_blocks {
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
    }
    let elapsed = start.elapsed();
    writer.finalize().context("finalizing output WAV")?;

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
    println!("OK");
    Ok(())
}

fn play_realtime(track: Track, opts: &PlayOpts) -> Result<()> {
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
    println!("  rate:         {}", opts.rate);
    println!("  gain:         {}", opts.gain);

    let track_sr = f64::from(track.sample_rate());
    let track_frames = track.frames();
    let abs_rate = opts.rate.abs().max(1e-12);
    #[allow(clippy::cast_precision_loss)]
    let natural_secs = (track_frames as f64) / track_sr / abs_rate;
    let play_secs = opts.duration.unwrap_or(natural_secs);

    let (engine, mut handle, _track_arc) =
        build_configured_engine_with_handle(track, engine_sr, opts.block_size, opts);

    println!("  playing:      {play_secs:.3} s");

    // Build the transport schedule. Events are sorted by wall-clock time
    // and then fired in order from this (main) thread. Each fire is a
    // single ringbuf push; the audio thread observes the change at the
    // start of its next render block (≤ buffer-size latency later).
    #[allow(clippy::cast_possible_truncation)]
    let schedule = build_transport_schedule(opts, play_secs, track_sr as f32);
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
    for ev in &schedule {
        let dt = (ev.wall_secs() - last_wall).max(0.0);
        thread::sleep(Duration::from_secs_f64(dt));
        ev.fire(&mut handle)?;
        let snap = handle.deck_state(0).unwrap();
        println!(
            "    @{:.3}s applied {:<14} | pos={:.1}fr playing={}",
            ev.wall_secs(),
            ev.describe(),
            snap.position_frames,
            snap.is_playing
        );
        last_wall = ev.wall_secs();
    }
    let remaining = (play_secs - last_wall).max(0.0);
    thread::sleep(Duration::from_secs_f64(remaining));

    let elapsed = start.elapsed();
    let cb_count = output.callback_count();
    let final_snap = handle.deck_state(0).unwrap();
    drop(output);

    println!(
        "  callbacks:    {cb_count} render calls in {:.3} s",
        elapsed.as_secs_f64()
    );
    println!(
        "  final state:  pos={:.1}fr playing={} at_end={}",
        final_snap.position_frames, final_snap.is_playing, final_snap.at_end
    );
    if cb_count == 0 {
        anyhow::bail!("CoreAudio fired zero render callbacks; device probably failed to start");
    }
    println!("OK");
    Ok(())
}

/// One transport event scheduled to fire from the main thread at a given
/// wall-clock offset since playback start.
#[derive(Debug, Clone, Copy)]
enum ScheduledEvent {
    Pause { wall_secs: f64 },
    Resume { wall_secs: f64 },
    Seek { wall_secs: f64, pos_frames: f64 },
}

impl ScheduledEvent {
    fn wall_secs(self) -> f64 {
        match self {
            Self::Pause { wall_secs }
            | Self::Resume { wall_secs }
            | Self::Seek { wall_secs, .. } => wall_secs,
        }
    }

    fn describe(self) -> String {
        match self {
            Self::Pause { .. } => "pause".to_string(),
            Self::Resume { .. } => "resume".to_string(),
            Self::Seek { pos_frames, .. } => format!("seek({pos_frames:.0}fr)"),
        }
    }

    fn fire(self, handle: &mut EngineHandle) -> Result<()> {
        match self {
            Self::Pause { .. } => handle.deck(0).pause()?,
            Self::Resume { .. } => handle.deck(0).play()?,
            Self::Seek { pos_frames, .. } => handle.deck(0).seek(pos_frames)?,
        }
        Ok(())
    }
}

// `wall_secs` is a method, not a struct field, on a Copy enum — wrap it
// for sorting since `partial_cmp` on `Self` returns Option (NaN guard).
struct ScheduledEventList(Vec<ScheduledEvent>);

impl ScheduledEventList {
    fn iter(&self) -> std::slice::Iter<'_, ScheduledEvent> {
        self.0.iter()
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'a> IntoIterator for &'a ScheduledEventList {
    type Item = &'a ScheduledEvent;
    type IntoIter = std::slice::Iter<'a, ScheduledEvent>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

fn build_transport_schedule(opts: &PlayOpts, play_secs: f64, track_sr: f32) -> ScheduledEventList {
    let mut events: Vec<ScheduledEvent> = Vec::new();
    if let Some(t) = opts.pause_at {
        if t >= 0.0 && t <= play_secs {
            events.push(ScheduledEvent::Pause { wall_secs: t });
        }
    }
    if let Some(t) = opts.resume_at {
        if t >= 0.0 && t <= play_secs {
            events.push(ScheduledEvent::Resume { wall_secs: t });
        }
    }
    if let Some((wall, pos_secs)) = opts.seek_at {
        if wall >= 0.0 && wall <= play_secs {
            let pos_frames = pos_secs * f64::from(track_sr);
            events.push(ScheduledEvent::Seek {
                wall_secs: wall,
                pos_frames,
            });
        }
    }
    events.sort_by(|a, b| {
        a.wall_secs()
            .partial_cmp(&b.wall_secs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ScheduledEventList(events)
}
