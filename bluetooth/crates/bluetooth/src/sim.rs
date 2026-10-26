//! Tests of the whole stack below the node: transports over the
//! CoreBluetooth radio's adapter, over a simulated CoreBluetooth whose
//! device identifiers never change (as between two Macs that keep each
//! other's identity), so every refusal rule is in play.

use std::sync::{Arc, Mutex};

use bombechat_core::{seal_private, Envelope, Identity};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_node::{Transport, TransportEvent};
use bombechat_session::SessionKeys;

use crate::adapter::{parse_tag, Access, Adapter, CbEvent, PeerId, Power, Role, Stack};
use crate::radio::Mode;
use crate::transport::{BleTransport, TOKEN_MS};
use crate::{BleHandle, SeededRng};

struct Station {
    open: bool,
    advert: Option<String>,
    scanning: bool,
    inbox: Vec<CbEvent>,
}

/// The air: every station, the connections between them (central,
/// peripheral), the clock, and whether they are out of range.
#[derive(Default)]
struct Air {
    stations: Vec<Station>,
    conns: Vec<(usize, usize)>,
    now: u64,
    apart: bool,
}

fn id(i: usize) -> PeerId {
    [i as u8 + 1; 16]
}

fn index(p: PeerId) -> usize {
    p[0] as usize - 1
}

impl Air {
    /// Every connection ends, as when the two go out of range.
    fn cut_all(&mut self) {
        for (c, p) in std::mem::take(&mut self.conns) {
            self.stations[c].inbox.push(CbEvent::Disconnected(id(p)));
            self.stations[p].inbox.push(CbEvent::Unsubscribed(id(c)));
        }
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
                air.stations[p].inbox.push(CbEvent::Unsubscribed(id(c)));
            } else {
                air.stations[c].inbox.push(CbEvent::Disconnected(id(p)));
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
            let seen: Vec<CbEvent> = air
                .stations
                .iter()
                .enumerate()
                .filter(|(i, s)| *i != me && s.open)
                .filter_map(|(i, s)| {
                    let name = s.advert.as_ref()?;
                    Some(CbEvent::Discovered(id(i), parse_tag(name)))
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
        let (me, p) = (self.me, index(peer));
        let there = air.stations[p].open && air.stations[p].advert.is_some() && !air.apart;
        if !there {
            air.stations[me].inbox.push(CbEvent::Failed(peer));
            return;
        }
        air.conns.push((me, p));
        air.stations[me].inbox.push(CbEvent::Ready(peer));
        air.stations[p].inbox.push(CbEvent::Subscribed(id(me)));
    }
    fn cancel(&mut self, peer: PeerId) {
        let mut air = self.air.lock().unwrap();
        let (me, p) = (self.me, index(peer));
        let before = air.conns.len();
        air.conns.retain(|c| *c != (me, p));
        if air.conns.len() < before {
            air.stations[p].inbox.push(CbEvent::Unsubscribed(id(me)));
        }
    }
    fn write(&mut self, peer: PeerId, bytes: &[u8]) -> bool {
        let mut air = self.air.lock().unwrap();
        let (me, p) = (self.me, index(peer));
        if !air.conns.contains(&(me, p)) {
            return false;
        }
        air.stations[p]
            .inbox
            .push(CbEvent::Written(id(me), bytes.to_vec()));
        true
    }
    fn notify(&mut self, central: PeerId, bytes: &[u8]) -> bool {
        let mut air = self.air.lock().unwrap();
        let (me, c) = (self.me, index(central));
        if !air.conns.contains(&(c, me)) {
            return false;
        }
        air.stations[c]
            .inbox
            .push(CbEvent::Notified(id(me), bytes.to_vec()));
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
    t: BleTransport<Radio>,
    h: BleHandle,
    id: Identity,
    heard: Reassembler,
    got: Vec<Envelope>,
    events: Vec<TransportEvent>,
}

const BASE: u64 = 1_800_000_000_000;

impl Mac {
    fn new(air: &Arc<Mutex<Air>>, seed: u64) -> Mac {
        let me = {
            let mut a = air.lock().unwrap();
            a.stations.push(Station {
                open: false,
                advert: None,
                scanning: false,
                inbox: Vec::new(),
            });
            a.stations.len() - 1
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
            t,
            h,
            id,
            heard: Reassembler::new(),
            got: Vec::new(),
            events: Vec::new(),
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

/// Two contacts whose identifiers never change: each refuses the other
/// as a stranger once both have rotated, but their confirmed sessions
/// exempt them, so they reconnect after every rotation and after a
/// blackout, and keep talking.
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
        t = talk(&air, &mut a, &mut b, t, 10 + round);
    }
    // a blackout longer than every session's silence limit
    air.lock().unwrap().apart = true;
    air.lock().unwrap().cut_all();
    t = run(&air, &mut [&mut a, &mut b], t, t + 90_000);
    assert!(!present(&a, &b), "apart");
    air.lock().unwrap().apart = false;
    t = run(&air, &mut [&mut a, &mut b], t, t + 60_000);
    assert!(present(&a, &b), "together again after the blackout");
    talk(&air, &mut a, &mut b, t, 20);
}
