//! In-memory track buffer.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Errors that can occur while loading a track.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// The file could not be opened.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// The file is in a format symphonia could not probe.
    #[error("unsupported or corrupt format: {0}")]
    Format(String),

    /// The file contained no audio tracks.
    #[error("file contains no audio tracks")]
    NoAudioTrack,

    /// Decoding produced an unexpected channel layout (zero or > 2 channels).
    #[error("unsupported channel layout: {0} channels")]
    UnsupportedChannels(u8),

    /// Decoding produced no usable samples.
    #[error("decode produced no samples")]
    Empty,
}

impl From<SymphoniaError> for LoadError {
    fn from(e: SymphoniaError) -> Self {
        Self::Format(e.to_string())
    }
}

/// An in-memory audio track.
///
/// Audio is stored interleaved (`L, R, L, R, …` for stereo, `M, M, …` for mono),
/// 32-bit float, in the file's original sample rate. Resampling to engine SR
/// happens at the engine boundary if needed (PRD §4.4).
///
/// Tracks are immutable after construction; the engine accesses them via
/// `Arc<Track>` so multiple decks can hold the same track without copies.
///
/// ## Metadata
///
/// Tempo lives here as an optional field, filled in by callers that have
/// run BPM analysis. `dub-io` deliberately does *not* depend on
/// `dub-bpm` — that would force every audio-loading site to pay the
/// analysis cost up-front. Instead, a typical pipeline is:
///
/// ```ignore
/// let track = Track::load_from_path(p)?;
/// let est = dub_bpm::analyze_bpm(track.samples(), track.sample_rate(), track.channels())?;
/// let track = track.with_bpm(Some(est.bpm));
/// ```
///
/// See PRD §5.3 (M7.5 / library import).
#[derive(Debug, Clone)]
pub struct Track {
    samples: Arc<[f32]>,
    sample_rate: u32,
    channels: u8,
    frames: usize,
    bpm: Option<f64>,
}

impl Track {
    /// Construct a `Track` directly from interleaved samples. Useful for tests.
    ///
    /// `samples.len()` must equal `frames * channels`. Channels must be 1 or 2.
    /// Returns `None` if the constraints are violated.
    #[must_use]
    pub fn from_interleaved(samples: Vec<f32>, sample_rate: u32, channels: u8) -> Option<Self> {
        if !(1..=2).contains(&channels) || sample_rate == 0 {
            return None;
        }
        let n = samples.len();
        if n == 0 || !n.is_multiple_of(usize::from(channels)) {
            return None;
        }
        let frames = n / usize::from(channels);
        Some(Self {
            samples: Arc::from(samples.into_boxed_slice()),
            sample_rate,
            channels,
            frames,
            bpm: None,
        })
    }

    /// Load a track from a path. Decodes the entire file into RAM.
    ///
    /// Format detection uses the file extension as a hint plus symphonia's
    /// content sniffer. WAV/PCM is the only format guaranteed in M1; other
    /// formats are added per-milestone via symphonia features.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError`] if the file is missing, in an unsupported
    /// format, contains no audio tracks, has more than 2 channels, or
    /// produces no decodable samples.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let path = path.as_ref();

        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            hint.with_extension(ext);
        }

        let probed = symphonia::default::get_probe().format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )?;

        let mut format = probed.format;
        let primary = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(LoadError::NoAudioTrack)?;
        let track_id = primary.id;

        let codec_params = primary.codec_params.clone();
        let mut decoder =
            symphonia::default::get_codecs().make(&codec_params, &DecoderOptions::default())?;

        // Sample rate / channels MAY come from codec params (typical for
        // RIFF formats) or only become known after the first packet is
        // decoded (typical for ISO MP4 / AAC where the channel count
        // lives in the audio object type, not the sample entry box). We
        // try params first, then fall back to the first decoded buffer's
        // spec inside the loop below.
        let mut sample_rate = codec_params.sample_rate;
        let mut channels: Option<u8> = codec_params
            .channels
            .map(|c| u8::try_from(c.count()).unwrap_or(u8::MAX));

        let mut samples: Vec<f32> = Vec::new();
        let mut sample_buf: Option<SampleBuffer<f32>> = None;

        loop {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(SymphoniaError::ResetRequired) => {
                    return Err(LoadError::Format("decoder reset required".into()));
                }
                Err(e) => return Err(e.into()),
            };
            if packet.track_id() != track_id {
                continue;
            }

            let audio = match decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(e) => return Err(e.into()),
            };

            // First decoded packet: lock in any spec fields we couldn't
            // get from codec_params. After this point, spec changes mean
            // a stream-format-change which we don't support yet.
            let spec = *audio.spec();
            if sample_rate.is_none() {
                sample_rate = Some(spec.rate);
            }
            if channels.is_none() {
                let count = spec.channels.count();
                channels = Some(u8::try_from(count).unwrap_or(u8::MAX));
            }

            // Capacity grows lazily here too — `audio.capacity()` is the
            // packet's max frame count, fixed for the codec.
            let buf = sample_buf
                .get_or_insert_with(|| SampleBuffer::<f32>::new(audio.capacity() as u64, spec));
            buf.copy_interleaved_ref(audio);
            samples.extend_from_slice(buf.samples());
        }

        let sample_rate =
            sample_rate.ok_or_else(|| LoadError::Format("no sample rate found".into()))?;
        let channels =
            channels.ok_or_else(|| LoadError::Format("no channel layout found".into()))?;
        if !(1..=2).contains(&channels) {
            return Err(LoadError::UnsupportedChannels(channels));
        }

        if samples.is_empty() {
            return Err(LoadError::Empty);
        }

        let frames = samples.len() / usize::from(channels);
        Ok(Self {
            samples: Arc::from(samples.into_boxed_slice()),
            sample_rate,
            channels,
            frames,
            bpm: None,
        })
    }

    /// Return a copy of this track with its BPM annotation updated.
    ///
    /// Cloning is cheap — the underlying `Arc<[f32]>` sample buffer is
    /// shared. This is a builder-style helper because `Track` is
    /// otherwise immutable after construction, which keeps the engine
    /// thread free of mutation hazards.
    #[must_use]
    pub fn with_bpm(self, bpm: Option<f64>) -> Self {
        Self { bpm, ..self }
    }

    /// Tempo annotation, or `None` if BPM analysis has not been run on
    /// this track yet.
    #[must_use]
    pub fn bpm(&self) -> Option<f64> {
        self.bpm
    }

    /// Number of audio frames (one frame = one sample per channel).
    #[must_use]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Sample rate the track was decoded at.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// 1 (mono) or 2 (stereo).
    #[must_use]
    pub fn channels(&self) -> u8 {
        self.channels
    }

    /// Length of the track in seconds (independent of any engine sample rate).
    #[must_use]
    pub fn duration_seconds(&self) -> f64 {
        // Practical limit: a single track cannot exceed 2^52 frames (~3
        // million years at 48kHz), which is well within f64 mantissa range.
        #[allow(clippy::cast_precision_loss)]
        let frames_f = self.frames as f64;
        frames_f / f64::from(self.sample_rate)
    }

    /// Borrow the raw interleaved sample buffer.
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Read one stereo frame at the given integer position.
    ///
    /// Mono tracks are duplicated to both channels. Out-of-range positions
    /// return silence.
    #[must_use]
    pub fn frame(&self, frame_index: usize) -> [f32; 2] {
        if frame_index >= self.frames {
            return [0.0, 0.0];
        }
        match self.channels {
            1 => {
                let s = self.samples[frame_index];
                [s, s]
            }
            2 => {
                let i = frame_index * 2;
                [self.samples[i], self.samples[i + 1]]
            }
            // The constructor guarantees 1..=2.
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn from_interleaved_constructs_stereo() {
        let track = Track::from_interleaved(vec![0.1, 0.2, 0.3, 0.4], 48_000, 2).unwrap();
        assert_eq!(track.frames(), 2);
        assert_eq!(track.channels(), 2);
        assert_eq!(track.sample_rate(), 48_000);
        assert!((track.duration_seconds() - (2.0 / 48_000.0)).abs() < 1e-9);
    }

    #[test]
    fn from_interleaved_constructs_mono() {
        let track = Track::from_interleaved(vec![0.1, 0.2, 0.3], 44_100, 1).unwrap();
        assert_eq!(track.frames(), 3);
        assert_eq!(track.channels(), 1);
    }

    #[test]
    fn from_interleaved_rejects_bad_layouts() {
        // Mismatched length for stereo
        assert!(Track::from_interleaved(vec![0.1, 0.2, 0.3], 48_000, 2).is_none());
        // Empty
        assert!(Track::from_interleaved(vec![], 48_000, 2).is_none());
        // Too many channels
        assert!(Track::from_interleaved(vec![0.0; 6], 48_000, 6).is_none());
        // Zero sample rate
        assert!(Track::from_interleaved(vec![0.1, 0.2], 0, 2).is_none());
    }

    #[test]
    fn frame_at_returns_silence_past_end() {
        let track = Track::from_interleaved(vec![0.5, -0.5], 48_000, 2).unwrap();
        // Exact f32 equality is correct: these are literal stored samples.
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(track.frame(0), [0.5, -0.5]);
            assert_eq!(track.frame(1), [0.0, 0.0]);
            assert_eq!(track.frame(usize::MAX), [0.0, 0.0]);
        }
    }

    #[test]
    fn mono_frame_duplicates_to_stereo() {
        let track = Track::from_interleaved(vec![0.7, -0.3], 48_000, 1).unwrap();
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(track.frame(0), [0.7, 0.7]);
            assert_eq!(track.frame(1), [-0.3, -0.3]);
        }
    }

    #[test]
    fn bpm_defaults_to_none() {
        let track = Track::from_interleaved(vec![0.1, 0.2], 48_000, 2).unwrap();
        assert!(track.bpm().is_none());
    }

    #[test]
    fn with_bpm_attaches_and_overrides() {
        let track = Track::from_interleaved(vec![0.1, 0.2], 48_000, 2).unwrap();
        let with = track.clone().with_bpm(Some(128.0));
        assert_eq!(with.bpm(), Some(128.0));
        // Original is unchanged (builder-style).
        assert!(track.bpm().is_none());

        // Override clears.
        let cleared = with.with_bpm(None);
        assert!(cleared.bpm().is_none());
    }

    proptest! {
        #[test]
        fn frame_never_panics(
            samples in proptest::collection::vec(-1.0f32..=1.0, 0..1024),
            channels in 1u8..=2,
            sample_rate in 8_000u32..=192_000,
            idx in 0usize..1_000_000,
        ) {
            // Trim samples to a multiple of `channels`.
            let n = samples.len();
            let trimmed = n - (n % usize::from(channels));
            let mut samples = samples;
            samples.truncate(trimmed);

            if let Some(track) = Track::from_interleaved(samples, sample_rate, channels) {
                let _ = track.frame(idx);
            }
        }
    }

    #[test]
    fn loads_a_real_wav_file() {
        // Generate a 0.1 s mono i16 WAV with hound, load it back, check
        // round-trip *and* that samples are properly normalized to f32 in
        // [-1.0, 1.0]. This guards against the (real) bug where a naive
        // cast from i16 → f32 would yield values up to 32767.
        let path = std::env::temp_dir().join("dub-io-test-sine.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        {
            let mut writer = hound::WavWriter::create(&path, spec).unwrap();
            for i in 0..4_800i32 {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32 / 48_000.0;
                let s = 0.5 * (t * 440.0 * std::f32::consts::TAU).sin();
                #[allow(clippy::cast_possible_truncation)]
                let q = (s * f32::from(i16::MAX)) as i16;
                writer.write_sample(q).unwrap();
            }
            writer.finalize().unwrap();
        }

        let track = Track::load_from_path(&path).expect("load WAV");
        assert_eq!(track.sample_rate(), 48_000);
        assert_eq!(track.channels(), 1);
        assert_eq!(track.frames(), 4_800);

        let peak = track
            .samples()
            .iter()
            .copied()
            .map(f32::abs)
            .fold(0.0f32, f32::max);
        assert!(
            peak < 1.0,
            "peak should be < 1.0 (was {peak}); decoder likely failed to normalize"
        );
        assert!(
            peak > 0.4 && peak < 0.55,
            "peak should be ~0.5 (was {peak})"
        );

        std::fs::remove_file(&path).ok();
    }
}
