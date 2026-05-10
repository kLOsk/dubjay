//! Calibration data + math + IO (M5.4.2).
//!
//! Persisted as JSON at `~/.dub/calibration/<device_key>.json`. One
//! file per (device_name, format) tuple. The file's `fingerprint`
//! field captures the *rig* signature (carrier amplitude /
//! confidence percentiles) — soundcard alone is insufficient because
//! cartridge / preamp / cabling all change the carrier's
//! characteristics. On startup `dub timecode-deck` probes the
//! current carrier briefly and compares against this fingerprint;
//! mismatch triggers automatic recalibration.
//!
//! ## Why all the percentile data
//!
//! We deliberately store P5 / P50 / P95 of both amplitude and
//! confidence for both phases (carrier + lift). The current
//! [`derive_thresholds`] only consumes a handful of these, but the
//! shape of the distribution is what makes the JSON forward-
//! compatible: a future formula change (e.g. M5.4.3 continuous
//! adaptation, M6 Traktor support) can re-derive thresholds from
//! existing files without forcing a remeasurement.
//!
//! ## What the fingerprint deliberately excludes
//!
//! Lift noise is *not* in the fingerprint. Lift noise is
//! environment-dominated — a quiet bedroom and a thumping club
//! through the same rig produce wildly different lift_p95 values.
//! Including lift in the fingerprint would false-flag every venue
//! change as "rig changed". Carrier amplitude is the right
//! invariant: it's the cartridge's signal level, dominant over
//! ambient noise on the wire.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use dub_timecode::Format;
use serde::{Deserialize, Serialize};

/// Schema version for the calibration JSON. Bump when the on-disk
/// format changes incompatibly so readers can refuse old/new files
/// rather than silently misinterpreting them.
pub const SCHEMA_VERSION: u32 = 1;

/// Default fingerprint match tolerance — `|saved - observed| / saved`.
/// 30 % catches cartridge swaps (always ≥ 50 % output change) without
/// false-flagging stylus age drift (typically 5-20 % over years) or
/// preamp gain drift between sessions (typically < 10 %).
pub const DEFAULT_FINGERPRINT_TOLERANCE: f32 = 0.30;

/// Below this carrier-to-lift SNR we surface a warning at calibrate
/// time: "OK for lab, may struggle in clubs". Clubs typically raise
/// lift noise by 10–100×, so SNR ≥ 50 at home gives ~5× margin in
/// loud venues.
pub const SNR_WARN_THRESHOLD: f32 = 50.0;

/// Below this carrier-to-lift SNR we *fail* calibration outright: a
/// stylus / preamp / cabling problem is far more likely than a
/// legitimately marginal rig at SNR < 10. Better to surface this as
/// an error than to ship thresholds that won't work.
pub const SNR_FAIL_THRESHOLD: f32 = 10.0;

/// Stored carrier statistics that uniquely identify the cartridge +
/// preamp + cabling chain. Same soundcard with a different cartridge
/// produces a totally different signature; same setup at home and at
/// the club produces (almost exactly) the same signature, so venue
/// changes do *not* trigger spurious recalibration.
///
/// Lift values are deliberately absent — see module docs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct RigFingerprint {
    /// Median carrier amplitude (RMS). Robust to brief dropouts.
    pub carrier_amp_p50: f32,
    /// 95th-percentile carrier amplitude. Captures the upper end of
    /// the cartridge's output level.
    pub carrier_amp_p95: f32,
    /// Median carrier decoder confidence. Distinguishes a clean
    /// cartridge (≈ 0.99) from a worn / dirty one (≈ 0.85).
    pub carrier_conf_p50: f32,
}

impl RigFingerprint {
    /// True iff `observed` is within `tolerance` (relative) of
    /// `self` on each fingerprint dimension. Used at
    /// `dub timecode-deck` startup to validate that the saved
    /// thresholds still match the rig in front of the user.
    #[must_use]
    pub fn matches(&self, observed: &Self, tolerance: f32) -> bool {
        within_relative(self.carrier_amp_p50, observed.carrier_amp_p50, tolerance)
            && within_relative(self.carrier_amp_p95, observed.carrier_amp_p95, tolerance)
            && within_relative(self.carrier_conf_p50, observed.carrier_conf_p50, tolerance)
    }

    /// Largest *relative* delta across the three fingerprint
    /// dimensions. Returned by [`Self::matches`]'s diagnostic
    /// twin so callers can show the user *how* far off they are
    /// when a mismatch fires.
    #[must_use]
    pub fn max_relative_delta(&self, observed: &Self) -> f32 {
        let d_amp_p50 = relative_delta(self.carrier_amp_p50, observed.carrier_amp_p50);
        let d_amp_p95 = relative_delta(self.carrier_amp_p95, observed.carrier_amp_p95);
        let d_conf_p50 = relative_delta(self.carrier_conf_p50, observed.carrier_conf_p50);
        d_amp_p50.max(d_amp_p95).max(d_conf_p50)
    }
}

/// Relative delta `|saved - observed| / |saved|`. `saved == 0`
/// returns `f32::INFINITY` (so it's flagged as mismatched) unless
/// `observed == 0` too (returns 0).
fn relative_delta(saved: f32, observed: f32) -> f32 {
    if !saved.is_finite() || !observed.is_finite() {
        return f32::INFINITY;
    }
    let abs_saved = saved.abs();
    if abs_saved < f32::EPSILON {
        return if observed.abs() < f32::EPSILON {
            0.0
        } else {
            f32::INFINITY
        };
    }
    ((saved - observed).abs()) / abs_saved
}

/// Helper for [`RigFingerprint::matches`] — true iff the relative
/// delta is at most `tolerance`.
fn within_relative(saved: f32, observed: f32, tolerance: f32) -> bool {
    relative_delta(saved, observed) <= tolerance
}

/// Statistics from one measurement phase. Captures the shape of the
/// distribution so future formula changes can re-derive thresholds
/// without remeasuring.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct MeasurementStats {
    pub amplitude_p5: f32,
    pub amplitude_p50: f32,
    pub amplitude_p95: f32,
    pub confidence_p5: f32,
    pub confidence_p50: f32,
    pub confidence_p95: f32,
    /// Number of decoder blocks included in the percentiles. Small
    /// counts (< 100) should produce a warning at calibrate time —
    /// the percentiles are too noisy to trust.
    pub n_blocks: u32,
}

/// Both phases captured by `dub calibrate`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CalibrationMeasurements {
    pub carrier: MeasurementStats,
    pub lift: MeasurementStats,
}

/// Threshold values seeded from the measurements. These map 1:1 onto
/// the relevant fields of [`dub_engine::TimecodeInputConfig`];
/// `dub timecode-deck` builds the engine config from these on
/// startup.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct CalibrationThresholds {
    pub engage: f32,
    pub disengage: f32,
    pub amplitude: f32,
    pub sticky_blocks_to_disengage: u32,
}

/// Top-level calibration record. One file per (device_name, format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Calibration {
    /// On-disk schema version. See [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// CoreAudio HAL device name at calibration time (e.g. "SL 3").
    /// Combined with `format` to derive the on-disk filename.
    pub device_name: String,
    /// Timecode format; v1 = `"serato-cv02"`.
    pub format: String,
    /// RFC-3339 / ISO-8601 UTC timestamp. Read by the freshness
    /// warning ("calibration is N days old — consider recalibrating
    /// in this venue").
    pub calibrated_at: String,
    /// Sample rate the engine was running at. Stored for diagnostic
    /// purposes; if the user later switches device SR, we'd want to
    /// recalibrate even if the fingerprint matched (block size in
    /// frames means a different number of seconds at a different
    /// SR).
    pub input_sample_rate: f32,
    /// Block size used by the lift policy. Stored so M5.4.3
    /// continuous adaptation can validate that the running engine
    /// uses the same block size as the calibrator (sticky window
    /// is block-counted, so cadence matters).
    pub block_frames: u32,
    pub fingerprint: RigFingerprint,
    pub thresholds: CalibrationThresholds,
    pub measurements: CalibrationMeasurements,
    /// `carrier_amp_p5 / lift_amp_p95`. Recorded so the timecode-
    /// deck startup banner can surface it ("SNR margin 480× —
    /// excellent"). See [`SNR_WARN_THRESHOLD`] /
    /// [`SNR_FAIL_THRESHOLD`].
    pub snr_margin: f32,
}

// =====================================================================
// Math: percentiles, threshold derivation, fingerprint matching.
// All pure, fully unit-testable. Sample data is never f64-precision
// (the decoder works in f32) but percentiles are computed in f32 so
// the input slice can be operated on in place.
// =====================================================================

/// Compute the value at the given percentile (`pct` in `[0, 100]`)
/// of the input slice. **Sorts `data` in place** — caller decides
/// whether that matters (in this module we always pass throwaway
/// vectors, so the sort is fine).
///
/// Empty inputs return `0.0`. NaN values are handled via
/// `total_cmp` so they sort to the end and don't poison the
/// percentile selection (the result is well-defined as long as
/// the non-NaN portion is non-empty).
#[must_use]
pub fn percentile_inplace(data: &mut [f32], pct: f32) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    data.sort_by(|a, b| a.total_cmp(b));
    let pct = pct.clamp(0.0, 100.0);
    #[allow(clippy::cast_precision_loss)]
    let idx = ((pct / 100.0) * (data.len() - 1) as f32).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let idx = (idx as usize).min(data.len() - 1);
    data[idx]
}

/// Compute P5/P50/P95 in one pass (well, three sorts — but
/// percentile_inplace re-sorts for each call, which is a O(n log n)
/// no-op once sorted; we're not chasing performance here).
#[must_use]
pub fn measurement_stats_from_samples(
    amplitudes: &mut [f32],
    confidences: &mut [f32],
) -> MeasurementStats {
    debug_assert_eq!(
        amplitudes.len(),
        confidences.len(),
        "amplitudes and confidences are paired per-block; lengths must match"
    );
    #[allow(clippy::cast_possible_truncation)]
    let n_blocks = amplitudes.len() as u32;
    MeasurementStats {
        amplitude_p5: percentile_inplace(amplitudes, 5.0),
        amplitude_p50: percentile_inplace(amplitudes, 50.0),
        amplitude_p95: percentile_inplace(amplitudes, 95.0),
        confidence_p5: percentile_inplace(confidences, 5.0),
        confidence_p50: percentile_inplace(confidences, 50.0),
        confidence_p95: percentile_inplace(confidences, 95.0),
        n_blocks,
    }
}

/// Derived threshold values for the user's rig.
///
/// **engage**: 0.03 below the carrier's 5th-percentile confidence,
/// floored at 0.7 (the M5.3 default — keeps engagement reachable
/// even on a borderline rig with carrier_conf_p5 around 0.73).
/// Capped at 1.0 (the decoder's hard ceiling).
///
/// **disengage**: 0.50 — the M5.3 default. v1 doesn't measure scratch
/// transients (the most demanding phase for the user; deferred to
/// M5.4.3+). Real-world testing across M5.3 + M5.4.1 sessions
/// confirmed 0.50 keeps scratching engaged without false re-engages.
///
/// **amplitude**: half the carrier's 5th-percentile amplitude. This
/// is the value the user manually arrived at (~50 % of carrier
/// minimum) during M5.4.1 SL3 testing — leaves margin against
/// scratch-transient amplitude dips, well above lift noise on any
/// rig that passes the [`SNR_FAIL_THRESHOLD`] check.
///
/// **sticky**: 4 — the M5.3 default. Not measured.
///
/// # Errors
/// Returns `None` when carrier/lift SNR is below
/// [`SNR_FAIL_THRESHOLD`] (typically a stylus / preamp / cabling
/// problem); the caller surfaces this to the user instead of
/// shipping thresholds that won't work.
#[must_use]
pub fn derive_thresholds(
    carrier: &MeasurementStats,
    lift: &MeasurementStats,
) -> Option<CalibrationThresholds> {
    let snr = snr_margin(carrier, lift);
    if snr < SNR_FAIL_THRESHOLD {
        return None;
    }
    let engage = (carrier.confidence_p5 - 0.03).clamp(0.7, 1.0);
    let amplitude = carrier.amplitude_p5 * 0.5;
    Some(CalibrationThresholds {
        engage,
        disengage: 0.50,
        amplitude,
        sticky_blocks_to_disengage: 4,
    })
}

/// Carrier-to-lift SNR as a simple ratio of amplitude percentiles.
/// "Margin" not "ratio" because we use the 5 % carrier and 95 %
/// lift — the conservative ends that bound the safe band.
#[must_use]
pub fn snr_margin(carrier: &MeasurementStats, lift: &MeasurementStats) -> f32 {
    if lift.amplitude_p95 < f32::EPSILON {
        // No lift noise observed — return a sentinel "huge" value so
        // the SNR check trivially passes. (In practice the lift
        // capture phase needs *some* signal to call itself a lift,
        // but a perfectly silent input is a valid input.)
        return f32::INFINITY;
    }
    carrier.amplitude_p5 / lift.amplitude_p95
}

// =====================================================================
// IO: filesystem layout, JSON load/save, path resolution.
// =====================================================================

/// Format the timecode format as the on-disk string. Stable across
/// versions — dropping a format would be a schema break, not a code
/// rename.
#[must_use]
pub fn format_string(format: Format) -> &'static str {
    // M6: thin wrapper over `Format::cli_name()`. Kept as a separate
    // public symbol because the calibration JSON schema treats this
    // as the on-disk format key — renaming `Format::cli_name` later
    // shouldn't silently rewrite every calibration file's `format`
    // field. If the canonical CLI vocabulary ever needs to diverge
    // from the on-disk vocabulary, this is where to fork.
    format.cli_name()
}

/// Sanitize a CoreAudio device name for use in a filename.
/// CoreAudio device names contain spaces, slashes, colons, and other
/// glyphs we don't want in `~/.dub/calibration/...` filenames.
#[must_use]
pub fn sanitize_device_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Stable on-disk key for a (device, format) pair.
#[must_use]
pub fn device_key(device_name: &str, format: Format) -> String {
    format!(
        "{}_{}",
        sanitize_device_name(device_name),
        format_string(format)
    )
}

/// Default base directory: `~/.dub/calibration/`. Created on demand
/// by [`Calibration::save`] so no extra setup step is required for
/// first-time users.
///
/// # Errors
/// `$HOME` not set (extremely unlikely on macOS).
pub fn default_calibration_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("$HOME not set")?;
    Ok(PathBuf::from(home).join(".dub").join("calibration"))
}

impl Calibration {
    /// On-disk path for a calibration with the given device + format
    /// inside `base_dir`. Filename is `<device_key>.json`.
    #[must_use]
    pub fn path_for(device_name: &str, format: Format, base_dir: &Path) -> PathBuf {
        base_dir.join(format!("{}.json", device_key(device_name, format)))
    }

    /// Read + parse a calibration JSON file.
    ///
    /// # Errors
    /// File not found, IO failure, JSON parse error, or schema
    /// version mismatch (bumped past `SCHEMA_VERSION`).
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading calibration at {}", path.display()))?;
        let cal: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing calibration at {}", path.display()))?;
        if cal.schema_version > SCHEMA_VERSION {
            return Err(anyhow!(
                "calibration {} has schema_version {} but this build only \
                 understands up to {} — upgrade Dub or recalibrate",
                path.display(),
                cal.schema_version,
                SCHEMA_VERSION
            ));
        }
        Ok(cal)
    }

    /// Atomically write the calibration JSON. Parent directories are
    /// created if missing. The write goes to a sibling `.tmp` file
    /// then renames into place, so a crash mid-write doesn't
    /// corrupt the previous calibration.
    ///
    /// # Errors
    /// IO failures (permissions, disk full, …).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating calibration dir {}", parent.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self).context("serializing calibration")?;
        std::fs::write(&tmp, &bytes)
            .with_context(|| format!("writing temp calibration at {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("committing calibration at {}", path.display()))?;
        Ok(())
    }
}

// =====================================================================
// Tests — pure logic only; the calibrate driver is tested by
// running it against real hardware (see ARCHITECTURE.md). Most
// tests use the user's actual SL3 measurements from the M5.4.1
// session as fixtures.
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_stats() -> MeasurementStats {
        MeasurementStats {
            amplitude_p5: 0.0,
            amplitude_p50: 0.0,
            amplitude_p95: 0.0,
            confidence_p5: 0.0,
            confidence_p50: 0.0,
            confidence_p95: 0.0,
            n_blocks: 0,
        }
    }

    /// User's M5.4.1 SL3 + Concorde Pro fixture. Real numbers from the
    /// live session. The pinned tests below confirm the formula
    /// produces the user's hand-found thresholds (or close to them).
    fn sl3_fixture() -> (MeasurementStats, MeasurementStats) {
        let carrier = MeasurementStats {
            amplitude_p5: 0.247,
            amplitude_p50: 0.31,
            amplitude_p95: 0.42,
            confidence_p5: 0.97,
            confidence_p50: 0.99,
            confidence_p95: 1.0,
            n_blocks: 1880,
        };
        let lift = MeasurementStats {
            amplitude_p5: 0.0001,
            amplitude_p50: 0.0003,
            amplitude_p95: 0.0005,
            confidence_p5: 0.0,
            confidence_p50: 0.1,
            confidence_p95: 0.4,
            n_blocks: 940,
        };
        (carrier, lift)
    }

    // ---- percentiles --------------------------------------------------

    #[test]
    fn percentile_empty_returns_zero() {
        assert_eq!(percentile_inplace(&mut [], 50.0), 0.0);
    }

    #[test]
    fn percentile_singleton_returns_value() {
        assert_eq!(percentile_inplace(&mut [0.42], 0.0), 0.42);
        assert_eq!(percentile_inplace(&mut [0.42], 50.0), 0.42);
        assert_eq!(percentile_inplace(&mut [0.42], 100.0), 0.42);
    }

    #[test]
    fn percentile_uniform_distribution() {
        let mut data: Vec<f32> = (0..=100).map(|i| i as f32 / 100.0).collect();
        // P5 ≈ 0.05, P50 = 0.50, P95 ≈ 0.95.
        assert!((percentile_inplace(&mut data, 5.0) - 0.05).abs() < 0.01);
        assert!((percentile_inplace(&mut data, 50.0) - 0.50).abs() < 0.01);
        assert!((percentile_inplace(&mut data, 95.0) - 0.95).abs() < 0.01);
    }

    #[test]
    fn percentile_clamps_out_of_range_pct() {
        let data = [0.1_f32, 0.2, 0.3, 0.4, 0.5];
        // pct < 0 clamps to 0 → smallest value.
        assert_eq!(percentile_inplace(&mut data.to_vec(), -10.0), 0.1);
        // pct > 100 clamps to 100 → largest value.
        assert_eq!(percentile_inplace(&mut data.to_vec(), 150.0), 0.5);
    }

    // ---- threshold derivation ----------------------------------------

    #[test]
    fn derive_thresholds_user_sl3_matches_hand_calibration() {
        // The user found by hand (M5.4.1): engage 0.95-0.98, amp
        // ~0.10-0.15, was advised 0.95 / 0.12. The derived values
        // should land within ~10 % of those — the formula is the
        // automation of the "find the cliff, step back" procedure
        // the user did manually.
        let (carrier, lift) = sl3_fixture();
        let t = derive_thresholds(&carrier, &lift).expect("SL3 has plenty of SNR");
        // engage = 0.97 - 0.03 = 0.94. Within 0.01 of advised 0.95.
        assert!(
            (t.engage - 0.94).abs() < 0.001,
            "engage should be 0.94, got {}",
            t.engage
        );
        // amplitude = 0.247 / 2 = 0.1235. Within 0.005 of advised 0.12.
        assert!(
            (t.amplitude - 0.1235).abs() < 0.001,
            "amplitude should be 0.1235, got {}",
            t.amplitude
        );
        // disengage + sticky still default.
        assert!((t.disengage - 0.50).abs() < 1e-6);
        assert_eq!(t.sticky_blocks_to_disengage, 4);
    }

    #[test]
    fn derive_thresholds_engage_floor_is_0_7() {
        // Marginal rig with carrier_conf_p5 = 0.65 — formula would
        // suggest engage = 0.62, which is below the 0.7 floor. We
        // expect the floor to clamp.
        let mut carrier = sl3_fixture().0;
        carrier.confidence_p5 = 0.65;
        let lift = sl3_fixture().1;
        let t = derive_thresholds(&carrier, &lift).unwrap();
        assert!(
            (t.engage - 0.7).abs() < 1e-6,
            "engage floor is 0.7, got {}",
            t.engage
        );
    }

    #[test]
    fn derive_thresholds_engage_caps_at_1_0() {
        // Pristine rig with carrier_conf_p5 = 1.0 — formula gives
        // 0.97, well below cap. But if for some reason
        // confidence_p5 were spuriously 1.05 (shouldn't happen,
        // decoder clamps), we still cap at 1.0.
        let mut carrier = sl3_fixture().0;
        carrier.confidence_p5 = 1.05;
        let lift = sl3_fixture().1;
        let t = derive_thresholds(&carrier, &lift).unwrap();
        assert!(t.engage <= 1.0);
    }

    #[test]
    fn derive_thresholds_rejects_too_low_snr() {
        // Lift noise too high relative to carrier — likely a stylus
        // or preamp problem, refuse to ship thresholds.
        let mut carrier = sl3_fixture().0;
        carrier.amplitude_p5 = 0.05;
        let mut lift = sl3_fixture().1;
        lift.amplitude_p95 = 0.01; // SNR = 5, below SNR_FAIL_THRESHOLD = 10
        let t = derive_thresholds(&carrier, &lift);
        assert!(t.is_none(), "low-SNR rig should fail derivation");
    }

    #[test]
    fn snr_margin_user_sl3_is_excellent() {
        let (carrier, lift) = sl3_fixture();
        let snr = snr_margin(&carrier, &lift);
        // 0.247 / 0.0005 = 494. Well above the 50 warn threshold.
        assert!((snr - 494.0).abs() < 1.0, "expected SNR ≈ 494, got {snr}");
        assert!(snr > SNR_WARN_THRESHOLD);
    }

    #[test]
    fn snr_margin_empty_lift_is_infinite() {
        let (carrier, mut lift) = sl3_fixture();
        lift.amplitude_p95 = 0.0;
        assert!(snr_margin(&carrier, &lift).is_infinite());
    }

    // ---- fingerprint match -------------------------------------------

    fn fp(a50: f32, a95: f32, c50: f32) -> RigFingerprint {
        RigFingerprint {
            carrier_amp_p50: a50,
            carrier_amp_p95: a95,
            carrier_conf_p50: c50,
        }
    }

    #[test]
    fn fingerprint_same_rig_matches() {
        let saved = fp(0.31, 0.42, 0.99);
        let observed = fp(0.32, 0.41, 0.99);
        assert!(saved.matches(&observed, DEFAULT_FINGERPRINT_TOLERANCE));
    }

    #[test]
    fn fingerprint_cartridge_swap_does_not_match() {
        // Concorde → Nightclub (~2× output level): always flagged.
        let saved = fp(0.31, 0.42, 0.99);
        let observed = fp(0.78, 1.05, 0.99);
        assert!(!saved.matches(&observed, DEFAULT_FINGERPRINT_TOLERANCE));
    }

    #[test]
    fn fingerprint_borderline_match_at_30_percent() {
        // Saved 0.31, observed 0.40 → 29 % delta → matches at 30 %.
        let saved = fp(0.31, 0.42, 0.99);
        let observed = fp(0.40, 0.55, 0.99);
        // p50 delta = 0.29 OK, p95 delta = 0.31 NOT OK at tolerance 0.30.
        assert!(!saved.matches(&observed, 0.30));
        // At tolerance 0.35 it passes.
        assert!(saved.matches(&observed, 0.35));
    }

    #[test]
    fn fingerprint_zero_saved_is_infinite_delta() {
        let saved = fp(0.0, 0.42, 0.99);
        let observed = fp(0.001, 0.41, 0.99);
        assert!(!saved.matches(&observed, DEFAULT_FINGERPRINT_TOLERANCE));
        assert!(saved.max_relative_delta(&observed).is_infinite());
    }

    #[test]
    fn fingerprint_both_zero_is_zero_delta() {
        let saved = fp(0.0, 0.0, 0.0);
        let observed = fp(0.0, 0.0, 0.0);
        assert!(saved.matches(&observed, 0.0));
    }

    #[test]
    fn fingerprint_max_delta_returns_largest_dimension() {
        let saved = fp(0.5, 0.5, 0.5);
        let observed = fp(0.55, 0.6, 0.51); // deltas 10 %, 20 %, 2 %
        let d = saved.max_relative_delta(&observed);
        assert!((d - 0.20).abs() < 0.01, "expected 0.20, got {d}");
    }

    // ---- IO: paths + serialization -----------------------------------

    #[test]
    fn sanitize_device_name_strips_punctuation() {
        assert_eq!(sanitize_device_name("SL 3"), "SL_3");
        assert_eq!(sanitize_device_name("Audio 6/USB"), "Audio_6_USB");
        assert_eq!(
            sanitize_device_name("Some : weird ; name"),
            "Some___weird___name"
        );
        // Already-clean names pass through unchanged.
        assert_eq!(sanitize_device_name("MyDevice-1"), "MyDevice-1");
    }

    #[test]
    fn device_key_includes_format() {
        assert_eq!(device_key("SL 3", Format::SeratoCv02), "SL_3_serato-cv02");
    }

    #[test]
    fn path_for_uses_device_key_filename() {
        let base = Path::new("/tmp/whatever");
        let p = Calibration::path_for("SL 3", Format::SeratoCv02, base);
        assert_eq!(p, Path::new("/tmp/whatever/SL_3_serato-cv02.json"));
    }

    #[test]
    fn calibration_round_trip_via_tempfile() {
        let (carrier, lift) = sl3_fixture();
        let cal = Calibration {
            schema_version: SCHEMA_VERSION,
            device_name: "SL 3".to_string(),
            format: format_string(Format::SeratoCv02).to_string(),
            calibrated_at: "2026-05-10T07:00:00Z".to_string(),
            input_sample_rate: 48_000.0,
            block_frames: 256,
            fingerprint: fp(
                carrier.amplitude_p50,
                carrier.amplitude_p95,
                carrier.confidence_p50,
            ),
            thresholds: derive_thresholds(&carrier, &lift).unwrap(),
            measurements: CalibrationMeasurements { carrier, lift },
            snr_margin: snr_margin(&carrier, &lift),
        };

        // Save + load via a tempdir.
        let dir = std::env::temp_dir().join(format!("dub-calibration-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SL_3_serato-cv02.json");
        cal.save(&path).expect("save");
        let loaded = Calibration::load(&path).expect("load");

        // Check round-trip fidelity on the structured fields.
        assert_eq!(loaded.schema_version, cal.schema_version);
        assert_eq!(loaded.device_name, cal.device_name);
        assert_eq!(loaded.format, cal.format);
        assert!((loaded.thresholds.engage - cal.thresholds.engage).abs() < 1e-6);
        assert!((loaded.thresholds.amplitude - cal.thresholds.amplitude).abs() < 1e-6);
        assert!(loaded.fingerprint.matches(&cal.fingerprint, 1e-6));
        // SNR is f32, so allow a tiny rounding tolerance.
        assert!((loaded.snr_margin - cal.snr_margin).abs() < 0.1);

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calibration_load_rejects_future_schema_version() {
        // Hand-craft a JSON with schema_version = 999.
        let dir = std::env::temp_dir().join(format!(
            "dub-calibration-future-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("future.json");
        let json = r#"{
            "schema_version": 999,
            "device_name": "x",
            "format": "serato-cv02",
            "calibrated_at": "2026-01-01T00:00:00Z",
            "input_sample_rate": 48000.0,
            "block_frames": 256,
            "fingerprint": {"carrier_amp_p50": 0.1, "carrier_amp_p95": 0.2, "carrier_conf_p50": 0.99},
            "thresholds": {"engage": 0.9, "disengage": 0.5, "amplitude": 0.05, "sticky_blocks_to_disengage": 4},
            "measurements": {
                "carrier": {"amplitude_p5": 0.1, "amplitude_p50": 0.15, "amplitude_p95": 0.2, "confidence_p5": 0.99, "confidence_p50": 0.99, "confidence_p95": 1.0, "n_blocks": 100},
                "lift": {"amplitude_p5": 0.001, "amplitude_p50": 0.001, "amplitude_p95": 0.001, "confidence_p5": 0.0, "confidence_p50": 0.0, "confidence_p95": 0.5, "n_blocks": 100}
            },
            "snr_margin": 100.0
        }"#;
        std::fs::write(&path, json).unwrap();
        let r = Calibration::load(&path);
        assert!(r.is_err(), "future schema version should be rejected");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn measurement_stats_from_samples_computes_p5_p50_p95() {
        let mut amps: Vec<f32> = (0..=100).map(|i| i as f32 / 100.0).collect();
        let mut confs: Vec<f32> = vec![0.99; 101];
        let s = measurement_stats_from_samples(&mut amps, &mut confs);
        assert!((s.amplitude_p5 - 0.05).abs() < 0.01);
        assert!((s.amplitude_p50 - 0.50).abs() < 0.01);
        assert!((s.amplitude_p95 - 0.95).abs() < 0.01);
        assert!((s.confidence_p50 - 0.99).abs() < 1e-6);
        assert_eq!(s.n_blocks, 101);
    }

    /// Avoid the warning: `empty_stats` is used by future tests for
    /// edge cases (zero-block measurement); keep it around but
    /// silence the dead-code lint.
    #[allow(dead_code)]
    fn _keep_empty_stats_alive() {
        let _ = empty_stats();
    }
}
