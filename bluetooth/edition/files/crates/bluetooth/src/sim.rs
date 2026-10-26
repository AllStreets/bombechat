//! Tests of the whole stack below the node: transports over the
//! CoreBluetooth radio's adapter, over a simulated CoreBluetooth whose
//! device identifiers never change (as between two Macs that keep each
//! other's identity), so every rule about withholding the token is in
//! play.

use std::sync::{Arc, Mutex};

use bombechat_core::{seal_private, Envelope, Identity};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_node::{Transport, TransportEvent};
use bombechat_session::{SessionKeys, TOKEN_LEN};

use crate::adapter::{parse_tag, Access, Adapter, CbEvent, PeerId, Power, Role, Stack, TOKEN_MARK};
use crate::radio::Mode;
use crate::transport::{BleTransport, FUTURE_TOKENS, TOKEN_MS};
use crate::{BleHandle, SeededRng};

struct Station {
    /// The station whose identifier this one shows: itself, or another
    /// it copies.
    ident: usize,
    open: bool,
    advert: Option<String>,
    scanning: bool,
    inbox: Vec<CbEvent>,
}

/// The air: every station, the connections between them (central,
/// peripheral), the clock, whether they are out of range, and everything
/// that crossed it.
#[derive(Default)]
struct Air {
    stations: Vec<Station>,
    conns: Vec<(usize, usize)>,
    now: u64,
    apart: bool,
    /// (from, to, bytes, when): every write and notification delivered.
    carried: Vec<(usize, usize, Vec<u8>, u64)>,
    /// The notifications among them.
    notes: Vec<(usize, usize, Vec<u8>, u64)>,
    /// Random draws the stacks made, for fresh seeds.
    drawn: u64,
}

fn id(i: usize) -> PeerId {
    [i as u8 + 1; 16]
}

impl Air {
    /// The identifier station `i` shows.
    fn ident(&self, i: usize) -> PeerId {
        id(self.stations[i].ident)
    }

    /// Every connection ends, as when the two go out of range.
    fn cut_all(&mut self) {
        for (c, p) in std::mem::take(&mut self.conns) {
            let (ic, ip) = (self.ident(c), self.ident(p));
            self.stations[c].inbox.push(CbEvent::Disconnected(ip));
            self.stations[p].inbox.push(CbEvent::Unsubscribed(ic));
        }
    }

    /// Notifications `from` sent `to`.
    fn notified(&self, from: usize, to: usize, since: u64) -> Vec<&[u8]> {
        self.notes
            .iter()
            .filter(|(f, t, _, at)| *f == from && *t == to && *at >= since)
            .map(|(_, _, b, _)| b.as_slice())
            .collect()
    }
}

#[derive(Clone)]
struct SimStack {
    air: Arc<Mutex<Air>>,
    me: usize,
}

impl Stack for SimStack {
    fn access(&self) -> Access {
        Access::Allowed
    }
    fn open(&mut self) {
        let mut air = self.air.lock().unwrap();
        let st = &mut air.stations[self.me];
        st.open = true;
        st.inbox.push(CbEvent::State(Role::Central, Power::On));
        st.inbox.push(CbEvent::State(Role::Peripheral, Power::On));
    }
    fn close(&mut self) {
        let mut air = self.air.lock().unwrap();
        let me = self.me;
        let (mine, rest): (Vec<_>, Vec<_>) =
            air.conns.iter().partition(|(c, p)| *c == me || *p == me);
        air.conns = rest;
        for (c, p) in mine {
            if c == me {
                let ic = air.ident(c);
                air.stations[p].inbox.push(CbEvent::Unsubscribed(ic));
            } else {
                let ip = air.ident(p);
                air.stations[c].inbox.push(CbEvent::Disconnected(ip));
            }
        }
        let st = &mut air.stations[me];
        st.open = false;
        st.advert = None;
        st.scanning = false;
        st.inbox.clear();
    }
    fn is_open(&self) -> bool {
        self.air.lock().unwrap().stations[self.me].open
    }
    fn now_ms(&self) -> u64 {
        self.air.lock().unwrap().now
    }
    fn events(&mut self) -> Vec<CbEvent> {
        let mut air = self.air.lock().unwrap();
        let me = self.me;
        // every advert in range is reported, as with AllowDuplicates
        if air.stations[me].scanning && !air.apart {
            let seen: Vec<CbEvent> = (0..air.stations.len())
                .filter(|i| *i != me && air.stations[*i].open)
                .filter_map(|i| {
                    let name = air.stations[i].advert.as_ref()?;
                    Some(CbEvent::Discovered(air.ident(i), parse_tag(name)))
                })
                .collect();
            air.stations[me].inbox.extend(seen);
        }
        std::mem::take(&mut air.stations[me].inbox)
    }
    fn add_service(&mut self) {
        let mut air = self.air.lock().unwrap();
        air.stations[self.me]
            .inbox
            .push(CbEvent::ServiceAdded(true));
    }
    fn remove_services(&mut self) {}
    fn start_advertising(&mut self, local_name: &str) {
        let mut air = self.air.lock().unwrap();
        let st = &mut air.stations[self.me];
        st.advert = Some(local_name.to_string());
        st.inbox.push(CbEvent::Advertising(true));
    }
    fn stop_advertising(&mut self) {
        self.air.lock().unwrap().stations[self.me].advert = None;
    }
    fn scan(&mut self) {
        self.air.lock().unwrap().stations[self.me].scanning = true;
    }
    fn stop_scan(&mut self) {
        self.air.lock().unwrap().stations[self.me].scanning = false;
    }
    fn connect(&mut self, peer: PeerId) {
        let mut air = self.air.lock().unwrap();
        let me = self.me;
        let there = (0..air.stations.len()).find(|p| {
            *p != me
                && air.ident(*p) == peer
                && air.stations[*p].open
                && air.stations[*p].advert.is_some()
        });
        let Some(p) = there.filter(|_| !air.apart) else {
            air.stations[me].inbox.push(CbEvent::Failed(peer));
            return;
        };
        air.conns.push((me, p));
        let im = air.ident(me);
        air.stations[me].inbox.push(CbEvent::Ready(peer));
        air.stations[p].inbox.push(CbEvent::Subscribed(im));
    }
    fn cancel(&mut self, peer: PeerId) {
        let mut air = self.air.lock().unwrap();
        let me = self.me;
        let gone: Vec<(usize, usize)> = air
            .conns
            .iter()
            .copied()
            .filter(|(c, p)| *c == me && air.ident(*p) == peer)
            .collect();
        air.conns.retain(|c| !gone.contains(c));
        let im = air.ident(me);
        for (_, p) in gone {
            air.stations[p].inbox.push(CbEvent::Unsubscribed(im));
        }
    }
    fn write(&mut self, peer: PeerId, bytes: &[u8]) -> bool {
        let mut air = self.air.lock().unwrap();
        let me = self.me;
        let Some(&(_, p)) = air
            .conns
            .iter()
            .find(|(c, p)| *c == me && air.ident(*p) == peer)
        else {
            return false;
        };
        let (im, now) = (air.ident(me), air.now);
        air.carried.push((me, p, bytes.to_vec(), now));
        air.stations[p]
            .inbox
            .push(CbEvent::Written(im, bytes.to_vec()));
        true
    }
    fn notify(&mut self, central: PeerId, bytes: &[u8]) -> bool {
        let mut air = self.air.lock().unwrap();
        let me = self.me;
        let Some(&(c, _)) = air
            .conns
            .iter()
            .find(|(c, p)| *p == me && air.ident(*c) == central)
        else {
            return false;
        };
        let (im, now) = (air.ident(me), air.now);
        air.carried.push((me, c, bytes.to_vec(), now));
        air.notes.push((me, c, bytes.to_vec(), now));
        air.stations[c]
            .inbox
            .push(CbEvent::Notified(im, bytes.to_vec()));
        true
    }
    fn max_write(&self, _peer: PeerId) -> usize {
        182
    }
    fn max_notify(&self, _central: PeerId) -> usize {
        182
    }
    fn random(&mut self, out: &mut [u8]) {
        let mut air = self.air.lock().unwrap();
        air.drawn = air.drawn.wrapping_add(1);
        let mut rng = SeededRng::new(air.drawn ^ (self.me as u64) << 32);
        rand_core::RngCore::fill_bytes(&mut rng, out);
    }
}

type Radio = Adapter<SimStack>;

struct Mac {
    me: usize,
    t: BleTransport<Radio>,
    h: BleHandle,
    id: Identity,
    heard: Reassembler,
    got: Vec<Envelope>,
    events: Vec<TransportEvent>,
    /// Every token it advertised, and when it began.
    tokens: Vec<([u8; TOKEN_LEN], u64)>,
}

const BASE: u64 = 1_800_000_000_000;

impl Mac {
    fn new(air: &Arc<Mutex<Air>>, seed: u64) -> Mac {
        Mac::showing(air, seed, None)
    }

    /// A station showing another's identifier (`copy`), or its own.
    fn showing(air: &Arc<Mutex<Air>>, seed: u64, copy: Option<usize>) -> Mac {
        let me = {
            let mut a = air.lock().unwrap();
            let me = a.stations.len();
            a.stations.push(Station {
                ident: copy.unwrap_or(me),
                open: false,
                advert: None,
                scanning: false,
                inbox: Vec::new(),
            });
            me
        };
        let radio = Adapter::new(SimStack {
            air: air.clone(),
            me,
        });
        let t = BleTransport::with_rng(radio, BASE, Box::new(SeededRng::new(seed)));
        let h = t.handle();
        let mut secret = [0u8; 64];
        rand_core::RngCore::fill_bytes(&mut SeededRng::new(seed ^ 0x5a), &mut secret);
        let id = Identity::from_secret_bytes(&secret);
        h.set_identity(Some(SessionKeys::new(&id)));
        Mac {
            me,
            t,
            h,
            id,
            heard: Reassembler::new(),
            got: Vec::new(),
            events: Vec::new(),
            tokens: Vec::new(),
        }
    }

    fn poll(&mut self, now: u64) {
        for e in self.t.poll(now) {
            if let TransportEvent::Frame(f) = &e {
                for done in self.heard.push(f, now) {
                    if let Event::Complete { bytes, .. } = done {
                        if let Ok(env) = Envelope::decode(&bytes) {
                            self.got.push(env);
                        }
                    }
                }
            }
            self.events.push(e);
        }
        self.h.take_events();
        if let Some(&t) = self.t.tokens().first() {
            if self.tokens.last().is_none_or(|(last, _)| *last != t) {
                self.tokens.push((t, now));
            }
        }
    }

    fn token(&self) -> [u8; TOKEN_LEN] {
        self.tokens.last().expect("running").0
    }

    fn send(&mut self, to: &Mac, text: &str, tag: u16) -> Envelope {
        let env = seal_private(&self.id, &[to.id.public()], text, 4_000_000_000, 60);
        for f in split(&env.encode(), tag) {
            self.t.send(f);
        }
        env
    }
}

fn run(air: &Arc<Mutex<Air>>, macs: &mut [&mut Mac], from: u64, to: u64) -> u64 {
    let mut now = from;
    while now <= to {
        air.lock().unwrap().now = now;
        for m in macs.iter_mut() {
            m.poll(now);
        }
        now += 250;
    }
    to
}

fn present(a: &Mac, b: &Mac) -> bool {
    a.h.peers().contacts == vec![b.id.public()] && b.h.peers().contacts == vec![a.id.public()]
}

/// Messages both ways within 10 s.
fn talk(air: &Arc<Mutex<Air>>, a: &mut Mac, b: &mut Mac, t: u64, tag: u16) -> u64 {
    let to_b = a.send(b, "to b", tag);
    let to_a = b.send(a, "to a", tag);
    let t = run(air, &mut [a, b], t, t + 10_000);
    assert!(b.got.contains(&to_b), "b heard a at {t}");
    assert!(a.got.contains(&to_a), "a heard b at {t}");
    t
}

/// First notifications shaped like a token: how many were one of the
/// sender's real tokens, and how many decoys (its tag, but not a token).
fn counts(notes: &[&[u8]], sender: &Mac) -> (usize, usize) {
    let shaped: Vec<&[u8]> = notes
        .iter()
        .filter(|n| n.len() == 1 + TOKEN_LEN && n[0] == TOKEN_MARK)
        .map(|n| &n[1..])
        .collect();
    let real = |t: &[u8]| sender.tokens.iter().any(|(k, _)| k[..] == *t);
    let tagged = |t: &[u8]| sender.tokens.iter().any(|(k, _)| k[..6] == t[..6]);
    let served = shaped.iter().filter(|t| real(t)).count();
    let decoys = shaped.iter().filter(|t| !real(t) && tagged(t)).count();
    (served, decoys)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Two contacts whose identifiers never change: once both have rotated,
/// each withholds its token from the other in both directions. They keep
/// meeting anyway, on the tokens they tell each other inside their
/// sessions, after three rotations each and after a 90 s blackout that
/// spans a rotation, and keep talking.
#[test]
fn contacts_with_stable_identifiers_keep_meeting_across_rotations_and_a_blackout() {
    let air = Arc::new(Mutex::new(Air::default()));
    let mut a = Mac::new(&air, 1);
    let mut b = Mac::new(&air, 2);
    a.h.set_contacts(vec![b.id.public()]);
    b.h.set_contacts(vec![a.id.public()]);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    let t = run(&air, &mut [&mut a, &mut b], 0, 5_000);
    assert!(present(&a, &b));
    let mut t = talk(&air, &mut a, &mut b, t, 1);
    // three tokens each: every link they opened is replaced, and each
    // has heard the other under an earlier token
    for round in 0..3u16 {
        t = run(&air, &mut [&mut a, &mut b], t, t + TOKEN_MS + 60_000);
        assert!(present(&a, &b), "together after rotation {round}");
        assert_eq!(a.t.contact_token(&b.id.public()), Some(b.token()));
        assert_eq!(b.t.contact_token(&a.id.public()), Some(a.token()));
        t = talk(&air, &mut a, &mut b, t, 10 + round);
    }
    assert!(
        a.tokens.len() > 3 && b.tokens.len() > 3,
        "three rotations each"
    );
    // once both had rotated, no clear token passed between them, either
    // way, and each was given decoys
    let since = a.tokens[1].1.max(b.tokens[1].1);
    for (from, to) in [(&a, &b), (&b, &a)] {
        let (tokens, decoys) = counts(&air.lock().unwrap().notified(from.me, to.me, since), from);
        assert_eq!(tokens, 0, "{} gave {} its token", from.me, to.me);
        assert!(decoys > 0, "{} never gave {} a decoy", from.me, to.me);
    }

    // a blackout longer than every session's silence limit, across one
    // rotation
    let (na, nb) = (
        a.t.next_rotation_ms().unwrap(),
        b.t.next_rotation_ms().unwrap(),
    );
    let first = na.min(nb);
    assert!(na.max(nb) > first + 60_000, "only one rotates in the dark");
    t = run(&air, &mut [&mut a, &mut b], t, first - 30_000);
    let before = (a.tokens.len(), b.tokens.len());
    air.lock().unwrap().apart = true;
    air.lock().unwrap().cut_all();
    t = run(&air, &mut [&mut a, &mut b], t, t + 90_000);
    assert!(!present(&a, &b), "apart");
    assert_eq!(
        a.tokens.len() + b.tokens.len(),
        before.0 + before.1 + 1,
        "a rotation in the dark"
    );
    air.lock().unwrap().apart = false;
    t = run(&air, &mut [&mut a, &mut b], t, t + 60_000);
    assert!(present(&a, &b), "together again after the blackout");
    talk(&air, &mut a, &mut b, t, 20);
}

/// A stranger that copies a contact's identifier (its address), after
/// this device's rotation: it gets a link but no token, its hellos get no
/// answer, and as it is no contact no session tells it the token. It
/// learns nothing of the new token.
#[test]
fn a_stranger_with_a_copied_identifier_gains_nothing_after_a_rotation() {
    let air = Arc::new(Mutex::new(Air::default()));
    let mut a = Mac::new(&air, 1);
    let mut b = Mac::new(&air, 2);
    a.h.set_contacts(vec![b.id.public()]);
    b.h.set_contacts(vec![a.id.public()]);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    let t = run(&air, &mut [&mut a, &mut b], 0, 5_000);
    assert!(present(&a, &b));
    // past b's rotation, then a leaves
    let rotated = b.t.next_rotation_ms().unwrap() + 1_000;
    let t = run(&air, &mut [&mut a, &mut b], t, rotated);
    assert!(b.tokens.len() >= 2);
    a.h.set_mode(None);
    let t = run(&air, &mut [&mut a, &mut b], t, t + 1_000);
    // the stranger shows a's identifier, and knows b's key
    let mut s = Mac::showing(&air, 3, Some(a.me));
    s.h.set_contacts(vec![b.id.public()]);
    s.h.set_mode(Some(Mode::Contacts));
    let from = t;
    let t = run(&air, &mut [&mut b, &mut s], t, t + 60_000);
    let air_now = air.lock().unwrap();
    let (tokens, decoys) = counts(&air_now.notified(b.me, s.me, from), &b);
    assert_eq!(tokens, 0, "no clear token");
    assert!(decoys > 0, "a link, with a decoy");
    // nothing that reached it carries b's token, or any b will rotate to,
    // in the clear or not
    let current = b.token();
    let schedule = b.t.token_schedule();
    assert_eq!(schedule.len(), 1 + FUTURE_TOKENS);
    let reached: Vec<&Vec<u8>> = air_now
        .carried
        .iter()
        .filter(|(_, to, _, at)| *to == s.me && *at >= from)
        .map(|(_, _, bytes, _)| bytes)
        .collect();
    assert!(!reached.is_empty());
    assert!(reached
        .iter()
        .all(|bytes| schedule.iter().all(|t| !contains(bytes, t))));
    drop(air_now);
    assert_eq!(s.t.contact_token(&b.id.public()), None);
    assert!(s.h.peers().contacts.is_empty(), "no session");
    assert!(b.h.peers().contacts.is_empty());
    assert_eq!(b.token(), current, "within one token");
    let _ = t;
}

/// Run until `done`, at most to `limit`; the time it first held.
fn run_until(
    air: &Arc<Mutex<Air>>,
    macs: &mut [&mut Mac],
    from: u64,
    limit: u64,
    done: impl Fn(&[&mut Mac]) -> bool,
) -> Option<u64> {
    let mut now = from;
    while now <= limit {
        air.lock().unwrap().now = now;
        for m in macs.iter_mut() {
            m.poll(now);
        }
        if done(macs) {
            return Some(now);
        }
        now += 250;
    }
    None
}

/// Two stable-identifier contacts who have each rotated once together
/// (so each withholds its token from the other), then go apart while
/// each rotates `apart` more times. Returns them, the clock, and when
/// they came back in range.
fn apart_through(apart: usize) -> (Arc<Mutex<Air>>, Mac, Mac, u64) {
    let air = Arc::new(Mutex::new(Air::default()));
    let mut a = Mac::new(&air, 1);
    let mut b = Mac::new(&air, 2);
    a.h.set_contacts(vec![b.id.public()]);
    b.h.set_contacts(vec![a.id.public()]);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    let t = run_until(&air, &mut [&mut a, &mut b], 0, TOKEN_MS * 2, |m| {
        m[0].tokens.len() >= 2 && m[1].tokens.len() >= 2
    })
    .expect("both rotate");
    let t = run(&air, &mut [&mut a, &mut b], t, t + 30_000);
    assert!(present(&a, &b));
    let before = (a.tokens.len(), b.tokens.len());
    air.lock().unwrap().apart = true;
    air.lock().unwrap().cut_all();
    let t = run_until(&air, &mut [&mut a, &mut b], t, t + 10 * TOKEN_MS, |m| {
        m[0].tokens.len() >= before.0 + apart && m[1].tokens.len() >= before.1 + apart
    })
    .expect("they rotate apart");
    // a moment longer, so neither is about to
    let t = run(&air, &mut [&mut a, &mut b], t, t + 30_000);
    assert!(!present(&a, &b));
    assert_eq!(
        (a.tokens.len() - before.0, b.tokens.len() - before.1),
        (apart, apart),
        "exactly {apart} rotations each in the dark"
    );
    air.lock().unwrap().apart = false;
    (air, a, b, t)
}

/// Apart while both rotate twice: each still recognises the other's
/// advert from the schedule told in their last session, names that token
/// though it is withheld, and they meet again within 60 s, with no clear
/// token between them.
#[test]
fn contacts_apart_through_two_rotations_each_meet_again_within_a_minute() {
    let (air, mut a, mut b, back) = apart_through(2);
    let met = run_until(&air, &mut [&mut a, &mut b], back, back + 60_000, |m| {
        present(m[0], m[1])
    })
    .expect("together again within 60 s");
    let t = run(&air, &mut [&mut a, &mut b], met, met + 1_000);
    for (from, to) in [(&a, &b), (&b, &a)] {
        let (tokens, _) = counts(&air.lock().unwrap().notified(from.me, to.me, back), from);
        assert_eq!(tokens, 0, "met on the schedule, not a clear token");
    }
    assert_eq!(a.t.contact_token(&b.id.public()), Some(b.token()));
    talk(&air, &mut a, &mut b, t, 30);
}

/// Apart through four rotations each, past the schedule: neither
/// recognises the other's advert, so they meet only once one is taken
/// for new and given the other's token in the clear, an hour after its
/// last traffic on a link given the token (the documented cost).
#[test]
fn contacts_apart_through_four_rotations_fall_back_to_the_hour() {
    let (air, mut a, mut b, back) = apart_through(FUTURE_TOKENS + 1);
    let met = run_until(
        &air,
        &mut [&mut a, &mut b],
        back,
        back + crate::adapter::CENTRAL_MEMORY_MS + 60_000,
        |m| present(m[0], m[1]),
    )
    .expect("together within the hour");
    let air_now = air.lock().unwrap();
    let real = |from: usize, n: &[u8]| {
        let sender = if from == a.me { &a } else { &b };
        n.len() == 1 + TOKEN_LEN
            && n[0] == TOKEN_MARK
            && sender.tokens.iter().any(|(k, _)| k[..] == n[1..])
    };
    let clear = air_now
        .notes
        .iter()
        .filter(|(f, to, n, at)| {
            [(a.me, b.me), (b.me, a.me)].contains(&(*f, *to)) && *at >= back && real(*f, n)
        })
        .map(|(_, _, _, at)| *at)
        .min()
        .expect("a clear token was given");
    assert!(clear <= met, "they met only after a clear token");
    // here the hour ran out about 11 minutes after they came back
    assert!(met > back + 60_000, "the schedule didn't reach that far");
}

/// I-new-5: a tracker that connects from one fixed address, served once,
/// comes back after this device's rotation and gets a decoy; a stranger
/// arriving fresh gets the real token. The two can't tell the difference:
/// the same mark, length and tag, and the same outcome, nothing back for
/// their hellos.
#[test]
fn a_tracker_from_a_fixed_address_cannot_tell_it_was_recognised() {
    let air = Arc::new(Mutex::new(Air::default()));
    let mut b = Mac::new(&air, 2);
    b.h.set_mode(Some(Mode::Contacts));
    let mut tracker = Mac::new(&air, 5);
    tracker.h.set_contacts(vec![b.id.public()]); // it knows b's key
    tracker.h.set_mode(Some(Mode::Contacts));
    let t = run(&air, &mut [&mut b, &mut tracker], 0, 5_000);
    let first = b.token();
    let (served, _) = counts(&air.lock().unwrap().notified(b.me, tracker.me, 0), &b);
    assert!(served > 0, "served once");
    let since = b.t.next_rotation_ms().unwrap();
    let t = run(&air, &mut [&mut b, &mut tracker], t, since + 1_000);
    assert_ne!(b.token(), first);
    let mut fresh = Mac::new(&air, 6);
    fresh.h.set_contacts(vec![b.id.public()]);
    fresh.h.set_mode(Some(Mode::Contacts));
    run(&air, &mut [&mut b, &mut tracker, &mut fresh], t, t + 60_000);
    let air_now = air.lock().unwrap();
    let to_tracker = air_now.notified(b.me, tracker.me, since);
    let to_fresh = air_now.notified(b.me, fresh.me, since);
    assert_eq!(counts(&to_tracker, &b).0, 0, "the tracker gets no token");
    assert!(counts(&to_tracker, &b).1 > 0, "but a decoy");
    assert!(counts(&to_fresh, &b).0 > 0, "the stranger is served");
    let shape = |n: &[u8]| (n.len(), n[0], n[1..1 + 6].to_vec());
    let current = b.token();
    for n in to_tracker.iter().chain(&to_fresh) {
        assert_eq!(shape(n), (1 + TOKEN_LEN, TOKEN_MARK, current[..6].to_vec()));
    }
    // the same outcome: nothing else comes back to either on those links
    for (who, notes) in [("tracker", &to_tracker), ("stranger", &to_fresh)] {
        assert!(
            notes.iter().all(|n| n.len() == 1 + TOKEN_LEN),
            "{who} heard more than the token's shape"
        );
    }
    drop(air_now);
    assert!(b.h.peers().contacts.is_empty());
    assert!(tracker.h.peers().contacts.is_empty() && fresh.h.peers().contacts.is_empty());
}

/// M-new-2: a contact this device can't recognise by schedule (it has
/// none for it), whose link it lost, is redialled on the next link to the
/// same advert even though that link is given a decoy: the token this
/// device was given for the advert before is found by the tag.
#[test]
fn a_lost_link_is_redialled_by_its_tag_through_a_decoy() {
    let air = Arc::new(Mutex::new(Air::default()));
    let mut a = Mac::new(&air, 1);
    let mut b = Mac::new(&air, 2);
    a.h.set_contacts(vec![b.id.public()]);
    b.h.set_contacts(vec![a.id.public()]);
    a.h.set_mode(Some(Mode::Contacts));
    b.h.set_mode(Some(Mode::Contacts));
    let t = run(&air, &mut [&mut a, &mut b], 0, 5_000);
    assert!(present(&a, &b));
    assert!(a.t.own_sessions() > 0);
    // a forgets what b told it, then every connection drops at once; b
    // gives a a decoy when it comes back (a was served already)
    a.t.forget_told();
    air.lock().unwrap().cut_all();
    let back = t;
    let t = run(&air, &mut [&mut a, &mut b], t, t + 10_000);
    let (_, decoys) = counts(&air.lock().unwrap().notified(b.me, a.me, back), &b);
    assert!(decoys > 0, "a was given a decoy");
    assert!(a.t.own_sessions() > 0, "a redialled b on its own link");
    assert!(present(&a, &b));
    let _ = t;
}
