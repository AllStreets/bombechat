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

use crate::adapter::{
    parse_tag, Access, Adapter, CbEvent, PeerId, Power, Role, Stack, TOKEN_MARK, WITHHELD_MARK,
};
use crate::radio::Mode;
use crate::transport::{BleTransport, TOKEN_MS};
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
        self.carried
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

fn counts(notes: &[&[u8]]) -> (usize, usize) {
    let tokens = notes
        .iter()
        .filter(|n| n.len() == 1 + TOKEN_LEN && n[0] == TOKEN_MARK)
        .count();
    let withheld = notes.iter().filter(|n| **n == [WITHHELD_MARK]).count();
    (tokens, withheld)
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
    // way, and each was linked without one
    let since = a.tokens[1].1.max(b.tokens[1].1);
    for (from, to) in [(a.me, b.me), (b.me, a.me)] {
        let (tokens, withheld) = counts(&air.lock().unwrap().notified(from, to, since));
        assert_eq!(tokens, 0, "{from} gave {to} its token");
        assert!(withheld > 0, "{from} never linked {to} without the token");
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
    let (tokens, withheld) = counts(&air_now.notified(b.me, s.me, from));
    assert_eq!(tokens, 0, "no clear token");
    assert!(withheld > 0, "a link, without the token");
    // nothing that reached it carries b's token, in the clear or not
    let current = b.token();
    let reached: Vec<&Vec<u8>> = air_now
        .carried
        .iter()
        .filter(|(_, to, _, at)| *to == s.me && *at >= from)
        .map(|(_, _, bytes, _)| bytes)
        .collect();
    assert!(!reached.is_empty());
    assert!(reached.iter().all(|bytes| !contains(bytes, &current)));
    drop(air_now);
    assert_eq!(s.t.contact_token(&b.id.public()), None);
    assert!(s.h.peers().contacts.is_empty(), "no session");
    assert!(b.h.peers().contacts.is_empty());
    assert_eq!(b.token(), current, "within one token");
    let _ = t;
}
