//! Shared audio routing helpers for the M5.5.2-style external-mixer
//! flow.
//!
//! Two responsibilities, both reused by `dub timecode-deck` (M5) and
//! `dub thru` (M7):
//!
//! 1. [`build_input_options`] turns a parsed `InputArgs` (deck A) plus an
//!    optional second stereo pair (deck B) into a single
//!    [`dub_audio::InputOptions`] with a 4-channel `channel_map` and
//!    `output_pairs` that demux into per-deck SPSC ringbuffers.
//!    CoreAudio refuses to open two AUs on the same physical input
//!    device, so this demux is the only RT-safe way to feed two decks.
//!
//! 2. [`resolve_output_routing`] picks the M5.5.2 output routing in
//!    priority order: explicit `--internal-mixer`, manual
//!    `--deck-a-out-ch` + `--deck-b-out-ch`, `--device-profile NAME`,
//!    auto-detected device profile (SL3, Audio 6, …), or a
//!    safe-but-loud fallback (internal mixer with a warning).
//!
//! The actual logic lived in `crate::timecode_deck` before M7. It's
//! pulled out here so M7's `dub thru` subcommand reuses it byte-for-
//! byte — same flag semantics, same device-profile table, same
//! fallback path — without duplication.

use anyhow::{anyhow, Result};

use crate::device_profiles;
use crate::input_cmds::InputArgs;

/// Subset of CLI args that drive output-routing resolution. Built by
/// each subcommand's `Opts` from its own flag set; the resolver doesn't
/// know about `--track`, `--format`, or any subcommand-specific
/// concerns.
///
/// All fields default to `None`/`false`; the resolver applies the
/// priority order documented on [`resolve_output_routing`].
#[derive(Debug, Default, Clone)]
pub struct RoutingArgs {
    /// `--internal-mixer` flag. Forces 2-ch internal-mixer routing
    /// (both decks summed into ch 1+2). Debug only.
    pub internal_mixer: bool,
    /// `--deck-a-out-ch N` — 1-based device output channel for deck
    /// A's left output. Deck A's right output lands on `N+1`.
    /// Combined with `deck_b_out_ch` for manual routing.
    pub deck_a_out_ch: Option<u32>,
    /// `--deck-b-out-ch N` — same for deck B.
    pub deck_b_out_ch: Option<u32>,
    /// `--output-channels N` override. When set, the AU opens with
    /// this many channels regardless of the device profile. Mostly
    /// useful for under-/over-counting aggregate devices.
    pub output_channels: Option<u32>,
    /// `--device-profile NAME` — pin a known-device profile by
    /// pattern even if the system default-output device doesn't
    /// match. Useful when the macOS default is the wrong device.
    pub device_profile: Option<String>,
}

/// Resolved output routing. Captured ahead of
/// `AudioOutput::start_with_options` so we can print a clear
/// "what we chose, and why" line before any audio starts — saves the
/// user from wondering why deck B is silent on an unknown interface.
pub struct ResolvedOutputRouting {
    /// Total channels to open the AU with.
    pub channels: u32,
    /// Per-deck routing handed to `Engine::render_routed`.
    pub routing: dub_engine::OutputRouting,
    /// Human-readable summary, printed at startup.
    pub summary: String,
}

impl ResolvedOutputRouting {
    /// Human-readable description for startup logging.
    #[must_use]
    pub fn describe(&self) -> &str {
        &self.summary
    }
}

/// Resolve the M5.5.2 output routing in priority order:
///
/// 1. `--internal-mixer` → 2-ch internal mixer (debug only). Loud and
///    explicit; mutually exclusive with all other routing flags
///    (validated at parse time on each subcommand).
/// 2. Explicit `--deck-a-out-ch` + `--deck-b-out-ch` → manual routing
///    over `--output-channels` (or the device's reported channel
///    count). Most permissive — works for unknown devices.
/// 3. `--device-profile NAME` → look up the profile by exact pattern
///    and apply its routing. Useful when the system default is the
///    wrong device.
/// 4. Auto-detect by `device.device_name` against
///    `device_profiles::KNOWN_DEVICES`. The path users hit when they
///    plug in their SL3 and run `dub timecode-deck` (or `dub thru`)
///    with no flags.
/// 5. Fallback (unknown device, no flags) → 2-ch internal mixer with a
///    loud warning. Matches Serato's "preparation mode" semantics for
///    laptop-only situations: the user can hear playback but should
///    not run a live set.
///
/// # Errors
/// `--deck-a-out-ch` / `--deck-b-out-ch` resolve to invalid 0-based
/// indices, the resulting routing exceeds `output_channels`, or the
/// named `--device-profile` is unknown.
pub fn resolve_output_routing(
    device: &dub_audio::DeviceInfo,
    args: &RoutingArgs,
) -> Result<ResolvedOutputRouting> {
    if args.internal_mixer {
        return Ok(ResolvedOutputRouting {
            channels: 2,
            routing: dub_engine::INTERNAL_MIXER_ROUTING,
            summary: "output routing: internal mixer (2 ch, both decks → ch 1+2)\n\
                 ⚠️  --internal-mixer is debug-only; not for live performance"
                .to_string(),
        });
    }

    if let (Some(a), Some(b)) = (args.deck_a_out_ch, args.deck_b_out_ch) {
        let a0 = device_profiles::one_based_to_zero_based(a)
            .ok_or_else(|| anyhow!("--deck-a-out-ch must be ≥ 1 (1-based), got {a}"))?;
        let b0 = device_profiles::one_based_to_zero_based(b)
            .ok_or_else(|| anyhow!("--deck-b-out-ch must be ≥ 1 (1-based), got {b}"))?;
        let channels = args.output_channels.unwrap_or(device.channels);
        if channels < 2 {
            return Err(anyhow!(
                "--output-channels must be ≥ 2; got {channels} (device reports {} ch)",
                device.channels
            ));
        }
        if a0 + 2 > channels || b0 + 2 > channels {
            return Err(anyhow!(
                "deck-a-out-ch={a} or deck-b-out-ch={b} doesn't fit in {channels} channels \
                 (each deck takes 2 channels). Pass --output-channels N if your device has \
                 more outputs than the default detected."
            ));
        }
        return Ok(ResolvedOutputRouting {
            channels,
            routing: [Some(a0), Some(b0)],
            summary: format!(
                "output routing: manual ({} ch, deck A → ch {}+{}, deck B → ch {}+{})",
                channels,
                a,
                a + 1,
                b,
                b + 1,
            ),
        });
    }

    let profile = if let Some(pattern) = args.device_profile.as_deref() {
        device_profiles::profile_by_pattern(pattern).ok_or_else(|| {
            anyhow!(
                "--device-profile {pattern:?} not found in known-device table; \
                 known patterns: {}",
                device_profiles::KNOWN_DEVICES
                    .iter()
                    .map(|d| d.name_pattern)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?
    } else if let Some(p) = device_profiles::match_device(&device.device_name) {
        p
    } else {
        // Unknown device, no manual routing — fall back to internal
        // mixer with a loud warning. Per the M5.5.2 design call: this
        // is preparation-mode-equivalent (the user can audition tracks
        // but the routing isn't right for an external mixer).
        return Ok(ResolvedOutputRouting {
            channels: 2,
            routing: dub_engine::INTERNAL_MIXER_ROUTING,
            summary: format!(
                "output routing: unknown device '{}' — falling back to internal mixer.\n\
                 ⚠️  no recognised interface profile; deck audio is summed to ch 1+2.\n\
                 ⚠️  for an external mixer, pass --deck-a-out-ch / --deck-b-out-ch (1-based) \
                 + --output-channels N, or --device-profile <name> if your interface is \
                 listed in the known-device table",
                device.device_name
            ),
        });
    };

    let channels = args.output_channels.unwrap_or(profile.output_channels);
    if profile.deck_a_first_channel + 2 > channels || profile.deck_b_first_channel + 2 > channels {
        return Err(anyhow!(
            "device profile '{}' wants {} channels but --output-channels {} is too small",
            profile.display_name,
            profile.output_channels,
            channels
        ));
    }
    let verified_note = if profile.verified {
        ""
    } else {
        "\n⚠️  this profile is unverified against real hardware — double-check the routing"
    };
    Ok(ResolvedOutputRouting {
        channels,
        routing: [
            Some(profile.deck_a_first_channel),
            Some(profile.deck_b_first_channel),
        ],
        summary: format!(
            "output routing: {} ({} ch, deck A → ch {}+{}, deck B → ch {}+{}){}",
            profile.display_name,
            channels,
            profile.deck_a_first_channel + 1,
            profile.deck_a_first_channel + 2,
            profile.deck_b_first_channel + 1,
            profile.deck_b_first_channel + 2,
            verified_note,
        ),
    })
}

/// Build [`dub_audio::InputOptions`] for the input AU, supporting both
/// single-deck (M5.3) and two-deck (M5.6) modes.
///
/// In single-deck mode this is a thin wrapper around
/// [`InputArgs::to_options`] — the legacy path, untouched.
///
/// In two-deck mode (`deck_b_channels = Some([5, 6])`):
///
/// 1. The AU is opened with `channels = 4` (or however many we need
///    to span both decks' channels).
/// 2. `channel_map` is `[a_l-1, a_r-1, b_l-1, b_r-1]` — 0-based
///    device channel indices for the SL3-style "deck A on 3+4,
///    deck B on 5+6" layout.
/// 3. `output_pairs = [(0, 1), (2, 3)]` — both pairs are stereo
///    contiguous in the AU's logical (post-channel-map) frame, so
///    pair indices map cleanly to logical positions.
///
/// # Errors
/// Deck A and deck B input pairs must not overlap (a shared channel
/// between decks is almost always a bug). Both must be stereo pairs.
pub fn build_input_options(
    input: &InputArgs,
    deck_b_channels: Option<&[u32]>,
) -> Result<dub_audio::InputOptions> {
    let mut opts = input.to_options();
    let Some(b) = deck_b_channels else {
        return Ok(opts);
    };
    let a = input.input_channels.as_deref().ok_or_else(|| {
        anyhow!("two-deck mode requires --input-channels for deck A (e.g. 3,4 for SL3 deck A)")
    })?;
    if a.len() != 2 {
        return Err(anyhow!(
            "two-deck mode requires --input-channels to be a pair (got {} channels)",
            a.len()
        ));
    }
    if b.len() != 2 {
        return Err(anyhow!(
            "--deck-b-input-channels must be a pair (got {} channels)",
            b.len()
        ));
    }
    let overlap = a.iter().any(|c| b.contains(c));
    if overlap {
        return Err(anyhow!(
            "--input-channels {a:?} and --deck-b-input-channels {b:?} share a channel; \
             each deck needs its own stereo pair (SL3: 3,4 + 5,6)"
        ));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let channel_map: Vec<i32> = a.iter().chain(b.iter()).map(|&c| (c as i32) - 1).collect();
    opts.channels = 4;
    opts.channel_map = Some(channel_map);
    opts.output_pairs = Some(vec![(0, 1), (2, 3)]);
    Ok(opts)
}
