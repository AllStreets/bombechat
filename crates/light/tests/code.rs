//! The light code end to end: encode, render, read back, collect.

use bombechat_light::{encode, read_clean, render, CodeSpec, Collector, LightEvent};
use proptest::prelude::*;
use rand::rngs::StdRng;
use rand::{Rng, RngCore, SeedableRng};

const PX: u32 = 480;

fn presets() -> [(&'static str, CodeSpec, u32); 3] {
    [
        ("slow", CodeSpec::slow(), 6),
        ("normal", CodeSpec::normal(), 12),
        ("fast", CodeSpec::fast(), 20),
    ]
}

fn envelope(len: usize, seed: u64) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    StdRng::seed_from_u64(seed).fill_bytes(&mut bytes);
    bytes
}

fn completed(events: &[LightEvent]) -> Option<(u16, Vec<u8>)> {
    events.iter().find_map(|e| match e {
        LightEvent::Complete { tag, bytes } => Some((*tag, bytes.clone())),
        _ => None,
    })
}

/// Feed frames from `frames` into a fresh collector until one completes;
/// returns the bytes and how many frames were pushed.
fn collect_until_complete(frames: impl Iterator<Item = Vec<u8>>, limit: usize) -> (Vec<u8>, usize) {
    let mut collector = Collector::new();
    for (i, frame) in frames.take(limit).enumerate() {
        if let Some((_, bytes)) = completed(&collector.push(&frame, i as u64)) {
            return (bytes, i + 1);
        }
    }
    panic!("did not complete within {limit} frames");
}

#[test]
fn round_trip_through_the_rendered_image() {
    for (name, spec, _) in presets() {
        let env = envelope(1500, 1);
        let mut enc = encode(&spec, &env, 7);
        let mut collector = Collector::new();
        let mut done = None;
        for i in 0..100 {
            let frame = enc.next_frame();
            let img = render(&spec, &frame, PX);
            let read = read_clean(&spec, &img);
            assert_eq!(read, frame, "{name}: frame {i} read back differently");
            let events = collector.push(&read, i);
            assert!(
                matches!(events.first(), Some(LightEvent::FrameOk { tag: 7, .. })),
                "{name}: frame {i} not accepted: {events:?}"
            );
            if let Some(c) = completed(&events) {
                done = Some(c);
                break;
            }
        }
        assert_eq!(done, Some((7, env)), "{name}");
    }
}

#[test]
fn frame_ok_counts_distinct_symbols() {
    let spec = CodeSpec::normal();
    let env = envelope(4096, 2);
    let mut enc = encode(&spec, &env, 3);
    let need = enc.source_symbols();
    let first = enc.next_frame();
    let mut c = Collector::new();
    assert_eq!(
        c.push(&first, 0),
        vec![LightEvent::FrameOk {
            tag: 3,
            got: 1,
            need
        }]
    );
    // A repeat of the same frame adds nothing.
    assert_eq!(
        c.push(&first, 1),
        vec![LightEvent::FrameOk {
            tag: 3,
            got: 1,
            need
        }]
    );
    let second = enc.next_frame();
    assert_eq!(
        c.push(&second, 2),
        vec![LightEvent::FrameOk {
            tag: 3,
            got: 2,
            need
        }]
    );
}

#[test]
fn joining_late_with_heavy_loss_still_completes() {
    for (name, spec, _) in presets() {
        let env = envelope(4096, 3);
        let mut enc = encode(&spec, &env, 11);
        for _ in 0..37 {
            enc.next_frame();
        }
        let mut rng = StdRng::seed_from_u64(4);
        let frames = std::iter::from_fn(|| Some(enc.next_frame())).filter(|_| rng.gen_bool(0.7));
        let k = spec.source_symbols(env.len()) as usize;
        let (bytes, used) = collect_until_complete(frames, 3 * k);
        assert_eq!(bytes, env, "{name}");
        assert!(
            used <= k + 3,
            "{name}: needed {used} received frames for K = {k}"
        );
    }
}

#[test]
fn interleaved_tags_complete_separately() {
    let spec = CodeSpec::normal();
    let a = envelope(4096, 5);
    let b = envelope(900, 6);
    let mut ea = encode(&spec, &a, 100);
    let mut eb = encode(&spec, &b, 200);
    let mut c = Collector::new();
    let mut got = Vec::new();
    for i in 0..60u64 {
        for frame in [ea.next_frame(), eb.next_frame()] {
            if let Some(done) = completed(&c.push(&frame, i)) {
                got.push(done);
            }
        }
    }
    got.sort();
    assert_eq!(got, vec![(100, a), (200, b)]);
}

#[test]
fn a_completed_tag_does_not_complete_again() {
    let spec = CodeSpec::fast();
    let env = envelope(300, 7);
    let mut enc = encode(&spec, &env, 9);
    let mut c = Collector::new();
    assert!(completed(&c.push(&enc.next_frame(), 0)).is_some());
    assert!(c.push(&enc.next_frame(), 1).is_empty());
}

#[test]
fn partials_expire_after_ten_minutes() {
    let spec = CodeSpec::slow();
    let env = envelope(2000, 8);
    let mut enc = encode(&spec, &env, 5);
    let mut c = Collector::new();
    c.push(&enc.next_frame(), 0);
    c.expire(600_001);
    // The first symbol was forgotten: the count starts again.
    let events = c.push(&enc.next_frame(), 600_002);
    assert!(
        matches!(events[0], LightEvent::FrameOk { got: 1, .. }),
        "{events:?}"
    );
}

#[test]
fn frames_with_a_bad_crc_are_rejected() {
    let spec = CodeSpec::normal();
    let env = envelope(4096, 9);
    let mut enc = encode(&spec, &env, 1);
    let frame = enc.next_frame();
    let mut rng = StdRng::seed_from_u64(10);
    for _ in 0..200 {
        let mut bad = frame.clone();
        let bit = rng.gen_range(0..bad.len() * 8);
        bad[bit / 8] ^= 0x80 >> (bit % 8);
        assert!(
            Collector::new().push(&bad, 0).is_empty(),
            "flipped bit {bit} accepted"
        );
    }
    assert!(!Collector::new().push(&frame, 0).is_empty());
}

#[test]
fn a_4096_byte_envelope_completes_within_budget() {
    for (name, spec, fps) in presets() {
        let env = envelope(4096, 11);
        let mut enc = encode(&spec, &env, 2);
        let k = enc.source_symbols() as usize;
        let (bytes, used) =
            collect_until_complete(std::iter::from_fn(|| Some(enc.next_frame())), k + 2);
        assert_eq!(bytes, env, "{name}");
        // Every preset sends 4096 bytes in at most 20 frames, 3.5 seconds.
        assert!(used <= 20, "{name}: {used} frames");
        assert!(
            used as f64 / fps as f64 <= 3.5,
            "{name}: {used} frames at {fps} fps"
        );
    }
}

#[test]
fn an_empty_envelope_completes() {
    let spec = CodeSpec::normal();
    let mut enc = encode(&spec, &[], 4);
    let events = Collector::new().push(&enc.next_frame(), 0);
    assert_eq!(completed(&events), Some((4, Vec::new())));
}

/// Fraction of a frame's data cells that are dark.
fn dark_fraction(frame: &[u8]) -> f64 {
    let ones: u32 = frame.iter().map(|b| b.count_ones()).sum();
    ones as f64 / (frame.len() * 8) as f64
}

/// Mean luma of a whole rendered image, 0.0 to 1.0.
fn mean_luma(img: &image::GrayImage) -> f64 {
    let sum: u64 = img.pixels().map(|p| p.0[0] as u64).sum();
    sum as f64 / (img.width() as u64 * img.height() as u64 * 255) as f64
}

#[test]
fn every_frame_darkens_about_half_its_cells() {
    for (name, spec, _) in presets() {
        for (kind, env) in [
            ("random", envelope(4096, 12)),
            ("zeros", vec![0u8; 4096]),
            ("ones", vec![0xffu8; 4096]),
        ] {
            let mut enc = encode(&spec, &env, 0xa5a5);
            let mut last_luma: Option<f64> = None;
            for i in 0..200 {
                let frame = enc.next_frame();
                let f = dark_fraction(&frame);
                assert!(
                    (0.45..=0.55).contains(&f),
                    "{name} {kind} frame {i}: {f:.3} dark"
                );

                // Whole-image mean luma, quiet zone and marks included, moves
                // only a few percent from one frame to the next.
                let luma = mean_luma(&render(&spec, &frame, PX));
                if let Some(prev) = last_luma {
                    let change = (luma - prev).abs();
                    assert!(
                        change < 0.05,
                        "{name} {kind} frame {i}: luma moved {change:.3}"
                    );
                }
                last_luma = Some(luma);
            }
        }
    }
}

proptest! {
    #[test]
    fn push_never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..800)) {
        let mut c = Collector::new();
        c.push(&bytes, 0);
    }

    #[test]
    fn push_never_panics_on_damaged_frames(
        flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..20),
        truncate in any::<Option<usize>>(),
    ) {
        let spec = CodeSpec::slow();
        let mut enc = encode(&spec, &[1, 2, 3, 4, 5], 1);
        let mut frame = enc.next_frame();
        for (at, mask) in flips {
            let len = frame.len();
            frame[at % len] ^= mask;
        }
        if let Some(n) = truncate {
            frame.truncate(n % (frame.len() + 1));
        }
        let mut c = Collector::new();
        c.push(&frame, 0);
    }
}
