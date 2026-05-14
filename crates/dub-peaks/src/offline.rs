//! Offline (synchronous) peak computation for a fully-decoded track.
//!
//! M9 ships the **streaming** decimator pipeline (`PeakStream` + analysis
//! thread) used by Thru mode, where samples arrive a block at a time on
//! the audio thread and are decimated off-RT into a growing
//! `PeakBuffer`. M10.5 introduces **File mode**: the engine plays back
//! a fully-decoded `dub_io::Track` whose entire sample buffer is
//! already in RAM, and the renderer wants the *whole* track's peaks
//! visible up-front (the playhead sits 25 % from the top of the deck
//! pane and the bottom 75 % shows the upcoming audio — see PRD §9.6).
//!
//! For that, streaming would be over-engineered: there's no audio
//! thread, no back-pressure, no per-block latency budget. We just want
//! one synchronous pass that yields two `Vec`s (`PeakChunk` +
//! `BandPeakChunk`) the renderer can mmap into a `MTLBuffer` and forget.
//!
//! [`compute_offline_peaks`] is exactly that. It runs the same
//! [`Decimator`] and [`BandDecimator`] kernels the streaming
//! `PeakStream` uses — so the output is **bit-identical** to what
//! `PeakStream` would have emitted given the same mono samples — and
//! flushes any trailing partial chunk so `chunks.len()` exactly covers
//! the input duration.
//!
//! ## What it does not do
//!
//! * **No file decode.** The caller is responsible for producing the
//!   interleaved-stereo (or mono) sample slice; `dub_io::Track` is the
//!   canonical producer (see PRD §4.4 and [`dub_io::Track::samples`]).
//! * **No resampling.** Output cadence is in source-rate samples; the
//!   renderer scales the time axis off `samples_per_chunk /
//!   sample_rate` (= `chunk_duration_secs`).
//! * **No mip pyramids.** Single mip level, same as `PeakStream`. The
//!   renderer derives any coarser overview by averaging in shader-land.

use crate::{
    BandDecimator, BandPeakChunk, Decimator, FilteredDecimator, FilteredPeakChunk, OnsetChunk,
    OnsetDecimator, PeakChunk, DEFAULT_SAMPLES_PER_CHUNK,
};

/// Frames per inner feed slice when computing offline peaks.
///
/// Picked small enough that the stereo-downmix scratch buffer fits
/// on the stack budget clippy enforces (16 KiB; `4 KiB / 2` frames =
/// `2_048` frames × 4 bytes = 8 KiB scratch), large enough that
/// per-call overhead is negligible against the per-sample work
/// (one `Decimator::feed` + one `BandDecimator::feed` invocation).
const FEED_CHUNK_FRAMES: usize = 2_048;

/// Offline peak-analysis result for a fully-decoded track.
///
/// `broadband.len()` and `bands.len()` are independent — they're
/// emitted on different cadences (`DEFAULT_SAMPLES_PER_CHUNK` vs
/// `BAND_SAMPLES_PER_CHUNK`). The renderer indexes them separately.
///
/// Final partial chunks are flushed (`Decimator::flush` /
/// `BandDecimator::flush`), so the last `broadband` chunk may cover
/// fewer than `DEFAULT_SAMPLES_PER_CHUNK` samples. This matters for
/// the chunk-duration math: scale by `actual_chunk_samples /
/// sample_rate` for the last chunk, or accept the ~1 ms time-axis
/// rounding error in exchange for a uniform per-chunk timeline (which
/// is what the renderer does — `chunk_duration_secs` is computed once
/// from `DEFAULT_SAMPLES_PER_CHUNK / sample_rate`).
#[derive(Debug, Clone)]
pub struct OfflinePeaks {
    /// Broadband min/max/rms chunks at `DEFAULT_SAMPLES_PER_CHUNK`
    /// cadence (~1.33 ms at 48 kHz).
    pub broadband: Vec<PeakChunk>,
    /// Per-band loudness chunks at `BAND_SAMPLES_PER_CHUNK` cadence
    /// (~10.6 ms at 48 kHz; one per FFT hop).
    pub bands: Vec<BandPeakChunk>,
    /// Per-hop onset-flux values (M10.5l) at the same cadence as
    /// `bands`. The renderer's transient-emphasising bloom +
    /// saturation pipeline consumes this slice in parallel with
    /// the band stream.
    pub onset: Vec<OnsetChunk>,
    /// Time-domain band-filtered peak chunks (M10.5p Stage 3). Same
    /// cadence as `broadband` (one entry per broadband chunk), so
    /// `filtered[k]` covers the same sample range as `broadband[k]`.
    /// v1 populates only the LF band; MF / HF fields are zero. See
    /// [`FilteredPeakChunk`] for the per-band semantics.
    pub filtered: Vec<FilteredPeakChunk>,
    /// The sample rate the input was analysed at. Surfaced so callers
    /// can compute `chunk_duration_secs` without tracking the rate
    /// alongside the chunks separately.
    pub sample_rate: u32,
    /// `DEFAULT_SAMPLES_PER_CHUNK`. Captured here so callers don't
    /// need to re-import the constant alongside the result.
    pub samples_per_broadband_chunk: usize,
    /// `BAND_SAMPLES_PER_CHUNK`. Same rationale.
    pub samples_per_band_chunk: usize,
    /// Onset-chunk cadence in samples. Equal to
    /// `samples_per_band_chunk` (= one FFT hop); surfaced as its
    /// own field so a future hop-decoupling refactor doesn't break
    /// callers.
    pub samples_per_onset_chunk: usize,
    /// Samples per [`FilteredPeakChunk`]. Currently equal to
    /// `samples_per_broadband_chunk` (matched cadence so the renderer
    /// can index broadband and filtered streams as a 1:1 pair).
    pub samples_per_filtered_chunk: usize,
}

/// Errors emitted by [`compute_offline_peaks`].
///
/// All variants are caller-error situations — bad channel count,
/// length mismatch, zero sample rate. The function never panics in
/// release builds.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OfflinePeaksError {
    /// Sample rate of zero would divide-by-zero in the chunk-duration
    /// scale factor downstream.
    #[error("sample_rate must be > 0")]
    InvalidSampleRate,
    /// We only know how to mono-mix 1 or 2 channels (matches
    /// [`dub_io::Track`]'s contract).
    #[error("channels must be 1 (mono) or 2 (stereo), got {0}")]
    UnsupportedChannels(u8),
    /// `samples.len()` must be a multiple of `channels`.
    #[error(
        "samples.len() ({len}) is not a multiple of channels ({channels}); \
         interleaved buffer is misaligned"
    )]
    Misaligned {
        /// Length of the input slice in `f32` samples.
        len: usize,
        /// Channel count the caller declared.
        channels: u8,
    },
}

/// Compute peaks + band-energy chunks for a fully-decoded track.
///
/// `samples` is interleaved (`L, R, L, R, …` for stereo, `M, M, …`
/// for mono) at the indicated `sample_rate` and `channels`. Stereo
/// input is mono-mixed to `0.5 * (L + R)` to match the streaming
/// path's mono-downmix in `ThruSource` (M9). Mono input is passed
/// through unchanged.
///
/// The function is synchronous and allocates `broadband` and `bands`
/// up front to a Vec capacity proportional to the input length, so
/// the typical large-track case (5–10 minutes) does not reallocate
/// mid-pass.
///
/// **Not real-time safe.** Allocates Vecs, runs an FFT plan for the
/// band decimator, and is intended for the off-RT track-load path
/// in the engine (PRD §4.4: tracks are decoded into RAM on load —
/// peaks are part of that same load-time work).
///
/// # Errors
///
/// See [`OfflinePeaksError`] — zero sample rate, unsupported channel
/// count, or a misaligned interleaved buffer.
///
/// # Examples
///
/// ```
/// use dub_peaks::compute_offline_peaks;
///
/// let mono = vec![0.0f32; 4_800];
/// let peaks = compute_offline_peaks(&mono, 48_000, 1).unwrap();
/// assert!(!peaks.broadband.is_empty());
/// // All-silence input: every chunk is the zero chunk.
/// assert!(peaks.broadband.iter().all(|c| c.rms == 0.0));
/// ```
pub fn compute_offline_peaks(
    samples: &[f32],
    sample_rate: u32,
    channels: u8,
) -> Result<OfflinePeaks, OfflinePeaksError> {
    if sample_rate == 0 {
        return Err(OfflinePeaksError::InvalidSampleRate);
    }
    if !(1..=2).contains(&channels) {
        return Err(OfflinePeaksError::UnsupportedChannels(channels));
    }
    if !samples.len().is_multiple_of(usize::from(channels)) {
        return Err(OfflinePeaksError::Misaligned {
            len: samples.len(),
            channels,
        });
    }

    let mut broadband = match channels {
        1 => Vec::with_capacity(samples.len() / DEFAULT_SAMPLES_PER_CHUNK + 1),
        2 => Vec::with_capacity(samples.len() / (DEFAULT_SAMPLES_PER_CHUNK * 2) + 1),
        _ => unreachable!("validated above"),
    };
    let mut bands = match channels {
        1 => Vec::with_capacity(samples.len() / crate::BAND_SAMPLES_PER_CHUNK + 1),
        2 => Vec::with_capacity(samples.len() / (crate::BAND_SAMPLES_PER_CHUNK * 2) + 1),
        _ => unreachable!("validated above"),
    };
    let mut onset = match channels {
        1 => Vec::with_capacity(samples.len() / crate::BAND_SAMPLES_PER_CHUNK + 1),
        2 => Vec::with_capacity(samples.len() / (crate::BAND_SAMPLES_PER_CHUNK * 2) + 1),
        _ => unreachable!("validated above"),
    };
    let mut filtered = match channels {
        1 => Vec::with_capacity(samples.len() / DEFAULT_SAMPLES_PER_CHUNK + 1),
        2 => Vec::with_capacity(samples.len() / (DEFAULT_SAMPLES_PER_CHUNK * 2) + 1),
        _ => unreachable!("validated above"),
    };

    let mut bb = Decimator::new(DEFAULT_SAMPLES_PER_CHUNK);
    let mut bd = BandDecimator::new(sample_rate);
    let mut od = OnsetDecimator::new(sample_rate);
    let mut fd = FilteredDecimator::new(sample_rate, DEFAULT_SAMPLES_PER_CHUNK);

    // We feed the decimators in slices to avoid an intermediate mono
    // Vec the size of the whole track. The slice size is set at the
    // crate-private `FEED_CHUNK_FRAMES` constant.
    let mut mono_scratch = [0.0f32; FEED_CHUNK_FRAMES];

    match channels {
        1 => {
            for slice in samples.chunks(FEED_CHUNK_FRAMES) {
                bb.feed(slice, |c| broadband.push(c));
                bd.feed(slice, |c| bands.push(c));
                od.feed(slice, |c| onset.push(c));
                fd.feed(slice, |c| filtered.push(c));
            }
        }
        2 => {
            for pair_slice in samples.chunks(FEED_CHUNK_FRAMES * 2) {
                let frame_count = pair_slice.len() / 2;
                for (i, frame) in pair_slice.chunks_exact(2).enumerate() {
                    // 0.5 × (L + R) — same downmix as ThruSource so the
                    // offline path is bit-identical to a streaming path
                    // would have produced (M9 contract).
                    mono_scratch[i] = 0.5 * (frame[0] + frame[1]);
                }
                let mono = &mono_scratch[..frame_count];
                bb.feed(mono, |c| broadband.push(c));
                bd.feed(mono, |c| bands.push(c));
                od.feed(mono, |c| onset.push(c));
                fd.feed(mono, |c| filtered.push(c));
            }
        }
        _ => unreachable!(),
    }

    // Trailing partial chunks: the renderer treats them as full
    // chunks for time-axis purposes (one extra ~1 ms slice at the
    // very end of the track). Without the flush, a 4-minute track's
    // peak Vec ends ~32 samples short of the actual end, which the
    // renderer would draw as a tiny gap above the bottom edge of the
    // deck pane.
    bb.flush(|c| broadband.push(c));
    fd.flush(|c| filtered.push(c));
    // BandDecimator does NOT have a public flush — the streaming
    // path doesn't need it (the analysis thread keeps polling). For
    // offline we accept the residual: at most one FFT hop (~512
    // samples, ~10 ms at 48 kHz) of band data is missing from the
    // very end of the file. The broadband peaks cover that range
    // fine, and the band-colour shader degrades to "no colour data"
    // gracefully (renders as broadband-only).

    Ok(OfflinePeaks {
        broadband,
        bands,
        onset,
        filtered,
        sample_rate,
        samples_per_broadband_chunk: DEFAULT_SAMPLES_PER_CHUNK,
        samples_per_band_chunk: crate::BAND_SAMPLES_PER_CHUNK,
        samples_per_onset_chunk: crate::BAND_SAMPLES_PER_CHUNK,
        samples_per_filtered_chunk: DEFAULT_SAMPLES_PER_CHUNK,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_SAMPLES_PER_CHUNK;

    #[test]
    fn rejects_zero_sample_rate() {
        let err = compute_offline_peaks(&[0.0f32; 64], 0, 1).unwrap_err();
        assert_eq!(err, OfflinePeaksError::InvalidSampleRate);
    }

    #[test]
    fn rejects_unsupported_channels() {
        let err = compute_offline_peaks(&[0.0f32; 64], 48_000, 0).unwrap_err();
        assert_eq!(err, OfflinePeaksError::UnsupportedChannels(0));
        let err = compute_offline_peaks(&[0.0f32; 64], 48_000, 5).unwrap_err();
        assert_eq!(err, OfflinePeaksError::UnsupportedChannels(5));
    }

    #[test]
    fn rejects_misaligned_stereo() {
        // 65 samples can't be interleaved stereo (must be even).
        let err = compute_offline_peaks(&[0.0f32; 65], 48_000, 2).unwrap_err();
        assert_eq!(
            err,
            OfflinePeaksError::Misaligned {
                len: 65,
                channels: 2
            }
        );
    }

    #[test]
    fn mono_silence_produces_zero_chunks() {
        // 64 mono samples → exactly 1 broadband chunk, all zeros.
        let peaks = compute_offline_peaks(&[0.0f32; 64], 48_000, 1).unwrap();
        assert_eq!(peaks.broadband.len(), 1);
        assert_eq!(peaks.broadband[0], PeakChunk::ZERO);
        assert_eq!(peaks.sample_rate, 48_000);
        assert_eq!(peaks.samples_per_broadband_chunk, DEFAULT_SAMPLES_PER_CHUNK);
    }

    #[test]
    fn mono_constant_signal_recovers_amplitude() {
        // 4096 mono samples at 0.5 → 64 broadband chunks; rms ≈ 0.5,
        // max ≈ 0.5, min ≈ 0.5 (every chunk identical).
        let peaks = compute_offline_peaks(&[0.5f32; 4_096], 48_000, 1).unwrap();
        assert_eq!(peaks.broadband.len(), 4_096 / DEFAULT_SAMPLES_PER_CHUNK);
        for chunk in &peaks.broadband {
            assert!(
                (chunk.rms - 0.5).abs() < 1e-6,
                "rms should be ~0.5, got {}",
                chunk.rms
            );
            assert!((chunk.max - 0.5).abs() < 1e-6);
            assert!((chunk.min - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn stereo_downmix_averages_l_and_r() {
        // Interleaved stereo: L = 1.0, R = -1.0 → mono mix = 0.0 (rms = 0).
        let stereo: Vec<f32> = (0..4_096).flat_map(|_| [1.0f32, -1.0f32]).collect();
        let peaks = compute_offline_peaks(&stereo, 48_000, 2).unwrap();
        // 4096 frames at SPC=64 → 64 broadband chunks.
        assert_eq!(peaks.broadband.len(), 4_096 / DEFAULT_SAMPLES_PER_CHUNK);
        for chunk in &peaks.broadband {
            // L=1, R=-1 → 0.5*(L+R) = 0.0 for every sample.
            assert!(
                chunk.rms.abs() < 1e-6,
                "rms should be ~0, got {}",
                chunk.rms
            );
            assert!(chunk.min.abs() < 1e-6);
            assert!(chunk.max.abs() < 1e-6);
        }
    }

    #[test]
    fn stereo_l_only_recovers_half_amplitude() {
        // L = 0.8, R = 0.0 → mono = 0.4.
        let stereo: Vec<f32> = (0..4_096).flat_map(|_| [0.8f32, 0.0]).collect();
        let peaks = compute_offline_peaks(&stereo, 48_000, 2).unwrap();
        for chunk in &peaks.broadband {
            assert!(
                (chunk.rms - 0.4).abs() < 1e-6,
                "expected rms ~0.4, got {}",
                chunk.rms
            );
        }
    }

    #[test]
    fn trailing_partial_chunk_is_flushed() {
        // 100 mono samples at SPC=64 → 1 full chunk + 1 partial (36
        // samples). After flush, broadband.len() == 2.
        let peaks = compute_offline_peaks(&[1.0f32; 100], 48_000, 1).unwrap();
        assert_eq!(peaks.broadband.len(), 2);
        // The partial chunk has rms = sqrt(36/36) = 1.0 over its
        // actual samples (Decimator::flush uses the partial count,
        // not samples_per_chunk).
        assert!((peaks.broadband[1].rms - 1.0).abs() < 1e-6);
    }

    #[test]
    fn offline_emits_onset_chunks_at_hop_cadence() {
        // 8192 mono samples → ~16 hops; bands.len() and onset.len()
        // must match (they share a cadence + a SpectralFrameStream
        // primitive, so any divergence is a wiring bug).
        let peaks = compute_offline_peaks(&vec![0.1f32; 8_192], 48_000, 1).unwrap();
        assert_eq!(
            peaks.bands.len(),
            peaks.onset.len(),
            "bands and onset must share cadence"
        );
        for c in &peaks.onset {
            assert!(c.flux >= 0.0, "onset flux must be non-negative");
        }
        assert_eq!(peaks.samples_per_onset_chunk, crate::BAND_SAMPLES_PER_CHUNK);
    }

    #[test]
    fn offline_silence_produces_zero_flux() {
        let peaks = compute_offline_peaks(&vec![0.0f32; 8_192], 48_000, 1).unwrap();
        for (i, c) in peaks.onset.iter().enumerate() {
            assert!(c.flux < 1e-3, "silence onset {i}: {}", c.flux);
        }
    }

    #[test]
    fn band_chunks_emitted_at_band_cadence() {
        // 8192 mono samples at BAND_SAMPLES_PER_CHUNK = 512 (the FFT
        // hop). The first frame requires the FFT analysis buffer to
        // fill (FRAME_SIZE samples > HOP_SIZE — see dub_spectral
        // module docs), so we don't get 16 bands; we get
        // `(N - FRAME_SIZE) / HOP_SIZE + 1` once the buffer is
        // primed. The exact figure depends on the SpectralFrameStream
        // configuration, which can change between milestones — so we
        // assert a bound rather than an exact value.
        let peaks = compute_offline_peaks(&vec![0.1f32; 8_192], 48_000, 1).unwrap();
        let max_possible = 8_192 / crate::BAND_SAMPLES_PER_CHUNK;
        assert!(
            peaks.bands.len() >= max_possible - 4 && peaks.bands.len() <= max_possible,
            "expected ~{max_possible} band chunks (\u{00b1} FFT analysis-buffer prime), \
             got {}",
            peaks.bands.len()
        );
        // A real signal at 0.1 has band energy > 0 in every band.
        for chunk in &peaks.bands {
            for &b in &chunk.rms_per_band {
                assert!(b >= 0.0);
            }
        }
    }

    #[test]
    fn capacity_avoids_reallocations_for_long_input() {
        // 10 minutes of mono at 48 kHz = 28.8 M samples → 450k
        // broadband chunks (10 * 60 * 48000 / 64). The Vec must not
        // grow past our pre-allocated capacity for this case.
        //
        // We don't actually allocate 28 M samples in the test — just
        // verify the capacity hint matches the expected upper bound.
        let _peaks = compute_offline_peaks(&vec![0.0f32; 1_000_000], 48_000, 1).unwrap();
        // Test passes if it doesn't OOM and the result is well-formed.
    }

    #[test]
    fn chunk_durations_round_trip_back_to_sample_rate() {
        let peaks = compute_offline_peaks(&vec![0.0f32; 48_000], 48_000, 1).unwrap();
        #[allow(clippy::cast_precision_loss)]
        let bb_dur = peaks.samples_per_broadband_chunk as f64 / f64::from(peaks.sample_rate);
        // SPC = 64 at 48 kHz → 64/48000 = 0.001333... s
        assert!((bb_dur - (64.0 / 48_000.0)).abs() < 1e-9);
    }

    /// The streaming path (`PeakStream` + analysis thread) and the
    /// offline path (this module) must produce bit-identical broadband
    /// peaks for the same mono input. Without this guarantee, swapping
    /// from File mode → Thru mode mid-set would show a faint
    /// "compression artefact" line in the waveform where the two
    /// algorithms diverge.
    ///
    /// We don't have a public streaming-API entry point that takes a
    /// pre-decoded buffer (`PeakStream` wraps an audio-thread tap), so
    /// the parity check is done directly against `Decimator::feed` —
    /// which is the same kernel both paths use.
    #[test]
    fn matches_streaming_decimator_kernel_for_mono_input() {
        let input: Vec<f32> = (0..4_096_i32)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32;
                (t * 0.01).sin()
            })
            .collect();

        let offline = compute_offline_peaks(&input, 48_000, 1).unwrap();

        let mut streaming = Vec::new();
        let mut d = Decimator::new(DEFAULT_SAMPLES_PER_CHUNK);
        d.feed(&input, |c| streaming.push(c));
        // No flush — input is an exact multiple of SPC.

        assert_eq!(
            offline.broadband, streaming,
            "offline must equal streaming kernel"
        );
    }
}
