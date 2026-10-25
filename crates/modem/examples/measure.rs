//! Measure the sound modem on real hardware.
//!
//! Plays a run of full-size frames through the default speaker, records
//! the default microphone while they play, demodulates the recording and
//! reports how many frames came back, the signal-to-noise ratio in the
//! band, and the rate the interface shows ([`measured_rate`]).
//!
//! ```sh
//! # one laptop playing into its own microphone, or two laptops (run the
//! # same command on the sender; record on the receiver with --save)
//! cargo run --release -p bombechat-modem --example measure
//! cargo run --release -p bombechat-modem --example measure -- --band audible --gain 0.3
//! # decode a recording made elsewhere (any WAV, mono or stereo)
//! cargo run --release -p bombechat-modem --example measure -- --wav heard.wav --frames 20
//! ```
//!
//! Flags:
//!
//! - `--band near|audible`  17.6 to 19.8 kHz (the default) or 2.0 to 4.2 kHz
//! - `--symbol-ms MS`       symbol length, default from `ModemConfig::default()`
//! - `--tones N`            2, 4 or 16, default from `ModemConfig::default()`
//! - `--frames N`           frames to send (and expect), default 20
//! - `--gap-ms MS`          silence between frames, default 250
//! - `--gain G`             scale the samples before playing, default 1.0
//! - `--save FILE`          also write what the microphone heard to a WAV
//! - `--wav FILE`           decode FILE instead of playing and recording
//!
//! The frames are the same every run (a fixed seed), so a recording made
//! on another machine can be scored with `--wav` and the same `--frames`.

use std::collections::BTreeSet;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use bombechat_modem::{
    measured_rate, modulate, split, AudioIo, Band, CpalAudio, DemodEvent, Demodulator, Event,
    ModemConfig, Reassembler, FRAME_DATA,
};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

/// Recording before the first frame plays, to measure the room's noise.
const LEAD_IN_MS: u64 = 600;
/// Recording after the last frame should have finished.
const TAIL_MS: u64 = 800;
/// Tag the frames are sent under.
const TAG: u16 = 0x6d65;

struct Args {
    band: Band,
    symbol_ms: Option<f32>,
    tones: Option<u8>,
    frames: usize,
    gap_ms: u32,
    gain: f32,
    save: Option<String>,
    wav: Option<String>,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        band: Band::NearSilent,
        symbol_ms: None,
        tones: None,
        frames: 20,
        gap_ms: 250,
        gain: 1.0,
        save: None,
        wav: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        let bad = |v: &str| format!("{flag}: cannot read {v:?}");
        match flag.as_str() {
            "--band" => {
                a.band = match value()?.as_str() {
                    "near" | "near-silent" | "nearsilent" => Band::NearSilent,
                    "audible" => Band::Audible,
                    v => return Err(bad(v)),
                }
            }
            "--symbol-ms" => {
                let v = value()?;
                a.symbol_ms = Some(v.parse().map_err(|_| bad(&v))?);
            }
            "--tones" => {
                let v = value()?;
                let t: u8 = v.parse().map_err(|_| bad(&v))?;
                if ![2, 4, 16].contains(&t) {
                    return Err("--tones must be 2, 4 or 16".to_string());
                }
                a.tones = Some(t);
            }
            "--frames" => {
                let v = value()?;
                a.frames = v.parse().map_err(|_| bad(&v))?;
                if a.frames == 0 || a.frames > 255 {
                    return Err("--frames must be 1 to 255".to_string());
                }
            }
            "--gap-ms" => {
                let v = value()?;
                a.gap_ms = v.parse().map_err(|_| bad(&v))?;
            }
            "--gain" => {
                let v = value()?;
                a.gain = v.parse().map_err(|_| bad(&v))?;
            }
            "--save" => a.save = Some(value()?),
            "--wav" => a.wav = Some(value()?),
            "-h" | "--help" => {
                return Err("see the comment at the top of crates/modem/examples/measure.rs".into())
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(a)
}

fn config(a: &Args, sample_rate: u32) -> ModemConfig {
    let d = ModemConfig::default();
    ModemConfig {
        sample_rate,
        band: a.band,
        tones: a.tones.unwrap_or(d.tones),
        symbol_ms: a.symbol_ms.unwrap_or(d.symbol_ms),
    }
}

/// `n` full-size frames from a fixed-seed envelope.
fn test_frames(n: usize) -> Vec<Vec<u8>> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let envelope: Vec<u8> = (0..n * FRAME_DATA)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    split(&envelope, TAG)
}

/// Every frame with `gap` samples of silence after it.
fn programme(cfg: &ModemConfig, frames: &[Vec<u8>], gap: usize, gain: f32) -> Vec<f32> {
    let mut out = Vec::new();
    for f in frames {
        out.extend(modulate(cfg, f).into_iter().map(|s| s * gain));
        out.extend(std::iter::repeat_n(0.0, gap));
    }
    out
}

/// Play `samples` and record the microphone from `LEAD_IN_MS` before to
/// `TAIL_MS` after. Returns the recording and the sample rate.
fn play_and_record(mut audio: CpalAudio, samples: Vec<f32>) -> Result<Vec<f32>, String> {
    let rate = audio.sample_rate();
    let heard = Arc::new(Mutex::new(Vec::<f32>::new()));
    let sink = heard.clone();
    audio
        .start_input(Box::new(move |s: &[f32]| {
            sink.lock()
                .unwrap_or_else(|p| p.into_inner())
                .extend_from_slice(s)
        }))
        .map_err(|e| format!("could not open the microphone: {e}"))?;
    thread::sleep(Duration::from_millis(LEAD_IN_MS));
    let play_ms = samples.len() as u64 * 1000 / rate as u64;
    audio
        .play(samples)
        .map_err(|e| format!("could not play: {e}"))?;
    thread::sleep(Duration::from_millis(play_ms + TAIL_MS));
    if let Some(e) = audio.take_error() {
        return Err(format!("the audio stream failed: {e}"));
    }
    drop(audio);
    let out = heard.lock().unwrap_or_else(|p| p.into_inner()).clone();
    Ok(out)
}

fn read_wav(path: &str) -> Result<(Vec<f32>, u32), String> {
    let mut r = hound::WavReader::open(path).map_err(|e| format!("{path}: {e}"))?;
    let spec = r.spec();
    let ch = spec.channels.max(1) as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let full = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.unwrap_or(0) as f32 / full)
                .collect()
        }
    };
    let mono = raw
        .chunks(ch)
        .map(|f| f.iter().sum::<f32>() / ch as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

fn write_wav(path: &str, samples: &[f32], rate: u32) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| format!("{path}: {e}"))?;
    for &s in samples {
        w.write_sample(s).map_err(|e| format!("{path}: {e}"))?;
    }
    w.finalize().map_err(|e| format!("{path}: {e}"))
}

/// In-band power of each 20 ms block of `x`, from an FFT of the block.
fn band_powers(x: &[f32], rate: u32, band: Band) -> Vec<f64> {
    let n = (rate as usize / 50).max(64);
    let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
    let (lo, hi) = band.range_hz();
    let k0 = (lo as f64 * n as f64 / rate as f64).floor() as usize;
    let k1 = ((hi as f64 * n as f64 / rate as f64).ceil() as usize).min(n / 2);
    x.chunks_exact(n)
        .map(|block| {
            let mut buf: Vec<Complex<f32>> = block
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos();
                    Complex::new(s * w, 0.0)
                })
                .collect();
            fft.process(&mut buf);
            buf[k0..=k1]
                .iter()
                .map(|c| c.norm_sqr() as f64)
                .sum::<f64>()
        })
        .collect()
}

fn percentile(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    s[((s.len() - 1) as f64 * p).round() as usize]
}

/// Signal-to-noise ratio in the band, in dB: the median block (most
/// blocks are frames) against the quietest 2% (the room before the first
/// frame and in the gaps between them).
fn snr_db(x: &[f32], rate: u32, band: Band) -> Option<f64> {
    // A microphone stream starts with a moment of digital silence before
    // the first real samples; that is not the room, so skip it and the
    // block it ends in.
    let start = x.iter().position(|s| *s != 0.0).unwrap_or(x.len());
    let x = &x[(start + rate as usize / 50).min(x.len())..];
    let p = band_powers(x, rate, band);
    let noise = percentile(&p, 0.02);
    let signal = percentile(&p, 0.5);
    (noise > 0.0).then(|| 10.0 * ((signal - noise).max(f64::MIN_POSITIVE) / noise).log10())
}

fn score(cfg: &ModemConfig, heard: &[f32], sent: &[Vec<u8>]) -> ExitCode {
    let peak = heard.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let rms =
        (heard.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / heard.len().max(1) as f64).sqrt();
    println!(
        "recording: {:.2} s at {} Hz, peak {:.4}, rms {:.5}",
        heard.len() as f64 / cfg.sample_rate as f64,
        cfg.sample_rate,
        peak,
        rms
    );
    if peak == 0.0 {
        println!(
            "the microphone gave digital silence: on macOS this usually means the app running \
             this (the terminal) has not been granted microphone access"
        );
        return ExitCode::from(2);
    }

    let mut demod = Demodulator::new(*cfg);
    let mut events = demod.push(heard);
    // flush the demodulator's look-ahead with a little silence
    events.extend(demod.push(&vec![0.0; cfg.sample_rate as usize / 2]));
    let got: Vec<Vec<u8>> = events
        .into_iter()
        .filter_map(|e| match e {
            DemodEvent::Frame(f) => Some(f),
            DemodEvent::Level { .. } => None,
        })
        .collect();

    let mut reasm = Reassembler::new();
    let mut ok = BTreeSet::new();
    let (mut bad, mut exact, mut byte_errors, mut bytes) = (0, 0, 0usize, 0usize);
    for f in &got {
        for e in reasm.push(f, 0) {
            match e {
                Event::FrameOk { tag: TAG, seq, .. } => {
                    ok.insert(seq);
                    let want = &sent[seq as usize];
                    if f == want {
                        exact += 1;
                    }
                    if f.len() == want.len() {
                        byte_errors += f.iter().zip(want).filter(|(a, b)| a != b).count();
                        bytes += want.len();
                    }
                }
                Event::FrameOk { .. } | Event::FrameBad => bad += 1,
                Event::Complete { .. } => {}
            }
        }
    }

    let n = sent.len();
    println!(
        "config: {:?} band, {} tones, {} ms symbols",
        cfg.band, cfg.tones, cfg.symbol_ms
    );
    println!(
        "frames: {} of {} decoded ({:.0}%), {} exact before error correction, {} heard but bad",
        ok.len(),
        n,
        100.0 * ok.len() as f64 / n as f64,
        exact,
        bad
    );
    if bytes > 0 {
        println!(
            "byte errors corrected: {} of {} ({:.2}%)",
            byte_errors,
            bytes,
            100.0 * byte_errors as f64 / bytes as f64
        );
    }
    match snr_db(heard, cfg.sample_rate, cfg.band) {
        Some(db) => println!("snr in band: {db:.1} dB"),
        None => println!("snr in band: no noise floor to measure against"),
    }
    println!("measured_rate: {:.1} bytes a second", measured_rate(cfg));
    if ok.len() == n {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    let a = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("measure: {e}");
            return ExitCode::from(64);
        }
    };
    let sent = test_frames(a.frames);

    if let Some(path) = &a.wav {
        let (heard, rate) = match read_wav(path) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("measure: {e}");
                return ExitCode::from(66);
            }
        };
        return score(&config(&a, rate), &heard, &sent);
    }

    let audio = match CpalAudio::new() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("measure: could not open the sound card: {e}");
            return ExitCode::from(69);
        }
    };
    let cfg = config(&a, audio.sample_rate());
    let gap = (a.gap_ms as usize * cfg.sample_rate as usize) / 1000;
    let samples = programme(&cfg, &sent, gap, a.gain);
    println!(
        "playing {} frames, {:.1} s of sound, and recording the microphone",
        sent.len(),
        samples.len() as f64 / cfg.sample_rate as f64
    );
    let heard = match play_and_record(audio, samples) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("measure: {e}");
            return ExitCode::from(69);
        }
    };
    if let Some(path) = &a.save {
        if let Err(e) = write_wav(path, &heard, cfg.sample_rate) {
            eprintln!("measure: {e}");
        }
    }
    score(&cfg, &heard, &sent)
}
