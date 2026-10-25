//! The Bluetooth transport inside a real node (Review Focus 4): the same
//! contact in range by Bluetooth and by sound at once is one message and
//! one receipt.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use bombechat_bluetooth::{BleTransport, FakeRadio, Mode, Room};
use bombechat_core::{seal_private, Envelope, Identity, Kind};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_node::{Node, NodeEvent, Transport, TransportEvent};
use bombechat_session::SessionKeys;
use bombechat_store::{Channel, Store};

const BASE: u64 = 1_800_000_000_000;

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

fn now_min() -> u32 {
    (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 60) as u32
}

#[test]
fn the_same_envelope_by_bluetooth_and_by_sound_is_one_message_and_one_receipt() {
    let dir = tempfile::TempDir::new().unwrap();
    let (a_id, b_id) = (Identity::generate(), Identity::generate());
    let room = Room::new(70);

    // a: a bare transport, sending
    let mut a = BleTransport::new(room.radio(), BASE);
    let ah = a.handle();
    ah.set_identity(Some(SessionKeys::new(&a_id)));
    ah.set_contacts(vec![b_id.public()]);
    ah.set_mode(Some(Mode::Contacts));

    // b: a node with Bluetooth and sound
    let store = Store::create(&dir.path().join("b.db"), "pass", &b_id, "b").unwrap();
    store.add_contact("a", &a_id.public(), true).unwrap();
    let ble = BleTransport::new(room.radio(), BASE);
    let bh = ble.handle();
    bh.set_identity(Some(SessionKeys::new(&b_id)));
    bh.set_contacts(vec![a_id.public()]);
    bh.set_mode(Some(Mode::Contacts));
    let sound = Wire::default();
    let mut b = Node::new(Some(store), vec![Box::new(ble), Box::new(sound.clone())]);

    let mut events = Vec::new();
    let mut a_heard = Vec::new();
    let mut a_sent = false;
    let mut now = 0;
    let mut step = |now: &mut u64, a: &mut BleTransport<FakeRadio>, events: &mut Vec<NodeEvent>| {
        for e in a.poll(*now) {
            match e {
                TransportEvent::Frame(f) => a_heard.push(f),
                TransportEvent::Sent(11) => a_sent = true,
                _ => {}
            }
        }
        events.extend(b.tick(*now));
        *now += 50;
    };
    while now < 2_000 {
        step(&mut now, &mut a, &mut events);
    }
    assert_eq!(bh.peers().contacts, vec![a_id.public()]);

    let env = seal_private(&a_id, &[b_id.public()], "both ways at once", now_min(), 60);
    for f in split(&env.encode(), 11) {
        a.send(f);
    }
    sound.inbox.lock().unwrap().extend(split(&env.encode(), 12));
    while now < 6_000 {
        step(&mut now, &mut a, &mut events);
    }

    let messages: Vec<&NodeEvent> = events
        .iter()
        .filter(|e| matches!(e, NodeEvent::Message(m) if m.text == "both ways at once"))
        .collect();
    assert!(a_sent, "the Bluetooth copy reached b too");
    assert_eq!(messages.len(), 1, "one message");
    let receipts = envelopes(&sound.sent.lock().unwrap())
        .into_iter()
        .chain(envelopes(&a_heard))
        .filter(|e| e.kind == Kind::Control)
        .count();
    assert_eq!(receipts, 1, "one receipt");
}
