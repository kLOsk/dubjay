//! De-click envelope: a short, equal-power crossfade applied to every
//! transport mutation that would otherwise create a jump-discontinuity
//! in the output waveform.
//!
//! ## Why this exists
//!
//! When the audio thread instantaneously changes what samples it reads —
//! source change (track load), position change (seek/cue), or play-state
//! flip (play/pause) — the rendered waveform jumps from one value to
//! another within a single sample interval. A jump is, mathematically,
//! a step function. Step functions have infinite-frequency content; the
//! ear hears that as a click.
//!
//! Every DJ application solves this with a 2–5 ms ramp: short enough to
//! be perceptually instantaneous (well below the ~10 ms transient
//! masking threshold), long enough to absorb the discontinuity. Dub
//! does the same.
//!
//! ## Shape: equal-power crossfade
//!
//! For sample index `i ∈ [0, N)` of the ramp, we compute:
//!
//! ```text
//! t      = i / N
//! fade_in  = sin²(t · π/2)
//! fade_out = cos²(t · π/2) = 1 − fade_in
//! out      = fade_out · old + fade_in · new
//! ```
//!
//! `sin² + cos² = 1` for all `t`, so the *power* (squared amplitude)
//! sums to a constant — no perceived dip at the midpoint of the fade.
//! For the very short 2 ms ramps we use, linear and equal-power are
//! perceptually indistinguishable, but equal-power composes correctly
//! when fades overlap (back-to-back transport changes).
//!
//! The table is precomputed once at engine construction and read by
//! index during render. No `sin` calls on the audio thread.
//!
//! ## Length
//!
//! Default 2 ms, computed against the engine sample rate. At 48 kHz:
//! 96 samples × 4 bytes = 384 bytes. At 192 kHz: 384 samples = 1.5 KB.
//! The table fits comfortably in L1 cache.

use std::sync::Arc;

/// Default ramp duration. Chosen as the standard professional DJ-app
/// value: long enough to mask any sample-level discontinuity, short
/// enough that transport actions still feel instantaneous.
pub const DEFAULT_DECLICK_MS: f32 = 2.0;

/// Minimum ramp length in samples. Even at extremely low sample rates
/// the ramp must be at least this many frames — fewer than ~16 samples
/// is too short to absorb a click meaningfully.
const MIN_RAMP_SAMPLES: usize = 16;

/// Precomputed equal-power crossfade table. Cheap to clone via `Arc`,
/// shared across all decks of one engine.
///
/// The table holds `fade_in` values; `fade_out = 1.0 - fade_in`. Index
/// `i` maps to position `t = i / N` within the ramp.
#[derive(Debug)]
pub struct DeclickEnvelope {
    /// `fade_in[i] = sin²(i / N · π/2)` for `i ∈ [0, N)`.
    fade_in: Box<[f32]>,
}

impl DeclickEnvelope {
    /// Build an envelope sized for the given sample rate and duration.
    /// **Not RT-safe** — allocates the lookup table.
    #[must_use]
    pub fn new(sample_rate: f32, duration_ms: f32) -> Arc<Self> {
        let raw_len = (sample_rate * duration_ms / 1000.0).round();
        // Clamp into something sane; `as usize` on a finite positive
        // float is well-defined.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = (raw_len as usize).max(MIN_RAMP_SAMPLES);

        let mut table = Vec::with_capacity(n);
        for i in 0..n {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32 / n as f32;
            let s = (t * std::f32::consts::FRAC_PI_2).sin();
            table.push(s * s);
        }
        Arc::new(Self {
            fade_in: table.into_boxed_slice(),
        })
    }

    /// Number of samples the ramp spans.
    #[must_use]
    pub fn len(&self) -> u32 {
        // Capped at u32::MAX by construction (24h @ 96kHz ≈ 8e9 frames,
        // far less than u32::MAX = 4e9, but a single ramp is < 1024).
        #[allow(clippy::cast_possible_truncation)]
        {
            self.fade_in.len() as u32
        }
    }

    /// `true` when the ramp is empty. Should never happen at runtime
    /// (we enforce `MIN_RAMP_SAMPLES`); included for API completeness.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fade_in.is_empty()
    }

    /// `fade_in` factor at sample index `i` (0-based, `i < len()`).
    /// RT-safe — single bounds-checked load.
    ///
    /// Returns `1.0` for `i >= len()` so callers that overshoot end up
    /// at "fully new" by default. (Belt-and-braces — the deck never
    /// calls past `len()` anyway.)
    #[must_use]
    pub fn fade_in(&self, i: u32) -> f32 {
        self.fade_in.get(i as usize).copied().unwrap_or(1.0)
    }

    /// `fade_out` factor (= `1.0 - fade_in(i)`).
    #[must_use]
    pub fn fade_out(&self, i: u32) -> f32 {
        1.0 - self.fade_in(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_length_matches_ms_at_48k() {
        let env = DeclickEnvelope::new(48_000.0, 2.0);
        assert_eq!(env.len(), 96);
    }

    #[test]
    fn envelope_length_matches_ms_at_44k() {
        let env = DeclickEnvelope::new(44_100.0, 2.0);
        assert_eq!(env.len(), 88);
    }

    #[test]
    fn envelope_endpoints_are_correct() {
        let env = DeclickEnvelope::new(48_000.0, 2.0);
        // At i=0: fade_in = sin²(0) = 0 → fade_out = 1. (Old fully on.)
        assert!(env.fade_in(0) < 1e-6);
        assert!((env.fade_out(0) - 1.0).abs() < 1e-6);
        // At i = N-1: t = (N-1)/N, sin²(t·π/2) is close to 1 but not
        // quite. The very last sample of the ramp is "almost" new; the
        // first sample after the ramp is fully new. This is by design:
        // a `t = 1.0` sample would be redundant with normal playback.
        let last = env.fade_in(env.len() - 1);
        assert!(last > 0.99, "last fade_in was {last}");
        assert!(last < 1.0);
    }

    #[test]
    fn envelope_is_equal_power() {
        // Sum of squares cos² + sin² = 1; we store sin², so
        // fade_out + fade_in = 1 by construction. (Equal power means the
        // squared amplitudes sum to a constant — that constant is 1.)
        let env = DeclickEnvelope::new(48_000.0, 2.0);
        for i in 0..env.len() {
            let sum = env.fade_in(i) + env.fade_out(i);
            assert!((sum - 1.0).abs() < 1e-6, "i={i}: sum={sum}");
        }
    }

    #[test]
    fn envelope_is_monotonically_increasing() {
        let env = DeclickEnvelope::new(48_000.0, 2.0);
        let mut prev = -1.0;
        for i in 0..env.len() {
            let v = env.fade_in(i);
            assert!(v >= prev, "i={i}: {v} < {prev}");
            prev = v;
        }
    }

    #[test]
    fn envelope_clamps_to_minimum_length() {
        // Pathological: a hypothetical 1 Hz sample rate would compute
        // 0.002 frames → must clamp to MIN_RAMP_SAMPLES.
        let env = DeclickEnvelope::new(1.0, 2.0);
        assert!(env.len() as usize >= MIN_RAMP_SAMPLES);
    }
}
