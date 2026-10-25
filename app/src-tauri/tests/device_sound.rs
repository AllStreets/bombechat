//! The real machine over a loopback sound card: what the Sound switch
//! and the Send under setting do, what it does with a microphone that
//! runs too slow for the near-silent band, and a message heard while
//! locked showing once.

use std::path::Path;
use std::sync::{Arc, Mutex};

use bombechat_app::commands::{self, Shared};
use bombechat_app::Device;
use bombechat_core::{seal_private, Identity, PublicIdentity};
use bombechat_modem::{
    measured_rate, modulate, split, AudioError, AudioIo, Band, FakeAudio, LoopbackAudio,
    ModemConfig,
};
use bombechat_node::NodeEvent;
use bombechat_store::{Channel, ContactId};
use tempfile::TempDir;

type Sink = Box<dyn FnMut(&[f32]) + Send>;

/// The machine's end of the loopback, shared so the test can still feed
/// its microphone the room and see what it played.
#[derive(Clone)]
struct Card(Arc<Mutex<FakeAudio>>);

impl AudioIo for Card {
    fn start_input(&mut self, sink: Sink) -> Result<(), AudioError> {
        self.0.lock().unwrap().start_input(sink)
    }
    fn stop_input(&mut self) {
        self.0.lock().unwrap().stop_input();
    }
    fn play(&mut self, samples: Vec<f32>) -> Result<(), AudioError> {
        self.0.lock().unwrap().play(samples)
    }
    fn sample_rate(&self) -> u32 {
        self.0.lock().unwrap().sample_rate()
    }
}

impl Card {
    fn played(&self) -> usize {
        self.0.lock().unwrap().played().len()
    }
    /// 200 ms of the room at `level` (a steady sample value).
    fn room(&self, level: f32) {
        self.0.lock().unwrap().feed(&vec![level; 9_600]);
    }
    /// Whether the microphone is actually open right now: a sink is
    /// registered with the loopback card, not just the transport's own
    /// `listening` flag.
    fn is_listening(&self) -> bool {
        self.0.lock().unwrap().is_listening()
    }
}

/// A machine on one end of a loopback pair, and the other end, which the
/// test speaks through.
struct Bench {
    shared: Shared,
    card: Card,
    voice: FakeAudio,
    now: u64,
    events: Vec<NodeEvent>,
}

impl Bench {
    fn new(path: &Path) -> Bench {
        let (voice, card) = LoopbackAudio::pair(48_000);
        let card = Card(Arc::new(Mutex::new(card)));
        let device = Device::with_audio(path.to_path_buf(), Box::new(card.clone()));
        Bench {
            shared: Shared::new(Box::new(device)),
            card,
            voice,
            now: 0,
            events: Vec::new(),
        }
    }

    /// `rounds` ticks of 200 ms of the room at `level`.
    fn run(&mut self, rounds: usize, level: f32) {
        for _ in 0..rounds {
            self.card.room(level);
            self.now += 200;
            let ev = commands::tick(&self.shared, self.now);
            self.events.extend(ev);
        }
    }

    /// Say a message across a quiet room, then let the machine hear it
    /// out.
    fn hear(&mut self, from: &Identity, to: &PublicIdentity, text: &str) -> [u8; 16] {
        self.hear_in(0.0, from, to, text)
    }

    /// The same, in a room at `level`.
    fn hear_in(
        &mut self,
        level: f32,
        from: &Identity,
        to: &PublicIdentity,
        text: &str,
    ) -> [u8; 16] {
        self.run(1, level);
        let now = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            / 60) as u32;
        let env = seal_private(from, std::slice::from_ref(to), text, now, 60);
        for frame in split(&env.encode(), 0x0b0e) {
            self.voice
                .play(modulate(&ModemConfig::default(), &frame))
                .unwrap();
            self.run(1, level);
        }
        self.run(3, level);
        env.id
    }

    fn me(&self) -> PublicIdentity {
        let key = commands::machine(&self.shared).unwrap().key;
        let byte = |i: usize| u8::from_str_radix(&key[i * 2..i * 2 + 2], 16).unwrap();
        let mut sign = [0u8; 32];
        let mut dh = [0u8; 32];
        for i in 0..32 {
            sign[i] = byte(i);
            dh[i] = byte(32 + i);
        }
        PublicIdentity { sign, dh }
    }

    fn messages(&self) -> Vec<[u8; 16]> {
        self.events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::Message(m) => Some(m.id),
                _ => None,
            })
            .collect()
    }

    fn chunks(&self) -> usize {
        self.events
            .iter()
            .filter(|e| matches!(e, NodeEvent::ChunkLanded { .. }))
            .count()
    }
}

/// A machine, unlocked, that knows `ana` as a checked contact.
fn bench(dir: &TempDir, ana: &Identity) -> (Bench, ContactId) {
    let mut b = Bench::new(&dir.path().join("bombechat.db"));
    commands::unlock(&b.shared, "correct horse".into(), Some("Ben".into())).unwrap();
    let hex: String = ana
        .public()
        .sign
        .iter()
        .chain(ana.public().dh.iter())
        .map(|x| format!("{x:02x}"))
        .collect();
    let id = commands::confirm_pair(&b.shared, hex, "Ana".into()).unwrap();
    b.run(1, 0.0);
    (b, ContactId(id))
}

#[test]
fn at_listen_and_send_a_message_heard_is_answered_with_a_receipt() {
    let dir = TempDir::new().unwrap();
    let ana = Identity::generate();
    let (mut b, _) = bench(&dir, &ana);
    let me = b.me();
    let id = b.hear(&ana, &me, "Water is at the church.");
    assert_eq!(b.messages(), vec![id]);
    b.run(5, 0.0);
    assert!(b.card.played() > 0, "the receipt played");
}

#[test]
fn at_listen_a_message_is_heard_and_nothing_is_queued_for_the_speaker() {
    let dir = TempDir::new().unwrap();
    let ana = Identity::generate();
    let (mut b, contact) = bench(&dir, &ana);
    commands::set_setting(&b.shared, "sound".into(), 1).unwrap();
    let me = b.me();
    let id = b.hear(&ana, &me, "Water is at the church.");
    assert_eq!(b.messages(), vec![id], "heard");
    b.run(5, 0.0);
    assert_eq!(b.card.played(), 0, "no receipt in a quiet room");

    // A send is refused, and kept as not sent.
    let refused = commands::send(&b.shared, vec![contact.0], "Thanks".into(), "sound".into());
    assert!(refused.is_ok());
    b.run(5, 0.0);
    assert_eq!(b.card.played(), 0);
    assert!(b.events.iter().any(|e| matches!(e,
        NodeEvent::ChannelState { channel: Channel::Sound, state } if state == "not sent: sound is set to listen only")));
    let sheet = commands::conversation(&b.shared, contact.0).unwrap();
    assert_eq!(sheet.last().unwrap()["state"], "failed");
    assert_eq!(
        commands::pair(&b.shared, "sound".into()),
        Err("not sent: sound is set to listen only".to_string())
    );
}

#[test]
fn at_off_nothing_is_demodulated() {
    let dir = TempDir::new().unwrap();
    let ana = Identity::generate();
    let (mut b, _) = bench(&dir, &ana);
    assert!(b.card.is_listening(), "listening by default");
    commands::set_setting(&b.shared, "sound".into(), 0).unwrap();
    assert!(!b.card.is_listening(), "off really closes the microphone");
    let me = b.me();
    b.hear(&ana, &me, "Water is at the church.");
    assert!(b.messages().is_empty());
    assert_eq!(b.chunks(), 0);
    assert_eq!(b.card.played(), 0);
    assert!(
        b.events.iter().any(
            |e| matches!(e, NodeEvent::ChannelState { channel: Channel::Sound, state } if state == "off")
        ),
        "the channel reports off, not a stale listening"
    );
}

#[test]
fn the_sound_switch_and_send_under_are_kept_for_the_next_launch() {
    let dir = TempDir::new().unwrap();
    let ana = Identity::generate();
    let (b, _) = bench(&dir, &ana);
    commands::set_setting(&b.shared, "sound".into(), 1).unwrap();
    commands::set_setting(&b.shared, "quiet".into(), 4).unwrap();
    drop(b);

    let mut b = Bench::new(&dir.path().join("bombechat.db"));
    commands::unlock(&b.shared, "correct horse".into(), None).unwrap();
    let me = b.me();
    let id = b.hear(&ana, &me, "Still listening only.");
    assert_eq!(b.messages(), vec![id]);
    b.run(5, 0.0);
    assert_eq!(b.card.played(), 0, "still at Listen after a restart");
}

#[test]
fn send_under_decides_when_the_receipt_plays() {
    let dir = TempDir::new().unwrap();
    let ana = Identity::generate();
    let (mut b, _) = bench(&dir, &ana);
    // The room murmurs at -60 dBFS: about 34 dB.
    let murmur = 0.001;
    commands::set_setting(&b.shared, "quiet".into(), 0).unwrap(); // 30 dB
    let me = b.me();
    b.hear_in(murmur, &ana, &me, "Bring the radio.");
    b.run(5, murmur);
    assert_eq!(b.card.played(), 0, "34 dB is not under 30");
    commands::set_setting(&b.shared, "quiet".into(), 2).unwrap(); // 40 dB
    b.run(5, murmur);
    assert!(b.card.played() > 0, "34 dB is under 40");
}

#[test]
fn a_message_heard_while_locked_shows_once_after_unlock() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    let ana = Identity::generate();
    let (b, contact) = bench(&dir, &ana);
    let me = b.me();
    drop(b);

    let mut b = Bench::new(&path);
    let id = b.hear(&ana, &me, "Heard while locked.");
    assert!(b.messages().is_empty(), "nothing opens while locked");
    commands::unlock(&b.shared, "correct horse".into(), None).unwrap();
    b.run(3, 0.0);
    let sheet = commands::conversation(&b.shared, contact.0).unwrap();
    let id_json = serde_json::json!(id.to_vec());
    assert_eq!(sheet.iter().filter(|m| m["id"] == id_json).count(), 1);
    assert_eq!(b.messages(), vec![id], "one message event");
    b.run(3, 0.0);
    assert_eq!(b.messages(), vec![id], "and only one");
}

#[test]
fn a_microphone_too_slow_for_near_silent_falls_back_to_the_audible_band() {
    let dir = TempDir::new().unwrap();
    let d = Device::with_audio(
        dir.path().join("bombechat.db"),
        Box::new(FakeAudio::new(16_000)),
    );
    let audible = ModemConfig {
        sample_rate: 16_000,
        band: Band::Audible,
        ..ModemConfig::default()
    };
    use bombechat_app::commands::Machine;
    assert_eq!(d.rates(), vec![(Channel::Sound, measured_rate(&audible))]);
}

#[test]
fn a_microphone_too_slow_for_either_band_is_reported_unavailable() {
    let dir = TempDir::new().unwrap();
    let d = Device::with_audio(
        dir.path().join("bombechat.db"),
        Box::new(FakeAudio::new(8_000)),
    );
    let s = Shared::new(Box::new(d));
    commands::tick(&s, 20);
    let kept = commands::channels(&s);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (kept[0].channel, kept[0].state.as_str()),
        ("sound", "unavailable: microphone sample rate too low")
    );
}

impl Bench {
    /// Say the first `frames` frames of a message on `cfg`'s band.
    fn say(&mut self, cfg: &ModemConfig, env_bytes: &[u8], tag: u16, frames: usize) {
        for frame in split(env_bytes, tag).into_iter().take(frames) {
            self.voice.play(modulate(cfg, &frame)).unwrap();
            self.run(1, 0.0);
        }
    }

    fn sound_states(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::ChannelState {
                    channel: Channel::Sound,
                    state,
                } => Some(state.clone()),
                _ => None,
            })
            .collect()
    }
}

fn now_min() -> u32 {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 60) as u32
}

#[test]
fn a_tone_band_change_mid_arrival_fails_it_cleanly_and_the_new_band_works() {
    let dir = TempDir::new().unwrap();
    let ana = Identity::generate();
    let (mut b, _) = bench(&dir, &ana);
    let me = b.me();
    let near = ModemConfig::default();
    let audible = ModemConfig {
        band: Band::Audible,
        ..ModemConfig::default()
    };

    let cut_off = seal_private(
        &ana,
        std::slice::from_ref(&me),
        &"long ".repeat(40),
        now_min(),
        60,
    );
    b.say(&near, &cut_off.encode(), 0x0c01, 2);
    assert!(b.sound_states().contains(&"receiving".to_string()));
    b.events.clear();

    commands::set_setting(&b.shared, "band".into(), 1).unwrap();
    b.run(2, 0.0);
    assert_eq!(
        b.sound_states(),
        vec!["failed", "listening"],
        "did not check"
    );
    assert!(b.messages().is_empty());
    assert_eq!(commands::channels(&b.shared)[0].band, Some("audible"));

    // The rest of the old message, still near silent, is not heard at all.
    let rest = split(&cut_off.encode(), 0x0c01).len();
    b.say(&near, &cut_off.encode(), 0x0c01, rest);
    b.run(3, 0.0);
    assert!(b.messages().is_empty());

    // A message on the audible band is.
    let next = seal_private(
        &ana,
        std::slice::from_ref(&me),
        "On the audible band.",
        now_min(),
        60,
    );
    b.say(&audible, &next.encode(), 0x0c02, usize::MAX);
    b.run(3, 0.0);
    assert_eq!(b.messages(), vec![next.id]);
}

#[test]
fn a_card_too_slow_for_near_silent_shows_the_band_it_is_really_on() {
    let dir = TempDir::new().unwrap();
    let d = Device::with_audio(
        dir.path().join("bombechat.db"),
        Box::new(FakeAudio::new(32_000)),
    );
    let s = Shared::new(Box::new(d));
    assert_eq!(commands::settings(&s)["band"], 0, "near silent asked for");
    let view = serde_json::to_value(commands::channels(&s)).unwrap();
    assert_eq!(view[0]["band"], "audible", "audible is what it is on");
}

/// The machine on a sound card whose microphone hears its own speaker,
/// tested through the commands the page uses: what the page is told, and
/// that nothing of it is kept.
#[test]
fn the_machine_tests_itself_through_the_commands_and_keeps_nothing() {
    let dir = TempDir::new().unwrap();
    let card = Card(Arc::new(Mutex::new(FakeAudio::looped(48_000))));
    let device = Device::with_audio(dir.path().join("bombechat.db"), Box::new(card.clone()));
    let shared = Shared::new(Box::new(device));
    assert_eq!(
        commands::self_test(&shared, "sound".into()),
        Err("locked".to_string()),
        "nothing to seal with before the store is open"
    );
    commands::unlock(&shared, "correct horse".into(), Some("Ben".into())).unwrap();

    // refused at Listen, as pairing is, with the switch untouched
    commands::set_setting(&shared, "sound".into(), 1).unwrap();
    assert_eq!(
        commands::self_test(&shared, "sound".into()),
        Err("not sent: sound is set to listen only".to_string())
    );
    commands::set_setting(&shared, "sound".into(), 2).unwrap();

    commands::self_test(&shared, "sound".into()).unwrap();
    let mut states = Vec::new();
    let mut chunks = 0;
    let mut now = 0;
    for _ in 0..400 {
        card.room(0.0);
        now += 200;
        for e in commands::tick(&shared, now) {
            let (name, payload) = commands::bridge_event(&e);
            match name {
                "selfTest" => states.push(payload),
                "chunk" => chunks += 1,
                "message" | "outbox" | "heard" => panic!("the test reached the page as {name}"),
                _ => {}
            }
        }
        if states.iter().any(|s| s["state"] == "heard") {
            break;
        }
    }
    let words: Vec<&str> = states
        .iter()
        .map(|s| s["state"].as_str().unwrap())
        .collect();
    assert_eq!(words, vec!["waiting", "playing", "heard"]);
    let heard = states.last().unwrap();
    assert_eq!(heard["text"], "The machine hears itself.");
    assert_eq!(heard["band"], "near silent");
    assert!(heard["rate"].as_f64().unwrap() > 0.0);
    assert!(chunks > 1, "the drums turned");
    assert!(states.iter().all(|s| s["tag"] == states[0]["tag"]));
    assert!(commands::contacts(&shared).unwrap().is_empty());
    assert!(commands::broadcasts(&shared).unwrap().is_empty());

    // and in the store itself, opened again beside the machine: no message
    // row of any kind (received, sent, waiting in the outbox, or one a
    // receipt was kept on) and no broadcast row
    let store =
        bombechat_store::Store::open(&dir.path().join("bombechat.db"), "correct horse").unwrap();
    assert_eq!(store.counts(), (0, 0));
    assert!(store.waiting().is_empty() && store.waiting_broadcasts().is_empty());
}
