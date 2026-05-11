//! End-to-end: synthesize a WAV on disk, load it through `dub-io`,
//! analyze it with `dub-bpm`, attach the result via [`Track::with_bpm`].
//!
//! This is the integration anchor for the load-time BPM pipeline
//! described in PRD §5.3. It exercises the file format path that a
//! library importer or interactive loader will eventually take, but
//! without committing to either of those user-facing surfaces yet.
//!
//! Living in `dub-bpm/tests/` keeps `dub-io` free of a `dub-bpm`
//! runtime dependency — these crates stay decoupled. The dev-dep
//! flows the other direction.

use std::path::PathBuf;

use dub_bpm::{analyze_bpm, synthetic};
use dub_io::Track;

fn write_click_track_wav(bpm: f64, duration_secs: f64, sample_rate: u32) -> PathBuf {
    let samples = synthetic::click_track(bpm, duration_secs, sample_rate);

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bpm_label = bpm as u32;
    let path = std::env::temp_dir().join(format!(
        "dub-bpm-pipeline-{}-{}.wav",
        bpm_label,
        std::process::id()
    ));
    let mut writer = hound::WavWriter::create(&path, spec).expect("create WAV");
    for &s in &samples {
        writer.write_sample(s).expect("write sample");
    }
    writer.finalize().expect("finalize WAV");
    path
}

#[test]
fn load_wav_then_analyze_then_attach_bpm() {
    let path = write_click_track_wav(120.0, 10.0, 48_000);

    let track = Track::load_from_path(&path).expect("load WAV");
    assert_eq!(track.sample_rate(), 48_000);
    assert_eq!(track.channels(), 1);
    assert!(track.bpm().is_none(), "freshly-loaded track has no BPM yet");

    let est = analyze_bpm(track.samples(), track.sample_rate(), track.channels())
        .expect("analysis should succeed on a 10s click track");
    assert!(
        (est.bpm - 120.0).abs() <= 1.0,
        "expected 120 BPM ± 1, got {} (confidence = {})",
        est.bpm,
        est.confidence
    );
    assert!(est.confidence > 0.0);

    let track = track.with_bpm(Some(est.bpm));
    assert_eq!(track.bpm(), Some(est.bpm));

    std::fs::remove_file(&path).ok();
}

#[test]
fn stereo_wav_loads_and_analyzes() {
    // Same click track, written stereo, loaded back, analyzed via the
    // interleaved-sample path. Exercises Track's stereo handling and
    // analyze_bpm's downmix together.
    let sr = 48_000u32;
    let mono = synthetic::click_track(140.0, 10.0, sr);

    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: sr,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let path = std::env::temp_dir().join(format!(
        "dub-bpm-pipeline-stereo-{}.wav",
        std::process::id()
    ));
    {
        let mut writer = hound::WavWriter::create(&path, spec).expect("create stereo WAV");
        for &s in &mono {
            writer.write_sample(s).expect("L");
            writer.write_sample(s).expect("R");
        }
        writer.finalize().expect("finalize");
    }

    let track = Track::load_from_path(&path).expect("load stereo WAV");
    assert_eq!(track.channels(), 2);
    let est = analyze_bpm(track.samples(), track.sample_rate(), track.channels())
        .expect("stereo analysis should succeed");
    assert!(
        (est.bpm - 140.0).abs() <= 1.0,
        "expected 140 BPM ± 1, got {} (confidence = {})",
        est.bpm,
        est.confidence
    );

    std::fs::remove_file(&path).ok();
}
