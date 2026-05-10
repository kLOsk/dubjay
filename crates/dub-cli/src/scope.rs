//! `dub scope` — live timecode-vinyl inspector (M5.4.1).
//!
//! Open the input device, decode timecode in real time, run the same
//! [`LiftPolicy`] state machine the engine uses in `dub timecode-deck`,
//! and render the result as a ratatui TUI:
//!
//! - **Lissajous** (left): X = L channel, Y = R channel. A clean
//!   carrier traces a near-perfect circle. A scratch tilts and
//!   distorts it; a lift collapses it to a noisy blob near the
//!   origin.
//! - **Status bars** (right): rate, confidence (gauge), amplitude
//!   (gauge), position, sticky countdown, configured thresholds.
//! - **Engagement state**: `[LOCKED]` / `[LIFT]` colored badge so the
//!   user can tell at a glance whether the policy thinks the
//!   carrier is alive.
//!
//! Live key bindings let the user tweak the engage / disengage /
//! amplitude thresholds in real time without restarting — this is
//! the "calibration sandbox" for M5.4.2 to plug into. The eventual
//! `dub calibrate` will measure recommended thresholds; `dub scope`
//! lets the user confirm them with their own ears + eyes before
//! persisting.
//!
//! ## Architecture
//!
//! Single-threaded. Audio comes off CoreAudio's IOProc into the
//! [`AudioInput`] ringbuf; we drain it from the main thread. No RT
//! constraints apply here — the main thread is allowed to allocate,
//! lock, and call back into ratatui's rendering. The IOProc itself
//! is RT-safe (M5.2 invariant) and unaffected by what we do here.
//!
//! Block size is fixed at [`BLOCK_FRAMES`] to match the engine's
//! default. This matters because the [`LiftPolicy`]'s sticky window
//! is measured in *blocks*, not seconds: if we call `policy.step` at
//! a different cadence than `dub timecode-deck` does, the user
//! cannot trust scope thresholds to transfer to playback. The
//! contract is: same block size → same policy behavior.

use std::io;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use dub_audio::{AudioInput, InputOptions};
use dub_engine::{
    LiftIntent, LiftPolicy, TimecodeInputConfig, DEFAULT_AMPLITUDE_THRESHOLD,
    DEFAULT_CONFIDENCE_THRESHOLD, DEFAULT_DISENGAGE_THRESHOLD, DEFAULT_STICKY_BLOCKS_TO_DISENGAGE,
};
use dub_timecode::{DecodeOutput, Decoder, Format};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Points};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;

use crate::input_cmds::parse_input_args;

/// Block size used for both the decoder and the lift policy. Matches
/// the [`TimecodeInputConfig::max_block_frames`] default (4096) only
/// in upper bound; the decoder/policy are happy with any non-zero
/// even count, but stickiness is *block*-counted, so we want the
/// scope's per-step semantics to match `dub timecode-deck`. 256 fr @
/// 48 kHz = 5.33 ms, sticky=4 = 21 ms — same as production playback.
const BLOCK_FRAMES: usize = 256;

/// Capacity of the Lissajous trail (frame pairs). At 48 kHz the
/// carrier circle completes once per 1/1000 s ≈ 48 frames; 1024 frames
/// shows ~21 ms of history (~21 carrier cycles), enough to make the
/// circle visually steady without smearing slow rate changes.
const LISSAJOUS_CAPACITY: usize = 1024;

/// Frame rate cap for the TUI. 30 Hz is smooth for the eye and
/// well below CoreAudio's IOProc rate, so the input ringbuf never
/// runs dry between frames at default block sizes.
const TARGET_FPS: u64 = 30;

/// Minimum time between key actions that mutate thresholds — debounces
/// auto-repeat on held keys so a single press doesn't slide the bar
/// across the whole range. 50 ms ≈ 1 step / two render frames.
const KEY_REPEAT_DEBOUNCE_MS: u64 = 50;

/// Step size for engage/disengage threshold tweaks (per arrow-key
/// press). 0.02 across [0,1] gives ~50 steps from floor to ceiling —
/// fast enough to find a band, fine enough to land precisely.
/// Hold Shift for [`SHIFT_MULTIPLIER`]× steps when ranging quickly.
const CONFIDENCE_STEP: f32 = 0.02;

/// Step size for amplitude threshold tweaks. Carriers sit 0.05–0.5,
/// lift sits below 0.005, so the useful range is roughly
/// [0.001, 0.5]. 0.005 puts ~100 steps across that span — fine
/// enough to land near a target without spending a minute on
/// repeats. Hold Shift for [`SHIFT_MULTIPLIER`]× to sweep the full
/// range in ~10 presses.
const AMPLITUDE_STEP: f32 = 0.005;

/// Multiplier applied to all threshold steps when the Shift modifier
/// is held. 10× gives a clean coarse/fine split: regular = "tune",
/// shift = "scan". Works on all four threshold keys (engage,
/// disengage, amplitude up/down).
const SHIFT_MULTIPLIER: f32 = 10.0;

/// Top of the amplitude bar's visual scale. CV02 carriers through an
/// SL3 sit at 0.1–0.5 RMS depending on cartridge + preamp gain;
/// 0.5 is a comfortable ceiling that keeps healthy carriers in the
/// upper half of the bar without pinning them. Values above this
/// clamp to a full bar.
const AMPLITUDE_GAUGE_MAX: f32 = 0.5;

// ---------------------------------------------------------------------
// Pure (testable) types: trail + bar mapping. The audio + rendering
// loop is below this section and explicitly *not* unit-tested
// (snapshot tests of TUI rendering are flake-prone and don't
// exercise real terminal behavior).
// ---------------------------------------------------------------------

/// Bounded ring of recent stereo frame pairs for the Lissajous plot.
/// Implemented as a fixed-capacity Vec with head/length so push is
/// O(1) and the iter() exposes oldest-to-newest in time order
/// (newest-on-top would also be valid; this matches the visual
/// "trail" intuition).
#[derive(Debug)]
pub struct LissajousTrail {
    /// Fixed-cap storage. `len <= capacity == storage.len()`.
    storage: Vec<(f32, f32)>,
    /// Index where the next push will write. Wraps modulo capacity.
    head: usize,
    /// Number of valid entries; reaches `capacity` and stays there
    /// while pushes continue.
    len: usize,
}

impl LissajousTrail {
    /// Create a trail with the given capacity. `capacity = 0` is
    /// allowed but useless.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            storage: vec![(0.0, 0.0); capacity],
            head: 0,
            len: 0,
        }
    }

    /// Push one stereo frame.
    pub fn push(&mut self, l: f32, r: f32) {
        if self.storage.is_empty() {
            return;
        }
        self.storage[self.head] = (l, r);
        self.head = (self.head + 1) % self.storage.len();
        if self.len < self.storage.len() {
            self.len += 1;
        }
    }

    /// Append a slice of interleaved stereo samples. Frame count
    /// equals `samples.len() / 2`; an odd tail sample is dropped.
    pub fn push_interleaved(&mut self, samples: &[f32]) {
        let frames = samples.len() / 2;
        for f in 0..frames {
            self.push(samples[f * 2], samples[f * 2 + 1]);
        }
    }

    /// Discard all entries (used by the `c` key in the TUI).
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// Number of currently-stored entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when no entries have been pushed yet (or after `clear`).
    /// Required by Clippy's `len_without_is_empty` lint; also used by
    /// the unit tests.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Iterate oldest-to-newest. After `len` reaches capacity the
    /// trail is a sliding window of the last `capacity` frames.
    pub fn iter(&self) -> impl Iterator<Item = (f32, f32)> + '_ {
        let cap = self.storage.len();
        let start = if self.len < cap {
            // Buffer not yet full: oldest is at index 0.
            0
        } else {
            // Buffer full: oldest is just after head.
            self.head
        };
        (0..self.len).map(move |i| self.storage[(start + i) % cap])
    }
}

/// Map a value in `[0, max]` to an integer column count `[0, width]`,
/// clamping out-of-range values. Used for the confidence / amplitude
/// gauges and the sticky-countdown bar.
///
/// `max == 0.0` returns `0` (avoids div-by-zero, treats the gauge as
/// "off"). Negative `value` clamps to 0; values above `max` clamp to
/// `width`.
#[must_use]
pub fn bar_cols(value: f32, max: f32, width: usize) -> usize {
    if !value.is_finite() || max <= 0.0 || width == 0 {
        return 0;
    }
    let ratio = (value / max).clamp(0.0, 1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cols = (ratio * width as f32).round() as usize;
    cols.min(width)
}

/// Apply [`SHIFT_MULTIPLIER`] to a base step when Shift is held.
/// Centralized so the engage/disengage/amplitude keys all behave
/// identically and the multiplier is one constant to tune.
#[must_use]
pub fn step_with_shift(base: f32, shift_held: bool) -> f32 {
    if shift_held {
        base * SHIFT_MULTIPLIER
    } else {
        base
    }
}

/// Compose a single-line bar of width `width` cells, with the
/// leading `filled` cells representing the current value and a
/// `tick` glyph drawn at the threshold position. Returns `(filled,
/// tick_col)` so the renderer can color regions independently;
/// `tick_col == width` means the tick is past the right edge and
/// should be drawn at the last cell instead of off-canvas.
///
/// Pure, testable. Used by both the confidence and amplitude bars.
#[must_use]
pub fn bar_layout(value: f32, max: f32, threshold: f32, width: usize) -> (usize, usize) {
    let filled = bar_cols(value, max, width);
    let tick = if !threshold.is_finite() || max <= 0.0 || width == 0 {
        0
    } else {
        let ratio = (threshold / max).clamp(0.0, 1.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let col = (ratio * width as f32).round() as usize;
        col.min(width.saturating_sub(1))
    };
    (filled, tick)
}

/// Map a signed rate in `[-range, +range]` to a horizontal slider
/// position in `[0, width]`. Center (column `width/2`) is `rate=0`.
/// Used for the rate visualization in the status panel.
///
/// Out-of-range rates clamp to the endpoints. `range == 0` returns
/// the center column.
#[must_use]
pub fn rate_slider_cols(rate: f64, range: f64, width: usize) -> usize {
    if width == 0 {
        return 0;
    }
    if range <= 0.0 || !rate.is_finite() {
        return width / 2;
    }
    let normalized = (rate / range).clamp(-1.0, 1.0); // -1..1
    let centered = (normalized + 1.0) * 0.5; // 0..1
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cols = (centered * width as f64).round() as usize;
    cols.min(width)
}

// ---------------------------------------------------------------------
// CLI option parsing
// ---------------------------------------------------------------------

/// Parsed `dub scope` options.
#[derive(Debug, Clone)]
struct Opts {
    device: Option<String>,
    /// 1-based device-channel indices feeding L, R. SL3 deck A is `[3, 4]`.
    input_channels: Option<Vec<u32>>,
    sample_rate: Option<f32>,
    buffer_size: Option<u32>,
    duration: Option<f64>,
    format: Format,
    /// Initial engage threshold (live-tunable in the TUI).
    engage: f32,
    /// Initial disengage threshold (live-tunable).
    disengage: f32,
    sticky: u32,
    /// Initial amplitude threshold (live-tunable).
    amplitude: f32,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            device: None,
            input_channels: None,
            sample_rate: None,
            buffer_size: None,
            duration: None,
            format: Format::SeratoCv02,
            engage: DEFAULT_CONFIDENCE_THRESHOLD,
            disengage: DEFAULT_DISENGAGE_THRESHOLD,
            sticky: DEFAULT_STICKY_BLOCKS_TO_DISENGAGE,
            amplitude: DEFAULT_AMPLITUDE_THRESHOLD,
        }
    }
}

fn parse_opts(args: &[String]) -> Result<Opts> {
    // Reuse the shared input-args parser for --device / --sr /
    // --buffer-size / --input-channels / --duration. Anything left
    // over is scope-specific and parsed below.
    let (input_args, leftover) = parse_input_args(args)?;
    let mut opts = Opts {
        device: input_args.device,
        input_channels: input_args.input_channels,
        sample_rate: input_args.sample_rate,
        buffer_size: input_args.buffer_size,
        duration: input_args.duration,
        ..Opts::default()
    };

    let mut iter = leftover.into_iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--engage" | "--confidence" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("{arg} expects a number in [0,1]"))?;
                opts.engage = v.parse::<f32>().with_context(|| format!("{arg} {v}"))?;
            }
            "--disengage" | "--disengage-threshold" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("--disengage expects a number in [0,1]"))?;
                opts.disengage = v
                    .parse::<f32>()
                    .with_context(|| format!("--disengage {v}"))?;
            }
            "--sticky" | "--sticky-blocks" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("--sticky expects an integer"))?;
                opts.sticky = v.parse::<u32>().with_context(|| format!("--sticky {v}"))?;
            }
            "--amplitude" | "--amplitude-threshold" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow!("--amplitude expects a number ≥ 0"))?;
                opts.amplitude = v
                    .parse::<f32>()
                    .with_context(|| format!("--amplitude {v}"))?;
            }
            "--format" => {
                let v = iter.next().ok_or_else(|| {
                    anyhow!("--format expects 'serato-cv02', 'traktor-mk1', or 'traktor-mk2'")
                })?;
                opts.format = Format::from_cli_arg(v.as_str()).ok_or_else(|| {
                    anyhow!(
                        "unknown --format '{v}' (supported: serato-cv02, traktor-mk1, traktor-mk2)"
                    )
                })?;
            }
            other if other.starts_with("--") => {
                return Err(anyhow!("unknown scope flag: {other}"));
            }
            other => {
                return Err(anyhow!("unexpected positional arg: {other}"));
            }
        }
    }

    if !(0.0..=1.0).contains(&opts.engage) {
        return Err(anyhow!("--engage must be in [0,1], got {}", opts.engage));
    }
    if !(0.0..=1.0).contains(&opts.disengage) {
        return Err(anyhow!(
            "--disengage must be in [0,1], got {}",
            opts.disengage
        ));
    }
    if opts.disengage > opts.engage {
        return Err(anyhow!(
            "--disengage ({}) must be ≤ --engage ({})",
            opts.disengage,
            opts.engage
        ));
    }
    if opts.amplitude < 0.0 || !opts.amplitude.is_finite() {
        return Err(anyhow!(
            "--amplitude must be ≥ 0 and finite, got {}",
            opts.amplitude
        ));
    }

    Ok(opts)
}

impl Opts {
    fn input_options(&self) -> InputOptions {
        let (channels, channel_map) = match &self.input_channels {
            Some(v) => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                let map: Vec<i32> = v.iter().map(|&c| (c as i32) - 1).collect();
                #[allow(clippy::cast_possible_truncation)]
                (v.len() as u32, Some(map))
            }
            None => (2, None),
        };
        InputOptions {
            device_name: self.device.clone(),
            channels,
            buffer_frames: self.buffer_size,
            sample_rate: self.sample_rate,
            channel_map,
            ..InputOptions::default()
        }
    }
}

// ---------------------------------------------------------------------
// Live UI state — owned by the main loop, mutated each render tick.
// ---------------------------------------------------------------------

/// Snapshot of everything the TUI renders. Refreshed each render
/// frame from the audio loop's most recent decoder/policy outputs.
struct UiState {
    /// Last decoder output, or `None` before the first block.
    last_output: Option<DecodeOutput>,
    /// Last policy intent. Used to color the rate text and the
    /// `[LOCKED] / [LIFT]` badge consistently with the policy's
    /// authoritative engaged flag.
    last_intent: Option<LiftIntent>,
    /// Authoritative engaged state read from [`LiftPolicy::is_engaged`].
    /// `last_intent` *also* implies engagement, but reading the policy
    /// flag directly is unambiguous (the intent variant alone can't
    /// distinguish "disengaged this block" from "still engaged but
    /// holding rate").
    policy_engaged: bool,
    /// `LiftPolicy::consecutive_below` mirrored here so the sticky bar
    /// can render an accurate countdown without holding a borrow on
    /// the policy across the draw call.
    consecutive_below: u32,
    /// Live thresholds (mutable via arrow keys; persisted as part of
    /// the state so the UI shows the *current* values).
    engage: f32,
    disengage: f32,
    amplitude: f32,
    sticky: u32,
    /// Counts of input ringbuf events for the diagnostics row.
    input_callbacks: u64,
    input_overflows: u64,
}

// ---------------------------------------------------------------------
// Entry point — parses, opens audio, drives the loop, restores
// terminal on every exit path (panic-safe via PanicGuard).
// ---------------------------------------------------------------------

/// `dub scope [--device N] [--input-channels N,M] [--engage T] ...`
///
/// # Errors
/// Audio-device open failures, terminal init failures, or decoder
/// configuration errors.
pub fn run(args: &[String]) -> Result<()> {
    let opts = parse_opts(args)?;

    // Open the input device first — if this fails the user gets a
    // clean error before we touch the terminal state.
    let mut input =
        AudioInput::start_with_options(&opts.input_options()).context("opening input device")?;
    let sr = input.sample_rate();
    let channels = input.channels() as usize;
    if channels < 2 {
        return Err(anyhow!(
            "scope requires a stereo input (got {channels} channel{}); \
             use --input-channels N,M to pick L,R",
            if channels == 1 { "" } else { "s" }
        ));
    }

    let cfg = TimecodeInputConfig {
        format: opts.format,
        input_sample_rate: sr,
        max_block_frames: BLOCK_FRAMES,
        confidence_threshold: opts.engage,
        disengage_threshold: opts.disengage,
        sticky_blocks_to_disengage: opts.sticky,
        amplitude_threshold: opts.amplitude,
    };

    // Now into TUI mode. Use a PanicGuard so a panic still leaves the
    // terminal usable (raw mode + alt screen disabled).
    let _guard = TerminalGuard::new()?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("creating ratatui terminal")?;
    terminal.clear().context("clearing terminal")?;

    let exit = drive_loop(&mut terminal, &mut input, cfg, &opts)?;

    drop(terminal);
    drop(_guard);

    if let Some(reason) = exit.message {
        println!("{reason}");
    }
    Ok(())
}

/// Reasons the main loop exited. Carries an optional message to
/// print after the terminal is restored (so it actually shows up).
struct LoopExit {
    message: Option<String>,
}

#[allow(clippy::too_many_lines)]
fn drive_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    input: &mut AudioInput,
    cfg: TimecodeInputConfig,
    opts: &Opts,
) -> Result<LoopExit> {
    let sr = input.sample_rate();
    let mut decoder = Decoder::new(cfg.format, sr);
    let mut policy = LiftPolicy::new(&cfg);
    let mut trail = LissajousTrail::new(LISSAJOUS_CAPACITY);

    // Sample-buffer state. We accumulate from `read_into` until a
    // full block is available, then process one block, then shift
    // the remainder down.
    let block_samples = BLOCK_FRAMES * 2;
    let mut acc = vec![0_f32; block_samples * 8];
    let mut acc_len = 0_usize;

    let mut state = UiState {
        last_output: None,
        last_intent: None,
        policy_engaged: false,
        consecutive_below: 0,
        engage: cfg.confidence_threshold,
        disengage: cfg.disengage_threshold,
        amplitude: cfg.amplitude_threshold,
        sticky: cfg.sticky_blocks_to_disengage,
        input_callbacks: 0,
        input_overflows: 0,
    };

    let render_interval = Duration::from_millis(1000 / TARGET_FPS);
    let mut next_render = Instant::now();
    let mut last_threshold_change = Instant::now() - Duration::from_secs(1);

    let start = Instant::now();
    let stop_at = opts.duration.map(|d| start + Duration::from_secs_f64(d));

    loop {
        // 1. Top up accumulator from the input ringbuf.
        let space = acc.len() - acc_len;
        if space > 0 {
            let n = input.read_into(&mut acc[acc_len..acc_len + space]);
            acc_len += n;
        }

        // 2. Process every full block we have.
        while acc_len >= block_samples {
            // Push raw samples into the lissajous trail before the
            // decoder consumes them — the trail wants to see what
            // the cartridge actually sent.
            trail.push_interleaved(&acc[..block_samples]);
            let out = decoder.process(&acc[..block_samples]);
            let intent = policy.step(out);
            state.last_output = Some(out);
            state.last_intent = Some(intent);
            state.policy_engaged = policy.is_engaged();
            state.consecutive_below = policy.consecutive_below();

            // Shift the remainder down and shrink len.
            acc.copy_within(block_samples..acc_len, 0);
            acc_len -= block_samples;
        }

        // 3. Render at 30 fps.
        let now = Instant::now();
        if now >= next_render {
            state.input_callbacks = input.callback_count();
            state.input_overflows = input.overflow_count();
            terminal
                .draw(|f| draw_ui(f, &trail, &state, &cfg, sr, input))
                .context("drawing UI")?;
            // Advance to the next slot, never falling behind by more
            // than one frame (avoids playing catch-up after a long
            // sleep on system suspend).
            next_render = now + render_interval;
        }

        // 4. Drain key events with a short timeout so the audio loop
        //    doesn't starve.
        let key_timeout = Duration::from_millis(2);
        if event::poll(key_timeout).context("polling for key events")? {
            if let Event::Key(k) = event::read().context("reading key event")? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                let now = Instant::now();
                let debounced = now.duration_since(last_threshold_change)
                    >= Duration::from_millis(KEY_REPEAT_DEBOUNCE_MS);
                let shift = k.modifiers.contains(KeyModifiers::SHIFT);
                let conf_step = step_with_shift(CONFIDENCE_STEP, shift);
                let amp_step = step_with_shift(AMPLITUDE_STEP, shift);
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        return Ok(LoopExit {
                            message: Some(format!(
                                "scope exited: {} input callbacks, {} overflows",
                                state.input_callbacks, state.input_overflows
                            )),
                        });
                    }
                    KeyCode::Char('c') => trail.clear(),
                    KeyCode::Up if debounced => {
                        state.engage = (state.engage + conf_step).min(1.0);
                        if state.engage < state.disengage {
                            state.disengage = state.engage;
                        }
                        apply_threshold_changes(&mut policy, &state);
                        last_threshold_change = now;
                    }
                    KeyCode::Down if debounced => {
                        state.engage = (state.engage - conf_step).max(state.disengage);
                        apply_threshold_changes(&mut policy, &state);
                        last_threshold_change = now;
                    }
                    KeyCode::PageUp if debounced => {
                        state.disengage = (state.disengage + conf_step).min(state.engage);
                        apply_threshold_changes(&mut policy, &state);
                        last_threshold_change = now;
                    }
                    KeyCode::PageDown if debounced => {
                        state.disengage = (state.disengage - conf_step).max(0.0);
                        apply_threshold_changes(&mut policy, &state);
                        last_threshold_change = now;
                    }
                    KeyCode::Right if debounced => {
                        state.amplitude = (state.amplitude + amp_step).min(1.0);
                        apply_threshold_changes(&mut policy, &state);
                        last_threshold_change = now;
                    }
                    KeyCode::Left if debounced => {
                        state.amplitude = (state.amplitude - amp_step).max(0.0);
                        apply_threshold_changes(&mut policy, &state);
                        last_threshold_change = now;
                    }
                    _ => {}
                }
            }
        }

        // 5. Honor --duration if set.
        if let Some(deadline) = stop_at {
            if Instant::now() >= deadline {
                return Ok(LoopExit {
                    message: Some(format!(
                        "scope timed out after {:.1} s ({} callbacks, {} overflows)",
                        opts.duration.unwrap_or(0.0),
                        state.input_callbacks,
                        state.input_overflows
                    )),
                });
            }
        }
    }
}

/// Rebuild the lift policy in place when thresholds change. Cheap —
/// `LiftPolicy::new` is field assignment + zeroing the engaged state.
/// Resetting `engaged` to `false` is the right move on a threshold
/// change: the user is asking "what would happen with these
/// thresholds?", and we'd rather fail closed (lift) than carry over
/// engagement from the old thresholds.
fn apply_threshold_changes(policy: &mut LiftPolicy, state: &UiState) {
    *policy = LiftPolicy::new(&TimecodeInputConfig {
        format: Format::SeratoCv02,
        input_sample_rate: 0.0,
        max_block_frames: BLOCK_FRAMES,
        confidence_threshold: state.engage,
        disengage_threshold: state.disengage,
        sticky_blocks_to_disengage: state.sticky,
        amplitude_threshold: state.amplitude,
    });
}

// ---------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------

fn draw_ui(
    f: &mut ratatui::Frame<'_>,
    trail: &LissajousTrail,
    state: &UiState,
    cfg: &TimecodeInputConfig,
    sr: f32,
    input: &AudioInput,
) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(10),   // body (lissajous + status)
            Constraint::Length(3), // footer
        ])
        .split(area);

    draw_header(f, chunks[0], cfg, sr, input);
    draw_body(f, chunks[1], trail, state);
    draw_footer(f, chunks[2]);
}

fn draw_header(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    cfg: &TimecodeInputConfig,
    sr: f32,
    input: &AudioInput,
) {
    let header = format!(
        "device: {}  sr: {sr:.0} Hz  ch: {}  buffer: {} fr ({:.2} ms)  block: {} fr  format: {:?}",
        input.device_name(),
        input.channels(),
        input.buffer_frames(),
        input.latency_seconds() * 1000.0,
        cfg.max_block_frames,
        cfg.format,
    );
    let widget = Paragraph::new(header).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" dub scope (M5.4.1) "),
    );
    f.render_widget(widget, area);
}

fn draw_body(f: &mut ratatui::Frame<'_>, area: Rect, trail: &LissajousTrail, state: &UiState) {
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    draw_lissajous(f, split[0], trail, state);
    draw_status(f, split[1], state);
}

fn draw_lissajous(f: &mut ratatui::Frame<'_>, area: Rect, trail: &LissajousTrail, state: &UiState) {
    // Color the dots by engagement: green when locked, dim red on
    // lift. Gives the user instant feedback that policy state has
    // flipped without scanning the right panel.
    let color = engaged_color(state);

    // Collect points. ratatui's Canvas wants &[(f64, f64)]; we
    // rebuild a Vec each frame because the trail is small (1024) and
    // the rendering itself is the bottleneck. A Vec::with_capacity
    // avoids growth reallocations within the frame.
    let mut points: Vec<(f64, f64)> = Vec::with_capacity(trail.len());
    for (l, r) in trail.iter() {
        points.push((f64::from(l), f64::from(r)));
    }

    let canvas = Canvas::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" lissajous (X=L, Y=R) "),
        )
        .x_bounds([-1.0, 1.0])
        .y_bounds([-1.0, 1.0])
        .marker(Marker::Braille)
        .paint(move |ctx| {
            ctx.draw(&Points {
                coords: &points,
                color,
            });
        });
    f.render_widget(canvas, area);
}

fn draw_status(f: &mut ratatui::Frame<'_>, area: Rect, state: &UiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" decoder + policy ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Vertical layout inside the status block. We fix heights for the
    // text rows and let the gauges share remaining space.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // STATE
            Constraint::Length(2), // rate
            Constraint::Length(2), // confidence label + gauge
            Constraint::Length(1), // confidence gauge body
            Constraint::Length(2), // amplitude label + gauge
            Constraint::Length(1), // amplitude gauge body
            Constraint::Length(2), // position
            Constraint::Length(2), // sticky
            Constraint::Min(2),    // thresholds + diag
        ])
        .split(inner);

    let last = state.last_output;
    let engaged = state.policy_engaged;
    let badge = if engaged {
        Span::styled(
            " [LOCKED] ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " [LIFT]   ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
    };
    let state_line = Line::from(vec![Span::raw("STATE       "), badge]);
    f.render_widget(Paragraph::new(state_line), chunks[0]);

    let rate = last.map_or(0.0, |o| o.rate);
    let rate_color = if last.map_or(0.0, |o| o.confidence) >= state.engage {
        Color::Green
    } else if last.map_or(0.0, |o| o.confidence) >= state.disengage {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    // Render a small ASCII slider showing rate direction + magnitude
    // in the [-2x, +2x] range. Center column = stopped; left of
    // center = reverse, right = forward. Visible at a glance even
    // with the colored numeric next to it.
    let slider_w = 13_usize; // odd → exact center column
    let pos = rate_slider_cols(rate, 2.0, slider_w);
    let mut slider = String::with_capacity(slider_w + 2);
    slider.push('[');
    for i in 0..slider_w {
        if i == pos {
            slider.push('|');
        } else if i == slider_w / 2 {
            slider.push(':'); // center tick
        } else {
            slider.push('·');
        }
    }
    slider.push(']');
    let rate_line = Line::from(vec![
        Span::raw("rate         "),
        Span::styled(
            format!("{rate:+.3}×"),
            Style::default().fg(rate_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(slider, Style::default().fg(rate_color)),
    ]);
    f.render_widget(Paragraph::new(rate_line), chunks[1]);

    // Confidence bar: range 0..1. Two ticks (engage solid, disengage
    // dashed) show *exactly* where the policy decides — flicker
    // around the engage tick is the calibration signal "you're at
    // the carrier's noise ceiling". Custom bar (not ratatui Gauge)
    // because Gauge doesn't support overlay markers.
    let conf = last.map_or(0.0, |o| o.confidence);
    let conf_color = if conf >= state.engage {
        Color::Green
    } else if conf >= state.disengage {
        Color::Yellow
    } else {
        Color::Red
    };
    f.render_widget(
        Paragraph::new(format!(
            "confidence   {conf:.3}    eng {:.2}  dis {:.2}",
            state.engage, state.disengage
        )),
        chunks[2],
    );
    let conf_bar = render_threshold_bar(
        conf,
        1.0,
        &[
            (state.engage, '┃', Color::White),
            (state.disengage, '╎', Color::Gray),
        ],
        chunks[3].width.saturating_sub(2) as usize,
        conf_color,
    );
    f.render_widget(Paragraph::new(conf_bar), chunks[3]);

    // Amplitude bar: scale 0..0.5 (typical CV02 RMS ceiling through
    // SL3). One tick at the threshold so its position is visible
    // even when amp is at lift level. Numeric ratio after the
    // threshold makes headroom unambiguous: "0.234 (×23 thr)" tells
    // you at a glance whether you have margin.
    let amp = last.map_or(0.0, |o| o.amplitude);
    let amp_color = if amp >= state.amplitude {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let ratio_str = if state.amplitude > f32::EPSILON {
        format!("×{:.1} thr", amp / state.amplitude)
    } else {
        "thr=0 (gate off)".to_string()
    };
    f.render_widget(
        Paragraph::new(format!(
            "amplitude    {amp:.4}    thr {:.4}  ({ratio_str})",
            state.amplitude
        )),
        chunks[4],
    );
    let amp_bar = render_threshold_bar(
        amp,
        AMPLITUDE_GAUGE_MAX,
        &[(state.amplitude, '┃', Color::White)],
        chunks[5].width.saturating_sub(2) as usize,
        amp_color,
    );
    f.render_widget(Paragraph::new(amp_bar), chunks[5]);

    let pos = last.map_or(0.0, |o| o.position_secs);
    f.render_widget(
        Paragraph::new(format!("position     {pos:+.3} s")),
        chunks[6],
    );

    // Sticky countdown: one cell per block of the configured window.
    // Filled cells = consecutive_below counter from the live policy.
    // Cells fill *as the deck is about to disengage*; visualizing the
    // approach is way more useful than just "are we below floor right
    // now?" — the user can see the policy walking toward disengage
    // and judge whether their thresholds are too aggressive.
    let sticky_max = state.sticky.max(1);
    let bar_w = sticky_max.min(16) as usize;
    #[allow(clippy::cast_precision_loss)]
    let cells = bar_cols(state.consecutive_below as f32, sticky_max as f32, bar_w);
    let bar = "█".repeat(cells) + &"░".repeat(bar_w - cells);
    f.render_widget(
        Paragraph::new(format!(
            "sticky       {bar}  {}/{}",
            state.consecutive_below.min(sticky_max),
            sticky_max
        )),
        chunks[7],
    );

    let info = format!(
        "thresholds   eng {:.2}  dis {:.2}  amp {:.4}  sticky {}\ninput        callbacks {}  overflows {}",
        state.engage, state.disengage, state.amplitude, state.sticky,
        state.input_callbacks, state.input_overflows,
    );
    f.render_widget(Paragraph::new(info), chunks[8]);
}

/// Render a horizontal bar with optional threshold tick marks
/// overlaid. `width` is the cell count of the bar body (no surrounding
/// brackets); the returned [`Line`] paints each cell as one of:
///
/// - `█` filled, in `fill_color` — `value` reaches this cell.
/// - tick glyph in tick color — a threshold lands on this cell
///   (overrides the fill character so the threshold is visible
///   whether the bar is full or empty there).
/// - `░` empty (dim gray) — neither filled nor a tick.
///
/// Multiple ticks are supported (used for the confidence bar:
/// engage = solid, disengage = dashed). Ticks earlier in the slice
/// are drawn last, so the first entry "wins" if two thresholds
/// land on the same cell.
fn render_threshold_bar<'a>(
    value: f32,
    max: f32,
    thresholds: &[(f32, char, Color)],
    width: usize,
    fill_color: Color,
) -> Line<'a> {
    if width == 0 {
        return Line::from("");
    }
    let filled = bar_cols(value, max, width);

    // Compute tick column positions up front. We treat each tick as
    // a Vec<bool>-equivalent grid; iterating once is enough since
    // ticks are usually 1-2 entries.
    let tick_positions: Vec<(usize, char, Color)> = thresholds
        .iter()
        .filter_map(|&(t, glyph, color)| {
            let (_, col) = bar_layout(value, max, t, width);
            // Drop ticks at column 0 unless threshold is non-zero
            // (a threshold of exactly 0 would render at col 0
            // and obscure the bar's left edge for no info).
            if t <= 0.0 {
                None
            } else {
                Some((col, glyph, color))
            }
        })
        .collect();

    let mut spans: Vec<Span<'a>> = Vec::with_capacity(width);
    for col in 0..width {
        // Later thresholds in the slice take precedence when two
        // overlap (so the more important one wins). Iterate in
        // reverse and pick the first match; if none, fall back to
        // fill / empty.
        if let Some(&(_, glyph, color)) = tick_positions.iter().rev().find(|(c, _, _)| *c == col) {
            spans.push(Span::styled(
                glyph.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        } else if col < filled {
            spans.push(Span::styled("█", Style::default().fg(fill_color)));
        } else {
            spans.push(Span::styled("░", Style::default().fg(Color::DarkGray)));
        }
    }
    Line::from(spans)
}

fn engaged_color(state: &UiState) -> Color {
    if state.policy_engaged {
        Color::Green
    } else if state.last_output.is_some() {
        Color::Red
    } else {
        Color::DarkGray
    }
}

fn draw_footer(f: &mut ratatui::Frame<'_>, area: Rect) {
    let help = "q/Esc quit · c clear · ↑/↓ engage · PgUp/PgDn disengage · ←/→ amplitude · Shift = 10× step";
    let widget = Paragraph::new(help).block(Block::default().borders(Borders::ALL));
    f.render_widget(widget, area);
}

// ---------------------------------------------------------------------
// Terminal lifecycle — RAII so panics restore cooked mode.
// ---------------------------------------------------------------------

struct TerminalGuard;

impl TerminalGuard {
    fn new() -> Result<Self> {
        terminal::enable_raw_mode().context("entering raw mode")?;
        execute!(
            io::stdout(),
            terminal::EnterAlternateScreen,
            crossterm::cursor::Hide
        )
        .context("entering alternate screen")?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            terminal::LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        let _ = terminal::disable_raw_mode();
    }
}

// ---------------------------------------------------------------------
// Tests — pure logic only (LissajousTrail, bar mapping, opt parsing).
// The TUI rendering loop is exercised end-to-end on real hardware
// (the `dub scope` smoke run). ratatui snapshot tests are flake-prone
// across terminal versions and not worth maintaining here.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_string()).collect()
    }

    #[test]
    fn lissajous_trail_capacity_zero_is_a_noop() {
        let mut t = LissajousTrail::new(0);
        t.push(0.5, -0.5);
        assert_eq!(t.len(), 0);
        assert!(t.is_empty());
        assert_eq!(t.iter().count(), 0);
    }

    #[test]
    fn lissajous_trail_fills_then_overwrites() {
        let mut t = LissajousTrail::new(3);
        t.push(1.0, 0.0);
        t.push(2.0, 0.0);
        t.push(3.0, 0.0);
        assert_eq!(t.len(), 3);
        // Iterates oldest-to-newest before wrap.
        let v: Vec<_> = t.iter().collect();
        assert_eq!(v, [(1.0, 0.0), (2.0, 0.0), (3.0, 0.0)]);

        // After one wrap, oldest entry is gone.
        t.push(4.0, 0.0);
        let v: Vec<_> = t.iter().collect();
        assert_eq!(v, [(2.0, 0.0), (3.0, 0.0), (4.0, 0.0)]);

        // After two more wraps the window is fully replaced.
        t.push(5.0, 0.0);
        t.push(6.0, 0.0);
        let v: Vec<_> = t.iter().collect();
        assert_eq!(v, [(4.0, 0.0), (5.0, 0.0), (6.0, 0.0)]);
    }

    #[test]
    fn lissajous_push_interleaved_drops_odd_tail() {
        let mut t = LissajousTrail::new(8);
        // 5 samples: 2 frames + 1 dangling sample.
        t.push_interleaved(&[0.1, 0.2, 0.3, 0.4, 0.5]);
        let v: Vec<_> = t.iter().collect();
        assert_eq!(v, [(0.1, 0.2), (0.3, 0.4)]);
    }

    #[test]
    fn lissajous_clear_resets_everything() {
        let mut t = LissajousTrail::new(4);
        for i in 0..6 {
            #[allow(clippy::cast_precision_loss)]
            let f = i as f32;
            t.push(f, -f);
        }
        assert_eq!(t.len(), 4);
        t.clear();
        assert_eq!(t.len(), 0);
        assert!(t.is_empty());
        assert_eq!(t.iter().count(), 0);
    }

    #[test]
    fn bar_cols_clamps_below_zero() {
        assert_eq!(bar_cols(-1.0, 1.0, 10), 0);
    }

    #[test]
    fn bar_cols_clamps_above_max() {
        assert_eq!(bar_cols(2.0, 1.0, 10), 10);
    }

    #[test]
    fn bar_cols_zero_max_is_zero() {
        assert_eq!(bar_cols(0.5, 0.0, 10), 0);
    }

    #[test]
    fn bar_cols_zero_width_is_zero() {
        assert_eq!(bar_cols(0.5, 1.0, 0), 0);
    }

    #[test]
    fn bar_cols_handles_nan() {
        assert_eq!(bar_cols(f32::NAN, 1.0, 10), 0);
    }

    #[test]
    fn bar_cols_rounds_to_nearest() {
        // 0.55 → 5.5 cols → rounds to 6 of 10.
        assert_eq!(bar_cols(0.55, 1.0, 10), 6);
        // 0.50 → exactly 5.
        assert_eq!(bar_cols(0.50, 1.0, 10), 5);
        // 0.05 → 0.5 → rounds to 1 (round half to even, but ratatui
        // doesn't care which way 0.5 lands; we just need stability).
        let r = bar_cols(0.05, 1.0, 10);
        assert!(r == 0 || r == 1, "got {r}");
    }

    #[test]
    fn rate_slider_zero_rate_is_centered() {
        // width=10, rate=0 → column 5.
        assert_eq!(rate_slider_cols(0.0, 2.0, 10), 5);
    }

    #[test]
    fn rate_slider_full_positive_pins_right() {
        assert_eq!(rate_slider_cols(2.0, 2.0, 10), 10);
        // Out-of-range pins to the same end.
        assert_eq!(rate_slider_cols(5.0, 2.0, 10), 10);
    }

    #[test]
    fn rate_slider_full_negative_pins_left() {
        assert_eq!(rate_slider_cols(-2.0, 2.0, 10), 0);
        assert_eq!(rate_slider_cols(-99.0, 2.0, 10), 0);
    }

    #[test]
    fn rate_slider_zero_range_is_centered() {
        assert_eq!(rate_slider_cols(1.0, 0.0, 10), 5);
        assert_eq!(rate_slider_cols(-1.0, 0.0, 10), 5);
    }

    #[test]
    fn rate_slider_handles_nan() {
        assert_eq!(rate_slider_cols(f64::NAN, 2.0, 10), 5);
    }

    #[test]
    fn parse_opts_defaults_match_engine_constants() {
        let o = parse_opts(&[]).unwrap();
        assert!((o.engage - DEFAULT_CONFIDENCE_THRESHOLD).abs() < 1e-6);
        assert!((o.disengage - DEFAULT_DISENGAGE_THRESHOLD).abs() < 1e-6);
        assert_eq!(o.sticky, DEFAULT_STICKY_BLOCKS_TO_DISENGAGE);
        assert!((o.amplitude - DEFAULT_AMPLITUDE_THRESHOLD).abs() < 1e-6);
    }

    #[test]
    fn parse_opts_threshold_flags() {
        let o = parse_opts(&s(&[
            "--engage",
            "0.85",
            "--disengage",
            "0.45",
            "--sticky",
            "8",
            "--amplitude",
            "0.02",
        ]))
        .unwrap();
        assert!((o.engage - 0.85).abs() < 1e-6);
        assert!((o.disengage - 0.45).abs() < 1e-6);
        assert_eq!(o.sticky, 8);
        assert!((o.amplitude - 0.02).abs() < 1e-6);
    }

    #[test]
    fn parse_opts_rejects_inverted_hysteresis() {
        let r = parse_opts(&s(&["--engage", "0.3", "--disengage", "0.5"]));
        assert!(r.is_err(), "disengage > engage should fail: {r:?}");
    }

    #[test]
    fn parse_opts_rejects_out_of_range_engage() {
        for bad in ["1.1", "-0.1", "2"] {
            let r = parse_opts(&s(&["--engage", bad]));
            assert!(r.is_err(), "engage={bad} should fail: {r:?}");
        }
    }

    #[test]
    fn parse_opts_rejects_negative_amplitude() {
        let r = parse_opts(&s(&["--amplitude", "-0.001"]));
        assert!(r.is_err(), "negative amplitude should fail: {r:?}");
    }

    #[test]
    fn parse_opts_passes_through_input_args() {
        let o = parse_opts(&s(&[
            "--device",
            "SL3",
            "--input-channels",
            "3,4",
            "--sr",
            "48000",
        ]))
        .unwrap();
        assert_eq!(o.device.as_deref(), Some("SL3"));
        assert_eq!(o.input_channels.as_deref(), Some(&[3_u32, 4_u32][..]));
        assert!((o.sample_rate.unwrap() - 48_000.0).abs() < 0.5);
    }

    #[test]
    fn parse_opts_format_aliases() {
        for v in ["serato-cv02", "serato", "cv02"] {
            let o = parse_opts(&s(&["--format", v])).unwrap();
            assert!(matches!(o.format, Format::SeratoCv02));
        }
        for v in ["traktor-mk1", "mk1"] {
            let o = parse_opts(&s(&["--format", v])).unwrap();
            assert!(
                matches!(o.format, Format::TraktorMk1),
                "alias '{v}' must parse as TraktorMk1"
            );
        }
        for v in ["traktor-mk2", "mk2"] {
            let o = parse_opts(&s(&["--format", v])).unwrap();
            assert!(
                matches!(o.format, Format::TraktorMk2),
                "alias '{v}' must parse as TraktorMk2"
            );
        }
    }

    #[test]
    fn parse_opts_rejects_unknown_format() {
        // Garbage formats should still error out.
        let r = parse_opts(&s(&["--format", "rekordbox"]));
        assert!(r.is_err(), "unsupported format must error");
        // Bare 'traktor' is also rejected as ambiguous (MK1 vs MK2).
        // See Format::from_cli_arg docstring.
        let r = parse_opts(&s(&["--format", "traktor"]));
        assert!(
            r.is_err(),
            "bare 'traktor' must error to avoid silent mis-routing"
        );
    }

    #[test]
    fn parse_opts_rejects_unknown_flag() {
        let r = parse_opts(&s(&["--bogus", "1"]));
        assert!(r.is_err());
    }

    #[test]
    fn parse_opts_rejects_positional() {
        let r = parse_opts(&s(&["wat"]));
        assert!(r.is_err());
    }

    #[test]
    fn step_with_shift_returns_base_when_unshifted() {
        // f32 1e-6 tolerance — the values we ship aren't exactly
        // representable in IEEE-754 binary (e.g. 0.02 = 0x3CA3D70A,
        // representing ~0.0199999996) so a stricter epsilon is a
        // false-precision trap, not a real-bug check.
        assert!((step_with_shift(0.02, false) - 0.02).abs() < 1e-6);
        assert!((step_with_shift(0.005, false) - 0.005).abs() < 1e-6);
    }

    #[test]
    fn step_with_shift_multiplies_when_shifted() {
        assert!((step_with_shift(0.02, true) - 0.2).abs() < 1e-6);
        assert!((step_with_shift(0.005, true) - 0.05).abs() < 1e-6);
        // The multiplier should always be exactly SHIFT_MULTIPLIER —
        // pin it so a typo in the constant fails this test. Integer
        // base × integer multiplier IS representable exactly, so
        // tolerance is 1e-9 here.
        assert!(
            (step_with_shift(1.0, true) - SHIFT_MULTIPLIER).abs() < 1e-9,
            "shift multiplier should be {SHIFT_MULTIPLIER}"
        );
    }

    #[test]
    fn bar_layout_basic_cases() {
        // Half-full bar, threshold at 1/3 of max, width 12:
        //   filled = round(0.5 * 12) = 6
        //   tick   = round((1/3) * 12) = 4
        let (filled, tick) = bar_layout(0.5, 1.0, 1.0 / 3.0, 12);
        assert_eq!(filled, 6);
        assert_eq!(tick, 4);
    }

    #[test]
    fn bar_layout_zero_threshold_pins_to_first_cell() {
        // Threshold of 0 → ratio 0 → col 0.
        let (_, tick) = bar_layout(0.5, 1.0, 0.0, 10);
        assert_eq!(tick, 0);
    }

    #[test]
    fn bar_layout_threshold_above_max_pins_to_last_cell() {
        // Threshold beyond max should clamp to the rightmost cell
        // (width-1), not run off the bar.
        let (_, tick) = bar_layout(0.5, 1.0, 5.0, 10);
        assert_eq!(tick, 9);
    }

    #[test]
    fn bar_layout_zero_max_returns_zero_tick() {
        let (filled, tick) = bar_layout(0.5, 0.0, 0.5, 10);
        assert_eq!(filled, 0);
        assert_eq!(tick, 0);
    }

    #[test]
    fn bar_layout_zero_width_returns_zero() {
        let (filled, tick) = bar_layout(0.5, 1.0, 0.5, 0);
        assert_eq!(filled, 0);
        assert_eq!(tick, 0);
    }

    #[test]
    fn bar_layout_handles_nan_threshold() {
        let (_, tick) = bar_layout(0.5, 1.0, f32::NAN, 10);
        assert_eq!(tick, 0);
    }

    #[test]
    fn bar_layout_amp_gauge_realistic_default_threshold() {
        // The default amplitude_threshold is 0.01, gauge max is 0.5,
        // bar width ~50 cells (typical right-panel half-width).
        // Threshold lands at column 1, which is what shipped — the
        // user couldn't see it move with 0.001 steps. Pin the
        // calculation here to catch a regression on the visual
        // mapping if AMPLITUDE_GAUGE_MAX or DEFAULT_AMPLITUDE_THRESHOLD
        // shift in the future.
        let (_, tick) = bar_layout(0.0, AMPLITUDE_GAUGE_MAX, 0.01, 50);
        assert_eq!(tick, 1, "default 0.01 thr at gauge_max=0.5 → col 1 of 50");
        // 0.128 (the value the user reached) should land at col 13.
        let (_, tick) = bar_layout(0.0, AMPLITUDE_GAUGE_MAX, 0.128, 50);
        assert_eq!(tick, 13, "0.128 thr at gauge_max=0.5 → col 13 of 50");
    }
}
