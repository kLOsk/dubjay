//! End-to-end peak capture: push a synthetic signal through a real
//! spawned `PeakStream` and assert the captured envelope matches
//! closed-form expectations. These tests pin the M9 acceptance:
//! "live waveform of a Thru-mode signal is captured as it plays".

use std::thread;
use std::time::{Duration, Instant};

use dub_peaks::{synthetic, PeakStream, PeakStreamConfig};
use ringbuf::traits::{Producer, Split};
use ringbuf::HeapRb;

fn ring(capacity: usize) -> (ringbuf::HeapProd<f32>, ringbuf::HeapCons<f32>) {
    HeapRb::<f32>::new(capacity).split()
}

fn push_all(tx: &mut ringbuf::HeapProd<f32>, mut samples: &[f32]) {
    while !samples.is_empty() {
        let n = tx.push_slice(samples);
        samples = &samples[n..];
        if !samples.is_empty() {
            thread::sleep(Duration::from_millis(2));
        }
    }
}

fn wait_until<F: FnMut() -> bool>(total_timeout: Duration, mut pred: F) -> bool {
    let start = Instant::now();
    while start.elapsed() < total_timeout {
        if pred() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    pred()
}

const SR: u32 = 48_000;

#[test]
fn constant_signal_yields_uniform_chunks() {
    let cfg = PeakStreamConfig {
        sample_rate: SR,
        samples_per_chunk: 64,
        buffer_capacity_secs: 1,
        bands_enabled: false,
    };
    let (mut tx, rx) = ring(4096);
    let stream = PeakStream::spawn(rx, cfg).expect("spawn");

    let signal = synthetic::constant(0.7, 64 * 32);
    push_all(&mut tx, &signal);

    let ok = wait_until(Duration::from_secs(2), || stream.len() >= 32);
    assert!(ok, "expected ≥32 chunks, got {}", stream.len());

    let snap = stream.buffer().snapshot();
    for (i, c) in snap.chunks[..32].iter().enumerate() {
        assert!((c.min - 0.7).abs() < 1e-6, "chunk {i} min = {}", c.min);
        assert!((c.max - 0.7).abs() < 1e-6, "chunk {i} max = {}", c.max);
        assert!((c.rms - 0.7).abs() < 1e-6, "chunk {i} rms = {}", c.rms);
    }
}

#[test]
fn bursts_have_silent_chunks_between() {
    // 64-sample bursts of 0.5 with 64-sample silence between. At
    // spc=64 the chunks should alternate (0.5,0.5,0.5) and (0,0,0).
    let cfg = PeakStreamConfig {
        sample_rate: SR,
        samples_per_chunk: 64,
        buffer_capacity_secs: 1,
        bands_enabled: false,
    };
    let (mut tx, rx) = ring(4096);
    let stream = PeakStream::spawn(rx, cfg).expect("spawn");

    let signal = synthetic::bursts(0.5, 64, 64, 8);
    push_all(&mut tx, &signal);

    let ok = wait_until(Duration::from_secs(2), || stream.len() >= 16);
    assert!(ok, "expected ≥16 chunks, got {}", stream.len());

    let snap = stream.buffer().snapshot();
    for i in 0..16 {
        let c = snap.chunks[i];
        if i % 2 == 0 {
            assert!(
                (c.rms - 0.5).abs() < 1e-6,
                "burst chunk {i} rms = {}",
                c.rms
            );
            assert!(
                (c.max - 0.5).abs() < 1e-6,
                "burst chunk {i} max = {}",
                c.max
            );
        } else {
            assert!(c.rms.abs() < 1e-9, "silent chunk {i} rms = {}", c.rms);
            assert!(c.max.abs() < 1e-9, "silent chunk {i} max = {}", c.max);
            assert!(c.min.abs() < 1e-9, "silent chunk {i} min = {}", c.min);
        }
    }
}

#[test]
fn incremental_extend_mirrors_full_stream() {
    // 100 chunks worth of saw-ramp; renderer-side extend_chunks
    // every 5 ms should reconstruct the same sequence as a final
    // snapshot.
    let cfg = PeakStreamConfig {
        sample_rate: SR,
        samples_per_chunk: 64,
        buffer_capacity_secs: 1,
        bands_enabled: false,
    };
    let (mut tx, rx) = ring(8192);
    let stream = PeakStream::spawn(rx, cfg).expect("spawn");

    let signal = synthetic::saw_ramp(1.0, 64 * 100);

    // Producer thread feeds the ring; main thread polls extend.
    let producer = thread::spawn(move || {
        push_all(&mut tx, &signal);
    });

    let mut mirror: Vec<dub_peaks::PeakChunk> = Vec::new();
    let mut start = 0usize;
    let ok = wait_until(Duration::from_secs(5), || {
        start = stream.buffer().extend_chunks(start, &mut mirror);
        mirror.len() >= 100
    });
    producer.join().expect("producer join");
    // One last drain to catch any tail.
    let _ = stream.buffer().extend_chunks(start, &mut mirror);

    assert!(ok, "incremental mirror only reached {}", mirror.len());

    let snap = stream.buffer().snapshot();
    assert_eq!(mirror.len(), snap.len());
    for (i, (m, s)) in mirror.iter().zip(snap.chunks.iter()).enumerate() {
        assert_eq!(m, s, "mirror diverges from snapshot at idx {i}");
    }

    // The ramp's max should be monotonically non-decreasing across
    // chunks (it's a strictly increasing input).
    for (i, w) in mirror.windows(2).enumerate() {
        assert!(
            w[1].max >= w[0].max,
            "saw ramp chunk {} → {}: max went {} → {}",
            i,
            i + 1,
            w[0].max,
            w[1].max
        );
    }
}
