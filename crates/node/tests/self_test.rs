//! The self-test: one node whose speaker its own microphone hears
//! (`FakeAudio::looped`, with white noise 10 dB under the signal on the
//! way round), sending a sealed message to itself. Time is driven by
//! explicit `now_ms` values and every wait is counted in rounds; nothing
//! sleeps.

use bombechat_core::{Envelope, Identity};
use bombechat_modem::{
    measured_rate, modulate, split, Band, DemodEvent, Demodulator, Event, FakeAudio, ModemConfig,
    QuietGate, Reassembler,
};
use bombechat_node::{
    Node, NodeEvent, SelfTestState, SoundSwitch, SoundTransport, Transport, NOTHING_TO_SEND_ON,
    SELF_TEST_MS, SELF_TEST_TEXT,
};
use bombechat_store::{Channel, Store};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tempfile::TempDir;

const PASS: &str = "correct horse";
/// Each round is this much test time.
const ROUND_MS: u64 = 500;

fn gauss(r: &mut StdRng) -> f32 {
    // Box-Muller.
    let u1: f32 = r.gen_range(f32::EPSILON..1.0);
    let u2: f32 = r.gen();
    (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
}

/// RMS of a full frame as the modem plays it.
fn signal_rms() -> f32 {
    let frame = &split(&[0x5a; 48], 1)[0];
    let s = modulate(&ModemConfig::default(), frame);
    (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
}

/// A laptop alone in a quiet room, hearing itself.
struct Laptop {
    node: Node,
    /// The test's handle on the sound card.
    audio: FakeAudio,
    rng: StdRng,
    now_ms: u64,
    events: Vec<NodeEvent>,
    dir: TempDir,
}

impl Laptop {
    /// `hears_itself` false: a speaker the microphone never picks up.
    fn new(hears_itself: bool) -> Laptop {
        let dir = TempDir::new().unwrap();
        let me = Identity::generate();
        let store = Store::create(&dir.path().join("me.db"), PASS, &me, "Ana").unwrap();
        let noise_rms = signal_rms() * 10f32.powf(-10.0 / 20.0);
        let mut nr = StdRng::seed_from_u64(7);
        let audio = if hears_itself {
            FakeAudio::looped(48_000).with_noise(move || gauss(&mut nr) * noise_rms)
        } else {
            FakeAudio::new(48_000)
        };
        // Two repeats, 90 s apart: what is left of the test once it is
        // heard must not play.
        let sound: Box<dyn Transport> = Box::new(SoundTransport::new(
            Box::new(audio.clone()),
            ModemConfig::default(),
            QuietGate::new(40.0, 2, 90),
        ));
        let node = Node::new(Some(store), vec![sound]);
        Laptop {
            node,
            audio,
            rng: StdRng::seed_from_u64(3),
            now_ms: 0,
            events: Vec::new(),
            dir,
        }
    }

    /// 200 ms of a quiet room (about 24 dB) at the microphone, then a tick.
    fn round(&mut self) {
        self.now_ms += ROUND_MS;
        let quiet: Vec<f32> = (0..9_600).map(|_| gauss(&mut self.rng) * 3e-4).collect();
        self.audio.feed(&quiet);
        let ev = self.node.tick(self.now_ms);
        self.events.extend(ev);
    }

    /// Rounds until `done` holds; panics after `max`. Returns how many.
    fn run(&mut self, max: usize, done: impl Fn(&[NodeEvent]) -> bool) -> usize {
        for n in 1..=max {
            self.round();
            if done(&self.events) {
                return n;
            }
        }
        panic!("gave up after {max} rounds");
    }

    fn rounds(&mut self, n: usize) {
        for _ in 0..n {
            self.round();
        }
    }

    fn tests(&self) -> Vec<(u16, SelfTestState)> {
        self.events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::SelfTest {
                    tag,
                    channel: Channel::Sound,
                    state,
                } => Some((*tag, state.clone())),
                _ => None,
            })
            .collect()
    }

    /// The envelopes in everything the speaker has played, decoded the way
    /// a listener would.
    fn played_envelopes(&self) -> Vec<Envelope> {
        let mut demod = Demodulator::new(ModemConfig::default());
        let mut reasm = Reassembler::new();
        let mut out = Vec::new();
        for e in demod.push(&self.audio.played()) {
            if let DemodEvent::Frame(f) = e {
                for r in reasm.push(&f, 0) {
                    if let Event::Complete { bytes, .. } = r {
                        out.push(Envelope::decode(&bytes).unwrap());
                    }
                }
            }
        }
        out
    }
}

fn chunk_tags(events: &[NodeEvent]) -> Vec<u16> {
    events
        .iter()
        .filter_map(|e| match e {
            NodeEvent::ChunkLanded { tag, .. } => Some(*tag),
            _ => None,
        })
        .collect()
}

#[test]
fn a_laptop_hears_its_own_sealed_message_and_keeps_none_of_it() {
    let mut l = Laptop::new(true);
    l.node.start_self_test(Channel::Sound).unwrap();
    assert!(l.node.self_testing());
    l.run(200, heard);
    let tests = l.tests();
    let tag = tests[0].0;
    let states: Vec<&SelfTestState> = tests.iter().map(|(_, s)| s).collect();
    assert!(
        matches!(
            states[..],
            [
                SelfTestState::Waiting,
                SelfTestState::Playing,
                SelfTestState::Heard { .. }
            ]
        ),
        "waiting for quiet, playing, heard: {states:?}"
    );
    assert!(tests.iter().all(|(t, _)| *t == tag), "one tag throughout");
    let SelfTestState::Heard {
        text, bytes, band, ..
    } = &tests[2].1
    else {
        unreachable!("matched above");
    };
    assert_eq!(text, SELF_TEST_TEXT);
    assert_eq!(*band, Some(Band::NearSilent));
    assert!(!l.node.self_testing());

    // The drums turned as for any message: every chunk, in order, under
    // the test's tag, the text among them.
    let chunks: Vec<(u16, u16, u16, Vec<u8>)> = l
        .events
        .iter()
        .filter_map(|e| match e {
            NodeEvent::ChunkLanded {
                tag,
                index,
                of,
                view,
                channel: Channel::Sound,
            } => Some((*tag, *index, *of, view.plain.to_vec())),
            _ => None,
        })
        .collect();
    let of = chunks[0].2;
    assert!(of > 1);
    assert_eq!(
        chunks.iter().map(|c| (c.0, c.1)).collect::<Vec<_>>(),
        (0..of).map(|i| (tag, i)).collect::<Vec<_>>()
    );
    let plain: Vec<u8> = chunks.iter().flat_map(|c| c.3.clone()).collect();
    assert!(plain
        .windows(SELF_TEST_TEXT.len())
        .any(|w| w == SELF_TEST_TEXT.as_bytes()));

    // Nothing for the sheet, the outbox or a receipt.
    assert!(!l.events.iter().any(|e| matches!(
        e,
        NodeEvent::Message(_) | NodeEvent::Outbox { .. } | NodeEvent::Heard { .. }
    )));

    // What it played was one envelope, sealed to this machine alone,
    // whose bytes the test reports; and no repeat of it plays after.
    let played = l.played_envelopes();
    assert_eq!(played.len(), 1, "the envelope, once, and nothing else");
    assert_eq!(*bytes, played[0].encode().len());
    let samples = l.audio.played().len();
    l.rounds(2 * 190_000 / ROUND_MS as usize);
    assert_eq!(l.audio.played().len(), samples, "its repeats were let go");

    // Nothing kept: open the store again and look for it.
    let id = played[0].id;
    let store = Store::open(&l.dir.path().join("me.db"), PASS).unwrap();
    assert!(!store.has_message(&id) && store.message(&id).is_none());
    assert!(store.waiting().is_empty() && store.broadcasts().is_empty());
    assert!(store.contacts().is_empty());
}

fn heard(events: &[NodeEvent]) -> bool {
    any_state(events, |s| matches!(s, SelfTestState::Heard { .. }))
}

#[test]
fn a_replay_of_the_test_is_not_heard_again() {
    let mut l = Laptop::new(true);
    l.node.start_self_test(Channel::Sound).unwrap();
    l.run(200, heard);
    let before = l.events.len();

    // The same envelope again, as if someone had caught it and played it
    // back, once as it was and once cut afresh under another tag (as a
    // carrier would pass it on): the same id, the same signature.
    let recording = l.audio.played();
    let envelope = l.played_envelopes().remove(0);
    let mut reframed = Vec::new();
    for frame in split(&envelope.encode(), l.tests()[0].0 ^ 0x5555) {
        reframed.extend(modulate(&ModemConfig::default(), &frame));
    }
    l.audio.feed(&recording);
    l.audio.feed(&reframed);
    l.rounds(10);
    let after = &l.events[before..];
    assert!(chunk_tags(after).is_empty(), "no chunk opened again");
    assert!(!after
        .iter()
        .any(|e| matches!(e, NodeEvent::SelfTest { .. } | NodeEvent::Message(_))));

    // And a test afresh is a new envelope: heard again.
    l.node.start_self_test(Channel::Sound).unwrap();
    l.run(200, |e| {
        e[before..].iter().any(|e| {
            matches!(
                e,
                NodeEvent::SelfTest {
                    state: SelfTestState::Heard { .. },
                    ..
                }
            )
        })
    });
}

#[test]
fn its_own_messages_and_notices_are_still_not_heard() {
    let mut l = Laptop::new(true);
    let them = Identity::generate();
    let store = Store::open(&l.dir.path().join("me.db"), PASS).unwrap();
    let ben = store.add_contact("Ben", &them.public(), true).unwrap();
    drop(store);

    // A message and a notice go out, and a test with them.
    let id = l
        .node
        .send(&[ben], "Water is at the church on Fifth.", Channel::Sound);
    let notice = l
        .node
        .broadcast("Shelter open at the high school.", Channel::Sound);
    l.node.start_self_test(Channel::Sound).unwrap();
    l.run(400, heard);
    let tag = l.tests()[0].0;
    // all three played, and the microphone heard them all
    assert_eq!(l.played_envelopes().len(), 3);
    let ids: Vec<[u8; 16]> = l.played_envelopes().iter().map(|e| e.id).collect();
    assert!(ids.contains(&id) && ids.contains(&notice));

    // only the test's chunks landed; the message and notice were not heard
    assert!(chunk_tags(&l.events).iter().all(|t| *t == tag));
    assert!(!l.events.iter().any(|e| matches!(
        e,
        NodeEvent::Message(_) | NodeEvent::Broadcast { .. } | NodeEvent::Heard { .. }
    )));
}

#[test]
fn a_test_nobody_hears_is_given_up_after_its_time() {
    let mut l = Laptop::new(false);
    l.node.start_self_test(Channel::Sound).unwrap();
    l.run(10, |e| any_state(e, |s| *s == SelfTestState::Playing));
    let playing_at = l.now_ms;
    let bytes = l.played_envelopes()[0].encode().len() as f32;
    let on_air = (bytes * 1000.0 / measured_rate(&ModemConfig::default())).ceil() as u64;

    // given up at its time on air plus SELF_TEST_MS from its first play,
    // in the first round after that and not before
    l.run(400, ended);
    let after = l.now_ms - playing_at;
    assert!(
        after >= on_air + SELF_TEST_MS && after < on_air + SELF_TEST_MS + ROUND_MS,
        "given up {after} ms after it began to play; on air {on_air} ms"
    );
    assert_eq!(l.tests().last().unwrap().1, SelfTestState::NotHeard);
    assert!(!l.node.self_testing());
}

#[test]
fn a_stopped_test_plays_no_more_and_is_not_heard() {
    let mut l = Laptop::new(true);
    // the room is loud until the test is stopped: nothing has played
    l.node.set_send_under(10.0);
    l.node.start_self_test(Channel::Sound).unwrap();
    l.rounds(4);
    l.node.stop_self_test();
    l.node.set_send_under(40.0);
    l.rounds(40);
    assert!(l.audio.played().is_empty(), "what was queued was let go");
    let states: Vec<SelfTestState> = l.tests().into_iter().map(|(_, s)| s).collect();
    assert_eq!(states, vec![SelfTestState::Waiting, SelfTestState::Stopped]);

    // turning Sound to Listen stops one too
    l.node.start_self_test(Channel::Sound).unwrap();
    l.node.set_sound(SoundSwitch::Listen);
    l.rounds(2);
    assert_eq!(l.tests().last().unwrap().1, SelfTestState::Stopped);
    assert!(!l.node.self_testing());
}

#[test]
fn a_test_is_refused_as_pairing_is() {
    let mut l = Laptop::new(true);
    l.node.set_sound(SoundSwitch::Listen);
    assert_eq!(
        l.node.start_self_test(Channel::Sound),
        Err("not sent: sound is set to listen only")
    );
    l.node.set_sound(SoundSwitch::Off);
    assert_eq!(
        l.node.start_self_test(Channel::Sound),
        Err("not sent: sound is off")
    );
    assert_eq!(
        l.node.start_self_test(Channel::Light),
        Err(NOTHING_TO_SEND_ON),
        "this laptop has no light transport"
    );
    l.rounds(1);
    assert!(l.tests().is_empty() && !l.node.self_testing());

    let mut locked = Node::new(None, Vec::new());
    assert_eq!(locked.start_self_test(Channel::Sound), Err("locked"));
}

fn any_state(events: &[NodeEvent], f: impl Fn(&SelfTestState) -> bool) -> bool {
    events
        .iter()
        .any(|e| matches!(e, NodeEvent::SelfTest { state, .. } if f(state)))
}

fn ended(events: &[NodeEvent]) -> bool {
    any_state(events, |s| {
        !matches!(s, SelfTestState::Waiting | SelfTestState::Playing)
    })
}
