//! Requests to connect between two real nodes over the fake radio: the
//! switch, asking one stranger by label, accepting with proof, the
//! in-person setting, and who is near.

use bombechat_bluetooth::{
    BleHandle, BleTransport, BluetoothSwitch, Room, SeededRng, BLUETOOTH_SEND_OFF,
};
use bombechat_core::Identity;
use bombechat_node::{Node, NodeEvent, OutboxState, RequestState, MEET_FIRST, NOT_IN_RANGE};
use bombechat_store::{Channel, Store};

/// The transport's clock reads this Unix time at zero: a fixed moment in
/// the past, so the hellos and messages the node seals by the wall clock
/// are never stale against it. No test here reads a clock; the node does.
const BASE: u64 = 1_700_000_000_000;
/// An identity drawn from `seed`: the same every run.
fn identity(seed: u64) -> Identity {
    let mut bytes = [0u8; 64];
    let mut rng = SeededRng::new(seed ^ 0x2B);
    rand_core::RngCore::fill_bytes(&mut rng, &mut bytes);
    Identity::from_secret_bytes(&bytes)
}

struct Peer {
    node: Node,
    h: BleHandle,
    events: Vec<NodeEvent>,
    _dir: tempfile::TempDir,
}

impl Peer {
    fn new(room: &Room, base: u64, seed: u64, name: &str) -> Peer {
        let dir = tempfile::TempDir::new().unwrap();
        let id = identity(seed);
        let store = Store::create(&dir.path().join("s.db"), "pass", &id, name).unwrap();
        let ble = BleTransport::with_rng(room.radio(), base, Box::new(SeededRng::new(seed)));
        let h = ble.handle();
        let mut node = Node::new(Some(store), vec![Box::new(ble)]);
        node.set_in_person_only(false);
        Peer {
            node,
            h,
            events: Vec::new(),
            _dir: dir,
        }
    }

    fn requests(&self) -> Vec<(u64, String)> {
        self.events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::Request { id, name, .. } => Some((*id, name.clone())),
                _ => None,
            })
            .collect()
    }

    fn requested(&self) -> Vec<RequestState> {
        self.events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::Requested { state, .. } => Some(*state),
                _ => None,
            })
            .collect()
    }

    fn last_peers(&self) -> Option<(Vec<bombechat_store::ContactId>, usize)> {
        self.events.iter().rev().find_map(|e| match e {
            NodeEvent::Peers {
                contacts_in_range,
                strangers_in_range,
            } => Some((contacts_in_range.clone(), *strangers_in_range)),
            _ => None,
        })
    }
}

fn run(peers: &mut [&mut Peer], from: u64, to: u64) -> u64 {
    let mut now = from;
    while now <= to {
        for p in peers.iter_mut() {
            let e = p.node.tick(now);
            p.events.extend(e);
        }
        now += 50;
    }
    to
}

fn open_pair(room: &Room) -> (Peer, Peer, u64) {
    let base = BASE;
    let (mut a, mut b) = (
        Peer::new(room, base, 1, "Ann"),
        Peer::new(room, base, 2, "Ben"),
    );
    a.h.set_switch(BluetoothSwitch::Open);
    b.h.set_switch(BluetoothSwitch::Open);
    let t = run(&mut [&mut a, &mut b], 0, 1_000);
    (a, b, t)
}

#[test]
fn the_switch_starts_off_and_maps_onto_the_modes() {
    use bombechat_bluetooth::Mode;
    assert_eq!(BluetoothSwitch::default(), BluetoothSwitch::Off);
    assert_eq!(BluetoothSwitch::Off.mode(), None);
    assert_eq!(BluetoothSwitch::Contacts.mode(), Some(Mode::Contacts));
    assert_eq!(BluetoothSwitch::Open.mode(), Some(Mode::Open));
    for (i, s) in [
        BluetoothSwitch::Off,
        BluetoothSwitch::Contacts,
        BluetoothSwitch::Open,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(BluetoothSwitch::from_index(i), Some(s));
    }
    assert_eq!(BluetoothSwitch::from_index(3), None);

    // at Off, a send by Bluetooth is refused in so many words
    let dir = tempfile::TempDir::new().unwrap();
    let (me, ben) = (identity(1), identity(2));
    let store = Store::create(&dir.path().join("s.db"), "pass", &me, "Ann").unwrap();
    let ben = store.add_contact("Ben", &ben.public(), true).unwrap();
    let room = Room::new(1);
    let ble = BleTransport::with_rng(room.radio(), BASE, Box::new(SeededRng::new(1)));
    let h = ble.handle();
    let mut node = Node::new(Some(store), vec![Box::new(ble)]);
    node.send(&[ben], "hi", Channel::Bluetooth);
    let events = node.tick(0);
    assert!(events.iter().any(|e| matches!(
        e,
        NodeEvent::ChannelState { channel: Channel::Bluetooth, state } if state == BLUETOOTH_SEND_OFF
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        NodeEvent::Outbox {
            state: OutboxState::Failed,
            ..
        }
    )));
    // the switch as set counts at once, before the transport's next poll
    h.set_switch(BluetoothSwitch::Contacts);
    node.send(&[ben], "now", Channel::Bluetooth);
    let events = node.tick(50);
    assert!(!events.iter().any(
        |e| matches!(e, NodeEvent::ChannelState { state, .. } if state == BLUETOOTH_SEND_OFF)
    ));
}

#[test]
fn one_stranger_is_asked_by_label_and_an_accept_makes_both_contacts() {
    let room = Room::new(10);
    let (mut a, mut b, t) = open_pair(&room);
    let labels = a.node.strangers();
    assert_eq!(labels.len(), 1, "Ben, by a random label");
    assert_eq!(a.last_peers(), Some((vec![], 1)));
    a.node.request(&labels[0]).unwrap();
    let t = run(&mut [&mut a, &mut b], t, t + 2_000);
    let asked = b.requests();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].1, "Ann");
    assert_eq!(a.requested(), vec![RequestState::Delivered]);
    assert!(
        b.node.contacts().is_empty(),
        "nothing stored until accepted"
    );

    let ann = b
        .node
        .accept_request(asked[0].0, "Ann from the bus")
        .unwrap();
    let t = run(&mut [&mut a, &mut b], t, t + 10_000);
    let bens = a.node.contacts();
    assert_eq!(bens.len(), 1);
    assert_eq!((bens[0].name.as_str(), bens[0].checked), ("Ben", false));
    assert_eq!(
        a.requested(),
        vec![RequestState::Delivered, RequestState::Accepted(bens[0].id)]
    );
    let anns = b.node.contacts();
    assert_eq!(
        (anns[0].id, anns[0].name.as_str(), anns[0].checked),
        (ann, "Ann from the bus", false)
    );
    assert_eq!(a.last_peers().unwrap().0, vec![bens[0].id]);
    assert_eq!(b.last_peers().unwrap().0, vec![ann]);
    assert!(a.node.strangers().is_empty(), "Ben is no stranger now");

    // and they talk by Bluetooth
    a.node
        .send(&[bens[0].id], "Road north is open.", Channel::Bluetooth);
    run(&mut [&mut a, &mut b], t, t + 3_000);
    assert!(b.events.iter().any(
        |e| matches!(e, NodeEvent::Message(m) if m.text == "Road north is open." && m.contact == ann)
    ));
    assert!(a
        .events
        .iter()
        .any(|e| matches!(e, NodeEvent::Heard { by, .. } if *by == bens[0].id)));
}

#[test]
fn with_key_checks_required_a_request_is_shown_but_not_accepted() {
    let room = Room::new(11);
    let (mut a, mut b, t) = open_pair(&room);
    b.node.set_in_person_only(true);
    let label = a.node.strangers()[0].clone();
    a.node.request(&label).unwrap();
    let t = run(&mut [&mut a, &mut b], t, t + 2_000);
    let id = b.requests()[0].0;
    assert_eq!(
        b.node.accept_request(id, "Ann"),
        Err("Meet them and compare keys first: this machine only adds people checked in person.")
    );
    run(&mut [&mut a, &mut b], t, t + 10_000);
    assert!(b.node.contacts().is_empty());
    assert!(a.node.contacts().is_empty());
    assert!(
        !b.events
            .iter()
            .any(|e| matches!(e, NodeEvent::RequestGone { .. })),
        "it stays"
    );
    // and such a machine sends none
    a.node.set_in_person_only(true);
    assert_eq!(a.node.request(&label), Err(MEET_FIRST));
}

#[test]
fn in_contacts_mode_no_request_is_shown_or_sent() {
    let room = Room::new(12);
    let base = BASE;
    let (mut a, mut b) = (
        Peer::new(&room, base, 1, "Ann"),
        Peer::new(&room, base, 2, "Ben"),
    );
    a.h.set_switch(BluetoothSwitch::Open);
    b.h.set_switch(BluetoothSwitch::Contacts);
    let t = run(&mut [&mut a, &mut b], 0, 1_000);
    assert!(
        b.node.strangers().is_empty(),
        "none to ask in Contacts mode"
    );
    let label = a.node.strangers()[0].clone();
    a.node.request(&label).unwrap();
    let t = run(&mut [&mut a, &mut b], t, t + 20_000);
    assert!(b.requests().is_empty(), "none shown");
    assert_eq!(a.requested(), vec![RequestState::Failed]);
    a.h.set_switch(BluetoothSwitch::Contacts);
    run(&mut [&mut a, &mut b], t, t + 200);
    assert!(a.node.strangers().is_empty());
    assert_eq!(a.node.request(&label), Err(NOT_IN_RANGE));
}
