//! The Bluetooth transport inside a real node (Review Focus 4): the same
//! contact in range by Bluetooth and by sound at once is one message and
//! one receipt, whichever arrives first; and a message sent while both are
//! up goes out once per channel at most.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use bombechat_bluetooth::{BleTransport, FakeRadio, Mode, Room, SeededRng};
use bombechat_core::{seal_private, Envelope, Identity, Kind};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_node::{Node, NodeEvent, Transport, TransportEvent};
use bombechat_session::SessionKeys;
use bombechat_store::{Channel, Store};

const BASE: u64 = 1_800_000_000_000;
/// Far ahead (about 7,600 years), so the envelope never expires and this
/// test reads no clock. This relies on the node taking envelopes dated
/// after its own clock: it only refuses ones that have expired
/// (`expires_min` older than its clock less the skew allowance). If the
/// node ever refuses envelopes from the future, seal at a fixed time
/// instead and give the node that clock.
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

/// A transport passed through, counting what crosses it: the frames the
/// node gave it, and the frames it handed the node.
struct Counted<T: Transport> {
    inner: T,
    given: Arc<Mutex<Vec<Vec<u8>>>>,
    handed: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl<T: Transport> Transport for Counted<T> {
    fn name(&self) -> Channel {
        self.inner.name()
    }
    fn send(&mut self, frame: Vec<u8>) {
        self.given.lock().unwrap().push(frame.clone());
        self.inner.send(frame);
    }
    fn poll(&mut self, now_ms: u64) -> Vec<TransportEvent> {
        let events = self.inner.poll(now_ms);
        for e in &events {
            if let TransportEvent::Frame(f) = e {
                self.handed.lock().unwrap().push(f.clone());
            }
        }
        events
    }
    fn delivered(&mut self, tag: u16) {
        self.inner.delivered(tag);
    }
    fn withdraw(&mut self, tag: u16) {
        self.inner.withdraw(tag);
    }
    fn set_identity(&mut self, me: Option<&Identity>) {
        self.inner.set_identity(me);
    }
    fn set_contacts(&mut self, contacts: &[bombechat_core::PublicIdentity]) {
        self.inner.set_contacts(contacts);
    }
    fn refusal(&self) -> Option<&'static str> {
        self.inner.refusal()
    }
}

struct Side {
    node: Node,
    sound: Wire,
    given: Arc<Mutex<Vec<Vec<u8>>>>,
    handed: Arc<Mutex<Vec<Vec<u8>>>>,
    events: Vec<NodeEvent>,
    _dir: tempfile::TempDir,
}

fn side(room: &Room, base: u64, seed: u64, me: &Identity, them: &Identity) -> Side {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Store::create(&dir.path().join("s.db"), "pass", me, "me").unwrap();
    store.add_contact("them", &them.public(), true).unwrap();
    let ble = BleTransport::with_rng(room.radio(), base, Box::new(SeededRng::new(seed)));
    ble.handle().set_mode(Some(Mode::Contacts));
    let (given, handed) = (Arc::default(), Arc::default());
    let counted = Counted {
        inner: ble,
        given: Arc::clone(&given),
        handed: Arc::clone(&handed),
    };
    let sound = Wire::default();
    let node = Node::new(
        Some(store),
        vec![Box::new(counted), Box::new(sound.clone())],
    );
    Side {
        node,
        sound,
        given,
        handed,
        events: Vec::new(),
        _dir: dir,
    }
}

fn tick_both(a: &mut Side, b: &mut Side, from: u64, to: u64) -> u64 {
    let mut now = from;
    while now <= to {
        a.events.extend(a.node.tick(now));
        b.events.extend(b.node.tick(now));
        now += 50;
    }
    to
}

/// Review Focus 4, the sending side: with Bluetooth and sound both up
/// between two contacts (who share two links, so two sessions), a message
/// sent by Bluetooth goes to the Bluetooth transport once and to sound not
/// at all, reaches the other node once, and is answered by one receipt.
/// The same envelope then also arriving by sound changes nothing.
#[test]
fn a_message_goes_out_once_per_channel_and_the_receiver_takes_it_once() {
    let base = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let room = Room::new(90);
    let (a_id, b_id) = (Identity::generate(), Identity::generate());
    let mut a = side(&room, base, 1, &a_id, &b_id);
    let mut b = side(&room, base, 2, &b_id, &a_id);
    let t = tick_both(&mut a, &mut b, 0, 2_000);
    let to_b = a.node.contacts()[0].id;

    let id = a.node.send(&[to_b], "once", Channel::Bluetooth);
    let t = tick_both(&mut a, &mut b, t, t + 5_000);

    let given = envelopes(&a.given.lock().unwrap());
    assert_eq!(
        given.iter().filter(|e| e.id == id).count(),
        1,
        "given to Bluetooth once"
    );
    assert!(
        envelopes(&a.sound.sent.lock().unwrap()).is_empty(),
        "nothing by sound"
    );
    let handed = envelopes(&b.handed.lock().unwrap());
    assert_eq!(
        handed.iter().filter(|e| e.id == id).count(),
        1,
        "across the two sessions, b's transport took it once"
    );
    let messages = |s: &Side| {
        s.events
            .iter()
            .filter(|e| matches!(e, NodeEvent::Message(m) if m.text == "once"))
            .count()
    };
    assert_eq!(messages(&b), 1);
    let heard = |s: &Side| {
        s.events
            .iter()
            .filter(|e| matches!(e, NodeEvent::Heard { id: h, .. } if *h == id))
            .count()
    };
    assert_eq!(heard(&a), 1, "one receipt");
    let receipts = envelopes(&b.given.lock().unwrap())
        .into_iter()
        .chain(envelopes(&b.sound.sent.lock().unwrap()))
        .filter(|e| e.kind == Kind::Control)
        .count();
    assert_eq!(receipts, 1, "sent once, by Bluetooth, where it came in");

    // the same envelope by sound as well: still one message, no receipt more
    let env = given.into_iter().find(|e| e.id == id).unwrap();
    b.sound
        .inbox
        .lock()
        .unwrap()
        .extend(split(&env.encode(), 77));
    tick_both(&mut a, &mut b, t, t + 3_000);
    assert_eq!(messages(&b), 1);
    assert_eq!(heard(&a), 1);
    let receipts = envelopes(&b.given.lock().unwrap())
        .into_iter()
        .chain(envelopes(&b.sound.sent.lock().unwrap()))
        .filter(|e| e.kind == Kind::Control)
        .count();
    assert_eq!(receipts, 1);
}
