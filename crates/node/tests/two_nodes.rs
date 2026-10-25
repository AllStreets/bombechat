//! Two (or three) nodes talking over a `LoopbackAudio` pair, with white
//! noise 10 dB below the modem signal mixed into everything that crosses
//! the room. Time is driven by explicit `now_ms` values; nothing sleeps.

use std::sync::{Arc, Mutex};

use bombechat_core::{Identity, PublicIdentity};
use bombechat_modem::{
    modulate, split, AudioError, AudioIo, FakeAudio, LoopbackAudio, ModemConfig, QuietGate,
};
use bombechat_node::{Node, NodeEvent, OutboxState, SoundTransport, Transport};
use bombechat_store::{Channel, ContactId, Store};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tempfile::TempDir;

const TEXT: &str = "Water is at the church on Fifth.";

/// The shipping modem settings: fast enough here that the tests need no
/// shortened symbols.
fn test_cfg() -> ModemConfig {
    ModemConfig::default()
}

/// Send under 40 dB, no repeats (the loopback room is reliable enough).
fn test_gate() -> QuietGate {
    QuietGate::new(40.0, 0, 90)
}

fn gauss(r: &mut StdRng) -> f32 {
    // Box-Muller.
    let u1: f32 = r.gen_range(f32::EPSILON..1.0);
    let u2: f32 = r.gen();
    (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
}

/// RMS of a full frame as the modem plays it.
fn signal_rms() -> f32 {
    let frame = &split(&[0x5a; 48], 1)[0];
    let s = modulate(&test_cfg(), frame);
    (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
}

type Sink = Box<dyn FnMut(&[f32]) + Send>;

/// One node's microphone and speaker. Playing goes through the loopback
/// pair (noise mixed in); hearing comes from the pair, plus the quiet
/// ambient room the test feeds between transmissions, the way a real
/// microphone never stops delivering samples.
struct Room {
    inner: FakeAudio,
    sink: Arc<Mutex<Option<Sink>>>,
}

/// The test's handle on a [`Room`]'s microphone.
struct Ambient {
    sink: Arc<Mutex<Option<Sink>>>,
    rng: StdRng,
}

impl Ambient {
    /// 200 ms of a quiet room (about -70 dBFS, roughly 24 dB).
    fn quiet(&mut self) {
        let samples: Vec<f32> = (0..9_600).map(|_| gauss(&mut self.rng) * 3e-4).collect();
        if let Some(sink) = self.sink.lock().unwrap().as_mut() {
            sink(&samples);
        }
    }
}

impl AudioIo for Room {
    fn start_input(&mut self, sink: Sink) -> Result<(), AudioError> {
        *self.sink.lock().unwrap() = Some(sink);
        let shared = self.sink.clone();
        self.inner.start_input(Box::new(move |s: &[f32]| {
            if let Some(sink) = shared.lock().unwrap().as_mut() {
                sink(s);
            }
        }))
    }

    fn play(&mut self, samples: Vec<f32>) -> Result<(), AudioError> {
        self.inner.play(samples)
    }

    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
}

fn room(inner: FakeAudio, seed: u64) -> (Room, Ambient) {
    let sink = Arc::new(Mutex::new(None));
    let ambient = Ambient {
        sink: sink.clone(),
        rng: StdRng::seed_from_u64(seed),
    };
    (Room { inner, sink }, ambient)
}

/// Two rooms joined by a loopback pair with -10 dB noise both ways.
fn rooms() -> ((Room, Ambient), (Room, Ambient)) {
    let noise_rms = signal_rms() * 10f32.powf(-10.0 / 20.0);
    let (a, b) = LoopbackAudio::pair(48_000);
    let mut ra = StdRng::seed_from_u64(1);
    let mut rb = StdRng::seed_from_u64(2);
    let a = a.with_noise(move || gauss(&mut ra) * noise_rms);
    let b = b.with_noise(move || gauss(&mut rb) * noise_rms);
    (room(a, 3), room(b, 4))
}

fn sound(r: Room) -> Vec<Box<dyn Transport>> {
    vec![Box::new(SoundTransport::new(
        Box::new(r),
        test_cfg(),
        test_gate(),
    ))]
}

fn new_store(dir: &TempDir, file: &str, me: &Identity, name: &str) -> Store {
    Store::create(&dir.path().join(file), "correct horse", me, name).unwrap()
}

/// A node under test plus its ambient microphone feed and every event it
/// has produced so far.
struct Peer {
    node: Node,
    ambient: Ambient,
    events: Vec<NodeEvent>,
}

impl Peer {
    fn new(node: Node, ambient: Ambient) -> Peer {
        Peer {
            node,
            ambient,
            events: Vec::new(),
        }
    }
}

/// Tick every peer in turn, 500 ms of test time per round, until `done`
/// holds, advancing `now`; panics if `max_rounds` pass first.
fn run(
    peers: &mut [&mut Peer],
    now: &mut u64,
    max_rounds: usize,
    done: impl Fn(&[&mut Peer]) -> bool,
) {
    for _ in 0..max_rounds {
        *now += 500;
        for p in peers.iter_mut() {
            p.ambient.quiet();
            let ev = p.node.tick(*now);
            p.events.extend(ev);
        }
        if done(peers) {
            return;
        }
    }
    panic!("gave up after {max_rounds} rounds");
}

fn offer(p: &Peer) -> Option<(PublicIdentity, String, String)> {
    p.events.iter().find_map(|e| match e {
        NodeEvent::PairOffer { peer, name, safety } => {
            Some((peer.clone(), name.clone(), safety.clone()))
        }
        _ => None,
    })
}

fn message_text(p: &Peer) -> Option<String> {
    p.events.iter().find_map(|e| match e {
        NodeEvent::Message(m) => Some(m.text.clone()),
        _ => None,
    })
}

/// Two nodes that already know each other (contacts added straight to
/// their stores, checked), so tests other than the pairing one do not
/// spend test time pairing by sound. Returns the peers and A's contact
/// id for B.
fn paired(dir: &TempDir) -> (Peer, Peer, ContactId) {
    let (a_id, b_id) = (Identity::generate(), Identity::generate());
    let a_store = new_store(dir, "a.db", &a_id, "Ana");
    let b_store = new_store(dir, "b.db", &b_id, "Ben");
    let b_in_a = a_store.add_contact("Ben", &b_id.public(), true).unwrap();
    b_store.add_contact("Ana", &a_id.public(), true).unwrap();
    let ((ra, aa), (rb, ab)) = rooms();
    let a = Peer::new(Node::new(Some(a_store), sound(ra)), aa);
    let b = Peer::new(Node::new(Some(b_store), sound(rb)), ab);
    (a, b, b_in_a)
}

#[test]
fn pair_then_message_then_receipt() {
    let dir = TempDir::new().unwrap();
    let (a_me, b_me) = (Identity::generate(), Identity::generate());
    let ((ra, aa), (rb, ab)) = rooms();
    let mut a = Peer::new(
        Node::new(Some(new_store(&dir, "a.db", &a_me, "Ana")), sound(ra)),
        aa,
    );
    let mut b = Peer::new(
        Node::new(Some(new_store(&dir, "b.db", &b_me, "Ben")), sound(rb)),
        ab,
    );
    let mut now = 1_000;

    a.node.start_pairing("Ana").unwrap();
    b.node.start_pairing("Ben").unwrap();
    run(&mut [&mut a, &mut b], &mut now, 200, |p| {
        offer(p[0]).is_some() && offer(p[1]).is_some()
    });

    let (b_seen_by_a, b_name, a_safety) = offer(&a).unwrap();
    let (a_seen_by_b, a_name, b_safety) = offer(&b).unwrap();
    assert_eq!(b_seen_by_a, b_me.public());
    assert_eq!(a_seen_by_b, a_me.public());
    assert_eq!((b_name.as_str(), a_name.as_str()), ("Ben", "Ana"));
    assert_eq!(a_safety, b_safety);
    assert_eq!(a_safety.len(), 60 + 11);

    let b_in_a = a.node.confirm_pairing(&b_seen_by_a, "Ben");
    let a_in_b = b.node.confirm_pairing(&a_seen_by_b, "Ana");

    let id = a.node.send(&[b_in_a], TEXT, Channel::Sound);
    run(&mut [&mut a, &mut b], &mut now, 400, |p| {
        message_text(p[1]).is_some()
            && p[0]
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::Heard { .. }))
    });

    let msg = b
        .events
        .iter()
        .find_map(|e| match e {
            NodeEvent::Message(m) => Some(m.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(msg.id, id);
    assert_eq!(msg.text, TEXT);
    assert_eq!(msg.contact, a_in_b);
    assert!(!msg.outgoing);
    assert_eq!(msg.channel, Channel::Sound);

    let heard: Vec<_> = a
        .events
        .iter()
        .filter_map(|e| match e {
            NodeEvent::Heard { id, by } => Some((*id, *by)),
            _ => None,
        })
        .collect();
    assert_eq!(heard, vec![(id, b_in_a)]);

    // A's outbox followed the message out: queued, playing, then sent once
    // the speaker has finished (loopback delivers before the speaker would).
    run(&mut [&mut a, &mut b], &mut now, 200, |p| {
        p[0].events.iter().any(
            |e| matches!(e, NodeEvent::Outbox { id: of, state: OutboxState::Sent } if *of == id),
        )
    });
    let progress: Vec<_> = a
        .events
        .iter()
        .filter_map(|e| match e {
            NodeEvent::Outbox { id: of, state } if *of == id => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(
        progress,
        vec![
            OutboxState::Waiting,
            OutboxState::Playing,
            OutboxState::Sent
        ]
    );
}

#[test]
fn chunks_land_in_order_with_views() {
    let dir = TempDir::new().unwrap();
    let (mut a, mut b, b_in_a) = paired(&dir);
    let mut now = 1_000;

    a.node.send(&[b_in_a], TEXT, Channel::Sound);
    run(&mut [&mut a, &mut b], &mut now, 200, |p| {
        message_text(p[1]).is_some()
    });

    let mut expected_index = 0u16;
    let mut of_seen = None;
    let mut message_after_chunks = false;
    for e in &b.events {
        match e {
            NodeEvent::ChunkLanded {
                index, of, view, ..
            } => {
                assert!(!message_after_chunks, "a chunk landed after the message");
                assert_eq!(*index, expected_index, "chunks out of order");
                assert_eq!(*of_seen.get_or_insert(*of), *of, "chunk count changed");
                assert_eq!(view.heard.len(), view.plain.len());
                let xor: Vec<u8> = view
                    .heard
                    .iter()
                    .zip(&view.key)
                    .map(|(h, k)| h ^ k)
                    .collect();
                assert_eq!(xor, view.plain);
                expected_index += 1;
            }
            NodeEvent::Message(m) => {
                assert_eq!(m.text, TEXT);
                message_after_chunks = true;
            }
            _ => {}
        }
    }
    assert!(message_after_chunks);
    let of = of_seen.expect("chunks landed");
    assert!(of > 1, "the message spans several chunks");
    assert_eq!(expected_index, of, "every chunk landed before the message");
}

#[test]
fn message_while_locked_is_kept() {
    let dir = TempDir::new().unwrap();
    let (a_me, b_me) = (Identity::generate(), Identity::generate());
    let a_store = new_store(&dir, "a.db", &a_me, "Ana");
    let b_store = new_store(&dir, "b.db", &b_me, "Ben");
    let b_in_a = a_store.add_contact("Ben", &b_me.public(), true).unwrap();
    b_store.add_contact("Ana", &a_me.public(), true).unwrap();

    let ((ra, aa), (rb, ab)) = rooms();
    let mut a = Peer::new(Node::new(Some(a_store), sound(ra)), aa);
    let mut b = Peer::new(Node::new(None, sound(rb)), ab);
    let mut now = 1_000;

    a.node.send(&[b_in_a], TEXT, Channel::Sound);
    // Long enough for A to play every frame and B to hear them all.
    for _ in 0..6 {
        run(&mut [&mut a, &mut b], &mut now, 1, |_| true);
    }
    assert!(message_text(&b).is_none(), "nothing opens while locked");

    b.node.unlock(b_store);
    now += 500;
    let ev = b.node.tick(now);
    let text = ev.iter().find_map(|e| match e {
        NodeEvent::Message(m) => Some(m.text.clone()),
        _ => None,
    });
    assert_eq!(text.as_deref(), Some(TEXT));
}

#[test]
fn broadcast_from_stranger_is_unverified() {
    let dir = TempDir::new().unwrap();
    let (b_me, c_me) = (Identity::generate(), Identity::generate());
    let ((rb, ab), (rc, ac)) = rooms();
    let mut b = Peer::new(
        Node::new(Some(new_store(&dir, "b.db", &b_me, "Ben")), sound(rb)),
        ab,
    );
    let mut c = Peer::new(
        Node::new(Some(new_store(&dir, "c.db", &c_me, "Cy")), sound(rc)),
        ac,
    );
    let mut now = 1_000;

    c.node.broadcast("Road north is open.");
    run(&mut [&mut c, &mut b], &mut now, 200, |p| {
        p[1].events
            .iter()
            .any(|e| matches!(e, NodeEvent::Broadcast { .. }))
    });

    let (signer, text, verified) = b
        .events
        .iter()
        .find_map(|e| match e {
            NodeEvent::Broadcast {
                signer,
                text,
                verified,
            } => Some((signer.clone(), text.clone(), *verified)),
            _ => None,
        })
        .unwrap();
    assert_eq!(signer, c_me.public());
    assert_eq!(text, "Road north is open.");
    assert!(!verified);
}

#[test]
fn mic_denied_reports_channel_state() {
    let t = SoundTransport::new(Box::new(FakeAudio::denied()), test_cfg(), test_gate());
    let mut node = Node::new(None, vec![Box::new(t)]);

    let ev = node.tick(0);
    let states: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            NodeEvent::ChannelState { channel, state } => Some((*channel, state.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        states,
        vec![(
            Channel::Sound,
            "unavailable: microphone permission denied".to_string()
        )]
    );

    // Reported once, and later ticks carry on without panicking.
    let later = node.tick(20);
    assert!(!later
        .iter()
        .any(|e| matches!(e, NodeEvent::ChannelState { .. })));
}
