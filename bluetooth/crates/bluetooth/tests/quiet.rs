//! Whatever becomes of a request, the requester sees what an ignored one
//! shows (Task 4, fix round 1, C1). A raw requester asks a real node; the
//! node or its transport decides 50 ms after the hello arrives (declined
//! by the person, dropped by a gap, turned down unseen); and everything the
//! requester can observe is compared with the same scene where the request
//! is simply ignored: every write the node makes on the link, with its
//! kind, length and moment, from the hello to five seconds after, and
//! whether a message sent just before and just after the request's time is
//! up is answered.
//!
//! Identities and the transport's randomness are seeded, and the
//! transport's clock is a fixed moment: no test reads a clock, except the
//! node itself, which seals and checks hellos by the wall clock (a fixed
//! moment in the past keeps those current).

use std::collections::HashMap;

use bombechat_bluetooth::wire::{fragment, Kind, LinkMessage, Reassembly, FROM_INITIATOR};
use bombechat_bluetooth::{
    bucket, Advert, BleTransport, BluetoothSwitch, FakeRadio, LinkId, Mode, Radio, RadioEvent,
    Room, SeededRng, REQUEST_MS,
};
use bombechat_core::Identity;
use bombechat_node::{pairing_hello, Node, NodeEvent};
use bombechat_session::{Session, SessionKeys, Transport as Cipher};
use bombechat_store::Store;

/// The transport's clock reads this Unix time at zero: a fixed moment in
/// the past, so hellos the node seals by the wall clock are never stale
/// against it.
const BASE: u64 = 1_700_000_000_000;
const STEP: u64 = 50;
/// Far ahead, so nothing these tests seal ever expires.
const SENT_MIN: u32 = 4_000_000_000;

fn identity(seed: u64) -> Identity {
    let mut bytes = [0u8; 64];
    let mut rng = SeededRng::new(seed ^ 0x51);
    rand_core::RngCore::fill_bytes(&mut rng, &mut bytes);
    Identity::from_secret_bytes(&bytes)
}

/// A radio that connects to the node's advert and shows none.
struct Raw {
    radio: FakeRadio,
    link: Option<LinkId>,
    assembly: HashMap<LinkId, Reassembly>,
    got: Vec<(LinkId, LinkMessage)>,
    next: u16,
}

impl Raw {
    fn new(room: &Room) -> Raw {
        let mut radio = room.radio();
        radio.start(Mode::Open);
        Raw {
            radio,
            link: None,
            assembly: HashMap::new(),
            got: Vec::new(),
            next: 0,
        }
    }

    fn poll(&mut self) {
        for e in self.radio.poll() {
            match e {
                RadioEvent::Up {
                    link,
                    advert: Advert::Token(_),
                } => self.link = Some(link),
                RadioEvent::Bytes(link, b) => {
                    let r = self.assembly.entry(link).or_default();
                    if let Some(m) = r.push(&b, 0).and_then(|m| LinkMessage::decode(&m)) {
                        self.got.push((link, m));
                    }
                }
                _ => {}
            }
        }
    }

    fn write(&mut self, kind: Kind, body: Vec<u8>) {
        let link = self.link.expect("linked");
        self.next = self.next.wrapping_add(1);
        let m = LinkMessage {
            kind,
            flags: FROM_INITIATOR,
            body,
        };
        for f in fragment(&m.encode(), self.next, self.radio.max_write(link)) {
            self.radio.write(link, &f);
        }
    }

    /// What arrived on its link since the last call.
    fn take(&mut self) -> Vec<LinkMessage> {
        let link = self.link;
        std::mem::take(&mut self.got)
            .into_iter()
            .filter(|(l, _)| Some(*l) == link)
            .map(|(_, m)| m)
            .collect()
    }
}

/// The node asked, and how it was set up.
struct Scene {
    room: Room,
    node: Node,
    now: u64,
    _dir: tempfile::TempDir,
}

impl Scene {
    /// A node in Open mode; locked if `locked`; with `contacts` stored.
    fn new(locked: bool, contacts: &[&Identity]) -> Scene {
        let room = Room::new(5);
        let dir = tempfile::TempDir::new().unwrap();
        let me = identity(1);
        let store = Store::create(&dir.path().join("s.db"), "pass", &me, "Cy").unwrap();
        for c in contacts {
            store.add_contact("known", &c.public(), false).unwrap();
        }
        let ble = BleTransport::with_rng(room.radio(), BASE, Box::new(SeededRng::new(7)));
        let h = ble.handle();
        let mut node = if locked {
            drop(store);
            // the node is locked, but its radio has keys: the case where a
            // request reaches a node that can't open it
            h.set_identity(Some(SessionKeys::new(&me)));
            Node::new(None, vec![Box::new(ble)])
        } else {
            Node::new(Some(store), vec![Box::new(ble)])
        };
        node.set_in_person_only(false);
        h.set_switch(BluetoothSwitch::Open);
        Scene {
            room,
            node,
            now: 0,
            _dir: dir,
        }
    }

    fn tick(&mut self) -> Vec<NodeEvent> {
        let e = self.node.tick(self.now);
        self.now += STEP;
        e
    }

    /// A raw requester beside the node, linked.
    fn raw(&mut self) -> Raw {
        let mut r = Raw::new(&self.room);
        self.tick();
        r.poll();
        assert!(r.link.is_some());
        r
    }
}

/// A request in flight, from the raw side.
struct Asked {
    cipher: Cipher,
    xid: u32,
    nonce: u64,
}

fn with_id(xid: u32, rest: &[u8]) -> Vec<u8> {
    let mut b = xid.to_be_bytes().to_vec();
    b.extend_from_slice(rest);
    b
}

/// The raw requester asks with `keys`, sending `hello`, and the node is
/// ticked until the hello has arrived (the tick that hands it over).
/// Returns the request in flight and that tick's node events.
fn ask(
    s: &mut Scene,
    raw: &mut Raw,
    keys: &SessionKeys,
    xid: u32,
    hello: &[u8],
) -> (Asked, Vec<NodeEvent>) {
    let mut hs = Session::request_initiator(keys);
    raw.write(Kind::Request, with_id(xid, &hs.write_message().unwrap()));
    s.tick();
    raw.poll();
    let reply = raw
        .take()
        .into_iter()
        .find(|m| m.kind == Kind::Handshake)
        .expect("the request is answered");
    hs.read_message(&reply.body[4..]).unwrap();
    raw.write(Kind::Handshake, with_id(xid, &hs.write_message().unwrap()));
    let cipher = hs.into_transport().unwrap();
    let mut asked = Asked {
        cipher,
        xid,
        nonce: 0,
    };
    // envelope type 1, ack 0, seq 0, length, body, padded
    let mut plain = vec![1u8, 0, 0, 0, 0, 0, 0, 0, 0];
    plain.extend_from_slice(&(hello.len() as u16).to_be_bytes());
    plain.extend_from_slice(hello);
    plain.resize(bucket(plain.len()), 0);
    seal(raw, &mut asked, &plain);
    let events = s.tick();
    (asked, events)
}

fn seal(raw: &mut Raw, a: &mut Asked, plain: &[u8]) {
    let mut body = with_id(a.xid, &a.nonce.to_be_bytes());
    body.extend_from_slice(&a.cipher.encrypt(a.nonce, plain).unwrap());
    a.nonce += 1;
    raw.write(Kind::Sealed, body);
}

/// A keepalive from the requester: it draws an acknowledgement while the
/// request is held.
fn keepalive(raw: &mut Raw, a: &mut Asked) {
    let mut plain = vec![0u8; 11];
    plain.resize(512, 0);
    seal(raw, a, &plain);
}

/// Everything the requester can observe: the node's writes on the link
/// (moment after the hello, kind, length) for five seconds, then whether
/// a keepalive is answered just before and just after the request's time
/// is up.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    early: Vec<(u64, Kind, usize)>,
    before_end: Vec<(u64, Kind, usize)>,
    after_end: Vec<(u64, Kind, usize)>,
}

fn watch(s: &mut Scene, raw: &mut Raw, from: u64, ms: u64) -> Vec<(u64, Kind, usize)> {
    let mut seen = Vec::new();
    let end = s.now + ms;
    while s.now < end {
        let at = s.now - from;
        s.tick();
        raw.poll();
        for m in raw.take() {
            seen.push((at, m.kind, m.body.len()));
        }
    }
    seen
}

/// Run the rest of a request: watch, wait out its time, probe.
fn observe(s: &mut Scene, raw: &mut Raw, mut asked: Asked, hello_at: u64) -> Seen {
    let early = watch(s, raw, hello_at, 5_000);
    while s.now < hello_at + REQUEST_MS - 5_000 {
        s.now += 1_000 - STEP;
        s.tick();
        raw.poll();
        raw.take();
    }
    keepalive(raw, &mut asked);
    let before_end = watch(s, raw, s.now, 1_000);
    while s.now < hello_at + REQUEST_MS + 5_000 {
        s.now += 1_000 - STEP;
        s.tick();
        raw.poll();
        raw.take();
    }
    keepalive(raw, &mut asked);
    let after_end = watch(s, raw, s.now, 1_000);
    Seen {
        early,
        before_end,
        after_end,
    }
}

fn request_id(events: &[NodeEvent]) -> Option<u64> {
    events.iter().find_map(|e| match e {
        NodeEvent::Request { id, .. } => Some(*id),
        _ => None,
    })
}

/// The requester's identity, and its own hello.
fn asker() -> (SessionKeys, Vec<u8>) {
    let who = identity(50);
    (
        SessionKeys::new(&who),
        pairing_hello(&who, "Ann", SENT_MIN).encode(),
    )
}

/// What the requester sees when the node shows the request and nobody
/// answers it, in a fresh scene.
fn ignored() -> Seen {
    let mut s = Scene::new(false, &[]);
    let mut raw = s.raw();
    let (keys, hello) = asker();
    let (asked, events) = ask(&mut s, &mut raw, &keys, 1, &hello);
    assert!(request_id(&events).is_some(), "shown");
    let at = s.now;
    observe(&mut s, &mut raw, asked, at)
}

/// The baseline must itself show what an ignored request shows: its hello
/// acknowledged, and held to the end.
fn sane(seen: &Seen) {
    assert!(
        seen.early.iter().any(|(_, k, _)| *k == Kind::Sealed),
        "the hello is acknowledged: {seen:?}"
    );
    assert!(!seen.before_end.is_empty(), "held until its time is up");
    assert!(seen.after_end.is_empty(), "and no longer");
}

#[test]
fn declined_by_the_person_50_ms_after_the_hello() {
    let base = ignored();
    sane(&base);
    let mut s = Scene::new(false, &[]);
    let mut raw = s.raw();
    let (keys, hello) = asker();
    let (asked, events) = ask(&mut s, &mut raw, &keys, 1, &hello);
    let at = s.now;
    s.node.decline_request(request_id(&events).expect("shown"));
    assert_eq!(observe(&mut s, &mut raw, asked, at), base);
}

#[test]
fn a_hello_not_the_askers_own_is_turned_down_unseen_and_looks_ignored() {
    let base = ignored();
    let mut s = Scene::new(false, &[]);
    let mut raw = s.raw();
    let (keys, _) = asker();
    let someone = pairing_hello(&identity(51), "Not Ann", SENT_MIN).encode();
    let (asked, events) = ask(&mut s, &mut raw, &keys, 1, &someone);
    assert!(request_id(&events).is_none(), "not shown");
    let at = s.now;
    assert_eq!(observe(&mut s, &mut raw, asked, at), base);
}

#[test]
fn a_request_from_someone_already_a_contact_looks_ignored() {
    let base = ignored();
    let mut s = Scene::new(false, &[&identity(50)]);
    let mut raw = s.raw();
    let (keys, hello) = asker();
    let (asked, events) = ask(&mut s, &mut raw, &keys, 1, &hello);
    assert!(request_id(&events).is_none(), "not shown");
    let at = s.now;
    assert_eq!(observe(&mut s, &mut raw, asked, at), base);
}

#[test]
fn a_request_to_a_locked_machine_looks_ignored() {
    let base = ignored();
    let mut s = Scene::new(true, &[]);
    let mut raw = s.raw();
    let (keys, hello) = asker();
    let (asked, events) = ask(&mut s, &mut raw, &keys, 1, &hello);
    assert!(request_id(&events).is_none(), "not shown");
    let at = s.now;
    assert_eq!(observe(&mut s, &mut raw, asked, at), base);
}

/// The same identity asked a minute earlier on another link: held back by
/// the per-identity gap. Compared with a scene where that earlier request
/// came from someone else.
#[test]
fn a_second_request_from_one_identity_on_another_link_looks_ignored() {
    let run = |same: bool| {
        let mut s = Scene::new(false, &[]);
        let mut first = s.raw();
        let earlier = if same { identity(50) } else { identity(52) };
        let earlier_hello = pairing_hello(&earlier, "Ann", SENT_MIN).encode();
        let (_, events) = ask(
            &mut s,
            &mut first,
            &SessionKeys::new(&earlier),
            9,
            &earlier_hello,
        );
        assert!(request_id(&events).is_some());
        while s.now < 60_000 {
            s.tick();
        }
        let mut raw = s.raw();
        let (keys, hello) = asker();
        let (asked, events) = ask(&mut s, &mut raw, &keys, 1, &hello);
        assert_eq!(request_id(&events).is_some(), !same);
        let at = s.now;
        observe(&mut s, &mut raw, asked, at)
    };
    let base = run(false);
    sane(&base);
    assert_eq!(run(true), base);
}

/// An earlier request on the same link was accepted and broken off (its
/// requester never acknowledged the answer), so the link brought a
/// request within ten minutes: held back by the per-link gap. Compared
/// with a scene where the earlier request came on another link.
#[test]
fn a_second_request_on_one_link_looks_ignored() {
    let run = |same_link: bool| {
        let mut s = Scene::new(false, &[]);
        let mut first = s.raw();
        let mut second = s.raw();
        let earlier = identity(52);
        let earlier_hello = pairing_hello(&earlier, "Bo", SENT_MIN).encode();
        let by = if same_link { &mut second } else { &mut first };
        let (_, events) = ask(&mut s, by, &SessionKeys::new(&earlier), 9, &earlier_hello);
        let id = request_id(&events).expect("shown");
        s.node.accept_request(id, "Bo").unwrap();
        // the answer is never acknowledged: the session stalls and ends
        while s.now < 90_000 {
            s.tick();
        }
        first.poll();
        first.take();
        second.poll();
        second.take();
        let (keys, hello) = asker();
        let (asked, events) = ask(&mut s, &mut second, &keys, 1, &hello);
        assert_eq!(request_id(&events).is_some(), !same_link);
        let at = s.now;
        observe(&mut s, &mut second, asked, at)
    };
    let base = run(false);
    sane(&base);
    assert_eq!(run(true), base);
}
