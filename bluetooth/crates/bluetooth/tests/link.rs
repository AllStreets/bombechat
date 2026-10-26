//! The link protocol end to end over the fake radio. Time is whatever the
//! test passes to `poll`, identities and transport randomness are seeded:
//! no test reads a clock, and every run makes the same writes.

use std::collections::HashMap;

use bombechat_bluetooth::wire::{
    fragment, Kind, LinkMessage, Reassembly, ASSEMBLY_MS, FROM_INITIATOR, MAX_ASSEMBLIES,
    MAX_MESSAGE,
};
use bombechat_bluetooth::{
    bucket, BleEvent, BleHandle, BleTransport, FakeRadio, LinkId, Mode, Radio, RadioEvent, Room,
    SeededRng, HOLD_MS, IDLE_MS, MAX_LINKS, QUEUE_CAP, REQUESTS_CAP, REQUEST_MS, SEEN_CAP, SEEN_MS,
    SILENCE_MS, STRANGER_MS, TOKEN_MS, TRIAL_CAP, TRIAL_RETRY_MS,
};
use bombechat_core::{seal_private, seal_public, Envelope, Identity, PublicIdentity};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_node::{Transport, TransportEvent};
use bombechat_session::{Session, SessionKeys, TOKEN_LEN};
use bombechat_store::Channel;

/// The Unix time at which every test clock reads zero.
const BASE: u64 = 1_800_000_000_000;
const STEP: u64 = 50;
/// Far ahead, so nothing these tests seal ever expires.
const SENT_MIN: u32 = 4_000_000_000;

fn identity(seed: u64) -> Identity {
    let mut bytes = [0u8; 64];
    let mut rng = SeededRng::new(seed ^ 0x1D);
    rand_core::RngCore::fill_bytes(&mut rng, &mut bytes);
    Identity::from_secret_bytes(&bytes)
}

struct Device {
    t: BleTransport<FakeRadio>,
    h: BleHandle,
    id: Identity,
    radio: FakeRadio,
    events: Vec<TransportEvent>,
    ble: Vec<BleEvent>,
}

impl Device {
    fn new(room: &Room, seed: u64) -> Device {
        Device::with(room.radio(), BASE, seed)
    }

    fn with(radio: FakeRadio, base: u64, seed: u64) -> Device {
        let watch = radio.watch();
        let t = BleTransport::with_rng(radio, base, Box::new(SeededRng::new(seed)));
        let h = t.handle();
        let id = identity(seed);
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

/// Run until `done` holds, at most to `limit`; the time it first held.
fn run_until(
    devices: &mut [&mut Device],
    from: u64,
    limit: u64,
    mut done: impl FnMut(&[&mut Device]) -> bool,
) -> Option<u64> {
    let mut now = from;
    while now <= limit {
        for d in devices.iter_mut() {
            d.poll(now);
        }
        if done(devices) {
            return Some(now);
        }
        now += STEP;
    }
    None
}

fn private(from: &Device, to: &[&Device], text: &str) -> Envelope {
    let recipients: Vec<PublicIdentity> = to.iter().map(|d| d.me()).collect();
    seal_private(&from.id, &recipients, text, SENT_MIN, 60)
}

fn pair(room: &Room) -> (Device, Device) {
    let (a, b) = (Device::new(room, 1), Device::new(room, 2));
    a.knows(&[&b]);
    b.knows(&[&a]);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    (a, b)
}

/// A radio driven by hand: the test writes what it likes and reads what
/// arrives, as whole link messages.
struct Raw {
    radio: FakeRadio,
    ups: Vec<(LinkId, Option<[u8; TOKEN_LEN]>)>,
    downs: Vec<LinkId>,
    got: Vec<(LinkId, LinkMessage)>,
    assembly: HashMap<LinkId, Reassembly>,
    next: u16,
}

impl Raw {
    fn new(room: &Room) -> Raw {
        Raw::with_token(room, [0xee; TOKEN_LEN])
    }

    fn with_token(room: &Room, token: [u8; TOKEN_LEN]) -> Raw {
        let mut radio = room.radio();
        radio.start(Mode::Open);
        radio.advertise(token);
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

    /// The link the device opened to this radio's advert.
    fn theirs(&self) -> LinkId {
        self.ups
            .iter()
            .rev()
            .find_map(|(l, t)| t.is_none().then_some(*l))
            .expect("a link from the device")
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

/// Every link message `radio` wrote since the last call, by link.
fn written(radio: &FakeRadio) -> Vec<(LinkId, LinkMessage)> {
    let mut r: HashMap<LinkId, Reassembly> = Default::default();
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

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

// ---- envelopes ----------------------------------------------------------

#[test]
fn two_contacts_exchange_envelopes_both_ways() {
    let room = Room::new(1);
    let (mut a, mut b) = pair(&room);
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
    let mut a = Device::with(room.radio_with_max_write(20), BASE, 1);
    let mut b = Device::new(&room, 2);
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
fn transport_messages_hide_their_kind_and_length() {
    let room = Room::new(3);
    let (mut a, mut b) = pair(&room);
    let t = run(&mut [&mut a, &mut b], 0, 2_000);
    a.radio.take_written();
    // two short messages of different lengths, in one bucket
    a.send(&private(&a, &[&b], "hi"), 1);
    a.send(&private(&a, &[&b], "a little longer than that"), 2);
    run(&mut [&mut a, &mut b], t, t + KEEPALIVE_SPAN);
    let sizes = |radio: &FakeRadio| -> std::collections::BTreeSet<usize> {
        written(radio)
            .iter()
            .filter(|(_, m)| m.kind == Kind::Sealed)
            .map(|(_, m)| m.body.len())
            .collect()
    };
    // a's envelopes of different lengths and its keepalives, and b's
    // acknowledgements: one size on the wire
    let (mine, theirs) = (sizes(&a.radio), sizes(&b.radio));
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert_eq!(mine, theirs);
}

const KEEPALIVE_SPAN: u64 = 25_000;

#[test]
fn a_stranger_in_range_gets_nothing_private_and_no_broadcast() {
    let room = Room::new(4);
    let (mut a, mut b, mut c) = (
        Device::new(&room, 1),
        Device::new(&room, 2),
        Device::new(&room, 3),
    );
    a.knows(&[&b]);
    b.knows(&[&a]);
    // c knows a, but a doesn't know c: c dials a, and hears nothing back
    c.knows(&[&a]);
    for d in [&a, &b, &c] {
        d.h.set_mode(Some(Mode::Open));
    }
    let t = run(&mut [&mut a, &mut b, &mut c], 0, 2_000);
    let secret = private(&a, &[&b], "only for b");
    let public = seal_public(&a.id, "everyone near", SENT_MIN, 60);
    a.send(&secret, 1);
    a.send(&public, 2);
    run(&mut [&mut a, &mut b, &mut c], t, t + 3_000);

    assert_eq!(b.heard(), vec![secret.clone(), public.clone()]);
    assert!(c.heard().is_empty(), "the stranger heard {:?}", c.heard());
    assert!(c.h.peers().contacts.is_empty());
    assert_eq!(a.h.peers().contacts, vec![b.me()]);
    assert_eq!(a.h.peers().strangers_in_range(), 1);

    // a wrote nothing but contact hellos to c: no answer, no session
    let to_c = links_between(&room, &a.radio, &c.radio);
    for (link, m) in written(&a.radio) {
        if to_c.contains(&link) {
            assert_eq!(m.kind, Kind::Contact, "a sent c a {:?}", m.kind);
        }
    }
}

#[test]
fn a_broadcast_goes_to_every_confirmed_contact() {
    let room = Room::new(5);
    let (mut a, mut b, mut c) = (
        Device::new(&room, 1),
        Device::new(&room, 2),
        Device::new(&room, 3),
    );
    a.knows(&[&b, &c]);
    b.knows(&[&a]);
    c.knows(&[&a]);
    for d in [&a, &b, &c] {
        d.h.set_mode(Some(Mode::Contacts));
    }
    let t = run(&mut [&mut a, &mut b, &mut c], 0, 2_000);
    let public = seal_public(&a.id, "to all", SENT_MIN, 60);
    a.send(&public, 5);
    run(&mut [&mut a, &mut b, &mut c], t, t + 2_000);
    assert_eq!(b.heard(), vec![public.clone()]);
    assert_eq!(c.heard(), vec![public]);
    assert!(a.has(&TransportEvent::Sent(5)));
}

#[test]
fn an_envelope_waits_for_its_contact_then_goes() {
    let room = Room::new(6);
    let (mut a, mut b) = pair(&room);
    room.set_in_range(&a.radio, &b.radio, false);
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
    let room = Room::new(7);
    let (mut a, b) = (Device::new(&room, 1), Device::new(&room, 2));
    a.knows(&[&b]);
    a.h.set_mode(Some(Mode::Contacts));
    let t = run(&mut [&mut a], 0, 500);
    a.send(&private(&a, &[&b], "never"), 4);
    run_by(&mut [&mut a], t, t + HOLD_MS + 1_000, 1_000);
    assert!(a.has(&TransportEvent::Dropped(4)));
    assert_eq!(a.t.queued(), 0);
}

#[test]
fn the_queue_holds_at_most_its_cap() {
    let room = Room::new(8);
    let (mut a, b) = (Device::new(&room, 1), Device::new(&room, 2));
    a.knows(&[&b]);
    a.h.set_mode(Some(Mode::Contacts));
    a.poll(0);
    let env = private(&a, &[&b], "waiting");
    let extra = 6;
    for tag in 0..(QUEUE_CAP + extra) as u16 {
        let mut e = env.clone();
        e.id[0] = tag as u8;
        e.id[1] = (tag >> 8) as u8;
        a.send(&e, tag);
    }
    a.poll(STEP);
    assert_eq!(a.t.queued(), QUEUE_CAP);
    let dropped = a
        .events
        .iter()
        .filter(|e| matches!(e, TransportEvent::Dropped(_)))
        .count();
    assert_eq!(dropped, extra);
}

// ---- order and loss -----------------------------------------------------

/// Two contacts trade twelve envelopes through knobs; the ids b heard.
fn lossy(seed: u64, drop: u32, duplicate: u32, reorder: u32) -> (Vec<Envelope>, Device, Device) {
    let room = Room::new(seed);
    let (mut a, mut b) = pair(&room);
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
    for i in 0..12 {
        assert!(a.has(&TransportEvent::Sent(100 + i)), "sent {i}");
    }
    (sent, a, b)
}

fn once_each(sent: &[Envelope], heard: &[Envelope]) {
    let mut h: Vec<[u8; 16]> = heard.iter().map(|e| e.id).collect();
    let mut s: Vec<[u8; 16]> = sent.iter().map(|e| e.id).collect();
    h.sort();
    s.sort();
    assert_eq!(h, s, "every envelope, once each");
}

#[test]
fn lost_writes_are_sent_again_until_every_envelope_arrives_once() {
    let (sent, _, b) = lossy(10, 200, 0, 0);
    once_each(&sent, &b.heard());
}

#[test]
fn repeated_and_reordered_writes_are_delivered_once_each_in_order() {
    let (sent, _, b) = lossy(11, 0, 300, 300);
    assert_eq!(b.heard(), sent);
}

#[test]
fn lost_repeated_and_reordered_writes_together_still_deliver_once_each() {
    let (sent, _, b) = lossy(12, 150, 200, 200);
    once_each(&sent, &b.heard());
}

#[test]
fn the_chaos_runs_make_the_same_writes_every_time() {
    let trace = || {
        let (_, a, b) = lossy(12, 150, 200, 200);
        let mut t: Vec<(u64, usize)> = a
            .radio
            .take_written()
            .iter()
            .map(|(l, w)| (l.0, w.len()))
            .collect();
        t.extend(
            b.radio
                .take_written()
                .iter()
                .map(|(l, w)| (l.0 + 1_000, w.len())),
        );
        t
    };
    let first = trace();
    assert!(first.len() > 100);
    for run in 1..50 {
        assert_eq!(trace(), first, "run {run} wrote differently");
    }
}

#[test]
fn contacts_meet_when_writes_are_lost_from_the_start() {
    let room = Room::new(14);
    room.set_drop(300);
    let (mut a, mut b) = pair(&room);
    // a round per token: with this much loss they may need the next token
    let met = run_until(&mut [&mut a, &mut b], 0, 2 * TOKEN_MS, |d| {
        !d[0].h.peers().contacts.is_empty() && !d[1].h.peers().contacts.is_empty()
    });
    let t = met.expect("they meet");
    let env = private(&a, &[&b], "despite it all");
    a.send(&env, 2);
    run(&mut [&mut a, &mut b], t, t + 30_000);
    assert_eq!(b.heard(), vec![env]);
}

/// Two contacts meet, run for `before`, go dark for `dark`, and an
/// envelope sent in the dark arrives. How long after the dark it took.
fn blackout(seed: u64, before: u64, dark: u64) -> u64 {
    let room = Room::new(seed);
    let (mut a, mut b) = pair(&room);
    let t = run_by(&mut [&mut a, &mut b], 0, before, 250);
    assert_eq!(a.h.peers().contacts, vec![b.me()], "met before the dark");
    room.set_drop(1000);
    let env = private(&a, &[&b], "sent in the dark");
    a.send(&env, 9);
    let end = run(&mut [&mut a, &mut b], t, t + dark);
    room.set_drop(0);
    let arrived = run_until(&mut [&mut a, &mut b], end, end + 120_000, |d| {
        d[1].heard().iter().any(|e| e.id == env.id)
    })
    .expect("it arrives");
    assert_eq!(b.heard().iter().filter(|e| e.id == env.id).count(), 1);
    assert_eq!(a.h.peers().contacts, vec![b.me()]);
    arrived - end
}

#[test]
fn a_40_second_blackout_recovers_within_35_seconds() {
    let after = blackout(15, 2_000, 40_000);
    assert!(after <= 35_000, "took {after} ms");
}

#[test]
fn a_blackout_longer_than_the_silence_limit_recovers_within_35_seconds() {
    let after = blackout(16, 2_000, SILENCE_MS + 30_000);
    assert!(after <= 35_000, "took {after} ms");
}

#[test]
fn contacts_on_long_lived_links_still_recover_from_a_blackout() {
    // past two rotations: the links have been replaced with fresh tokens
    let after = blackout(17, 2 * TOKEN_MS + 60_000, SILENCE_MS + 30_000);
    assert!(after <= 35_000, "took {after} ms");
}

// ---- fragments ----------------------------------------------------------

/// A raw radio linked to a device (Open mode, no contacts), and the link
/// the device sees from it.
fn raw_beside(room: &Room) -> (Device, Raw, LinkId) {
    let mut a = Device::new(room, 1);
    a.h.set_mode(Some(Mode::Open));
    let mut raw = Raw::new(room);
    a.poll(0);
    raw.poll();
    let (link, _) = raw.opened();
    (a, raw, link)
}

#[test]
fn many_unfinished_messages_are_bounded_in_count_and_time() {
    let room = Room::new(20);
    let (mut a, mut raw, link) = raw_beside(&room);
    for id in 0..500u16 {
        let mut f = id.to_be_bytes().to_vec();
        f.extend_from_slice(&0u16.to_be_bytes());
        f.extend_from_slice(&100u16.to_be_bytes());
        f.extend_from_slice(&[7u8; 100]);
        raw.radio.write(link, &f);
    }
    a.poll(STEP);
    assert_eq!(a.t.assembling(link), (MAX_ASSEMBLIES, MAX_ASSEMBLIES * 100));
    a.poll(STEP + ASSEMBLY_MS);
    assert_eq!(a.t.assembling(link), (0, 0));
}

#[test]
fn one_message_that_never_ends_holds_at_most_64_kib() {
    let room = Room::new(21);
    let (mut a, mut raw, link) = raw_beside(&room);
    let mut most = 0;
    for index in 0..1000u16 {
        let mut f = 9000u16.to_be_bytes().to_vec();
        f.extend_from_slice(&index.to_be_bytes());
        f.extend_from_slice(&4000u16.to_be_bytes());
        f.extend_from_slice(&[1u8; 170]);
        raw.radio.write(link, &f);
        if index % 20 == 0 {
            a.poll(STEP);
            most = most.max(a.t.assembling(link).1);
        }
    }
    a.poll(STEP);
    most = most.max(a.t.assembling(link).1);
    assert!(most <= MAX_MESSAGE, "held {most}");
    assert!(most > MAX_MESSAGE - 1_000, "it did fill up first: {most}");
}

#[test]
fn random_writes_never_panic_and_stay_bounded() {
    let room = Room::new(22);
    let (mut a, mut raw, link) = raw_beside(&room);
    let mut rng = SeededRng::new(0x1234_5678);
    let mut next = || rand_core::RngCore::next_u64(&mut rng);
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
fn a_link_going_down_mid_message_drops_the_part_and_the_rest_completes_nothing() {
    let room = Room::new(23);
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
    assert!(!a.t.links().contains(&link));
    // the rest, arriving on a later link, starts a new part and no more
    raw.radio.stop();
    raw.radio.start(Mode::Open);
    raw.radio.advertise([0xef; TOKEN_LEN]);
    a.poll(3 * STEP);
    raw.poll();
    let (again, _) = raw.opened();
    for f in &frags[3..] {
        raw.radio.write(again, f);
    }
    a.poll(4 * STEP);
    assert_eq!(a.t.assembling(again).0, 1, "a part, not a message");
}

// ---- contact handshakes -------------------------------------------------

/// `raw` acting with `keys`: dial `a` on the link it opened, as a contact
/// would, at Unix time `unix`, naming `token`. What came back that isn't
/// a's own hellos.
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
    let friend = identity(40);
    let stranger = identity(41);
    a.h.set_contacts(vec![friend.public()]);
    let fk = SessionKeys::new(&friend);
    let sk = SessionKeys::new(&stranger);
    let now = 1_000;
    a.poll(now);

    assert!(dial(&mut raw, &mut a, &sk, BASE + now, None, now).is_empty());
    assert_eq!(
        a.t.seen_ephemerals(),
        0,
        "a stranger adds nothing to the cache"
    );
    assert!(dial(&mut raw, &mut a, &fk, BASE + now + 120_001, None, now).is_empty());
    assert!(dial(&mut raw, &mut a, &fk, BASE + now - 120_001, None, now).is_empty());
    assert!(dial(&mut raw, &mut a, &fk, BASE + now, Some([1; TOKEN_LEN]), now).is_empty());
    assert_eq!(a.t.seen_ephemerals(), 0);
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
    let friend = identity(40);
    a.h.set_contacts(vec![friend.public()]);
    a.poll(STEP);
    let (_, token) = raw.opened();
    let mut hs = Session::contact_initiator(&SessionKeys::new(&friend), &a.me(), BASE, token);
    let hello = contact_hello(77, &hs.write_message().unwrap());
    assert_eq!(
        replay(&mut raw, &mut a, link, &hello, 100),
        vec![Kind::Handshake]
    );
    assert!(replay(&mut raw, &mut a, link, &hello, 200).is_empty());
    let mut other = hello.clone();
    other.body[..4].copy_from_slice(&78u32.to_be_bytes());
    assert!(replay(&mut raw, &mut a, link, &other, 300).is_empty());
    a.poll(400);
    assert!(
        a.h.peers().contacts.is_empty(),
        "a replay never shows presence"
    );
    a.poll(SEEN_MS + 1_000);
    assert_eq!(a.t.seen_ephemerals(), 0);
}

#[test]
fn the_seen_cache_is_bounded() {
    let room = Room::new(32);
    let (mut a, mut raw, link) = raw_beside(&room);
    let friend = identity(40);
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
    a.h.set_contacts(vec![a.me()]);
    a.poll(STEP);
    let keys = SessionKeys::new(&a.id);
    assert!(dial(&mut raw, &mut a, &keys, BASE + STEP, None, STEP).is_empty());
}

#[test]
fn the_token_rotates_between_12_and_15_minutes_and_the_previous_still_answers() {
    let room = Room::new(34);
    let (mut a, mut raw, _) = raw_beside(&room);
    let friend = identity(40);
    let fk = SessionKeys::new(&friend);
    a.h.set_contacts(vec![friend.public()]);
    a.poll(STEP);
    let first = a.radio.token().unwrap();
    a.poll(12 * 60_000 - STEP);
    assert_eq!(a.radio.token(), Some(first), "not before 12 minutes");
    let mut t = 12 * 60_000;
    while a.radio.token() == Some(first) {
        a.poll(t);
        t += 1_000;
    }
    assert!(t <= TOKEN_MS + 1_000, "by 15 minutes");
    let second = a.radio.token().unwrap();
    assert_eq!(a.t.tokens(), vec![second, first]);
    // the raw radio's links were closed on rotation; it links again
    raw.poll();
    a.poll(t);
    raw.poll();
    assert_eq!(
        dial(&mut raw, &mut a, &fk, BASE + t, Some(first), t),
        vec![Kind::Handshake]
    );
}

/// Contact hellos `radio` wrote on `link` (or on every link).
fn hellos(radio: &FakeRadio, link: Option<LinkId>) -> usize {
    written(radio)
        .iter()
        .filter(|(l, m)| link.is_none_or(|x| x == *l) && m.kind == Kind::Contact)
        .count()
}

#[test]
fn every_round_is_exactly_eight_hellos_whatever_the_contact_count() {
    for contacts in [0usize, 3, 20] {
        let room = Room::new(36);
        let mut a = Device::new(&room, 1);
        let others: Vec<PublicIdentity> = (0..contacts)
            .map(|i| identity(100 + i as u64).public())
            .collect();
        a.h.set_contacts(others);
        a.h.set_mode(Some(Mode::Contacts));
        let mut raw = Raw::new(&room);
        a.poll(0);
        raw.poll();
        let link = raw.theirs();
        assert_eq!(TRIAL_CAP, 8);
        assert_eq!(hellos(&a.radio, Some(link)), 8, "{contacts} contacts");
        // once more, unanswered, and then no more for this token
        run(&mut [&mut a], STEP, TRIAL_RETRY_MS + STEP);
        assert_eq!(hellos(&a.radio, Some(link)), 8, "{contacts}: the resend");
        run(&mut [&mut a], 2 * TRIAL_RETRY_MS, 60_000);
        assert_eq!(hellos(&a.radio, None), 0, "{contacts}: nothing more");
    }
}

#[test]
fn a_token_gets_one_round_however_often_it_reconnects() {
    let room = Room::new(37);
    let mut a = Device::new(&room, 1);
    a.h.set_contacts((0..20).map(|i| identity(100 + i).public()).collect());
    a.h.set_mode(Some(Mode::Contacts));
    let mut raw = Raw::new(&room);
    let t = run(&mut [&mut a], 0, 30_000);
    assert_eq!(hellos(&a.radio, None), 2 * TRIAL_CAP);
    for _ in 0..3 {
        raw.radio.stop();
        raw.radio.start(Mode::Contacts);
        raw.radio.advertise([0xee; TOKEN_LEN]);
    }
    run(&mut [&mut a], t, t + 30_000);
    assert_eq!(hellos(&a.radio, None), 0, "same token, no new round");
    raw.radio.advertise([0xab; TOKEN_LEN]);
    run(&mut [&mut a], t + 30_050, t + 30_100);
    assert_eq!(
        hellos(&a.radio, None),
        TRIAL_CAP,
        "a new token, a new round"
    );
}

#[test]
fn the_most_recently_seen_contact_is_tried_first() {
    let room = Room::new(38);
    let (mut a, mut b) = (Device::new(&room, 1), Device::new(&room, 2));
    let mut contacts: Vec<PublicIdentity> = (0..19).map(|i| identity(100 + i).public()).collect();
    contacts.push(b.me());
    a.h.set_contacts(contacts);
    b.knows(&[&a]);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    // they meet: b dials a, a confirms b
    let t = run(&mut [&mut a, &mut b], 0, 3_000);
    assert_eq!(a.h.peers().contacts, vec![b.me()]);
    // b's radio restarts: a sees a new token, and tries b in its round
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
    let sealed = mine
        .iter()
        .any(|(l, m)| opened.contains(l) && m.kind == Kind::Sealed);
    assert!(sealed, "a reached b on its own link");
}

#[test]
fn clocks_two_minutes_apart_still_meet_and_three_do_not() {
    for (skew, meets) in [(110_000u64, true), (180_000, false)] {
        let room = Room::new(39);
        let mut a = Device::new(&room, 1);
        let mut b = Device::with(room.radio(), BASE + skew, 2);
        a.knows(&[&b]);
        b.knows(&[&a]);
        a.h.set_mode(Some(Mode::Contacts));
        b.h.set_mode(Some(Mode::Contacts));
        run(&mut [&mut a, &mut b], 0, 15_000);
        assert_eq!(!a.h.peers().contacts.is_empty(), meets, "skew {skew}");
        assert_eq!(!b.h.peers().contacts.is_empty(), meets, "skew {skew}");
    }
}

// ---- links --------------------------------------------------------------

#[test]
fn a_stranger_cannot_follow_a_device_across_its_rotation() {
    let room = Room::new(50);
    let mut a = Device::new(&room, 1);
    a.h.set_mode(Some(Mode::Open));
    let mut raw = Raw::new(&room);
    a.poll(0);
    raw.poll();
    // the stranger keeps its link to a busy with request handshakes
    let into_a = raw.opened().0;
    let from_a = raw.theirs();
    let mut t = 0;
    while a.t.tokens().len() < 2 && t <= TOKEN_MS + 10_000 {
        let mut hs = Session::request_initiator(&SessionKeys::new(&identity(60)));
        let mut body = (t as u32).to_be_bytes().to_vec();
        body.extend_from_slice(&hs.write_message().unwrap());
        raw.write(
            into_a,
            &LinkMessage {
                kind: Kind::Request,
                flags: FROM_INITIATOR,
                body,
            },
        );
        t += 10_000;
        a.poll(t);
    }
    assert_eq!(a.t.tokens().len(), 2, "a rotated");
    raw.poll();
    assert!(
        raw.downs.contains(&into_a),
        "the stranger's link closed at rotation"
    );
    assert!(raw.downs.contains(&from_a), "and a's link to it");
}

#[test]
fn an_idle_link_closes_after_two_minutes_and_stays_closed_until_a_token_changes() {
    let room = Room::new(51);
    let mut a = Device::new(&room, 1);
    a.h.set_mode(Some(Mode::Open));
    let mut raw = Raw::new(&room);
    run(&mut [&mut a], 0, 1_000);
    raw.poll();
    let into_a = raw.opened().0;
    assert!(a.t.links().contains(&into_a));
    run(&mut [&mut a], 1_050, IDLE_MS + 2_000);
    raw.poll();
    assert!(raw.downs.contains(&into_a));
    assert!(!a.t.links().contains(&into_a));
    let ups = raw.ups.len();
    run(&mut [&mut a], IDLE_MS + 2_050, IDLE_MS + 10_000);
    raw.poll();
    assert_eq!(raw.ups.len(), ups, "no reconnect to the same adverts");
}

#[test]
fn in_contacts_mode_a_strangers_link_closes_when_its_round_is_over() {
    let room = Room::new(52);
    let mut a = Device::new(&room, 1);
    a.h.set_contacts(vec![identity(99).public()]);
    a.h.set_mode(Some(Mode::Contacts));
    let mut raw = Raw::new(&room);
    a.poll(0);
    raw.poll();
    let theirs = raw.theirs();
    run(&mut [&mut a], STEP, 2 * TRIAL_RETRY_MS + 2 * STEP);
    raw.poll();
    assert!(raw.downs.contains(&theirs), "closed once the round is over");
}

#[test]
fn a_link_this_device_opened_is_replaced_before_its_token_goes_stale() {
    let room = Room::new(53);
    let (mut a, mut b) = pair(&room);
    let t = run_by(&mut [&mut a, &mut b], 0, 2_000, 250);
    let first: Vec<LinkId> = a.t.links();
    run_by(&mut [&mut a, &mut b], t, TOKEN_MS + 30_000, 250);
    let later = a.t.links();
    assert!(
        first.iter().all(|l| !later.contains(l)),
        "{first:?} {later:?}"
    );
    assert_eq!(
        a.h.peers().contacts,
        vec![b.me()],
        "and they are together again"
    );
}

#[test]
fn no_more_than_the_link_cap_are_kept() {
    let room = Room::new(54);
    let mut a = Device::new(&room, 1);
    a.h.set_mode(Some(Mode::Open));
    let raws: Vec<Raw> = (0..MAX_LINKS)
        .map(|i| Raw::with_token(&room, [i as u8; TOKEN_LEN]))
        .collect();
    a.poll(0);
    a.poll(STEP);
    assert_eq!(a.t.links().len(), MAX_LINKS, "{} raws", raws.len());
}

// ---- presence -----------------------------------------------------------

#[test]
fn strangers_are_counted_once_and_for_two_minutes_after_they_leave() {
    let room = Room::new(60);
    let (mut a, mut c) = (Device::new(&room, 1), Device::new(&room, 3));
    a.h.set_mode(Some(Mode::Open));
    c.h.set_mode(Some(Mode::Open));
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
    seal_public(&d.id, "bombechat pair hello v1\nAda", SENT_MIN, 60)
}

fn open_pair(room: &Room, sa: u64, sc: u64) -> (Device, Device, u64) {
    let (mut a, mut c) = (Device::new(room, sa), Device::new(room, sc));
    a.h.set_mode(Some(Mode::Open));
    c.h.set_mode(Some(Mode::Open));
    let t = run(&mut [&mut a, &mut c], 0, 1_000);
    (a, c, t)
}

fn requests(d: &Device) -> Vec<&BleEvent> {
    d.ble
        .iter()
        .filter(|e| matches!(e, BleEvent::Request { .. }))
        .collect()
}

#[test]
fn a_request_reaches_a_stranger_in_open_mode_who_reveals_nothing() {
    let room = Room::new(70);
    let (mut a, mut c, t) = open_pair(&room, 1, 3);
    let link = a.h.peers().strangers[0];
    let hello = hello_of(&a).encode();
    a.h.request(link, hello.clone());
    c.radio.take_written();
    run(&mut [&mut a, &mut c], t, t + 2_000);

    let got = requests(&c);
    assert_eq!(got.len(), 1);
    let BleEvent::Request { from, hello: h, .. } = got[0] else {
        unreachable!()
    };
    assert_eq!(*from, a.me());
    assert_eq!(*h, hello);
    assert!(a.ble.contains(&BleEvent::RequestSent { link }));
    assert!(
        c.heard().is_empty(),
        "the request is not an envelope for c's node"
    );
    let me = c.me();
    let cert = SessionKeys::new(&c.id).certificate().to_vec();
    for (_, m) in written(&c.radio) {
        assert!(matches!(
            m.kind,
            Kind::Handshake | Kind::Sealed | Kind::Contact
        ));
        for secret in [&me.sign[..], &me.dh[..], &cert[64..]] {
            assert!(!contains(&m.body, secret), "c gave itself away");
        }
    }
}

#[test]
fn a_hello_that_is_not_an_envelope_never_reaches_the_app() {
    let room = Room::new(71);
    let (mut a, mut raw, link) = raw_beside(&room);
    let keys = SessionKeys::new(&identity(60));
    assert!(raw_request(
        &mut raw,
        &mut a,
        link,
        &keys,
        1,
        &[0x42; 300],
        STEP
    ));
    assert!(requests(&a).is_empty());
    assert_eq!(a.t.requests_held(), 0);
}

/// `raw` asks `a` to connect on `link`, as a stranger with `keys` would:
/// the whole request handshake, then `hello` sealed as the first message.
/// Whether `a` answered.
fn raw_request(
    raw: &mut Raw,
    a: &mut Device,
    link: LinkId,
    keys: &SessionKeys,
    xid: u32,
    hello: &[u8],
    now: u64,
) -> bool {
    let with_id = |rest: &[u8]| {
        let mut b = xid.to_be_bytes().to_vec();
        b.extend_from_slice(rest);
        b
    };
    let mut hs = Session::request_initiator(keys);
    raw.got.clear();
    raw.write(
        link,
        &LinkMessage {
            kind: Kind::Request,
            flags: FROM_INITIATOR,
            body: with_id(&hs.write_message().unwrap()),
        },
    );
    a.poll(now);
    raw.poll();
    let Some((_, reply)) = raw
        .got
        .iter()
        .find(|(l, m)| *l == link && m.kind == Kind::Handshake)
    else {
        return false;
    };
    hs.read_message(&reply.body[4..]).unwrap();
    raw.write(
        link,
        &LinkMessage {
            kind: Kind::Handshake,
            flags: FROM_INITIATOR,
            body: with_id(&hs.write_message().unwrap()),
        },
    );
    let t = hs.into_transport().unwrap();
    // envelope type 1, ack 0, seq 0, length, body, padded
    let mut plain = vec![1u8, 0, 0, 0, 0, 0, 0, 0, 0];
    plain.extend_from_slice(&(hello.len() as u16).to_be_bytes());
    plain.extend_from_slice(hello);
    plain.resize(bucket(plain.len()), 0);
    let mut body = with_id(&0u64.to_be_bytes());
    body.extend_from_slice(&t.encrypt(0, &plain).unwrap());
    raw.write(
        link,
        &LinkMessage {
            kind: Kind::Sealed,
            flags: FROM_INITIATOR,
            body,
        },
    );
    a.poll(now + 1);
    true
}

#[test]
fn requests_are_one_per_link_four_in_all_and_forgotten_after_three_minutes() {
    let room = Room::new(72);
    let (mut a, mut raw, link) = raw_beside(&room);
    let keys = SessionKeys::new(&identity(60));
    let hello = hello_of(&a).encode();
    // the probe: one stranger asks thirty times on one link
    let mut now = STEP;
    for i in 0..30 {
        assert!(raw_request(&mut raw, &mut a, link, &keys, i, &hello, now));
        now += 1_000;
    }
    let asked = a
        .ble
        .iter()
        .filter(|e| matches!(e, BleEvent::Request { .. }))
        .count();
    assert_eq!(asked, 30, "each reported, each replacing the last");
    assert_eq!(a.t.requests_held(), 1, "one per link");
    // five more strangers, each on its own link
    let mut others: Vec<(Raw, LinkId)> = (0..5)
        .map(|i| {
            let mut r = Raw::with_token(&room, [0x10 + i; TOKEN_LEN]);
            a.poll(now);
            r.poll();
            let l = room
                .links()
                .into_iter()
                .find(|(_, c, p)| *c == r.radio.index() && *p == a.radio.index())
                .unwrap()
                .0;
            (r, l)
        })
        .collect();
    for (i, (r, l)) in others.iter_mut().enumerate() {
        let k = SessionKeys::new(&identity(70 + i as u64));
        assert!(raw_request(r, &mut a, *l, &k, 1, &hello, now));
        now += 1_000;
    }
    assert_eq!(a.t.requests_held(), REQUESTS_CAP);
    // nothing acknowledged after three minutes
    let mut t = now;
    while t < now + REQUEST_MS + 2_000 {
        t += 1_000;
        a.poll(t);
    }
    assert_eq!(a.t.requests_held(), 0, "forgotten after three minutes");
    assert_eq!(a.t.held_ahead(), 0);
}

#[test]
fn an_accepted_request_is_kept_past_three_minutes() {
    let room = Room::new(73);
    let (mut a, mut raw, link) = raw_beside(&room);
    let keys = SessionKeys::new(&identity(60));
    let hello = hello_of(&a).encode();
    assert!(raw_request(&mut raw, &mut a, link, &keys, 1, &hello, STEP));
    let BleEvent::Request { link: at, from, .. } = requests(&a)[0].clone() else {
        unreachable!()
    };
    assert_eq!(from, identity(60).public());
    a.h.accept_request(at);
    let mut t = STEP;
    while t < REQUEST_MS + 5_000 {
        t += 1_000;
        a.poll(t);
    }
    assert_eq!(a.t.requests_held(), 1);
    a.h.decline_request(at);
    a.poll(t + 1_000);
    assert_eq!(a.t.requests_held(), 0);
}

#[test]
fn requests_are_not_answered_or_sent_in_contacts_mode() {
    let room = Room::new(74);
    let (mut a, mut c) = (Device::new(&room, 1), Device::new(&room, 3));
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
    a.h.request(link, hello_of(&a).encode());
    run(&mut [&mut a, &mut c], t, t + 500);
    assert_eq!(a.ble, vec![BleEvent::RequestFailed { link }]);
}

// ---- the switch ---------------------------------------------------------

#[test]
fn off_stops_the_radio_drops_what_waits_and_fails_requests_in_flight() {
    let room = Room::new(80);
    let (mut a, mut c, t) = open_pair(&room, 1, 3);
    let b = Device::new(&room, 2);
    a.knows(&[&b]);
    a.send(&private(&a, &[&b], "waiting"), 6);
    let link = a.h.peers().strangers[0];
    a.h.request(link, hello_of(&a).encode());
    a.poll(t + STEP);
    a.h.set_mode(None);
    a.poll(t + 2 * STEP);
    assert!(!a.radio.is_on() && !a.t.is_running());
    assert!(a.has(&TransportEvent::Dropped(6)));
    assert!(a.ble.contains(&BleEvent::RequestFailed { link }));
    assert_eq!(a.radio.token(), None);
    assert_eq!(a.h.peers(), Default::default());
    c.poll(t + 3 * STEP);

    // locked: no keys, no radio, whatever the mode
    a.h.set_mode(Some(Mode::Open));
    a.h.set_identity(None);
    a.poll(t + 3 * STEP);
    assert!(!a.radio.is_on());
}

#[test]
fn a_device_starts_off() {
    let room = Room::new(81);
    let mut a = Device::new(&room, 1);
    a.poll(0);
    assert!(!a.radio.is_on());
    assert_eq!(a.radio.starts_and_stops(), (0, 0));
}

#[test]
fn listening_off_leaves_envelopes_with_the_sender_until_it_is_on() {
    let room = Room::new(82);
    let (mut a, mut b) = pair(&room);
    let t = run(&mut [&mut a, &mut b], 0, 2_000);
    b.t.set_listening(false);
    let env = private(&a, &[&b], "hold on");
    a.send(&env, 5);
    let t = run(&mut [&mut a, &mut b], t, t + 10_000);
    assert!(b.heard().is_empty());
    assert!(!a.has(&TransportEvent::Sent(5)), "not acknowledged");
    b.t.set_listening(true);
    run(&mut [&mut a, &mut b], t, t + 5_000);
    assert_eq!(b.heard(), vec![env]);
    assert!(a.has(&TransportEvent::Sent(5)));
}

#[test]
fn silence_closes_a_session_and_presence_follows() {
    let room = Room::new(83);
    let (mut a, mut b) = pair(&room);
    let t = run(&mut [&mut a, &mut b], 0, 2_000);
    assert_eq!(a.h.peers().contacts, vec![b.me()]);
    room.set_drop(1000);
    run_by(&mut [&mut a, &mut b], t, t + SILENCE_MS + 2_000, 250);
    assert!(
        a.h.peers().contacts.is_empty(),
        "gone after a minute of silence"
    );
    assert!(b.h.peers().contacts.is_empty());
}

#[test]
fn quiet_contacts_stay_present_on_keepalives() {
    let room = Room::new(84);
    let (mut a, mut b) = pair(&room);
    let t = run(&mut [&mut a, &mut b], 0, 2_000);
    a.radio.take_written();
    let mut now = t;
    while now < 5 * SILENCE_MS {
        now += 250;
        a.poll(now);
        b.poll(now);
        assert_eq!(a.h.peers().contacts, vec![b.me()], "at {now}");
    }
    assert_eq!(
        hellos(&a.radio, None),
        0,
        "no session had to be dialled again"
    );
}
