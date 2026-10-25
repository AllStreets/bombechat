//! The link protocol end to end over the fake radio. Time is whatever the
//! test passes to `poll`: no test reads a clock.

use bombechat_bluetooth::wire::{
    fragment, Kind, LinkMessage, Reassembly, ASSEMBLY_MS, FROM_INITIATOR, MAX_ASSEMBLIES,
    MAX_MESSAGE,
};
use bombechat_bluetooth::{
    BleEvent, BleHandle, BleTransport, FakeRadio, LinkId, Mode, Radio, RadioEvent, Room, HOLD_MS,
    SEEN_CAP, SEEN_MS, STALL_MS, STRANGER_MS, TOKEN_MS, TRIAL_CAP, TRIAL_WINDOW_MS,
};
use bombechat_core::{seal_private, seal_public, Envelope, Identity, PublicIdentity};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_node::{Transport, TransportEvent};
use bombechat_session::{Session, SessionKeys, TOKEN_LEN};
use bombechat_store::Channel;

/// The Unix time at which every test clock reads zero.
const BASE: u64 = 1_800_000_000_000;
const STEP: u64 = 50;

struct Device {
    t: BleTransport<FakeRadio>,
    h: BleHandle,
    id: Identity,
    radio: FakeRadio,
    events: Vec<TransportEvent>,
    ble: Vec<BleEvent>,
}

impl Device {
    fn new(room: &Room) -> Device {
        Device::with(room.radio(), BASE)
    }

    fn with(radio: FakeRadio, base: u64) -> Device {
        let watch = radio.watch();
        let t = BleTransport::new(radio, base);
        let h = t.handle();
        let id = Identity::generate();
        h.set_identity(Some(SessionKeys::new(&id)));
        Device {
            t,
            h,
            id,
            radio: watch,
            events: Vec::new(),
            ble: Vec::new(),
        }
    }

    fn me(&self) -> PublicIdentity {
        self.id.public()
    }

    fn knows(&self, others: &[&Device]) {
        self.h.set_contacts(others.iter().map(|d| d.me()).collect());
    }

    fn send(&mut self, env: &Envelope, tag: u16) {
        for f in split(&env.encode(), tag) {
            self.t.send(f);
        }
    }

    fn poll(&mut self, now: u64) {
        self.events.extend(self.t.poll(now));
        self.ble.extend(self.h.take_events());
    }

    /// Envelopes handed to the node so far, rebuilt from its frames.
    fn heard(&self) -> Vec<Envelope> {
        let mut r = Reassembler::new();
        self.events
            .iter()
            .filter_map(|e| match e {
                TransportEvent::Frame(f) => Some(f),
                _ => None,
            })
            .flat_map(|f| r.push(f, 0))
            .filter_map(|e| match e {
                Event::Complete { bytes, .. } => Envelope::decode(&bytes).ok(),
                _ => None,
            })
            .collect()
    }

    fn has(&self, want: &TransportEvent) -> bool {
        self.events.contains(want)
    }
}

/// Poll every device each step from `from` to `to`.
fn run(devices: &mut [&mut Device], from: u64, to: u64) -> u64 {
    run_by(devices, from, to, STEP)
}

fn run_by(devices: &mut [&mut Device], from: u64, to: u64, step: u64) -> u64 {
    let mut now = from;
    while now <= to {
        for d in devices.iter_mut() {
            d.poll(now);
        }
        now += step;
    }
    to
}

fn private(from: &Device, to: &[&Device], text: &str) -> Envelope {
    let recipients: Vec<PublicIdentity> = to.iter().map(|d| d.me()).collect();
    seal_private(&from.id, &recipients, text, 30_000_000, 60)
}

/// A radio driven by hand: the test writes what it likes and reads what
/// arrives, as whole link messages.
struct Raw {
    radio: FakeRadio,
    ups: Vec<(LinkId, Option<[u8; TOKEN_LEN]>)>,
    downs: Vec<LinkId>,
    got: Vec<(LinkId, LinkMessage)>,
    assembly: std::collections::HashMap<LinkId, Reassembly>,
    next: u16,
}

impl Raw {
    fn new(room: &Room) -> Raw {
        let mut radio = room.radio();
        radio.start(Mode::Open);
        radio.advertise([0xee; TOKEN_LEN]);
        Raw {
            radio,
            ups: Vec::new(),
            downs: Vec::new(),
            got: Vec::new(),
            assembly: Default::default(),
            next: 0,
        }
    }

    fn poll(&mut self) {
        for e in self.radio.poll() {
            match e {
                RadioEvent::Up { link, token } => self.ups.push((link, token)),
                RadioEvent::Down(link) => self.downs.push(link),
                RadioEvent::Bytes(link, b) => {
                    let r = self.assembly.entry(link).or_default();
                    if let Some(m) = r.push(&b, 0).and_then(|m| LinkMessage::decode(&m)) {
                        self.got.push((link, m));
                    }
                }
                RadioEvent::Unavailable(_) => {}
            }
        }
    }

    /// The link this radio opened to a device's advert, and its token.
    fn opened(&self) -> (LinkId, [u8; TOKEN_LEN]) {
        self.ups
            .iter()
            .rev()
            .find_map(|(l, t)| t.map(|t| (*l, t)))
            .expect("a link to an advert")
    }

    fn write(&mut self, link: LinkId, m: &LinkMessage) {
        self.next = self.next.wrapping_add(1);
        for f in fragment(&m.encode(), self.next, self.radio.max_write(link)) {
            self.radio.write(link, &f);
        }
    }

    fn kinds_on(&self, link: LinkId) -> Vec<Kind> {
        self.got
            .iter()
            .filter(|(l, _)| *l == link)
            .map(|(_, m)| m.kind)
            .collect()
    }
}

fn contact_hello(xid: u32, msg1: &[u8]) -> LinkMessage {
    let mut body = xid.to_be_bytes().to_vec();
    body.extend_from_slice(msg1);
    LinkMessage {
        kind: Kind::Contact,
        flags: FROM_INITIATOR,
        body,
    }
}

/// Every link message `radio` wrote, by link.
fn written(radio: &FakeRadio) -> Vec<(LinkId, LinkMessage)> {
    let mut r: std::collections::HashMap<LinkId, Reassembly> = Default::default();
    radio
        .take_written()
        .into_iter()
        .filter_map(|(link, b)| {
            let m = r.entry(link).or_default().push(&b, 0)?;
            Some((link, LinkMessage::decode(&m)?))
        })
        .collect()
}

fn links_between(room: &Room, a: &FakeRadio, b: &FakeRadio) -> Vec<LinkId> {
    room.links()
        .into_iter()
        .filter(|(_, c, p)| {
            (*c == a.index() && *p == b.index()) || (*c == b.index() && *p == a.index())
        })
        .map(|(l, _, _)| l)
        .collect()
}

// ---- envelopes ----------------------------------------------------------

#[test]
fn two_contacts_exchange_envelopes_both_ways() {
    let room = Room::new(1);
    let (mut a, mut b) = (Device::new(&room), Device::new(&room));
    a.knows(&[&b]);
    b.knows(&[&a]);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    let t = run(&mut [&mut a, &mut b], 0, 2_000);

    assert_eq!(a.h.peers().contacts, vec![b.me()]);
    assert_eq!(b.h.peers().contacts, vec![a.me()]);
    assert_eq!(a.h.peers().strangers_in_range(), 0);

    let to_b = private(&a, &[&b], "for b");
    let to_a = private(&b, &[&a], "for a");
    a.send(&to_b, 7);
    b.send(&to_a, 9);
    run(&mut [&mut a, &mut b], t, t + 2_000);

    assert_eq!(b.heard(), vec![to_b]);
    assert_eq!(a.heard(), vec![to_a]);
    assert!(a.has(&TransportEvent::Playing(7)) && a.has(&TransportEvent::Sent(7)));
    assert!(b.has(&TransportEvent::Sent(9)));
    assert_eq!(a.t.name(), Channel::Bluetooth);
}

#[test]
fn small_writes_and_long_envelopes_still_arrive() {
    let room = Room::new(2);
    let mut a = Device::with(room.radio_with_max_write(20), BASE);
    let mut b = Device::new(&room);
    a.knows(&[&b]);
    b.knows(&[&a]);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    let t = run(&mut [&mut a, &mut b], 0, 2_000);
    let long = private(&a, &[&b], &"x".repeat(2000));
    a.send(&long, 1);
    run(&mut [&mut a, &mut b], t, t + 3_000);
    assert_eq!(b.heard(), vec![long]);
}

#[test]
fn a_stranger_in_range_gets_nothing_private_and_no_broadcast() {
    let room = Room::new(3);
    let (mut a, mut b, mut c) = (Device::new(&room), Device::new(&room), Device::new(&room));
    a.knows(&[&b]);
    b.knows(&[&a]);
    // c knows a, but a doesn't know c: c dials a, and hears nothing back
    c.knows(&[&a]);
    for d in [&a, &b, &c] {
        d.h.set_mode(Some(Mode::Open));
    }
    let t = run(&mut [&mut a, &mut b, &mut c], 0, 2_000);
    let secret = private(&a, &[&b], "only for b");
    let public = seal_public(&a.id, "everyone near", 30_000_000, 60);
    a.send(&secret, 1);
    a.send(&public, 2);
    run(&mut [&mut a, &mut b, &mut c], t, t + 3_000);

    assert_eq!(b.heard(), vec![secret.clone(), public.clone()]);
    assert!(c.heard().is_empty(), "the stranger heard {:?}", c.heard());
    assert!(c.h.peers().contacts.is_empty());
    assert_eq!(a.h.peers().contacts, vec![b.me()]);
    assert_eq!(a.h.peers().strangers_in_range(), 1);

    // a wrote nothing but contact trials to c: no answer, no data
    let to_c = links_between(&room, &a.radio, &c.radio);
    for (link, m) in written(&a.radio) {
        if to_c.contains(&link) {
            assert_eq!(m.kind, Kind::Contact, "a sent c a {:?}", m.kind);
        }
    }
    // and nothing c received holds the envelopes' bytes
    let sealed = secret.encode();
    for (link, m) in written(&b.radio) {
        assert!(!to_c.contains(&link) || !contains(&m.body, &sealed[..32]));
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn a_broadcast_goes_to_every_confirmed_contact() {
    let room = Room::new(4);
    let (mut a, mut b, mut c) = (Device::new(&room), Device::new(&room), Device::new(&room));
    a.knows(&[&b, &c]);
    b.knows(&[&a]);
    c.knows(&[&a]);
    for d in [&a, &b, &c] {
        d.h.set_mode(Some(Mode::Contacts));
    }
    let t = run(&mut [&mut a, &mut b, &mut c], 0, 2_000);
    let public = seal_public(&a.id, "to all", 30_000_000, 60);
    a.send(&public, 5);
    run(&mut [&mut a, &mut b, &mut c], t, t + 2_000);
    assert_eq!(b.heard(), vec![public.clone()]);
    assert_eq!(c.heard(), vec![public]);
    assert!(a.has(&TransportEvent::Sent(5)));
}

#[test]
fn an_envelope_waits_for_its_contact_then_goes() {
    let room = Room::new(5);
    let (mut a, mut b) = (Device::new(&room), Device::new(&room));
    a.knows(&[&b]);
    b.knows(&[&a]);
    room.set_in_range(&a.radio, &b.radio, false);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    let t = run(&mut [&mut a, &mut b], 0, 1_000);
    let env = private(&a, &[&b], "later");
    a.send(&env, 3);
    let t = run(&mut [&mut a, &mut b], t, t + 5_000);
    assert!(b.heard().is_empty());
    assert!(!a.has(&TransportEvent::Playing(3)));

    room.set_in_range(&a.radio, &b.radio, true);
    run(&mut [&mut a, &mut b], t, t + 3_000);
    assert_eq!(b.heard(), vec![env]);
    assert!(a.has(&TransportEvent::Sent(3)));
}

#[test]
fn an_envelope_nobody_came_for_is_dropped_after_the_hold() {
    let room = Room::new(6);
    let (mut a, b) = (Device::new(&room), Device::new(&room));
    a.knows(&[&b]);
    a.h.set_mode(Some(Mode::Contacts));
    let t = run(&mut [&mut a], 0, 500);
    a.send(&private(&a, &[&b], "never"), 4);
    run_by(&mut [&mut a], t, t + HOLD_MS + 1_000, 1_000);
    assert!(a.has(&TransportEvent::Dropped(4)));
    assert_eq!(a.t.queued(), 0);
}

// ---- order and loss -----------------------------------------------------

fn lossy(seed: u64, drop: u32, duplicate: u32, reorder: u32) {
    let room = Room::new(seed);
    let (mut a, mut b) = (Device::new(&room), Device::new(&room));
    a.knows(&[&b]);
    b.knows(&[&a]);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    room.set_drop(drop);
    room.set_duplicate(duplicate);
    room.set_reorder(reorder);
    let mut t = run(&mut [&mut a, &mut b], 0, 2_000);
    let sent: Vec<Envelope> = (0..12)
        .map(|i| private(&a, &[&b], &format!("message {i} {}", "y".repeat(300))))
        .collect();
    for (i, env) in sent.iter().enumerate() {
        a.send(env, 100 + i as u16);
        t = run(&mut [&mut a, &mut b], t, t + 200);
    }
    run(&mut [&mut a, &mut b], t, t + 60_000);
    // once each; across a pair's two sessions the order may differ, and
    // the node does not depend on it
    let mut heard: Vec<[u8; 16]> = b.heard().iter().map(|e| e.id).collect();
    let mut want: Vec<[u8; 16]> = sent.iter().map(|e| e.id).collect();
    heard.sort();
    want.sort();
    assert_eq!(heard, want, "every envelope, once each");
    for i in 0..12 {
        assert!(a.has(&TransportEvent::Sent(100 + i)), "sent {i}");
    }
}

#[test]
fn lost_writes_are_sent_again() {
    lossy(10, 200, 0, 0);
}

#[test]
fn repeated_and_reordered_writes_are_delivered_once_in_order() {
    lossy(11, 0, 300, 300);
}

#[test]
fn everything_at_once() {
    lossy(12, 150, 200, 200);
}

#[test]
fn contacts_meet_when_writes_are_lost_from_the_start() {
    let room = Room::new(14);
    room.set_drop(300);
    let (mut a, mut b) = (Device::new(&room), Device::new(&room));
    a.knows(&[&b]);
    b.knows(&[&a]);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    let t = run(&mut [&mut a, &mut b], 0, 60_000);
    assert_eq!(a.h.peers().contacts, vec![b.me()]);
    assert_eq!(b.h.peers().contacts, vec![a.me()]);
    let env = private(&a, &[&b], "despite it all");
    a.send(&env, 2);
    run(&mut [&mut a, &mut b], t, t + 30_000);
    assert_eq!(b.heard(), vec![env]);
}

#[test]
fn a_silent_spell_closes_the_session_and_a_new_one_carries_on() {
    let room = Room::new(13);
    let (mut a, mut b) = (Device::new(&room), Device::new(&room));
    a.knows(&[&b]);
    b.knows(&[&a]);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    let t = run(&mut [&mut a, &mut b], 0, 2_000);
    room.set_drop(1000);
    let env = private(&a, &[&b], "through the gap");
    a.send(&env, 8);
    let t = run(&mut [&mut a, &mut b], t, t + STALL_MS + 5_000);
    assert!(b.heard().is_empty());
    room.set_drop(0);
    run(&mut [&mut a, &mut b], t, t + 20_000);
    assert_eq!(b.heard(), vec![env]);
    assert_eq!(a.h.peers().contacts, vec![b.me()]);
}

// ---- fragments ----------------------------------------------------------

/// A raw radio linked to a device, and the link the device sees.
fn raw_beside(room: &Room) -> (Device, Raw, LinkId) {
    let mut a = Device::new(room);
    a.h.set_mode(Some(Mode::Open));
    let mut raw = Raw::new(room);
    a.poll(0);
    raw.poll();
    let (link, _) = raw.opened();
    (a, raw, link)
}

#[test]
fn endless_fragments_are_bounded_in_count_size_and_time() {
    let room = Room::new(20);
    let (mut a, mut raw, link) = raw_beside(&room);

    // many messages, each never finished
    for id in 0..500u16 {
        let mut f = id.to_be_bytes().to_vec();
        f.extend_from_slice(&0u16.to_be_bytes());
        f.extend_from_slice(&100u16.to_be_bytes());
        f.extend_from_slice(&[7u8; 100]);
        raw.radio.write(link, &f);
    }
    a.poll(STEP);
    let (count, bytes) = a.t.assembling(link);
    assert!(count <= MAX_ASSEMBLIES, "{count} part way");
    assert!(bytes <= MAX_ASSEMBLIES * MAX_MESSAGE);

    // one message that keeps growing past 64 KiB
    for index in 0..1000u16 {
        let mut f = 9000u16.to_be_bytes().to_vec();
        f.extend_from_slice(&index.to_be_bytes());
        f.extend_from_slice(&4000u16.to_be_bytes());
        f.extend_from_slice(&[1u8; 170]);
        raw.radio.write(link, &f);
        if index % 50 == 0 {
            a.poll(2 * STEP);
        }
    }
    a.poll(2 * STEP);
    let (_, bytes) = a.t.assembling(link);
    assert!(bytes <= MAX_ASSEMBLIES * MAX_MESSAGE);

    // and nothing part way outlives its time
    a.poll(2 * STEP + ASSEMBLY_MS);
    assert_eq!(a.t.assembling(link), (0, 0));
}

#[test]
fn random_writes_never_panic_and_stay_bounded() {
    let room = Room::new(21);
    let (mut a, mut raw, link) = raw_beside(&room);
    let mut x: u64 = 0x1234_5678;
    let mut next = || {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    for round in 0..10_000u64 {
        let len = (next() % 182) as usize;
        let mut b: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        if round % 3 == 0 && b.len() >= 6 {
            // a plausible header: small id, index below count
            b[0] = 0;
            b[1] = (next() % 40) as u8;
            b[2] = 0;
            b[3] = (next() % 4) as u8;
            b[4] = 0;
            b[5] = 4;
        }
        raw.radio.write(link, &b);
        if round % 100 == 0 {
            a.poll(round);
            assert!(a.t.assembling(link).0 <= MAX_ASSEMBLIES);
        }
    }
    a.poll(20_000);
    assert!(a
        .events
        .iter()
        .all(|e| !matches!(e, TransportEvent::Frame(_))));
}

#[test]
fn a_link_going_down_mid_message_drops_the_part() {
    let room = Room::new(22);
    let (mut a, mut raw, link) = raw_beside(&room);
    let m = LinkMessage {
        kind: Kind::Request,
        flags: FROM_INITIATOR,
        body: vec![3u8; 1000],
    }
    .encode();
    let frags = fragment(&m, 1, 182);
    for f in &frags[..3] {
        raw.radio.write(link, f);
    }
    a.poll(STEP);
    assert_eq!(a.t.assembling(link).0, 1);
    room.disconnect(link);
    a.poll(2 * STEP);
    assert_eq!(a.t.assembling(link), (0, 0));
    // the rest, arriving on a later link, starts nothing
    raw.radio.stop();
    raw.radio.start(Mode::Open);
    raw.radio.advertise([0xef; TOKEN_LEN]);
    raw.poll();
    a.poll(3 * STEP);
    raw.poll();
    let (again, _) = raw.opened();
    for f in &frags[3..] {
        raw.radio.write(again, f);
    }
    a.poll(4 * STEP);
    assert_eq!(a.t.assembling(again).0, 1);
    assert!(raw.kinds_on(again).iter().all(|k| *k == Kind::Contact));
}

// ---- contact handshakes -------------------------------------------------

/// `raw` acting with `keys`: dial `a` on the link it opened, as a contact
/// would, at Unix time `unix`, naming `token`. Returns what came back.
fn dial(
    raw: &mut Raw,
    a: &mut Device,
    keys: &SessionKeys,
    unix: u64,
    token: Option<[u8; TOKEN_LEN]>,
    now: u64,
) -> Vec<Kind> {
    let (link, advert) = raw.opened();
    let mut hs = Session::contact_initiator(keys, &a.me(), unix, token.unwrap_or(advert));
    let msg1 = hs.write_message().unwrap();
    replay(
        raw,
        a,
        link,
        &contact_hello(now as u32 ^ 0x5a5a, &msg1),
        now,
    )
}

fn replay(raw: &mut Raw, a: &mut Device, link: LinkId, m: &LinkMessage, now: u64) -> Vec<Kind> {
    raw.got.clear();
    raw.write(link, m);
    a.poll(now);
    raw.poll();
    raw.kinds_on(link)
        .into_iter()
        .filter(|k| *k != Kind::Contact)
        .collect()
}

#[test]
fn only_a_contact_with_a_fresh_hello_for_this_device_is_answered() {
    let room = Room::new(30);
    let (mut a, mut raw, _) = raw_beside(&room);
    let friend = Identity::generate();
    let stranger = Identity::generate();
    a.h.set_contacts(vec![friend.public()]);
    let fk = SessionKeys::new(&friend);
    let sk = SessionKeys::new(&stranger);
    let now = 1_000;
    a.poll(now);

    // a stranger: nothing
    assert!(dial(&mut raw, &mut a, &sk, BASE + now, None, now).is_empty());
    assert_eq!(
        a.t.seen_ephemerals(),
        0,
        "a stranger adds nothing to the cache"
    );
    // a contact with the clock off by more than two minutes: nothing
    assert!(dial(&mut raw, &mut a, &fk, BASE + now + 120_001, None, now).is_empty());
    assert!(dial(&mut raw, &mut a, &fk, BASE + now - 120_001, None, now).is_empty());
    // a contact naming another token: nothing
    assert!(dial(&mut raw, &mut a, &fk, BASE + now, Some([1; TOKEN_LEN]), now).is_empty());
    assert_eq!(a.t.seen_ephemerals(), 0);
    // a contact, fresh and for this device: answered
    assert_eq!(
        dial(&mut raw, &mut a, &fk, BASE + now + 119_000, None, now),
        vec![Kind::Handshake]
    );
    assert_eq!(a.t.seen_ephemerals(), 1);
    // unconfirmed: not present yet
    assert!(a.h.peers().contacts.is_empty());
}

#[test]
fn a_replayed_hello_is_never_answered_twice() {
    let room = Room::new(31);
    let (mut a, mut raw, link) = raw_beside(&room);
    let friend = Identity::generate();
    a.h.set_contacts(vec![friend.public()]);
    a.poll(STEP);
    let (_, token) = raw.opened();
    let mut hs = Session::contact_initiator(&SessionKeys::new(&friend), &a.me(), BASE, token);
    let hello = contact_hello(77, &hs.write_message().unwrap());
    assert_eq!(
        replay(&mut raw, &mut a, link, &hello, 100),
        vec![Kind::Handshake]
    );
    // the same bytes again, and under another exchange number
    assert!(replay(&mut raw, &mut a, link, &hello, 200).is_empty());
    let mut other = hello.clone();
    other.body[..4].copy_from_slice(&78u32.to_be_bytes());
    assert!(replay(&mut raw, &mut a, link, &other, 300).is_empty());
    // the replay never shows the contact as near
    a.poll(400);
    assert!(a.h.peers().contacts.is_empty());
    // and the cache forgets after its time, by which the hello is stale
    a.poll(SEEN_MS + 1_000);
    assert_eq!(a.t.seen_ephemerals(), 0);
    assert!(replay(&mut raw, &mut a, link, &other, SEEN_MS + 2_000).is_empty());
}

#[test]
fn the_seen_cache_is_bounded() {
    let room = Room::new(32);
    let (mut a, mut raw, link) = raw_beside(&room);
    let friend = Identity::generate();
    let fk = SessionKeys::new(&friend);
    a.h.set_contacts(vec![friend.public()]);
    a.poll(STEP);
    let (_, token) = raw.opened();
    for i in 0..(SEEN_CAP as u32 + 50) {
        let mut hs = Session::contact_initiator(&fk, &a.me(), BASE + 1_000, token);
        raw.write(link, &contact_hello(i, &hs.write_message().unwrap()));
        if i % 64 == 0 {
            a.poll(1_000);
            raw.radio.poll();
        }
    }
    a.poll(1_000);
    assert_eq!(a.t.seen_ephemerals(), SEEN_CAP);
}

#[test]
fn a_device_never_answers_itself() {
    let room = Room::new(33);
    let (mut a, mut raw, _) = raw_beside(&room);
    // listing itself as a contact changes nothing
    a.h.set_contacts(vec![a.me()]);
    a.poll(STEP);
    let keys = SessionKeys::new(&a.id);
    assert!(dial(&mut raw, &mut a, &keys, BASE + STEP, None, STEP).is_empty());
}

#[test]
fn the_previous_token_is_still_answered_after_rotation() {
    let room = Room::new(34);
    let (mut a, mut raw, _) = raw_beside(&room);
    let friend = Identity::generate();
    let fk = SessionKeys::new(&friend);
    a.h.set_contacts(vec![friend.public()]);
    a.poll(STEP);
    let first = a.radio.token().unwrap();
    a.poll(TOKEN_MS + STEP);
    let second = a.radio.token().unwrap();
    assert_ne!(first, second);
    assert_eq!(a.t.tokens(), vec![second, first]);
    let now = TOKEN_MS + 2 * STEP;
    assert_eq!(
        dial(&mut raw, &mut a, &fk, BASE + now, Some(first), now),
        vec![Kind::Handshake]
    );
    a.poll(2 * TOKEN_MS + STEP);
    let now = 2 * TOKEN_MS + 2 * STEP;
    assert!(dial(&mut raw, &mut a, &fk, BASE + now, Some(first), now).is_empty());
}

#[test]
fn clocks_two_minutes_apart_still_meet_and_three_do_not() {
    for (skew, meets) in [(110_000u64, true), (180_000, false)] {
        let room = Room::new(35);
        let mut a = Device::new(&room);
        let mut b = Device::with(room.radio(), BASE + skew);
        a.knows(&[&b]);
        b.knows(&[&a]);
        a.h.set_mode(Some(Mode::Contacts));
        b.h.set_mode(Some(Mode::Contacts));
        run(&mut [&mut a, &mut b], 0, 15_000);
        assert_eq!(!a.h.peers().contacts.is_empty(), meets, "skew {skew}");
        assert_eq!(!b.h.peers().contacts.is_empty(), meets, "skew {skew}");
    }
}

/// Contact message 1s `radio` wrote on `link`.
fn hellos(radio: &FakeRadio, link: LinkId) -> usize {
    written(radio)
        .iter()
        .filter(|(l, m)| *l == link && m.kind == Kind::Contact)
        .count()
}

#[test]
fn trials_are_capped_per_token_and_window() {
    let room = Room::new(36);
    let mut a = Device::new(&room);
    let others: Vec<Identity> = (0..20).map(|_| Identity::generate()).collect();
    a.h.set_contacts(others.iter().map(|i| i.public()).collect());
    a.h.set_mode(Some(Mode::Contacts));
    let mut raw = Raw::new(&room);
    a.poll(0);
    raw.poll();
    let link = raw.ups[0].0;
    assert_eq!(
        hellos(&a.radio, link),
        TRIAL_CAP,
        "a first round of {TRIAL_CAP}"
    );
    let t = run(&mut [&mut a], STEP, 60_000);
    assert_eq!(
        hellos(&a.radio, link),
        2 * TRIAL_CAP,
        "two repeats each, afresh"
    );
    // reconnecting on the same token buys nothing
    room.disconnect(link);
    raw.radio.stop();
    raw.radio.start(Mode::Contacts);
    raw.radio.advertise([0xee; TOKEN_LEN]);
    let t = run(&mut [&mut a], t, t + 2 * STEP);
    a.radio.take_written();
    run(&mut [&mut a], t, TRIAL_WINDOW_MS - STEP);
    assert!(written(&a.radio)
        .iter()
        .all(|(_, m)| m.kind != Kind::Contact));
    // a new window, the next contacts
    run(&mut [&mut a], TRIAL_WINDOW_MS, TRIAL_WINDOW_MS + STEP);
    let fresh = written(&a.radio)
        .iter()
        .filter(|(_, m)| m.kind == Kind::Contact)
        .count();
    assert_eq!(fresh, TRIAL_CAP);
}

#[test]
fn the_most_recently_seen_contact_is_tried_first() {
    let room = Room::new(37);
    let (mut a, mut b) = (Device::new(&room), Device::new(&room));
    let mut contacts: Vec<PublicIdentity> =
        (0..19).map(|_| Identity::generate().public()).collect();
    contacts.push(b.me());
    a.h.set_contacts(contacts);
    b.knows(&[&a]);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    // they meet: b dials a, a confirms b
    let t = run(&mut [&mut a, &mut b], 0, 3_000);
    assert_eq!(a.h.peers().contacts, vec![b.me()]);
    // b's radio restarts: a sees a new token, and tries b among its first
    b.h.set_mode(None);
    let t = run(&mut [&mut a, &mut b], t, t + 1_000);
    b.h.set_mode(Some(Mode::Contacts));
    a.radio.take_written();
    run(&mut [&mut a, &mut b], t, t + 1_000);
    let opened: Vec<LinkId> = room
        .links()
        .into_iter()
        .filter(|(_, c, _)| *c == a.radio.index())
        .map(|(l, _, _)| l)
        .collect();
    let mine = written(&a.radio);
    let data = mine
        .iter()
        .any(|(l, m)| opened.contains(l) && m.kind == Kind::Data);
    let tried = mine
        .iter()
        .filter(|(l, m)| opened.contains(l) && m.kind == Kind::Contact)
        .count();
    assert!(data, "a reached b on its own link");
    assert!(tried <= TRIAL_CAP);
}

// ---- presence -----------------------------------------------------------

#[test]
fn strangers_are_counted_for_two_minutes_after_they_leave() {
    let room = Room::new(40);
    let (mut a, mut c) = (Device::new(&room), Device::new(&room));
    a.h.set_mode(Some(Mode::Contacts));
    c.h.set_mode(Some(Mode::Contacts));
    let t = run(&mut [&mut a, &mut c], 0, 1_000);
    let peers = a.h.peers();
    assert_eq!(peers.strangers_in_range(), 1, "one stranger, counted once");
    assert!(peers.contacts.is_empty());
    room.set_in_range(&a.radio, &c.radio, false);
    let t = run(&mut [&mut a, &mut c], t, t + STRANGER_MS - 2_000);
    assert_eq!(a.h.peers().strangers_in_range(), 1);
    run(&mut [&mut a, &mut c], t, t + 4_000);
    assert_eq!(a.h.peers().strangers_in_range(), 0);
}

// ---- requests -----------------------------------------------------------

fn hello_of(d: &Device) -> Envelope {
    seal_public(&d.id, "bombechat pair hello v1\nAda", 30_000_000, 60)
}

#[test]
fn a_request_reaches_a_stranger_in_open_mode_who_reveals_nothing() {
    let room = Room::new(50);
    let (mut a, mut c) = (Device::new(&room), Device::new(&room));
    a.h.set_mode(Some(Mode::Open));
    c.h.set_mode(Some(Mode::Open));
    let t = run(&mut [&mut a, &mut c], 0, 1_000);
    let link = a.h.peers().strangers[0];
    let hello = hello_of(&a).encode();
    a.h.request(link, hello.clone());
    c.radio.take_written();
    run(&mut [&mut a, &mut c], t, t + 2_000);

    let got: Vec<&BleEvent> = c
        .ble
        .iter()
        .filter(|e| matches!(e, BleEvent::Request { .. }))
        .collect();
    assert_eq!(got.len(), 1);
    let BleEvent::Request { from, hello: h, .. } = got[0] else {
        unreachable!()
    };
    assert_eq!(*from, a.me());
    assert_eq!(*h, hello);
    assert!(a.ble.contains(&BleEvent::RequestSent { link }));
    // the request is not an envelope for c's node
    assert!(c.heard().is_empty());
    // nothing c wrote holds its keys or certificate
    let me = c.me();
    let cert = SessionKeys::new(&c.id).certificate().to_vec();
    for (_, m) in written(&c.radio) {
        assert!(matches!(
            m.kind,
            Kind::Handshake | Kind::Ack | Kind::Contact
        ));
        for secret in [&me.sign[..], &me.dh[..], &cert[64..]] {
            assert!(!contains(&m.body, secret), "c gave itself away");
        }
    }
}

#[test]
fn requests_are_not_answered_or_sent_in_contacts_mode() {
    let room = Room::new(51);
    let (mut a, mut c) = (Device::new(&room), Device::new(&room));
    a.h.set_mode(Some(Mode::Open));
    c.h.set_mode(Some(Mode::Contacts));
    let t = run(&mut [&mut a, &mut c], 0, 1_000);
    let link = a.h.peers().strangers[0];
    a.h.request(link, hello_of(&a).encode());
    c.radio.take_written();
    let t = run(&mut [&mut a, &mut c], t, t + 20_000);
    assert!(c.ble.is_empty());
    assert!(a.ble.contains(&BleEvent::RequestFailed { link }));
    assert!(written(&c.radio)
        .iter()
        .all(|(_, m)| m.kind == Kind::Contact));

    // and a device in Contacts mode sends none
    a.h.set_mode(Some(Mode::Contacts));
    a.ble.clear();
    let t = run(&mut [&mut a, &mut c], t, t + 500);
    let link = a.h.peers().strangers[0];
    a.h.request(link, hello_of(&a).encode());
    run(&mut [&mut a, &mut c], t, t + 500);
    assert_eq!(a.ble, vec![BleEvent::RequestFailed { link }]);
}

// ---- the switch ---------------------------------------------------------

#[test]
fn off_stops_the_radio_and_drops_what_waits() {
    let room = Room::new(60);
    let (mut a, b) = (Device::new(&room), Device::new(&room));
    a.knows(&[&b]);
    assert!(!a.radio.is_on(), "off until a mode is chosen");
    a.h.set_mode(Some(Mode::Contacts));
    a.poll(0);
    assert!(a.radio.is_on() && a.t.is_running());
    a.send(&private(&a, &[&b], "waiting"), 6);
    a.poll(STEP);
    a.h.set_mode(None);
    a.poll(2 * STEP);
    assert!(!a.radio.is_on() && !a.t.is_running());
    assert!(a.has(&TransportEvent::Dropped(6)));
    assert_eq!(a.radio.token(), None);
    assert_eq!(a.h.peers(), Default::default());

    // locked: no keys, no radio, whatever the mode
    a.h.set_mode(Some(Mode::Open));
    a.h.set_identity(None);
    a.poll(3 * STEP);
    assert!(!a.radio.is_on());
}
