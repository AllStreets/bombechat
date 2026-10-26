//! The Bluetooth transport inside a real node (Review Focus 4): the same
//! contact in range by Bluetooth and by sound at once is one message and
//! one receipt, whichever arrives first.

use std::sync::{Arc, Mutex};

use bombechat_bluetooth::{BleTransport, FakeRadio, Mode, Room, SeededRng};
use bombechat_core::{seal_private, Envelope, Identity, Kind};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_node::{Node, NodeEvent, Transport, TransportEvent};
use bombechat_session::SessionKeys;
use bombechat_store::{Channel, Store};

const BASE: u64 = 1_800_000_000_000;
/// Far ahead, so the envelope never expires and no clock is read.
const SENT_MIN: u32 = 4_000_000_000;

/// A sound transport the test plays into by hand, keeping what it sends.
#[derive(Clone, Default)]
struct Wire {
    inbox: Arc<Mutex<Vec<Vec<u8>>>>,
    sent: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Transport for Wire {
    fn name(&self) -> Channel {
        Channel::Sound
    }
    fn send(&mut self, frame: Vec<u8>) {
        self.sent.lock().unwrap().push(frame);
    }
    fn poll(&mut self, _now_ms: u64) -> Vec<TransportEvent> {
        std::mem::take(&mut *self.inbox.lock().unwrap())
            .into_iter()
            .map(TransportEvent::Frame)
            .collect()
    }
}

fn envelopes(frames: &[Vec<u8>]) -> Vec<Envelope> {
    let mut r = Reassembler::new();
    frames
        .iter()
        .flat_map(|f| r.push(f, 0))
        .filter_map(|e| match e {
            Event::Complete { bytes, .. } => Envelope::decode(&bytes).ok(),
            _ => None,
        })
        .collect()
}

struct Scene {
    a: BleTransport<FakeRadio>,
    b: Node,
    sound: Wire,
    now: u64,
    events: Vec<NodeEvent>,
    a_heard: Vec<Vec<u8>>,
    a_sent: bool,
}

impl Scene {
    fn step(&mut self) {
        for e in self.a.poll(self.now) {
            match e {
                TransportEvent::Frame(f) => self.a_heard.push(f),
                TransportEvent::Sent(11) => self.a_sent = true,
                _ => {}
            }
        }
        self.events.extend(self.b.tick(self.now));
        self.now += 50;
    }

    fn run_to(&mut self, until: u64) {
        while self.now < until {
            self.step();
        }
    }
}

/// `sound_first`: the sound copy is heard before the Bluetooth one is
/// sent, or after it has arrived.
fn both_ways(sound_first: bool) {
    let dir = tempfile::TempDir::new().unwrap();
    let (a_id, b_id) = (Identity::generate(), Identity::generate());
    let room = Room::new(70);

    // a: a bare transport, sending
    let a = BleTransport::with_rng(room.radio(), BASE, Box::new(SeededRng::new(1)));
    let ah = a.handle();
    ah.set_identity(Some(SessionKeys::new(&a_id)));
    ah.set_contacts(vec![b_id.public()]);
    ah.set_mode(Some(Mode::Contacts));

    // b: a node with Bluetooth and sound
    let store = Store::create(&dir.path().join("b.db"), "pass", &b_id, "b").unwrap();
    store.add_contact("a", &a_id.public(), true).unwrap();
    let ble = BleTransport::with_rng(room.radio(), BASE, Box::new(SeededRng::new(2)));
    let bh = ble.handle();
    bh.set_identity(Some(SessionKeys::new(&b_id)));
    bh.set_contacts(vec![a_id.public()]);
    bh.set_mode(Some(Mode::Contacts));
    let sound = Wire::default();
    let b = Node::new(Some(store), vec![Box::new(ble), Box::new(sound.clone())]);

    let mut s = Scene {
        a,
        b,
        sound,
        now: 0,
        events: Vec::new(),
        a_heard: Vec::new(),
        a_sent: false,
    };
    s.run_to(2_000);
    assert_eq!(bh.peers().contacts, vec![a_id.public()]);

    let env = seal_private(&a_id, &[b_id.public()], "both ways at once", SENT_MIN, 60);
    let by_sound = split(&env.encode(), 12);
    if sound_first {
        s.sound.inbox.lock().unwrap().extend(by_sound.clone());
        s.run_to(3_000);
    }
    for f in split(&env.encode(), 11) {
        s.a.send(f);
    }
    s.run_to(6_000);
    assert!(s.a_sent, "the Bluetooth copy reached b");
    if !sound_first {
        s.sound.inbox.lock().unwrap().extend(by_sound);
        s.run_to(8_000);
    }

    let messages = s
        .events
        .iter()
        .filter(|e| matches!(e, NodeEvent::Message(m) if m.text == "both ways at once"))
        .count();
    assert_eq!(messages, 1, "one message");
    let receipts = envelopes(&s.sound.sent.lock().unwrap())
        .into_iter()
        .chain(envelopes(&s.a_heard))
        .filter(|e| e.kind == Kind::Control)
        .count();
    assert_eq!(receipts, 1, "one receipt");
}

#[test]
fn sound_then_bluetooth_is_one_message_and_one_receipt() {
    both_ways(true);
}

#[test]
fn bluetooth_then_sound_is_one_message_and_one_receipt() {
    both_ways(false);
}
