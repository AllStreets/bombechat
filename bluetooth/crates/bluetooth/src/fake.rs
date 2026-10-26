//! An in-memory radio for tests: several radios in one room, linked as
//! real ones would be, with knobs that lose, repeat and reorder writes.
//!
//! Every started radio that sees another's advert connects to it, so two
//! devices in range share two links, one each way. The radio that
//! connected learns the other's token in [`RadioEvent::Up`] (or only its
//! advert's tag, if the other withholds it); the other learns none. Nothing here reads a clock: knobs draw from a seeded
//! generator, so a test runs the same every time.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use bombechat_session::TOKEN_LEN;
use rand_core::{impls, RngCore};

use crate::radio::{Advert, LinkId, Mode, Radio, RadioEvent, TAG_LEN};

/// A write size typical of a Mac: a 185-byte ATT MTU less its header.
pub const FAKE_MAX_WRITE: usize = 182;

struct Station {
    on: bool,
    mode: Option<Mode>,
    token: Option<[u8; TOKEN_LEN]>,
    max_write: usize,
    inbox: Vec<RadioEvent>,
    starts: u32,
    stops: u32,
    written: Vec<(LinkId, Vec<u8>)>,
}

#[derive(Clone, Copy)]
struct FakeLink {
    central: usize,
    peripheral: usize,
    /// The advert the central connected to.
    token: [u8; TOKEN_LEN],
}

struct RoomState {
    stations: Vec<Station>,
    links: BTreeMap<u64, FakeLink>,
    next_link: u64,
    rng: u64,
    drop_per_mille: u32,
    duplicate_per_mille: u32,
    reorder_per_mille: u32,
    apart: HashSet<(usize, usize)>,
    /// (central, peripheral) pairs where the peripheral withholds its
    /// token, as when it heard that central under an earlier one: the
    /// central learns only the advert's tag.
    refused: HashSet<(usize, usize)>,
    /// Links a radio closed, by (central, peripheral, the advert it had
    /// connected to): not reopened while the peripheral still shows it.
    closed: Vec<(usize, usize, [u8; TOKEN_LEN])>,
}

impl RoomState {
    fn next(&mut self) -> u64 {
        // SplitMix64
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn chance(&mut self, per_mille: u32) -> bool {
        per_mille > 0 && (self.next() % 1000) < per_mille as u64
    }

    fn in_range(&self, a: usize, b: usize) -> bool {
        !self.apart.contains(&(a.min(b), a.max(b)))
    }

    /// Link every started radio to every advertising one in range that it
    /// isn't linked to yet.
    fn connect(&mut self) {
        let stations = &self.stations;
        self.closed
            .retain(|(_, p, t)| stations[*p].token == Some(*t));
        let n = self.stations.len();
        for c in 0..n {
            for p in 0..n {
                if c == p || !self.stations[c].on || !self.stations[p].on || !self.in_range(c, p) {
                    continue;
                }
                let Some(token) = self.stations[p].token else {
                    continue;
                };
                if self
                    .links
                    .values()
                    .any(|l| l.central == c && l.peripheral == p)
                    || self.closed.iter().any(|(cc, pp, _)| *cc == c && *pp == p)
                {
                    continue;
                }
                self.next_link += 1;
                let id = LinkId(self.next_link);
                self.links.insert(
                    self.next_link,
                    FakeLink {
                        central: c,
                        peripheral: p,
                        token,
                    },
                );
                let advert = if self.refused.contains(&(c, p)) {
                    // withheld: only the tag the advert shows
                    let mut tag = [0u8; TAG_LEN];
                    tag.copy_from_slice(&token[..TAG_LEN]);
                    Advert::Withheld(Some(tag))
                } else {
                    Advert::Token(token)
                };
                self.stations[c]
                    .inbox
                    .push(RadioEvent::Up { link: id, advert });
                self.stations[p].inbox.push(RadioEvent::Up {
                    link: id,
                    advert: Advert::Unseen,
                });
            }
        }
    }

    /// Take a link down, telling each end that is still on.
    fn cut(&mut self, link: u64) {
        if let Some(l) = self.links.remove(&link) {
            for s in [l.central, l.peripheral] {
                if self.stations[s].on {
                    self.stations[s].inbox.push(RadioEvent::Down(LinkId(link)));
                }
            }
        }
    }

    fn deliver(&mut self, to: usize, link: LinkId, bytes: Vec<u8>) {
        let event = RadioEvent::Bytes(link, bytes);
        if self.chance(self.reorder_per_mille) {
            // slip in before up to three earlier writes on the same link
            let inbox = &self.stations[to].inbox;
            let mut at = inbox.len();
            let back = 1 + (self.next() % 3) as usize;
            for _ in 0..back {
                if at > 0
                    && matches!(&self.stations[to].inbox[at - 1], RadioEvent::Bytes(l, _) if *l == link)
                {
                    at -= 1;
                } else {
                    break;
                }
            }
            self.stations[to].inbox.insert(at, event);
        } else {
            self.stations[to].inbox.push(event);
        }
    }
}

/// The space radios share. Cloning gives another handle on the same room.
#[derive(Clone)]
pub struct Room(Arc<Mutex<RoomState>>);

impl Room {
    /// An empty room whose knobs draw from `seed`.
    pub fn new(seed: u64) -> Room {
        Room(Arc::new(Mutex::new(RoomState {
            stations: Vec::new(),
            links: BTreeMap::new(),
            next_link: 0,
            rng: seed,
            drop_per_mille: 0,
            duplicate_per_mille: 0,
            reorder_per_mille: 0,
            apart: HashSet::new(),
            refused: HashSet::new(),
            closed: Vec::new(),
        })))
    }

    /// A new radio in the room, off, taking writes of [`FAKE_MAX_WRITE`].
    pub fn radio(&self) -> FakeRadio {
        self.radio_with_max_write(FAKE_MAX_WRITE)
    }

    /// A new radio in the room, off, taking writes of `max_write` bytes.
    pub fn radio_with_max_write(&self, max_write: usize) -> FakeRadio {
        let mut s = self.0.lock().unwrap();
        s.stations.push(Station {
            on: false,
            mode: None,
            token: None,
            max_write,
            inbox: Vec::new(),
            starts: 0,
            stops: 0,
            written: Vec::new(),
        });
        FakeRadio {
            room: self.0.clone(),
            me: s.stations.len() - 1,
        }
    }

    /// Lose this many writes in a thousand.
    pub fn set_drop(&self, per_mille: u32) {
        self.0.lock().unwrap().drop_per_mille = per_mille;
    }

    /// Deliver this many writes in a thousand twice.
    pub fn set_duplicate(&self, per_mille: u32) {
        self.0.lock().unwrap().duplicate_per_mille = per_mille;
    }

    /// Deliver this many writes in a thousand ahead of up to three
    /// earlier ones on the same link.
    pub fn set_reorder(&self, per_mille: u32) {
        self.0.lock().unwrap().reorder_per_mille = per_mille;
    }

    /// Put two radios in or out of each other's range. Out of range,
    /// their links go down; back in range, they link again.
    pub fn set_in_range(&self, a: &FakeRadio, b: &FakeRadio, in_range: bool) {
        let mut s = self.0.lock().unwrap();
        let pair = (a.me.min(b.me), a.me.max(b.me));
        if in_range {
            s.apart.remove(&pair);
            s.connect();
        } else {
            s.apart.insert(pair);
            let gone: Vec<u64> = s
                .links
                .iter()
                .filter(|(_, l)| (l.central.min(l.peripheral), l.central.max(l.peripheral)) == pair)
                .map(|(id, _)| *id)
                .collect();
            for id in gone {
                s.cut(id);
            }
        }
    }

    /// Have `peripheral` withhold its token from `central` from now on:
    /// links from `central` to its advert show only the tag (the other way
    /// is unaffected), and any such link now goes down.
    pub fn refuse(&self, central: &FakeRadio, peripheral: &FakeRadio) {
        let mut s = self.0.lock().unwrap();
        s.refused.insert((central.me, peripheral.me));
        let gone: Vec<u64> = s
            .links
            .iter()
            .filter(|(_, l)| l.central == central.me && l.peripheral == peripheral.me)
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            s.cut(id);
        }
        s.connect();
    }

    /// Take one link down, as a radio dropping out mid-message would.
    pub fn disconnect(&self, link: LinkId) {
        self.0.lock().unwrap().cut(link.0);
    }

    /// Every live link as `(link, central, peripheral)`, radios by
    /// [`FakeRadio::index`].
    pub fn links(&self) -> Vec<(LinkId, usize, usize)> {
        self.0
            .lock()
            .unwrap()
            .links
            .iter()
            .map(|(id, l)| (LinkId(*id), l.central, l.peripheral))
            .collect()
    }
}

/// One radio in a [`Room`].
pub struct FakeRadio {
    room: Arc<Mutex<RoomState>>,
    me: usize,
}

impl FakeRadio {
    /// This radio's number in its room.
    pub fn index(&self) -> usize {
        self.me
    }

    /// A second handle on this radio, for a test to watch it while a
    /// transport drives it.
    pub fn watch(&self) -> FakeRadio {
        FakeRadio {
            room: self.room.clone(),
            me: self.me,
        }
    }

    /// Whether it is started: advertising, scanning and linking.
    pub fn is_on(&self) -> bool {
        self.room.lock().unwrap().stations[self.me].on
    }

    /// The mode it was last started in, while on.
    pub fn mode(&self) -> Option<Mode> {
        let s = self.room.lock().unwrap();
        let st = &s.stations[self.me];
        st.mode.filter(|_| st.on)
    }

    /// The token it advertises, while on.
    pub fn token(&self) -> Option<[u8; TOKEN_LEN]> {
        let s = self.room.lock().unwrap();
        let st = &s.stations[self.me];
        st.token.filter(|_| st.on)
    }

    /// How many times it was started and stopped.
    pub fn starts_and_stops(&self) -> (u32, u32) {
        let s = self.room.lock().unwrap();
        (s.stations[self.me].starts, s.stations[self.me].stops)
    }

    /// Every write it made, in order, and forget them.
    pub fn take_written(&self) -> Vec<(LinkId, Vec<u8>)> {
        std::mem::take(&mut self.room.lock().unwrap().stations[self.me].written)
    }
}

impl Radio for FakeRadio {
    fn start(&mut self, mode: Mode) {
        let mut s = self.room.lock().unwrap();
        let st = &mut s.stations[self.me];
        if !st.on {
            st.starts += 1;
        }
        st.on = true;
        st.mode = Some(mode);
        s.connect();
    }

    fn stop(&mut self) {
        let mut s = self.room.lock().unwrap();
        if s.stations[self.me].on {
            s.stations[self.me].stops += 1;
        }
        s.stations[self.me].on = false;
        s.stations[self.me].token = None;
        let mine: Vec<u64> = s
            .links
            .iter()
            .filter(|(_, l)| l.central == self.me || l.peripheral == self.me)
            .map(|(id, _)| *id)
            .collect();
        for id in mine {
            s.cut(id);
        }
        s.stations[self.me].inbox.clear();
    }

    fn advertise(&mut self, token: [u8; TOKEN_LEN]) {
        let mut s = self.room.lock().unwrap();
        s.stations[self.me].token = Some(token);
        s.connect();
    }

    fn disconnect(&mut self, link: LinkId) {
        let mut s = self.room.lock().unwrap();
        let Some(l) = s.links.get(&link.0).copied() else {
            return;
        };
        if l.central != self.me && l.peripheral != self.me {
            return;
        }
        s.closed.push((l.central, l.peripheral, l.token));
        // a stale advert may be showing a fresh one already
        let me = self.me;
        s.links.remove(&link.0);
        let other = if l.central == self.me {
            l.peripheral
        } else {
            l.central
        };
        if s.stations[other].on {
            s.stations[other].inbox.push(RadioEvent::Down(link));
        }
        s.connect();
        // anything still queued for this end on the link is gone
        s.stations[me]
            .inbox
            .retain(|e| !matches!(e, RadioEvent::Bytes(l, _) if *l == link));
    }

    fn write(&mut self, link: LinkId, bytes: &[u8]) {
        let mut s = self.room.lock().unwrap();
        let Some(l) = s.links.get(&link.0).copied() else {
            return;
        };
        let to = if l.central == self.me {
            l.peripheral
        } else if l.peripheral == self.me {
            l.central
        } else {
            return;
        };
        let limit = s.stations[l.central]
            .max_write
            .min(s.stations[l.peripheral].max_write);
        assert!(
            bytes.len() <= limit,
            "a write of {} bytes on a link that takes {limit}",
            bytes.len()
        );
        s.stations[self.me].written.push((link, bytes.to_vec()));
        let (drop, duplicate) = (s.drop_per_mille, s.duplicate_per_mille);
        if s.chance(drop) {
            return;
        }
        if s.chance(duplicate) {
            s.deliver(to, link, bytes.to_vec());
        }
        s.deliver(to, link, bytes.to_vec());
    }

    fn poll(&mut self) -> Vec<RadioEvent> {
        std::mem::take(&mut self.room.lock().unwrap().stations[self.me].inbox)
    }

    fn max_write(&self, link: LinkId) -> usize {
        let s = self.room.lock().unwrap();
        match s.links.get(&link.0) {
            Some(l) => s.stations[l.central]
                .max_write
                .min(s.stations[l.peripheral].max_write),
            None => s.stations[self.me].max_write,
        }
    }
}

/// A seeded random source for tests (SplitMix64), so a transport's
/// exchange numbers, tokens and decoys come out the same every run.
pub struct SeededRng(u64);

impl SeededRng {
    pub fn new(seed: u64) -> SeededRng {
        SeededRng(seed)
    }
}

impl RngCore for SeededRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        impls::fill_bytes_via_next(self, dest)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}
