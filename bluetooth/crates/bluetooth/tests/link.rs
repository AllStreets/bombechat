//! The link protocol end to end over the fake radio. Time is whatever the
//! test passes to `poll`, identities and transport randomness are seeded:
//! no test reads a clock, and every run makes the same writes.

use std::collections::{BTreeSet, HashMap};

use bombechat_bluetooth::wire::{
    fragment, Kind, LinkMessage, Reassembly, ASSEMBLY_MS, FROM_INITIATOR, MAX_ASSEMBLIES,
    MAX_MESSAGE,
};
use bombechat_bluetooth::{
    bucket, Advert, BleHandle, BleTransport, FakeRadio, LinkId, Mode, Radio, RadioEvent, Room,
    SeededRng, TestClock, ACK_DELAY_MAX_MS, CONTACT_LINKS, FUTURE_TOKENS, HOLD_MS, IDLE_MS,
    KEEPALIVE_MS, MAX_LINKS_IN, MAX_LINKS_OUT, PAST_TOKENS, QUEUE_CAP, REQUESTS_CAP,
    REQUESTS_NEED_OPEN, REQUESTS_PART_WAY_CAP, REQUEST_ATTEMPTS, REQUEST_MS, SEEN_CAP, SEEN_MS,
    SILENCE_MS, STRANGER_MS, TOKEN_MS, TRIAL_CAP, TRIAL_RETRY_MS,
};
use bombechat_core::{seal_private, seal_public, Envelope, Identity, PublicIdentity};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_node::{pairing_hello, Transport, TransportEvent, NOT_IN_RANGE};
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
}

impl Device {
    fn new(room: &Room, seed: u64) -> Device {
        Device::with(room.radio(), BASE, seed)
    }

    fn with(radio: FakeRadio, base: u64, seed: u64) -> Device {
        Device::with_clock(radio, TestClock::new(base), seed)
    }

    fn with_clock(radio: FakeRadio, clock: TestClock, seed: u64) -> Device {
        let watch = radio.watch();
        let mut t = BleTransport::with_rng(radio, clock, Box::new(SeededRng::new(seed)));
        let h = t.handle();
        let id = identity(seed);
        t.set_identity(Some(&id));
        Device {
            t,
            h,
            id,
            radio: watch,
            events: Vec::new(),
        }
    }

    fn me(&self) -> PublicIdentity {
        self.id.public()
    }

    fn knows(&mut self, others: &[&Device]) {
        let contacts: Vec<PublicIdentity> = others.iter().map(|d| d.me()).collect();
        self.t.set_contacts(&contacts);
    }

    fn send(&mut self, env: &Envelope, tag: u16) {
        for f in split(&env.encode(), tag) {
            self.t.send(f);
        }
    }

    fn poll(&mut self, now: u64) {
        self.events.extend(self.t.poll(now));
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
    let (mut a, mut b) = (Device::new(room, 1), Device::new(room, 2));
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
    ups: Vec<(LinkId, Advert)>,
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
        let mut raw = Raw::silent(room);
        raw.radio.advertise(token);
        raw
    }

    /// A radio that connects to adverts but shows none: only one link to
    /// each device, the one it opens.
    fn silent(room: &Room) -> Raw {
        let mut radio = room.radio();
        radio.start(Mode::Open);
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
                RadioEvent::Up { link, advert } => self.ups.push((link, advert)),
                RadioEvent::Down(link) => self.downs.push(link),
                RadioEvent::Bytes(link, b) => {
                    let r = self.assembly.entry(link).or_default();
                    if let Some(m) = r.push(&b, 0).and_then(|m| LinkMessage::decode(&m)) {
                        self.got.push((link, m));
                    }
                }
                RadioEvent::Unavailable(_) | RadioEvent::Available => {}
            }
        }
    }

    /// The link this radio opened to a device's advert, and its token.
    fn opened(&self) -> (LinkId, [u8; TOKEN_LEN]) {
        self.ups
            .iter()
            .rev()
            .find_map(|(l, a)| match a {
                Advert::Token(t) => Some((*l, *t)),
                _ => None,
            })
            .expect("a link to an advert")
    }

    /// The link the device opened to this radio's advert.
    fn theirs(&self) -> LinkId {
        self.ups
            .iter()
            .rev()
            .find_map(|(l, a)| (*a == Advert::Unseen).then_some(*l))
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

/// When one Mac withholds its token from the other (it heard that Mac
/// under an earlier token, and the identifier never changes, as between
/// two Macs on the same Apple ID), the pair still meets over the links the
/// other way, in both directions, and stays present to each other. Once
/// they have talked, the withholding Mac's advert is recognised by the
/// token it gave inside the session, so the other reaches it over its own
/// link too.
#[test]
fn a_pair_with_one_direction_withheld_still_talks_both_ways() {
    for (withheld_by_b, seed) in [(true, 11), (false, 12)] {
        let room = Room::new(seed);
        let (mut a, mut b) = pair(&room);
        if withheld_by_b {
            room.refuse(&a.radio, &b.radio);
        } else {
            room.refuse(&b.radio, &a.radio);
        }
        let t = run(&mut [&mut a, &mut b], 0, 3_000);
        assert_eq!(a.h.peers().contacts, vec![b.me()]);
        assert_eq!(b.h.peers().contacts, vec![a.me()]);
        let to_b = private(&a, &[&b], "for b");
        let to_a = private(&b, &[&a], "for a");
        a.send(&to_b, 7);
        b.send(&to_a, 9);
        let t = run(&mut [&mut a, &mut b], t, t + 3_000);
        assert_eq!(b.heard(), vec![to_b]);
        assert_eq!(a.heard(), vec![to_a]);
        assert!(a.has(&TransportEvent::Sent(7)) && b.has(&TransportEvent::Sent(9)));
        // and it carries on across tokens and the links' own replacement
        let t = run_by(&mut [&mut a, &mut b], t, t + 2 * TOKEN_MS, 250);
        assert_eq!(a.h.peers().contacts, vec![b.me()]);
        assert_eq!(b.h.peers().contacts, vec![a.me()]);
        // the side denied the token now has a session on a link it opened
        let (denied, other) = if withheld_by_b { (&a, &b) } else { (&b, &a) };
        assert_eq!(
            denied.t.contact_token(&other.me()),
            other.radio.token(),
            "it knows the other's current token"
        );
        let opened: Vec<LinkId> = room
            .links()
            .into_iter()
            .filter(|(_, c, p)| *c == denied.radio.index() && *p == other.radio.index())
            .map(|(l, _, _)| l)
            .collect();
        denied.radio.take_written();
        let (denied, other) = if withheld_by_b {
            (&mut a, &mut b)
        } else {
            (&mut b, &mut a)
        };
        run_by(&mut [denied, other], t, t + KEEPALIVE_SPAN, 250);
        let denied = if withheld_by_b { &a } else { &b };
        let sealed = written(&denied.radio)
            .iter()
            .any(|(l, m)| opened.contains(l) && m.kind == Kind::Sealed);
        assert!(sealed, "a session on its own link, though withheld");
    }
}

/// Contacts tell each other their tokens inside the session: when it is
/// confirmed, and at once at every rotation.
#[test]
fn contacts_tell_each_other_their_token_in_session_and_at_every_rotation() {
    let room = Room::new(13);
    let (mut a, mut b) = pair(&room);
    let mut t = run(&mut [&mut a, &mut b], 0, 3_000);
    assert_eq!(a.t.contact_token(&b.me()), b.radio.token());
    assert_eq!(b.t.contact_token(&a.me()), a.radio.token());
    for _ in 0..3 {
        let before = b.radio.token();
        let at = run_until(&mut [&mut a, &mut b], t, t + TOKEN_MS + 1_000, |d| {
            d[1].radio.token() != before
        })
        .expect("b rotates");
        // the next step, a knows
        t = run(&mut [&mut a, &mut b], at + STEP, at + STEP);
        assert_eq!(a.t.contact_token(&b.me()), b.radio.token(), "at {at}");
    }
    // a stranger is never told
    let mut c = Device::new(&room, 3);
    c.knows(&[&a, &b]);
    c.h.set_mode(Some(Mode::Contacts));
    run(&mut [&mut a, &mut b, &mut c], t, t + 30_000);
    assert_eq!(c.t.contact_token(&a.me()), None);
    assert_eq!(c.t.contact_token(&b.me()), None);
}

/// A removed contact's sessions end at once, so it hears no further
/// token, and what it told this device is forgotten.
#[test]
fn a_removed_contact_is_told_no_further_tokens() {
    let room = Room::new(14);
    let (mut a, mut b) = pair(&room);
    let t = run(&mut [&mut a, &mut b], 0, 3_000);
    let known = a.radio.token();
    assert_eq!(b.t.contact_token(&a.me()), known);
    let told = b.t.contact_schedule(&a.me());
    assert_eq!(told, a.t.token_schedule());
    a.t.set_contacts(&[]);
    let t = run(&mut [&mut a, &mut b], t, t + STEP);
    assert_eq!(a.t.contact_token(&b.me()), None, "forgotten");
    // the tokens it was told ahead are drawn again: none will be current
    let accepted = a.t.tokens();
    assert!(
        told[1..].iter().all(|t| !accepted.contains(t)),
        "the schedule b knows is void"
    );
    // a rotates, more than once; b still counts a as a contact and keeps
    // dialling, but a answers nothing and tells it nothing
    let t = run_by(&mut [&mut a, &mut b], t, t + 2 * TOKEN_MS, 250);
    assert_ne!(a.radio.token(), known, "a rotated");
    assert_eq!(
        b.t.contact_schedule(&a.me()),
        told,
        "nothing newer reached b"
    );
    assert!(
        !told.contains(&a.radio.token().unwrap()),
        "nor will it match"
    );
    assert!(b.h.peers().contacts.is_empty());
    let _ = t;
}

/// M-new-3: removing a contact draws new tokens ahead, but the contacts
/// that remain hear the new schedule with their sessions' next
/// keepalives, never in a burst at the moment of removal.
#[test]
fn a_removal_retells_the_schedule_only_with_the_next_keepalive() {
    let room = Room::new(16);
    let (mut a, mut b) = pair(&room);
    let gone = identity(77).public();
    a.t.set_contacts(&[b.me(), gone]);
    let t = run(&mut [&mut a, &mut b], 0, 5_000);
    let before = a.t.token_schedule();
    assert_eq!(b.t.contact_schedule(&a.me()), before);
    a.radio.take_written();
    a.t.set_contacts(&[b.me()]);
    a.poll(t); // the same moment: only the removal is new
    assert!(
        written(&a.radio)
            .iter()
            .all(|(_, m)| m.kind != Kind::Sealed),
        "nothing sealed went at the removal"
    );
    let after = a.t.token_schedule();
    assert_eq!(after[0], before[0], "the current token stays");
    assert_ne!(after, before, "the tokens ahead are new");
    assert_eq!(b.t.contact_schedule(&a.me()), before, "not yet told");
    let told = run_until(
        &mut [&mut a, &mut b],
        t + STEP,
        t + KEEPALIVE_MS + 2_000,
        |d| d[1].t.contact_schedule(&d[0].me()) == after,
    )
    .expect("told with the next keepalive");
    assert!(told > t + STEP, "{told}");
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

/// How soon an envelope draws a reply doesn't mark it: the receiver's
/// acknowledgement waits a random 0 to 400 ms, drawn from its own
/// generator, so it differs from one exchange to the next.
#[test]
fn an_acknowledgement_waits_a_random_moment_of_up_to_400_ms() {
    const FINE: u64 = 10;
    let mut delays = Vec::new();
    for k in 0..8u64 {
        let room = Room::new(90 + k);
        let (mut a, mut b) = (Device::new(&room, 2 * k + 1), Device::new(&room, 2 * k + 2));
        a.knows(&[&b]);
        b.knows(&[&a]);
        a.h.set_mode(Some(Mode::Contacts));
        b.h.set_mode(Some(Mode::Contacts));
        let t = run_until(&mut [&mut a, &mut b], 0, 10_000, |d| {
            d.iter().all(|d| !d.h.peers().contacts.is_empty())
        })
        .expect("together");
        // let the handshakes' own replies settle
        let mut now = run(&mut [&mut a, &mut b], t, t + 2_000);
        a.send(&private(&a, &[&b], "timed"), 1);
        let mut arrived = None;
        let replied = loop {
            now += FINE;
            assert!(now < t + 10_000, "no reply");
            a.poll(now);
            b.radio.take_written();
            b.poll(now);
            let wrote = !b.radio.take_written().is_empty();
            if arrived.is_none() && !b.heard().is_empty() {
                arrived = Some(now);
                if wrote {
                    break now;
                }
                continue;
            }
            if arrived.is_some() && wrote {
                break now;
            }
        };
        delays.push(replied - arrived.unwrap());
    }
    assert!(
        delays.iter().all(|d| *d <= ACK_DELAY_MAX_MS + FINE),
        "{delays:?}"
    );
    let (lo, hi) = (delays.iter().min().unwrap(), delays.iter().max().unwrap());
    assert!(hi - lo >= 100, "they vary: {delays:?}");
}

/// One message from `a` and `b`'s answer, watched from outside at 10 ms
/// steps: what `a` wrote (bytes), and how long after it `b` wrote and how
/// much, if it did within two seconds. `spontaneous` waits for a write of
/// `a`'s own (not an answer to something `b` just wrote).
fn exchange(
    a: &mut Device,
    b: &mut Device,
    mut now: u64,
    spontaneous: bool,
) -> (u64, usize, Option<(u64, usize)>) {
    const FINE: u64 = 10;
    let size = |w: Vec<(LinkId, Vec<u8>)>| w.iter().map(|(_, b)| b.len()).sum::<usize>();
    let mut b_last = None::<u64>;
    let (at, sent, same_step) = loop {
        now += FINE;
        a.poll(now);
        let aw = a.radio.take_written();
        b.poll(now);
        let bw = b.radio.take_written();
        // a's answer to something b just wrote is not the sample
        let sample = !(spontaneous && b_last.is_some_and(|t| now - t < 1_000));
        if sample && !aw.is_empty() {
            // b may answer in the very step it hears
            break (now, size(aw), (!bw.is_empty()).then(|| size(bw)));
        }
        if !bw.is_empty() {
            b_last = Some(now);
        }
        assert!(now < 60_000_000, "a never wrote");
    };
    let mut t = at;
    let mut reply = same_step.map(|n| (0, n));
    while reply.is_none() && t < at + 2_000 {
        t += FINE;
        a.poll(t);
        a.radio.take_written();
        b.poll(t);
        let bw = b.radio.take_written();
        if !bw.is_empty() {
            reply = Some((t - at, size(bw)));
        }
    }
    (t, sent, reply)
}

/// I1: an envelope and a keepalive draw the same answer. Whether a reply
/// comes, its size, and when (a random 0 to 400 ms, from the same
/// generator) are alike, so an observer of the link can't tell which of
/// the two was received. (In steady state the two sides' keepalive timers
/// also fall into step a held moment apart, so this holds partly by that;
/// `session::a_keepalive_draws_an_acknowledgement_and_an_acknowledgement_none`
/// pins the acknowledgement itself.)
#[test]
fn an_envelope_and_a_keepalive_draw_the_same_answer() {
    let (mut envelopes, mut keepalives) = (Vec::new(), Vec::new());
    for k in 0..12u64 {
        let room = Room::new(200 + k);
        let (mut a, mut b) = (
            Device::new(&room, 2 * k + 41),
            Device::new(&room, 2 * k + 42),
        );
        a.knows(&[&b]);
        b.knows(&[&a]);
        a.h.set_mode(Some(Mode::Contacts));
        b.h.set_mode(Some(Mode::Contacts));
        let t = run_until(&mut [&mut a, &mut b], 0, 10_000, |d| {
            d.iter().all(|d| !d.h.peers().contacts.is_empty())
        })
        .expect("together");
        let t = run(&mut [&mut a, &mut b], t, t + 3_000);
        a.radio.take_written();
        b.radio.take_written();
        a.send(&private(&a, &[&b], "short"), 1);
        let (t, sent, reply) = exchange(&mut a, &mut b, t, false);
        envelopes.push((sent, reply.expect("an envelope is answered")));
        // let it settle, then the next thing a writes on its own is a
        // keepalive
        let t = run(&mut [&mut a, &mut b], t, t + 3_000);
        a.radio.take_written();
        b.radio.take_written();
        let (_, sent, reply) = exchange(&mut a, &mut b, t, true);
        keepalives.push((sent, reply.expect("a keepalive is answered")));
    }
    let sizes: BTreeSet<usize> = envelopes
        .iter()
        .chain(&keepalives)
        .flat_map(|(sent, (_, got))| [*sent, *got])
        .collect();
    assert_eq!(
        sizes.len(),
        1,
        "one size for everything: {envelopes:?} {keepalives:?}"
    );
    let delays = |v: &[(usize, (u64, usize))]| v.iter().map(|(_, (d, _))| *d).collect::<Vec<u64>>();
    let (de, dk) = (delays(&envelopes), delays(&keepalives));
    for d in [&de, &dk] {
        assert!(d.iter().all(|d| *d <= ACK_DELAY_MAX_MS + 10), "{d:?}");
        let spread = d.iter().max().unwrap() - d.iter().min().unwrap();
        assert!(spread >= 150, "spread over the range: {d:?}");
    }
    let mean = |d: &[u64]| d.iter().sum::<u64>() as f64 / d.len() as f64;
    assert!(
        (mean(&de) - mean(&dk)).abs() < 150.0,
        "same distribution: {de:?} {dk:?}"
    );
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
    a.t.set_contacts(&[friend.public()]);
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
    a.t.set_contacts(&[friend.public()]);
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
    a.t.set_contacts(&[friend.public()]);
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
    a.t.set_contacts(&[a.me()]);
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
    a.t.set_contacts(&[friend.public()]);
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
    assert_eq!(a.t.tokens()[..2], [second, first]);
    // the raw radio's links were closed on rotation; it links again
    raw.poll();
    a.poll(t);
    raw.poll();
    assert_eq!(
        dial(&mut raw, &mut a, &fk, BASE + t, Some(first), t),
        vec![Kind::Handshake]
    );
}

/// A device that withholds its token gets no hello it could relay to a
/// contact: a contact's token learned in session is named only on a link
/// to an advert showing that token's tag, so on any other link the round
/// is all decoys, and replaying it to the contact gets no answer.
#[test]
fn a_device_withholding_its_token_gets_no_hello_worth_relaying() {
    let room = Room::new(15);
    let (mut a, mut b) = pair(&room);
    let t = run(&mut [&mut a, &mut b], 0, 3_000);
    assert_eq!(a.t.contact_token(&b.me()), b.radio.token());
    a.radio.take_written();
    // x withholds its token from a; y holds a link into b
    let mut x = Raw::with_token(&room, [0x3c; TOKEN_LEN]);
    room.refuse(&a.radio, &x.radio);
    let mut y = Raw::with_token(&room, [0x4d; TOKEN_LEN]);
    let t = run(&mut [&mut a, &mut b], t, t + STEP);
    x.poll();
    y.poll();
    let to_x = links_between(&room, &a.radio, &x.radio);
    let hellos_to_x: Vec<LinkMessage> = written(&a.radio)
        .into_iter()
        .filter(|(l, m)| to_x.contains(l) && m.kind == Kind::Contact)
        .map(|(_, m)| m)
        .collect();
    assert_eq!(hellos_to_x.len(), TRIAL_CAP, "a whole round, all decoys");
    let into_b = room
        .links()
        .into_iter()
        .find(|(_, c, p)| *c == y.radio.index() && *p == b.radio.index())
        .map(|(l, _, _)| l)
        .expect("y's link into b");
    let mut answers = Vec::new();
    for m in &hellos_to_x {
        answers.extend(replay(&mut y, &mut b, into_b, m, t + STEP));
    }
    assert!(
        answers.is_empty(),
        "b answered a relayed hello: {answers:?}"
    );
}

/// A contact who learned a token inside a session may name it for about
/// an hour: the current token and the four before it answer, the fifth
/// back doesn't; the three told ahead answer; another device's don't.
#[test]
fn a_hello_may_name_the_last_four_tokens_but_not_the_fifth_or_anothers() {
    assert_eq!(PAST_TOKENS, 4);
    let room = Room::new(35);
    let (mut a, mut raw, _) = raw_beside(&room);
    let mut other = Device::new(&room, 9);
    other.h.set_mode(Some(Mode::Open));
    let friend = identity(40);
    let fk = SessionKeys::new(&friend);
    a.t.set_contacts(&[friend.public()]);
    let mut t = STEP;
    a.poll(t);
    other.poll(t);
    let mut mine = vec![a.radio.token().unwrap()];
    let mut theirs = vec![other.radio.token().unwrap()];
    while mine.len() < 6 {
        t += 1_000;
        a.poll(t);
        other.poll(t);
        let now = a.radio.token().unwrap();
        if *mine.last().unwrap() != now {
            mine.push(now);
        }
        let now = other.radio.token().unwrap();
        if *theirs.last().unwrap() != now {
            theirs.push(now);
        }
    }
    mine.reverse(); // newest first: mine[k] is k tokens back
    assert_eq!(a.t.tokens()[..5], mine[..5]);
    let ahead = a.t.token_schedule()[1..].to_vec();
    assert_eq!(ahead.len(), FUTURE_TOKENS);
    assert_eq!(a.t.tokens()[5..], ahead[..], "and the three told ahead");
    raw.poll();
    a.poll(t);
    raw.poll();
    for (back, token) in mine.iter().enumerate() {
        t += STEP;
        let answered = dial(&mut raw, &mut a, &fk, BASE + t, Some(*token), t);
        if back <= PAST_TOKENS {
            assert_eq!(answered, vec![Kind::Handshake], "{back} back");
        } else {
            assert!(answered.is_empty(), "{back} back: {answered:?}");
        }
    }
    // the tokens told ahead answer too, before they are current
    for (ahead, token) in ahead.iter().enumerate() {
        t += STEP;
        let answered = dial(&mut raw, &mut a, &fk, BASE + t, Some(*token), t);
        assert_eq!(answered, vec![Kind::Handshake], "{} ahead", ahead + 1);
    }
    // a hello meant for another device, naming any of its recent or
    // coming tokens, relayed here: no answer
    for token in theirs.iter().chain(&other.t.token_schedule()[1..]) {
        t += STEP;
        assert!(dial(&mut raw, &mut a, &fk, BASE + t, Some(*token), t).is_empty());
    }
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
        a.t.set_contacts(&others);
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
    a.t.set_contacts(
        &(0..20)
            .map(|i| identity(100 + i).public())
            .collect::<Vec<_>>(),
    );
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
    a.t.set_contacts(&contacts);
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

/// How long the Macs in the sleep tests slept: well past the two-minute
/// contact window.
const SLEEP: u64 = 10 * 60_000;

#[test]
fn a_mac_that_slept_ten_minutes_still_meets_its_contacts() {
    let room = Room::new(40);
    let clock = TestClock::new(BASE);
    let mut a = Device::with_clock(room.radio(), clock.clone(), 1);
    // b's wall clock reads the true time: ten minutes past a's launch
    // time plus a's monotonic clock, once a has slept
    let mut b = Device::with(room.radio(), BASE + SLEEP, 2);
    a.knows(&[&b]);
    b.knows(&[&a]);
    a.h.set_mode(Some(Mode::Contacts));
    let t = run(&mut [&mut a], 0, 5_000);
    // a sleeps: its monotonic clock stops, its wall clock goes on
    clock.shift(SLEEP as i64);
    b.h.set_mode(Some(Mode::Contacts));
    run(&mut [&mut a, &mut b], t, t + 15_000);
    assert_eq!(a.h.peers().contacts, vec![b.me()]);
    assert_eq!(b.h.peers().contacts, vec![a.me()]);
}

#[test]
fn contact_hellos_are_stamped_and_judged_by_the_wall_clock_as_it_reads_then() {
    let room = Room::new(41);
    let clock = TestClock::new(BASE);
    let mut a = Device::with_clock(room.radio(), clock.clone(), 1);
    let friend = identity(40);
    let fk = SessionKeys::new(&friend);
    a.t.set_contacts(&[friend.public()]);
    a.h.set_mode(Some(Mode::Open));
    let token = [0xee; TOKEN_LEN];
    let mut raw = Raw::with_token(&room, token);
    // a slept after it was launched, before it met anyone
    clock.shift(SLEEP as i64);
    a.poll(0);
    raw.poll();
    a.poll(STEP);
    raw.poll();
    let now = STEP;
    let unix = BASE + SLEEP + now;

    // as the initiator: a's round to raw's advert carries the true time,
    // so the contact it names reads it as fresh
    let link = raw.theirs();
    let fresh = raw
        .got
        .iter()
        .filter(|(l, m)| *l == link && m.kind == Kind::Contact)
        .filter(|(_, m)| {
            Session::contact_responder(&fk, unix, &[token])
                .read_message(&m.body[4..])
                .is_ok()
        })
        .count();
    assert_eq!(fresh, 1, "the one hello to the friend is fresh");

    // as the responder: a hello stamped with the true time is answered
    assert_eq!(
        dial(&mut raw, &mut a, &fk, unix, None, now),
        vec![Kind::Handshake]
    );
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
    let first = a.radio.token();
    while a.radio.token() == first && t <= TOKEN_MS + 10_000 {
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
    assert_ne!(a.radio.token(), first, "a rotated");
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
    a.t.set_contacts(&[identity(99).public()]);
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
fn links_to_strangers_adverts_stop_short_of_the_room_kept_for_contacts() {
    let room = Room::new(54);
    let mut a = Device::new(&room, 1);
    a.h.set_mode(Some(Mode::Open));
    let raws: Vec<Raw> = (0..MAX_LINKS_OUT)
        .map(|i| Raw::with_token(&room, [i as u8; TOKEN_LEN]))
        .collect();
    a.poll(0);
    a.poll(STEP);
    let mine = room
        .links()
        .into_iter()
        .filter(|(_, c, _)| *c == a.radio.index())
        .count();
    assert_eq!(mine, MAX_LINKS_OUT - CONTACT_LINKS, "{} raws", raws.len());
}

/// The links between `a` and `b` that `a` opened.
fn opened_by(room: &Room, a: &Device, b: &Device) -> usize {
    room.links()
        .into_iter()
        .filter(|(_, c, p)| *c == a.radio.index() && *p == b.radio.index())
        .count()
}

#[test]
fn strangers_subscribing_in_numbers_never_keep_a_from_dialling_its_contacts() {
    let room = Room::new(55);
    let mut a = Device::new(&room, 1);
    a.h.set_mode(Some(Mode::Contacts));
    // 40 strangers subscribe to a's advert, more than a takes
    let mut strangers: Vec<Raw> = (0..40).map(|_| Raw::silent(&room)).collect();
    a.poll(0);
    a.poll(STEP);
    for s in &mut strangers {
        s.poll();
    }
    assert_eq!(a.t.links().len(), MAX_LINKS_IN, "a full house of strangers");
    // a contact comes into range: a's link to its advert is let in, and
    // a's hello goes out on it
    let mut b = Device::new(&room, 2);
    a.knows(&[&b]);
    b.knows(&[&a]);
    b.h.set_mode(Some(Mode::Contacts));
    run(&mut [&mut a, &mut b], 2 * STEP, 10_000);
    assert_eq!(opened_by(&room, &a, &b), 1, "a's own link to b is kept");
    assert_eq!(a.h.peers().contacts, vec![b.me()]);
    assert_eq!(b.h.peers().contacts, vec![a.me()]);
}

#[test]
fn a_known_contacts_advert_gets_a_link_when_strangers_adverts_fill_the_rest() {
    let room = Room::new(56);
    let (mut a, mut b) = pair(&room);
    let t = run(&mut [&mut a, &mut b], 0, 5_000);
    assert_eq!(
        a.h.peers().contacts,
        vec![b.me()],
        "they met, and told tokens"
    );
    room.set_in_range(&a.radio, &b.radio, false);
    // more strangers' adverts than a opens links to, out of b's range
    let raws: Vec<Raw> = (0..MAX_LINKS_OUT)
        .map(|i| {
            let raw = Raw::with_token(&room, [0x80 | i as u8; TOKEN_LEN]);
            room.set_in_range(&raw.radio, &b.radio, false);
            raw
        })
        .collect();
    let t = run(&mut [&mut a, &mut b], t, t + 1_000);
    let mine = |a: &Device| {
        room.links()
            .into_iter()
            .filter(|(_, c, _)| *c == a.radio.index())
            .count()
    };
    assert_eq!(
        mine(&a),
        MAX_LINKS_OUT - CONTACT_LINKS,
        "{} raws",
        raws.len()
    );
    // b is back: a knows its advert by the token b told it, and opens a
    // link to it from the room kept back
    room.set_in_range(&a.radio, &b.radio, true);
    run(&mut [&mut a, &mut b], t, t + 5_000);
    assert_eq!(opened_by(&room, &a, &b), 1);
    assert_eq!(a.h.peers().contacts, vec![b.me()]);
}

// ---- presence -----------------------------------------------------------

#[test]
fn strangers_are_counted_once_for_fifteen_minutes_and_listed_while_a_link_is_up() {
    let room = Room::new(60);
    let (mut a, mut c) = (Device::new(&room, 1), Device::new(&room, 3));
    a.h.set_mode(Some(Mode::Open));
    c.h.set_mode(Some(Mode::Open));
    let t = run(&mut [&mut a, &mut c], 0, 1_000);
    let peers = a.h.peers();
    assert_eq!(peers.strangers_in_range(), 1, "one stranger, counted once");
    assert_eq!(peers.strangers.len(), 1, "and a request can go to them");
    assert!(peers.contacts.is_empty());
    assert!(a.events.contains(&TransportEvent::Peers {
        contacts: vec![],
        strangers: vec![peers.strangers[0].0],
        strangers_seen: 1,
    }));
    // idle, the link closes and isn't made again until c's token changes:
    // nothing to ask on, but c is still counted
    let closed = run_until(&mut [&mut a, &mut c], t, t + IDLE_MS + 5_000, |d| {
        d[0].h.peers().strangers.is_empty()
    })
    .expect("the idle link closes");
    assert_eq!(a.h.peers().strangers_in_range(), 1, "still counted");
    room.set_in_range(&a.radio, &c.radio, false);
    let t = run_by(
        &mut [&mut a, &mut c],
        closed,
        closed + STRANGER_MS - 2_000,
        1_000,
    );
    assert_eq!(a.h.peers().strangers_in_range(), 1);
    run_by(&mut [&mut a, &mut c], t, t + 4_000, 500);
    assert_eq!(
        a.h.peers().strangers_in_range(),
        0,
        "gone after fifteen minutes"
    );
}

// ---- requests -----------------------------------------------------------

fn hello_of(d: &Device) -> Envelope {
    pairing_hello(&d.id, "Ada", SENT_MIN)
}

fn open_pair(room: &Room, sa: u64, sc: u64) -> (Device, Device, u64) {
    let (mut a, mut c) = (Device::new(room, sa), Device::new(room, sc));
    a.h.set_mode(Some(Mode::Open));
    c.h.set_mode(Some(Mode::Open));
    let t = run(&mut [&mut a, &mut c], 0, 1_000);
    (a, c, t)
}

/// The requests `d` reported: (request, from, hello).
fn requests(d: &Device) -> Vec<(u64, PublicIdentity, Vec<u8>)> {
    d.events
        .iter()
        .filter_map(|e| match e {
            TransportEvent::Request {
                request,
                from,
                hello,
            } => Some((*request, from.clone(), hello.clone())),
            _ => None,
        })
        .collect()
}

fn gone(d: &Device) -> Vec<u64> {
    d.events
        .iter()
        .filter_map(|e| match e {
            TransportEvent::RequestGone { request } => Some(*request),
            _ => None,
        })
        .collect()
}

fn accepted(d: &Device) -> bool {
    d.events
        .iter()
        .any(|e| matches!(e, TransportEvent::RequestAccepted { .. }))
}

#[test]
fn a_request_reaches_a_stranger_in_open_mode_who_reveals_nothing() {
    let room = Room::new(70);
    let (mut a, mut c, t) = open_pair(&room, 1, 3);
    let link = a.h.peers().strangers[0];
    let hello = hello_of(&a).encode();
    a.t.request(link.0, hello.clone()).unwrap();
    c.radio.take_written();
    run(&mut [&mut a, &mut c], t, t + 2_000);

    let got = requests(&c);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].1, a.me());
    assert_eq!(got[0].2, hello);
    assert!(a.has(&TransportEvent::RequestDelivered { stranger: link.0 }));
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
fn a_request_goes_only_to_a_stranger_listed_now() {
    let room = Room::new(69);
    let (mut a, _c, _) = open_pair(&room, 1, 3);
    assert_eq!(a.t.request(9_999, hello_of(&a).encode()), Err(NOT_IN_RANGE));
    a.poll(1_100);
    assert!(!a
        .events
        .iter()
        .any(|e| matches!(e, TransportEvent::RequestFailed { .. })));
}

#[test]
fn an_accept_goes_back_with_proof_and_both_are_contacts() {
    let room = Room::new(75);
    let (mut a, mut c, t) = open_pair(&room, 1, 3);
    let link = a.h.peers().strangers[0];
    a.t.request(link.0, hello_of(&a).encode()).unwrap();
    let t = run(&mut [&mut a, &mut c], t, t + 2_000);
    let (req, from, _) = requests(&c)[0].clone();
    assert_eq!(from, a.me());
    assert!(!accepted(&a), "nothing yet");
    // as the node does on accept: a contact first, then the answer
    c.knows(&[&a]);
    let answer = pairing_hello(&c.id, "Cy", SENT_MIN).encode();
    c.t.answer_request(req, Some(answer.clone()));
    let done = run_until(&mut [&mut a, &mut c], t, t + 20_000, |d| accepted(d[0]))
        .expect("the accept is proved");
    assert!(a.has(&TransportEvent::RequestAccepted {
        stranger: link.0,
        from: c.me(),
        hello: answer,
    }));
    a.knows(&[&c]);
    let t = run(&mut [&mut a, &mut c], done, done + 2_000);
    assert_eq!(a.h.peers().contacts, vec![c.me()]);
    assert_eq!(c.h.peers().contacts, vec![a.me()]);
    assert!(gone(&c).contains(&req), "the request is over");
    assert!(!a.events.iter().any(|e| matches!(
        e,
        TransportEvent::RequestFailed { .. } | TransportEvent::RequestUnanswered { .. }
    )));
    let env = private(&a, &[&c], "hello Cy");
    a.send(&env, 9);
    run(&mut [&mut a, &mut c], t, t + 3_000);
    assert_eq!(c.heard(), vec![env]);
}

#[test]
fn a_decline_sends_nothing_and_the_request_runs_out_unanswered() {
    let room = Room::new(76);
    let (mut a, mut c, t) = open_pair(&room, 1, 3);
    let link = a.h.peers().strangers[0];
    a.t.request(link.0, hello_of(&a).encode()).unwrap();
    let t = run(&mut [&mut a, &mut c], t, t + 2_000);
    let req = requests(&c)[0].0;
    c.radio.take_written();
    c.t.answer_request(req, None);
    let t = run_by(&mut [&mut a, &mut c], t, t + REQUEST_MS - 2_000, 1_000);
    assert_eq!(c.t.requests_reported(), 0, "no longer shown");
    assert_eq!(c.t.requests_held(), 1, "held quietly, as an ignored one is");
    assert!(
        written(&c.radio).iter().all(|(l, _)| l.0 != req),
        "nothing sent back"
    );
    assert!(!a.events.iter().any(|e| matches!(
        e,
        TransportEvent::RequestFailed { .. } | TransportEvent::RequestUnanswered { .. }
    )));
    run_by(&mut [&mut a, &mut c], t, t + 4_000, 1_000);
    // delivered, so it ended unanswered: never "not delivered"
    assert!(a.has(&TransportEvent::RequestUnanswered { stranger: link.0 }));
    assert!(!a
        .events
        .iter()
        .any(|e| matches!(e, TransportEvent::RequestFailed { .. })));
    assert!(!accepted(&a));
}

#[test]
fn a_delivered_request_whose_link_drops_ends_unanswered() {
    let room = Room::new(77);
    let (mut a, mut c, t) = open_pair(&room, 1, 3);
    let link = a.h.peers().strangers[0];
    a.t.request(link.0, hello_of(&a).encode()).unwrap();
    let t = run(&mut [&mut a, &mut c], t, t + 2_000);
    assert!(a.has(&TransportEvent::RequestDelivered { stranger: link.0 }));
    room.set_in_range(&a.radio, &c.radio, false);
    run(&mut [&mut a, &mut c], t, t + 2_000);
    assert!(a.has(&TransportEvent::RequestUnanswered { stranger: link.0 }));
    assert!(!a
        .events
        .iter()
        .any(|e| matches!(e, TransportEvent::RequestFailed { .. })));
}

/// When the requester's link closes, after a decline or with no answer:
/// the same moment, so a decline can't be told from being ignored.
fn request_link_closes(decline: bool) -> u64 {
    let room = Room::new(68);
    let (mut a, mut c, t) = open_pair(&room, 1, 3);
    let link = a.h.peers().strangers[0];
    a.t.request(link.0, hello_of(&a).encode()).unwrap();
    let t = run(&mut [&mut a, &mut c], t, t + 2_000);
    if decline {
        c.t.answer_request(requests(&c)[0].0, None);
    }
    let mut now = t;
    while a.t.links().contains(&link) {
        now += 1_000;
        a.poll(now);
        c.poll(now);
        assert!(now < 2 * TOKEN_MS, "the link closes in time");
    }
    now
}

#[test]
fn a_declined_request_keeps_its_link_as_long_as_an_ignored_one() {
    let ignored = request_link_closes(false);
    assert_eq!(request_link_closes(true), ignored);
    assert!(ignored > REQUEST_MS, "held for the request's whole life");
}

#[test]
fn an_answer_is_proved_only_by_its_own_key() {
    let room = Room::new(77);
    let (mut a, mut c, t) = open_pair(&room, 1, 3);
    let link = a.h.peers().strangers[0];
    a.t.request(link.0, hello_of(&a).encode()).unwrap();
    let t = run(&mut [&mut a, &mut c], t, t + 2_000);
    let req = requests(&c)[0].0;
    // c answers with someone else's hello: its proof, as c, proves nothing
    c.knows(&[&a]);
    let dee = identity(9);
    c.t.answer_request(req, Some(pairing_hello(&dee, "Dee", SENT_MIN).encode()));
    run(&mut [&mut a, &mut c], t, t + 30_000);
    assert!(!accepted(&a));
    assert!(a.h.peers().contacts.is_empty());
}

/// A silent stranger beside `a`, and the one link it opened to it.
fn stranger_beside(room: &Room, a: &mut Device, now: u64) -> (Raw, LinkId) {
    let mut r = Raw::silent(room);
    a.poll(now);
    r.poll();
    let link = r.opened().0;
    (r, link)
}

#[test]
fn a_hello_that_is_not_an_envelope_never_reaches_the_node() {
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
    assert_eq!(a.t.requests_reported(), 0, "never shown");
    assert_eq!(a.t.requests_held(), 1, "but held as an ignored one is");
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

/// Review Focus 2: one stranger asking six hundred times a minute, on one
/// link under a new identity each time and under one identity on several
/// links, gets one request reported each way, and pushes out no one
/// else's.
#[test]
fn a_flood_of_requests_from_one_stranger_pushes_out_no_one_elses() {
    let room = Room::new(72);
    let mut a = Device::new(&room, 1);
    a.h.set_mode(Some(Mode::Open));
    let hello = hello_of(&a).encode();
    let mut now = STEP;
    // five people ask, each on their own link
    for i in 0..5u64 {
        let (mut r, l) = stranger_beside(&room, &mut a, now);
        let k = SessionKeys::new(&identity(70 + i));
        assert!(raw_request(&mut r, &mut a, l, &k, 1, &hello, now));
        now += 1_000;
    }
    assert_eq!(requests(&a).len(), 5);

    // the flood: one link, six hundred a minute, a new identity each time
    let (mut flood, link) = stranger_beside(&room, &mut a, now);
    let mut answered = 0;
    for i in 0..600u32 {
        let k = SessionKeys::new(&identity(1_000 + u64::from(i)));
        if raw_request(&mut flood, &mut a, link, &k, i, &hello, now) {
            answered += 1;
        }
        now += 100;
    }
    assert_eq!(answered, 1, "only the first on the link is even answered");
    assert_eq!(requests(&a).len(), 6, "one more request");

    // one identity on several links: one request
    let same = SessionKeys::new(&identity(500));
    for _ in 0..5 {
        let (mut r, l) = stranger_beside(&room, &mut a, now);
        raw_request(&mut r, &mut a, l, &same, 1, &hello, now);
        now += 1_000;
    }
    assert_eq!(requests(&a).len(), 7, "one more request");
    assert!(gone(&a).is_empty(), "no one else's request went");
    assert_eq!(a.t.requests_reported(), 7);
}

/// Whether a requester's handshake, begun before `churn` other links each
/// send a message 1, still finishes: the part-way cap is the bound a
/// stranger with that many links can hold against a real requester.
fn finishes_after_churn(churn: usize) -> bool {
    let room = Room::new(67);
    let mut a = Device::new(&room, 1);
    a.h.set_mode(Some(Mode::Open));
    let mut now = STEP;
    let (mut real, link) = stranger_beside(&room, &mut a, now);
    let keys = SessionKeys::new(&identity(60));
    let mut hs = Session::request_initiator(&keys);
    let with_id = |rest: &[u8]| {
        let mut b = 1u32.to_be_bytes().to_vec();
        b.extend_from_slice(rest);
        b
    };
    real.write(
        link,
        &LinkMessage {
            kind: Kind::Request,
            flags: FROM_INITIATOR,
            body: with_id(&hs.write_message().unwrap()),
        },
    );
    a.poll(now);
    real.poll();
    let reply = real
        .got
        .iter()
        .find(|(l, m)| *l == link && m.kind == Kind::Handshake)
        .map(|(_, m)| m.body[4..].to_vec())
        .expect("answered");
    // others open handshakes and leave them part way
    for i in 0..churn {
        now += STEP;
        let (mut r, l) = stranger_beside(&room, &mut a, now);
        let mut other = Session::request_initiator(&SessionKeys::new(&identity(200 + i as u64)));
        r.write(
            l,
            &LinkMessage {
                kind: Kind::Request,
                flags: FROM_INITIATOR,
                body: with_id(&other.write_message().unwrap()),
            },
        );
        a.poll(now);
    }
    hs.read_message(&reply).unwrap();
    real.write(
        link,
        &LinkMessage {
            kind: Kind::Handshake,
            flags: FROM_INITIATOR,
            body: with_id(&hs.write_message().unwrap()),
        },
    );
    let t = hs.into_transport().unwrap();
    let hello = hello_of(&a).encode();
    let mut plain = vec![1u8, 0, 0, 0, 0, 0, 0, 0, 0];
    plain.extend_from_slice(&(hello.len() as u16).to_be_bytes());
    plain.extend_from_slice(&hello);
    plain.resize(bucket(plain.len()), 0);
    let mut body = with_id(&0u64.to_be_bytes());
    body.extend_from_slice(&t.encrypt(0, &plain).unwrap());
    real.write(
        link,
        &LinkMessage {
            kind: Kind::Sealed,
            flags: FROM_INITIATOR,
            body,
        },
    );
    a.poll(now + STEP);
    !requests(&a).is_empty()
}

/// Review M4: a stranger with that many radios can keep pushing out
/// handshakes part way. The bound is pinned: fewer than
/// REQUESTS_PART_WAY_CAP others leave a real requester's handshake
/// alone, that many push it out. Once its hello has arrived it is never
/// pushed out this way (the flood test).
#[test]
fn handshakes_part_way_are_pushed_out_only_past_their_cap() {
    assert!(finishes_after_churn(REQUESTS_PART_WAY_CAP - 1));
    assert!(!finishes_after_churn(REQUESTS_PART_WAY_CAP));
}

#[test]
fn at_most_twenty_are_held_the_oldest_goes_and_an_accepted_one_never_does() {
    let room = Room::new(78);
    let mut a = Device::new(&room, 1);
    a.h.set_mode(Some(Mode::Open));
    let hello = hello_of(&a).encode();
    let mut now = STEP;
    let ask = |a: &mut Device, i: u64, now: u64| {
        let (mut r, l) = stranger_beside(&room, a, now);
        let k = SessionKeys::new(&identity(100 + i));
        assert!(raw_request(&mut r, a, l, &k, 1, &hello, now));
        requests(a).last().unwrap().0
    };
    let first = ask(&mut a, 0, now);
    a.t.answer_request(first, Some(hello_of(&a).encode()));
    now += 200;
    let second = ask(&mut a, 1, now);
    for i in 2..=REQUESTS_CAP as u64 + 1 {
        now += 200;
        ask(&mut a, i, now);
    }
    assert_eq!(requests(&a).len(), REQUESTS_CAP + 2);
    assert_eq!(gone(&a), vec![second], "the oldest not accepted went");
    assert_eq!(
        a.t.requests_reported(),
        REQUESTS_CAP + 1,
        "and the accepted one stays"
    );
}

#[test]
fn a_request_is_forgotten_ten_minutes_after_it_arrived() {
    let room = Room::new(73);
    let (mut a, mut raw, link) = raw_beside(&room);
    let keys = SessionKeys::new(&identity(60));
    let hello = hello_of(&a).encode();
    assert!(raw_request(&mut raw, &mut a, link, &keys, 1, &hello, STEP));
    let req = requests(&a)[0].0;
    let mut t = STEP;
    while t < REQUEST_MS - 1_000 {
        t += 1_000;
        a.poll(t);
    }
    assert_eq!(a.t.requests_held(), 1);
    a.poll(t + 2_000);
    assert_eq!(a.t.requests_held(), 0);
    assert_eq!(gone(&a), vec![req]);
    assert_eq!(a.t.held_ahead(), 0);
}

#[test]
fn requests_are_not_answered_or_sent_in_contacts_mode() {
    let room = Room::new(74);
    let (mut a, mut c) = (Device::new(&room, 1), Device::new(&room, 3));
    a.h.set_mode(Some(Mode::Open));
    c.h.set_mode(Some(Mode::Contacts));
    let t = run(&mut [&mut a, &mut c], 0, 1_000);
    assert!(
        c.h.peers().strangers.is_empty(),
        "none to ask in Contacts mode"
    );
    let link = a.h.peers().strangers[0];
    a.t.request(link.0, hello_of(&a).encode()).unwrap();
    c.radio.take_written();
    a.radio.take_written();
    let t = run(&mut [&mut a, &mut c], t, t + 20_000);
    assert!(requests(&c).is_empty());
    assert!(a.has(&TransportEvent::RequestFailed { stranger: link.0 }));
    let tries = written(&a.radio)
        .iter()
        .filter(|(l, m)| *l == link && m.kind == Kind::Request)
        .count();
    assert_eq!(
        tries, REQUEST_ATTEMPTS as usize,
        "tried that often, then failed"
    );
    assert!(written(&c.radio)
        .iter()
        .all(|(_, m)| m.kind == Kind::Contact));

    // and a device in Contacts mode sends none
    a.h.set_mode(Some(Mode::Contacts));
    assert_eq!(
        a.t.request(link.0, hello_of(&a).encode()),
        Err(REQUESTS_NEED_OPEN)
    );
    run(&mut [&mut a, &mut c], t, t + 500);
}

#[test]
fn turning_to_contacts_mode_ends_every_request_both_ways() {
    let room = Room::new(79);
    let (mut a, mut c, t) = open_pair(&room, 1, 3);
    let link = a.h.peers().strangers[0];
    a.t.request(link.0, hello_of(&a).encode()).unwrap();
    let t = run(&mut [&mut a, &mut c], t, t + 2_000);
    let req = requests(&c)[0].0;
    c.h.set_mode(Some(Mode::Contacts));
    a.h.set_mode(Some(Mode::Contacts));
    run(&mut [&mut a, &mut c], t, t + 200);
    assert_eq!(gone(&c), vec![req], "no request shown in Contacts mode");
    assert!(a.has(&TransportEvent::RequestUnanswered { stranger: link.0 }));
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
    a.t.request(link.0, hello_of(&a).encode()).unwrap();
    a.poll(t + STEP);
    a.h.set_mode(None);
    a.poll(t + 2 * STEP);
    assert!(!a.radio.is_on() && !a.t.is_running());
    assert!(a.has(&TransportEvent::Dropped(6)));
    assert!(a.has(&TransportEvent::RequestFailed { stranger: link.0 }));
    assert_eq!(a.radio.token(), None);
    assert_eq!(a.h.peers(), Default::default());
    c.poll(t + 3 * STEP);

    // locked: no keys, no radio, whatever the mode
    a.h.set_mode(Some(Mode::Open));
    a.t.set_identity(None);
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
