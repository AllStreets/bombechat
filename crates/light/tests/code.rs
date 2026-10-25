//! The light code end to end: encode, render, read back, collect.

use bombechat_light::code::{BLOCK_DARK_MAX, BLOCK_DARK_MIN};
use bombechat_light::{
    encode, geometry, orientation, read_clean, render, CodeSpec, Collector, Layout, LightEvent,
    Orientation,
};
use image::GrayImage;
use proptest::prelude::*;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, RngCore, SeedableRng};

const PX: u32 = 480;

fn presets() -> [(&'static str, CodeSpec); 3] {
    [
        ("slow", CodeSpec::slow()),
        ("normal", CodeSpec::normal()),
        ("fast", CodeSpec::fast()),
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

fn accepted(events: &[LightEvent]) -> bool {
    matches!(events.first(), Some(LightEvent::FrameOk { .. }))
}

/// Feed frames into a fresh collector until one completes; returns the
/// bytes and how many frames were pushed.
fn collect_until_complete(frames: impl Iterator<Item = Vec<u8>>, limit: usize) -> (Vec<u8>, usize) {
    let mut collector = Collector::new();
    for (i, frame) in frames.take(limit).enumerate() {
        if let Some((_, bytes)) = completed(&collector.push(&frame, i as u64)) {
            return (bytes, i + 1);
        }
    }
    panic!("did not complete within {limit} frames");
}

/// Whether the cell at `row`, `col` of a rendered image is dark.
fn cell_dark(spec: &CodeSpec, img: &GrayImage, row: u16, col: u16) -> bool {
    let (x, y) = geometry(spec, img.width()).centre(row, col);
    img.get_pixel(x, y).0[0] < 128
}

fn flip_bit(frame: &mut [u8], bit: usize) {
    frame[bit / 8] ^= 0x80 >> (bit % 8);
}

#[test]
fn round_trip_through_the_rendered_image() {
    for (name, spec) in presets() {
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
    for (name, spec) in presets() {
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
fn a_preset_change_mid_send_completes_under_the_new_preset() {
    let old = envelope(4096, 20);
    let new = envelope(3000, 21);
    let mut slow = encode(&CodeSpec::slow(), &old, 42);
    let mut fast = encode(&CodeSpec::fast(), &new, 42);
    let mut c = Collector::new();
    for i in 0..5 {
        assert!(completed(&c.push(&slow.next_frame(), i)).is_none());
    }
    let mut done = None;
    for i in 5..60 {
        let events = c.push(&fast.next_frame(), i);
        assert!(accepted(&events), "frame {i} after the change was refused");
        if let Some(d) = completed(&events) {
            done = Some(d);
            break;
        }
    }
    assert_eq!(done, Some((42, new)));
}

#[test]
fn a_completed_tag_is_not_delivered_twice_during_a_long_send() {
    let spec = CodeSpec::fast();
    let env = envelope(300, 7);
    let mut enc = encode(&spec, &env, 9);
    let mut c = Collector::new();
    assert!(completed(&c.push(&enc.next_frame(), 0)).is_some());
    // The sender keeps flashing for twenty minutes; every frame refreshes
    // the memory that tag 9 is done.
    for minute in 1..=20u64 {
        assert!(c.push(&enc.next_frame(), minute * 60_000).is_empty());
    }
}

#[test]
fn a_rejected_tag_is_collected_afresh() {
    let spec = CodeSpec::fast();
    let env = envelope(300, 8);
    let mut enc = encode(&spec, &env, 9);
    let mut c = Collector::new();
    assert!(completed(&c.push(&enc.next_frame(), 0)).is_some());
    c.reject(9);
    assert_eq!(completed(&c.push(&enc.next_frame(), 1)), Some((9, env)));
}

#[test]
fn partials_expire_after_ten_minutes_even_without_an_expire_call() {
    let spec = CodeSpec::slow();
    let env = envelope(2000, 8);
    let mut enc = encode(&spec, &env, 5);
    let mut c = Collector::new();
    c.push(&enc.next_frame(), 0);
    // The first symbol was forgotten: the count starts again.
    let events = c.push(&enc.next_frame(), 600_001);
    assert!(
        matches!(events[0], LightEvent::FrameOk { got: 1, .. }),
        "{events:?}"
    );
}

#[test]
fn scattered_misread_cells_are_corrected() {
    // One percent of the data cells, flipped at random, in 100 frames per
    // preset.
    for (name, spec) in presets() {
        let env = envelope(4096, 12);
        let mut enc = encode(&spec, &env, 1);
        let cells = spec.frame_bytes() * 8;
        let n = cells / 100;
        let mut rng = StdRng::seed_from_u64(13);
        let mut all: Vec<usize> = (0..cells).collect();
        for i in 0..100 {
            let mut frame = enc.next_frame();
            all.shuffle(&mut rng);
            for &bit in &all[..n] {
                flip_bit(&mut frame, bit);
            }
            assert!(
                accepted(&Collector::new().push(&frame, 0)),
                "{name}: frame {i} with {n} flipped cells refused"
            );
        }
    }
}

#[test]
fn a_band_of_misread_rows_is_corrected() {
    // Every data cell in a band of adjacent rows read wrong, the band
    // swept down the grid. For normal and fast the band is more than one
    // codeword can repair; interleaving spreads it over both.
    for (name, spec, rows) in [
        ("slow", CodeSpec::slow(), 4u16),
        ("normal", CodeSpec::normal(), 5),
        ("fast", CodeSpec::fast(), 6),
    ] {
        let layout = Layout::new(&spec);
        let env = envelope(4096, 14);
        let mut enc = encode(&spec, &env, 1);
        for top in 2..spec.cells - rows {
            let mut frame = enc.next_frame();
            let mut flipped = 0;
            for (bit, &(row, _)) in layout.data_positions().iter().enumerate() {
                if bit < frame.len() * 8 && row >= top && row < top + rows {
                    flip_bit(&mut frame, bit);
                    flipped += 1;
                }
            }
            assert!(
                accepted(&Collector::new().push(&frame, 0)),
                "{name}: rows {top}..{} ({flipped} cells) not corrected",
                top + rows
            );
        }
    }
}

#[test]
fn frames_past_repair_are_rejected() {
    for (name, spec) in presets() {
        let env = envelope(4096, 9);
        let mut enc = encode(&spec, &env, 1);
        let mut rng = StdRng::seed_from_u64(10);
        for i in 0..200 {
            let mut frame = enc.next_frame();
            // Trash a third of the coded region's bytes.
            for b in frame.iter_mut().skip(8) {
                if rng.gen_bool(0.33) {
                    *b ^= rng.gen_range(1..=255u8);
                }
            }
            assert!(
                Collector::new().push(&frame, 0).is_empty(),
                "{name}: trashed frame {i} accepted"
            );
        }
    }
}

#[test]
fn a_4096_byte_envelope_completes_within_budget() {
    for (name, spec) in presets() {
        let env = envelope(4096, 11);
        let mut enc = encode(&spec, &env, 2);
        let k = enc.source_symbols() as usize;
        let (bytes, used) =
            collect_until_complete(std::iter::from_fn(|| Some(enc.next_frame())), k + 2);
        assert_eq!(bytes, env, "{name}");
        let seconds = used as f64 / spec.frames_per_second() as f64;
        assert!(used <= 26, "{name}: {used} frames");
        assert!(seconds <= 4.5, "{name}: {seconds:.2} s");
    }
}

#[test]
fn an_empty_envelope_completes() {
    let spec = CodeSpec::normal();
    let mut enc = encode(&spec, &[], 4);
    let events = Collector::new().push(&enc.next_frame(), 0);
    assert_eq!(completed(&events), Some((4, Vec::new())));
}

#[test]
fn every_8x8_block_stays_40_to_60_percent_dark() {
    for (name, spec) in presets() {
        let n = spec.cells;
        for (kind, env) in [
            ("random", envelope(4096, 12)),
            ("zeros", vec![0u8; 4096]),
            ("ones", vec![0xffu8; 4096]),
        ] {
            let mut enc = encode(&spec, &env, 0xa5a5);
            for i in 0..300 {
                let img = render(&spec, &enc.next_frame(), PX);
                for br in 0..n / 8 {
                    for bc in 0..n / 8 {
                        let mut dark = 0;
                        for r in 0..8 {
                            for c in 0..8 {
                                dark += cell_dark(&spec, &img, br * 8 + r, bc * 8 + c) as u32;
                            }
                        }
                        assert!(
                            (BLOCK_DARK_MIN..=BLOCK_DARK_MAX).contains(&dark),
                            "{name} {kind} frame {i} block ({br}, {bc}): {dark} of 64 dark"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_mirrored_picture_is_detected_as_mirrored() {
    for (name, spec) in presets() {
        let mut enc = encode(&spec, &envelope(500, 15), 1);
        let img = render(&spec, &enc.next_frame(), PX);
        // A mirrored picture, once its three finders are lined up the
        // usual way round, is the drawing reflected across its diagonal.
        let mut mirrored = GrayImage::new(PX, PX);
        for (x, y, p) in img.enumerate_pixels() {
            mirrored.put_pixel(y, x, *p);
        }
        assert_eq!(
            orientation(&spec, |r, c| cell_dark(&spec, &img, r, c)),
            Some(Orientation::Normal),
            "{name}"
        );
        assert_eq!(
            orientation(&spec, |r, c| cell_dark(&spec, &mirrored, r, c)),
            Some(Orientation::Mirrored),
            "{name}"
        );
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
        flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..60),
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
