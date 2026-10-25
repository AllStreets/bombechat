use bombechat_modem::{split, Event, Reassembler, FRAME_DATA};
use proptest::prelude::*;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

/// Flip `n` distinct bytes of `frame` to a different value each, using
/// `rng` to pick which bytes and what to flip them to.
fn flip_bytes(frame: &mut [u8], n: usize, rng: &mut StdRng) {
    let mut positions: Vec<usize> = (0..frame.len()).collect();
    positions.shuffle(rng);
    for &p in positions.iter().take(n) {
        let flip = loop {
            let candidate = rng.gen::<u8>();
            if candidate != 0 {
                break candidate;
            }
        };
        frame[p] ^= flip;
    }
}

#[test]
fn roundtrip_in_order() {
    let env = vec![7u8; 300];
    let mut r = Reassembler::new();
    let mut out = None;
    for f in split(&env, 9) {
        for e in r.push(&f, 0) {
            if let Event::Complete { bytes, .. } = e {
                out = Some(bytes);
            }
        }
    }
    assert_eq!(out.unwrap(), env);
}

#[test]
fn out_of_order_and_duplicates() {
    let env: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
    let frames = split(&env, 42);

    let mut order: Vec<usize> = (0..frames.len()).collect();
    order.reverse();

    let mut r = Reassembler::new();
    let mut completes: Vec<Vec<u8>> = Vec::new();
    // Push every frame twice, in reverse order.
    for &i in order.iter().chain(order.iter()) {
        for e in r.push(&frames[i], 0) {
            if let Event::Complete { bytes, .. } = e {
                completes.push(bytes);
            }
        }
    }

    assert_eq!(completes.len(), 1);
    assert_eq!(completes[0], env);
}

#[test]
fn corrects_eight_bad_bytes() {
    let env = vec![3u8; 150];
    let mut frames = split(&env, 5);
    let mut rng = StdRng::seed_from_u64(1);
    flip_bytes(&mut frames[0], 8, &mut rng);

    let mut r = Reassembler::new();
    let mut out = None;
    for f in &frames {
        for e in r.push(f, 0) {
            if let Event::Complete { bytes, .. } = e {
                out = Some(bytes);
            }
        }
    }
    assert_eq!(
        out.unwrap(),
        env,
        "8 corrupted bytes must still be correctable"
    );
}

#[test]
fn nine_bad_bytes_is_frame_bad_then_repeat_fixes() {
    let env = vec![9u8; 150];
    let frames = split(&env, 6);

    let mut corrupted = frames[0].clone();
    let mut rng = StdRng::seed_from_u64(2);
    flip_bytes(&mut corrupted, 9, &mut rng);

    let mut r = Reassembler::new();
    let events = r.push(&corrupted, 0);
    assert_eq!(
        events,
        vec![Event::FrameBad],
        "9 corrupted bytes must exceed correction capacity"
    );

    // A clean repeat of frame 0, followed by the rest, completes normally.
    let mut out = None;
    for f in &frames {
        for e in r.push(f, 0) {
            if let Event::Complete { bytes, .. } = e {
                out = Some(bytes);
            }
        }
    }
    assert_eq!(out.unwrap(), env);
}

#[test]
fn two_senders_interleaved() {
    let env1 = vec![1u8; 100];
    let env2 = vec![2u8; 130];
    let frames1 = split(&env1, 1);
    let frames2 = split(&env2, 2);

    let mut r = Reassembler::new();
    let mut got1 = None;
    let mut got2 = None;

    let steps = frames1.len().max(frames2.len());
    for i in 0..steps {
        if let Some(f) = frames1.get(i) {
            for e in r.push(f, 0) {
                if let Event::Complete { tag, bytes } = e {
                    assert_eq!(tag, 1);
                    got1 = Some(bytes);
                }
            }
        }
        if let Some(f) = frames2.get(i) {
            for e in r.push(f, 0) {
                if let Event::Complete { tag, bytes } = e {
                    assert_eq!(tag, 2);
                    got2 = Some(bytes);
                }
            }
        }
    }

    assert_eq!(got1.unwrap(), env1);
    assert_eq!(got2.unwrap(), env2);
}

#[test]
fn prefix_grows_contiguously() {
    // Exactly 5 frames, so completion never happens while we only push 3.
    let env: Vec<u8> = (0..(5 * FRAME_DATA) as u32)
        .map(|i| (i % 256) as u8)
        .collect();
    let frames = split(&env, 11);
    assert_eq!(frames.len(), 5);

    let mut r = Reassembler::new();
    r.push(&frames[0], 0);
    r.push(&frames[2], 0);
    assert_eq!(
        r.prefix(11).unwrap(),
        &env[0..FRAME_DATA],
        "seq 2 arrived but seq 1 is missing, so the prefix stops after frame 0"
    );

    r.push(&frames[1], 0);
    assert_eq!(
        r.prefix(11).unwrap(),
        &env[0..3 * FRAME_DATA],
        "seq 1 filled the gap, so the prefix now covers frames 0..=2"
    );
}

#[test]
fn partial_expires_after_ten_minutes() {
    let env = vec![4u8; 200];
    let frames = split(&env, 20);
    let half = frames.len() / 2;

    let mut r = Reassembler::new();
    for f in &frames[..half] {
        r.push(f, 0);
    }
    assert!(r.prefix(20).is_some(), "partial should exist before expiry");

    r.expire(600_001);
    assert!(r.prefix(20).is_none(), "stale partial must be dropped");

    // The remaining frames alone, pushed after expiry, must not complete
    // the message: the dropped first half is gone, not remembered.
    let mut completed = false;
    for f in &frames[half..] {
        for e in r.push(f, 600_001) {
            if matches!(e, Event::Complete { .. }) {
                completed = true;
            }
        }
    }
    assert!(
        !completed,
        "second half alone must not complete a dropped message"
    );

    // Starting over from frame 0 still works after the expiry.
    let mut out = None;
    for f in &frames {
        for e in r.push(f, 600_001) {
            if let Event::Complete { bytes, .. } = e {
                out = Some(bytes);
            }
        }
    }
    assert_eq!(out.unwrap(), env);
}

#[test]
fn duplicate_frame_of_completed_tag_does_not_double_complete() {
    let env = vec![5u8; 60];
    let frames = split(&env, 77);

    let mut r = Reassembler::new();
    let mut completes = 0;
    for f in &frames {
        for e in r.push(f, 0) {
            if matches!(e, Event::Complete { .. }) {
                completes += 1;
            }
        }
    }
    assert_eq!(completes, 1);

    // The message is already complete; repeating its last frame within
    // the ten-minute window must not produce a second Complete.
    for e in r.push(&frames[frames.len() - 1], 1_000) {
        assert!(!matches!(e, Event::Complete { .. }));
    }
}

#[test]
fn a_forgotten_tag_completes_again_from_its_frames() {
    let env = vec![5u8; 60];
    let frames = split(&env, 77);
    let completes = |r: &mut Reassembler| {
        frames
            .iter()
            .flat_map(|f| r.push(f, 0))
            .filter(|e| matches!(e, Event::Complete { .. }))
            .count()
    };
    let mut r = Reassembler::new();
    assert_eq!(completes(&mut r), 1);
    assert_eq!(completes(&mut r), 0, "remembered as complete");
    r.forget(77);
    assert_eq!(completes(&mut r), 1, "collected afresh");
}

#[test]
#[should_panic(expected = "255 frames")]
fn split_panics_when_envelope_needs_more_than_255_frames() {
    let too_big = vec![0u8; 255 * FRAME_DATA + 1];
    let _ = split(&too_big, 1);
}

proptest! {
    #[test]
    fn push_never_panics(b in proptest::collection::vec(any::<u8>(), 0..300)) {
        let _ = Reassembler::new().push(&b, 0);
    }
}
