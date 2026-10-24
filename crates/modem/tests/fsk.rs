use std::path::PathBuf;

use bombechat_modem::{
    measured_rate, modulate, split, Band, DemodEvent, Demodulator, Event, ModemConfig, Reassembler,
};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn rng(seed: u64) -> StdRng {
    StdRng::seed_from_u64(seed)
}

/// One standard normal sample (Box-Muller).
fn gauss(r: &mut StdRng) -> f32 {
    let u1: f64 = r.gen_range(f64::MIN_POSITIVE..1.0);
    let u2: f64 = r.gen();
    ((-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()) as f32
}

/// A full-size frame (48 data bytes, 69 bytes on air) from a seeded envelope.
fn sample_frame() -> Vec<u8> {
    let mut r = rng(1);
    let env: Vec<u8> = (0..100).map(|_| r.gen()).collect();
    split(&env, 0x5a5a).remove(0)
}

fn frames_of(events: &[DemodEvent]) -> Vec<Vec<u8>> {
    events
        .iter()
        .filter_map(|e| match e {
            DemodEvent::Frame(f) => Some(f.clone()),
            DemodEvent::Level { .. } => None,
        })
        .collect()
}

fn decode(cfg: ModemConfig, samples: &[f32]) -> Vec<Vec<u8>> {
    let mut d = Demodulator::new(cfg);
    frames_of(&d.push(samples))
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt() as f32
}

fn white(r: &mut StdRng, n: usize, rms: f32) -> Vec<f32> {
    (0..n).map(|_| gauss(r) * rms).collect()
}

/// Resample by `ratio` with linear interpolation: output sample `i` is the
/// input at position `i * ratio`. A ratio above 1 plays the signal back
/// faster (every frequency scaled up by `ratio`), as a receiver whose sound
/// card clock runs slow would hear it.
fn resample(x: &[f32], ratio: f64) -> Vec<f32> {
    let mut out = Vec::new();
    let mut i = 0usize;
    loop {
        let pos = i as f64 * ratio;
        let k = pos.floor() as usize;
        if k + 1 >= x.len() {
            break;
        }
        let frac = (pos - k as f64) as f32;
        out.push(x[k] * (1.0 - frac) + x[k + 1] * frac);
        i += 1;
    }
    out
}

/// Pink noise (Paul Kellet's filter on white noise), low-passed at 8 kHz
/// with a 255-tap Blackman windowed-sinc filter, scaled to `target_rms`.
/// Stands in for a room full of speech: lots of energy, none of it near
/// the near-silent band.
fn speech_band_noise(r: &mut StdRng, n: usize, fs: f32, target_rms: f32) -> Vec<f32> {
    let (mut b0, mut b1, mut b2, mut b3, mut b4, mut b5, mut b6) =
        (0f32, 0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
    let pink: Vec<f32> = (0..n)
        .map(|_| {
            let w = gauss(r);
            b0 = 0.99886 * b0 + w * 0.0555179;
            b1 = 0.99332 * b1 + w * 0.0750759;
            b2 = 0.96900 * b2 + w * 0.153852;
            b3 = 0.86650 * b3 + w * 0.3104856;
            b4 = 0.55000 * b4 + w * 0.5329522;
            b5 = -0.7616 * b5 - w * 0.0168980;
            let out = b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362;
            b6 = w * 0.115926;
            out
        })
        .collect();

    let taps = 255usize;
    let fc = 8000.0 / fs;
    let mid = (taps - 1) as f32 / 2.0;
    let h: Vec<f32> = (0..taps)
        .map(|i| {
            let t = i as f32 - mid;
            let sinc = if t == 0.0 {
                2.0 * fc
            } else {
                (std::f32::consts::TAU * fc * t).sin() / (std::f32::consts::PI * t)
            };
            let a = std::f32::consts::TAU * i as f32 / (taps - 1) as f32;
            let blackman = 0.42 - 0.5 * a.cos() + 0.08 * (2.0 * a).cos();
            sinc * blackman
        })
        .collect();
    let mut low: Vec<f32> = (0..n)
        .map(|i| {
            let mut acc = 0f32;
            for (j, hj) in h.iter().enumerate() {
                if i >= j {
                    acc += hj * pink[i - j];
                }
            }
            acc
        })
        .collect();
    let scale = target_rms / rms(&low);
    low.iter_mut().for_each(|v| *v *= scale);
    low
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Read fixture `name` if it is checked in; otherwise generate it with
/// `gen`, write it as 16-bit mono PCM and read it back, so a freshly
/// generated fixture and a checked-in one go through the same path.
fn fixture(name: &str, sample_rate: u32, gen: impl FnOnce() -> Vec<f32>) -> Vec<f32> {
    let path = fixture_path(name);
    if !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        // Write under a unique name and rename into place, so tests running
        // in parallel never read a half-written fixture.
        let tmp = path.with_extension(format!("{:?}.tmp", std::thread::current().id()));
        let mut w = hound::WavWriter::create(&tmp, spec).unwrap();
        for s in gen() {
            w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
                .unwrap();
        }
        w.finalize().unwrap();
        std::fs::rename(&tmp, &path).unwrap();
    }
    let mut r = hound::WavReader::open(&path).unwrap();
    assert_eq!(r.spec().sample_rate, sample_rate, "fixture {name}");
    r.samples::<i16>()
        .map(|s| s.unwrap() as f32 / i16::MAX as f32)
        .collect()
}

/// The signal for `frame` with `lead` samples of quiet noise before it and
/// 100 ms of the same after it.
fn padded(cfg: &ModemConfig, frame: &[u8], lead: usize, seed: u64) -> Vec<f32> {
    let mut r = rng(seed);
    let quiet = 0.001; // -60 dBFS
    let mut x = white(&mut r, lead, quiet);
    x.extend(modulate(cfg, frame));
    x.extend(white(&mut r, cfg.sample_rate as usize / 10, quiet));
    x
}

/// `padded` plus white noise 10 dB below the RMS of the modem signal.
fn noisy(cfg: &ModemConfig, frame: &[u8], lead: usize, seed: u64) -> Vec<f32> {
    let signal_rms = rms(&modulate(cfg, frame));
    let noise_rms = signal_rms * 10f32.powf(-10.0 / 20.0);
    let mut x = padded(cfg, frame, lead, seed);
    let mut r = rng(seed ^ 0xdead_beef);
    for v in x.iter_mut() {
        *v += gauss(&mut r) * noise_rms;
    }
    x
}

fn noisy_fixture() -> Vec<f32> {
    let cfg = ModemConfig::default();
    fixture("noisy.wav", cfg.sample_rate, || {
        noisy(&cfg, &sample_frame(), 7_919, 11)
    })
}

// ---------------------------------------------------------------------------
// Tests from the brief
// ---------------------------------------------------------------------------

#[test]
fn clean_loopback() {
    let f = sample_frame();
    let mut d = Demodulator::new(Default::default());
    let ev = d.push(&modulate(&Default::default(), &f));
    assert!(ev.contains(&DemodEvent::Frame(f.clone())));

    // The same signal recorded to a checked-in WAV decodes the same way.
    let cfg = ModemConfig::default();
    let x = fixture("clean.wav", cfg.sample_rate, || padded(&cfg, &f, 3_000, 5));
    assert_eq!(decode(cfg, &x), vec![f]);
}

#[test]
fn survives_10db_noise() {
    let f = sample_frame();
    assert_eq!(
        decode(ModemConfig::default(), &noisy_fixture()),
        vec![f.clone()]
    );

    for seed in 20..24 {
        let x = noisy(
            &ModemConfig::default(),
            &f,
            1_000 + seed as usize * 997,
            seed,
        );
        assert_eq!(
            decode(ModemConfig::default(), &x),
            vec![f.clone()],
            "seed {seed}"
        );
    }
}

#[test]
fn survives_half_percent_clock_drift() {
    let f = sample_frame();
    let cfg = ModemConfig::default();
    let x = fixture("drifted.wav", cfg.sample_rate, || {
        resample(&padded(&cfg, &f, 4_321, 7), 1.005)
    });
    assert_eq!(decode(cfg, &x), vec![f.clone()], "fixture, ratio 1.005");

    // Both directions, with and without noise on top.
    for (i, ratio) in [1.005, 1.0 / 1.005, 0.995, 1.0 / 0.995]
        .into_iter()
        .enumerate()
    {
        let x = resample(&padded(&cfg, &f, 2_000 + i * 1_000, 30 + i as u64), ratio);
        assert_eq!(decode(cfg, &x), vec![f.clone()], "ratio {ratio}");
        let x = resample(&noisy(&cfg, &f, 2_500 + i * 1_000, 40 + i as u64), ratio);
        assert_eq!(decode(cfg, &x), vec![f.clone()], "noisy, ratio {ratio}");
    }
}

#[test]
fn arbitrary_start_offset() {
    let f = sample_frame();
    let cfg = ModemConfig::default();
    let mut r = rng(99);
    let mut offsets = vec![0usize, 1, 47_999];
    offsets.extend((0..6).map(|_| r.gen_range(0..48_000)));
    for lead in offsets {
        let x = padded(&cfg, &f, lead, lead as u64);
        assert_eq!(decode(cfg, &x), vec![f.clone()], "lead {lead}");
    }
}

#[test]
fn silence_and_speech_band_noise_give_no_frames() {
    let cfg = ModemConfig::default();
    let fs = cfg.sample_rate as usize;
    let mut d = Demodulator::new(cfg);

    let silence = vec![0f32; fs];
    let ev = d.push(&silence);
    assert!(frames_of(&ev).is_empty());

    let noise = speech_band_noise(&mut rng(3), 10 * fs, fs as f32, 0.2);
    let ev = d.push(&noise);
    assert!(
        frames_of(&ev).is_empty(),
        "false frames in speech-band noise"
    );

    // The audible band shares the room with speech; it must not invent
    // frames either.
    let mut d = Demodulator::new(ModemConfig {
        band: Band::Audible,
        ..cfg
    });
    assert!(
        frames_of(&d.push(&noise)).is_empty(),
        "false frames, audible band"
    );
}

#[test]
fn audible_band_loopback() {
    let f = sample_frame();
    let cfg = ModemConfig {
        band: Band::Audible,
        ..Default::default()
    };
    assert_eq!(decode(cfg, &padded(&cfg, &f, 1_234, 8)), vec![f.clone()]);
    assert_eq!(decode(cfg, &noisy(&cfg, &f, 2_345, 9)), vec![f]);
}

#[test]
fn near_silent_energy_stays_in_band() {
    let cfg = ModemConfig::default();
    let x = modulate(&cfg, &sample_frame());
    let n = x.len().next_power_of_two();
    let mut buf: Vec<Complex<f32>> = x.iter().map(|&v| Complex::new(v, 0.0)).collect();
    buf.resize(n, Complex::new(0.0, 0.0));
    FftPlanner::new().plan_fft_forward(n).process(&mut buf);

    let hz_per_bin = cfg.sample_rate as f64 / n as f64;
    let (mut total, mut inside) = (0f64, 0f64);
    for (i, c) in buf.iter().take(n / 2 + 1).enumerate() {
        let e = c.norm_sqr() as f64;
        total += e;
        let hz = i as f64 * hz_per_bin;
        if (17_400.0..=20_000.0).contains(&hz) {
            inside += e;
        }
    }
    let share = inside / total;
    assert!(share >= 0.99, "only {:.4} of the energy is in band", share);
}

// ---------------------------------------------------------------------------
// Further behaviour
// ---------------------------------------------------------------------------

#[test]
fn chunk_size_does_not_change_the_events() {
    let x = noisy_fixture();
    let whole = Demodulator::new(Default::default()).push(&x);
    assert_eq!(frames_of(&whole), vec![sample_frame()]);
    for chunk in [1usize, 64, 4096] {
        let mut d = Demodulator::new(Default::default());
        let mut ev = Vec::new();
        for c in x.chunks(chunk) {
            ev.extend(d.push(c));
        }
        assert_eq!(ev, whole, "chunk size {chunk}");
    }
}

#[test]
fn level_every_100_ms() {
    let cfg = ModemConfig::default();
    let fs = cfg.sample_rate as f32;
    let tone: Vec<f32> = (0..cfg.sample_rate as usize)
        .map(|i| 0.5 * (std::f32::consts::TAU * 1_000.0 * i as f32 / fs).sin())
        .collect();
    let ev = Demodulator::new(cfg).push(&tone);
    let levels: Vec<f32> = ev
        .iter()
        .filter_map(|e| match e {
            DemodEvent::Level { dbfs } => Some(*dbfs),
            DemodEvent::Frame(_) => None,
        })
        .collect();
    assert_eq!(levels.len(), 10);
    // A 0.5 amplitude sine has RMS 0.3536, which is -9.03 dBFS.
    for l in levels {
        assert!((l + 9.03).abs() < 0.05, "level {l}");
    }

    let ev = Demodulator::new(cfg).push(&vec![0f32; 4_800]);
    match ev.as_slice() {
        [DemodEvent::Level { dbfs }] => assert!(*dbfs <= -100.0, "silence at {dbfs}"),
        other => panic!("expected one Level, got {other:?}"),
    }
}

#[test]
fn back_to_back_frames_reassemble() {
    let cfg = ModemConfig::default();
    let mut r = rng(17);
    let env: Vec<u8> = (0..200).map(|_| r.gen()).collect();
    let frames = split(&env, 0x0102);
    assert_eq!(frames.len(), 5);
    assert!(frames.last().unwrap().len() < frames[0].len());

    let mut x = white(&mut r, 2_000, 0.001);
    for f in &frames {
        x.extend(modulate(&cfg, f));
    }
    x.extend(white(&mut r, 4_800, 0.001));

    let heard = decode(cfg, &x);
    assert_eq!(heard, frames);
    for ratio in [1.005, 0.995] {
        assert_eq!(decode(cfg, &resample(&x, ratio)), frames, "ratio {ratio}");
    }
    let mut re = Reassembler::new();
    let complete: Vec<Vec<u8>> = heard
        .iter()
        .flat_map(|f| re.push(f, 0))
        .filter_map(|e| match e {
            Event::Complete { bytes, .. } => Some(bytes),
            _ => None,
        })
        .collect();
    assert_eq!(complete, vec![env]);
}

#[test]
fn joining_mid_frame_waits_for_the_next_preamble() {
    // A receiver that starts listening after a preamble hears only data
    // tones. They must not pass for a preamble, and the next real frame
    // must still be caught.
    let cfg = ModemConfig::default();
    let f = sample_frame();
    let chirp_len = cfg.sample_rate as usize / 10;
    let mut x = Vec::new();
    for tag in 0..4u16 {
        let other = split(&[tag as u8; 48], tag).remove(0);
        x.extend_from_slice(&modulate(&cfg, &other)[chirp_len..]);
    }
    x.extend(padded(&cfg, &f, 0, 4));
    assert_eq!(decode(cfg, &x), vec![f]);
}

#[test]
fn shortest_frame_decodes() {
    let cfg = ModemConfig::default();
    let f = split(&[], 7).remove(0);
    assert_eq!(f.len(), 21);
    assert_eq!(decode(cfg, &padded(&cfg, &f, 500, 2)), vec![f]);
}

#[test]
fn measured_rate_matches_airtime() {
    let cfg = ModemConfig::default();
    let full = sample_frame();
    assert_eq!(full.len(), 69);
    let seconds = modulate(&cfg, &full).len() as f32 / cfg.sample_rate as f32;
    let expected = 48.0 / seconds;
    let rate = measured_rate(&cfg);
    assert!((rate - expected).abs() < 1e-3, "{rate} vs {expected}");
    // 100 ms chirp + (3 + 69) bytes * 2 symbols * 20 ms = 2.98 s per 48 bytes.
    assert!((rate - 48.0 / 2.98).abs() < 1e-3, "{rate}");
}
