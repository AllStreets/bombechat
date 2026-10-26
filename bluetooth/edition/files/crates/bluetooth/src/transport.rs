//! The link layer as one of the node's transports: links and their
//! lifetimes, contact rounds and redials, requests, and envelopes to and
//! from the node. Sessions are in `session`, the waiting envelopes in
//! `queue`.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use bombechat_core::{Envelope, PublicIdentity, MAX_ENVELOPE};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_node::{Transport, TransportEvent};
use bombechat_session::{Session, SessionKeys, TOKEN_LEN};
use bombechat_store::Channel;
use rand_core::{OsRng, RngCore};

use crate::handle::{BleEvent, BleHandle, Control, Peers};
use crate::link::{body_with_id, ExKey, Exchange, Link, Side};
use crate::queue::{Key, Queue};
use crate::radio::{LinkId, Mode, Radio, RadioEvent};
use crate::route::{targets, Targets};
use crate::session::{LinkSession, SessKind, ACK_DELAY_MAX_MS};
use crate::wire::{Kind, LinkMessage};

/// The advertising token changes at a random moment between this...
pub const TOKEN_MIN_MS: u64 = 12 * 60_000;
/// ...and this after the last change. A link this device opened is
/// closed once it is this old, so its token is never more than one
/// rotation stale.
pub const TOKEN_MS: u64 = 15 * 60_000;

/// Every round of contact hellos to a token is exactly this many: real
/// candidates first, decoys of the same size after.
pub const TRIAL_CAP: usize = 8;
/// A round's hellos unanswered this long are sent once more, afresh...
pub const TRIAL_RETRY_MS: u64 = 4_000;
/// ...so each goes at most this many times. One round per token.
pub const TRIAL_ATTEMPTS: u8 = 2;
/// A token's round is remembered this long: past any token's life.
pub const ROUND_MEMORY_MS: u64 = 2 * TOKEN_MS;
/// At most this many tokens' rounds are remembered.
pub const ROUNDS_CAP: usize = 256;

/// After a contact session on a link this device opened ends, the contact
/// is dialled again after this long, then twice as long each time...
pub const REDIAL_FIRST_MS: u64 = 1_000;
/// ...up to this. Redials are outside the rounds: they spend no budget.
pub const REDIAL_MAX_MS: u64 = 30_000;

/// The seen-ephemeral cache holds at most this many keys...
pub const SEEN_CAP: usize = 4096;
/// ...each for this long: the contact window either way.
pub const SEEN_MS: u64 = 4 * 60_000;

/// A handshake half done, or a session the other side has not yet
/// confirmed, is dropped after this long.
pub const HANDSHAKE_MS: u64 = 30_000;

/// A request received is forgotten this long after its hello arrived,
/// unless the person accepted it...
pub const REQUEST_MS: u64 = 3 * 60_000;
/// ...and at most this many are held, one per link; a new one replaces
/// the link's earlier one, or the oldest.
pub const REQUESTS_CAP: usize = 4;
/// A request this device sends is begun afresh, unanswered or broken
/// off, until it has been tried this many times; then it has failed.
pub const REQUEST_ATTEMPTS: u8 = 3;

/// A link with no session and no handshake for this long is closed.
pub const IDLE_MS: u64 = 120_000;
/// At most this many links; one more is closed at once.
pub const MAX_LINKS: usize = 32;

/// A link counts towards the strangers in range for this long after it
/// was last seen...
pub const STRANGER_MS: u64 = 120_000;
/// ...and at most this many are remembered.
const SEEN_LINKS_CAP: usize = 64;

/// At most this many exchanges on one link.
const EXCHANGES_CAP: usize = 24;
/// At most this many unconfirmed contact sessions on one link.
const UNCONFIRMED_CAP: usize = 4;

const ID_LEN: usize = 4;

fn key_of(p: &PublicIdentity) -> Key {
    let mut k = [0u8; 64];
    k[..32].copy_from_slice(&p.sign);
    k[32..].copy_from_slice(&p.dh);
    k
}

struct Tokens {
    current: [u8; TOKEN_LEN],
    previous: Option<[u8; TOKEN_LEN]>,
    next_ms: u64,
}

struct Redial {
    link: LinkId,
    contact: PublicIdentity,
    next_ms: u64,
    backoff_ms: u64,
}

/// The Bluetooth transport: [`Channel::Bluetooth`] for the node, over any
/// [`Radio`]. Its clock is the `now_ms` the node passes to
/// [`Transport::poll`]; it reads none of its own.
pub struct BleTransport<R: Radio> {
    radio: R,
    rng: Box<dyn RngCore + Send>,
    shared: Arc<Mutex<Control>>,
    unix_base_ms: u64,
    keys: Option<SessionKeys>,
    contacts: Vec<PublicIdentity>,
    contact_keys: BTreeSet<Key>,
    mode: Option<Mode>,
    running: bool,
    tokens: Option<Tokens>,
    links: BTreeMap<LinkId, Link>,
    seen_links: BTreeMap<LinkId, (u64, bool)>,
    last_seen: BTreeMap<Key, u64>,
    last_tried: BTreeMap<Key, u64>,
    rounds: BTreeMap<[u8; TOKEN_LEN], u64>,
    redial: BTreeMap<Key, Redial>,
    seen_ephemerals: HashMap<[u8; 32], u64>,
    seen_order: VecDeque<([u8; 32], u64)>,
    outbound_frames: Vec<Vec<u8>>,
    outbound: Reassembler,
    queue: Queue,
    events: Vec<TransportEvent>,
    next_tag: u16,
    listening: bool,
    sending: bool,
    now_ms: u64,
}

impl<R: Radio> BleTransport<R> {
    /// A transport over `radio`, off until it has keys and a mode.
    /// `unix_base_ms` is the Unix time in milliseconds when the node's
    /// clock read zero: contact handshakes carry the time they were sent,
    /// and a device more than two minutes off answers none.
    pub fn new(radio: R, unix_base_ms: u64) -> BleTransport<R> {
        BleTransport::with_rng(radio, unix_base_ms, Box::new(OsRng))
    }

    /// As [`BleTransport::new`], drawing exchange numbers, tokens, decoys
    /// and rotation times from `rng`: tests pass a seeded one.
    pub fn with_rng(radio: R, unix_base_ms: u64, rng: Box<dyn RngCore + Send>) -> BleTransport<R> {
        BleTransport {
            radio,
            rng,
            shared: Arc::new(Mutex::new(Control::default())),
            unix_base_ms,
            keys: None,
            contacts: Vec::new(),
            contact_keys: BTreeSet::new(),
            mode: None,
            running: false,
            tokens: None,
            links: BTreeMap::new(),
            seen_links: BTreeMap::new(),
            last_seen: BTreeMap::new(),
            last_tried: BTreeMap::new(),
            rounds: BTreeMap::new(),
            redial: BTreeMap::new(),
            seen_ephemerals: HashMap::new(),
            seen_order: VecDeque::new(),
            outbound_frames: Vec::new(),
            outbound: Reassembler::new(),
            queue: Queue::default(),
            events: Vec::new(),
            next_tag: 0,
            listening: true,
            sending: true,
            now_ms: 0,
        }
    }

    /// The app's hold on this transport.
    pub fn handle(&self) -> BleHandle {
        BleHandle(self.shared.clone())
    }

    /// The radio, for tests to look at.
    pub fn radio(&self) -> &R {
        &self.radio
    }

    /// Whether the radio is started.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The tokens a contact message 1 may name: current, then previous.
    pub fn tokens(&self) -> Vec<[u8; TOKEN_LEN]> {
        match &self.tokens {
            Some(t) => std::iter::once(t.current).chain(t.previous).collect(),
            None => Vec::new(),
        }
    }

    /// Ephemeral keys in the seen cache.
    pub fn seen_ephemerals(&self) -> usize {
        self.seen_ephemerals.len()
    }

    /// Fragmented messages part way on `link`, and the bytes they hold.
    pub fn assembling(&self, link: LinkId) -> (usize, usize) {
        self.links.get(&link).map_or((0, 0), |l| {
            (l.reassembly.in_progress(), l.reassembly.held_bytes())
        })
    }

    /// Envelopes waiting to go.
    pub fn queued(&self) -> usize {
        self.queue.items.len()
    }

    /// Live links.
    pub fn links(&self) -> Vec<LinkId> {
        self.links.keys().copied().collect()
    }

    /// Requests received and held (answering, or heard and waiting).
    pub fn requests_held(&self) -> usize {
        self.links
            .values()
            .flat_map(|l| l.exchanges.values())
            .filter(|e| is_request_in(e))
            .count()
    }

    /// Bytes held ahead of a gap, across every session.
    pub fn held_ahead(&self) -> usize {
        self.links
            .values()
            .flat_map(|l| l.exchanges.values())
            .map(|e| match e {
                Exchange::Session(s) => s.held_ahead(),
                _ => 0,
            })
            .sum()
    }

    fn unix_now(&self) -> u64 {
        self.unix_base_ms.saturating_add(self.now_ms)
    }

    fn me(&self) -> Option<Key> {
        self.keys.as_ref().map(|k| key_of(k.public()))
    }

    fn notify(&self, event: BleEvent) {
        self.shared.lock().unwrap().notify(event);
    }

    fn random_token(&mut self) -> [u8; TOKEN_LEN] {
        let mut t = [0u8; TOKEN_LEN];
        self.rng.fill_bytes(&mut t);
        t
    }

    fn next_rotation(&mut self) -> u64 {
        let spread = TOKEN_MS - TOKEN_MIN_MS + 1;
        self.now_ms + TOKEN_MIN_MS + self.rng.next_u64() % spread
    }

    // ---- switching ----------------------------------------------------

    fn apply_control(&mut self) {
        let (keys, contacts, mode, requests, answers) = {
            let mut c = self.shared.lock().unwrap();
            (
                c.keys.take(),
                c.contacts.take(),
                c.mode.take(),
                std::mem::take(&mut c.requests),
                std::mem::take(&mut c.answers),
            )
        };
        let mut restart = false;
        if let Some(keys) = keys {
            self.keys = keys;
            restart = true;
        }
        let mut mode_changed = false;
        if let Some(mode) = mode {
            mode_changed = mode != self.mode;
            self.mode = mode;
        }
        if let Some(contacts) = contacts {
            self.set_contacts(contacts);
        }
        let want = self.mode.is_some() && self.keys.is_some();
        if self.running && (restart || !want) {
            self.halt();
        }
        if want && !self.running {
            self.begin();
        } else if want && mode_changed {
            if let Some(mode) = self.mode {
                self.radio.start(mode);
            }
            if self.mode == Some(Mode::Contacts) {
                self.drop_requests();
            }
        }
        for (link, hello) in requests {
            self.begin_request(link, hello, 1);
        }
        for (link, accept) in answers {
            let Some(l) = self.links.get_mut(&link) else {
                continue;
            };
            if accept {
                for e in l.exchanges.values_mut() {
                    if let Exchange::Session(s) = e {
                        if s.kind == SessKind::RequestIn {
                            s.accepted_by_person = true;
                        }
                    }
                }
            } else {
                l.exchanges.retain(|_, e| !is_request_in(e));
            }
        }
    }

    fn set_contacts(&mut self, contacts: Vec<PublicIdentity>) {
        let me = self.me();
        self.contacts = contacts
            .into_iter()
            .filter(|c| Some(key_of(c)) != me)
            .collect();
        self.contact_keys = self.contacts.iter().map(key_of).collect();
        let keys = &self.contact_keys;
        self.last_seen.retain(|k, _| keys.contains(k));
        self.last_tried.retain(|k, _| keys.contains(k));
        self.redial.retain(|k, _| keys.contains(k));
        for link in self.links.values_mut() {
            link.exchanges.retain(|_, e| match e {
                Exchange::Trial {
                    contact: Some(c), ..
                } => keys.contains(&key_of(c)),
                Exchange::Session(s) if s.kind == SessKind::Contact => {
                    s.remote.as_ref().is_some_and(|r| keys.contains(&key_of(r)))
                }
                _ => true,
            });
        }
    }

    fn begin(&mut self) {
        let Some(mode) = self.mode else { return };
        self.running = true;
        self.radio.start(mode);
        let current = self.random_token();
        let next_ms = self.next_rotation();
        self.tokens = Some(Tokens {
            current,
            previous: None,
            next_ms,
        });
        self.radio.advertise(current);
    }

    fn halt(&mut self) {
        let ids: Vec<LinkId> = self.links.keys().copied().collect();
        for id in ids {
            self.link_gone(id);
        }
        self.radio.stop();
        self.running = false;
        self.tokens = None;
        self.seen_links.clear();
        self.rounds.clear();
        self.redial.clear();
        for tag in self.queue.drain_unreported() {
            self.events.push(TransportEvent::Dropped(tag));
        }
    }

    fn drop_requests(&mut self) {
        let mut failed = Vec::new();
        for (id, link) in &mut self.links {
            link.exchanges.retain(|_, e| {
                if is_request_out(e) {
                    failed.push(*id);
                    return false;
                }
                !is_request_in(e)
            });
        }
        failed.dedup();
        for link in failed {
            self.notify(BleEvent::RequestFailed { link });
        }
    }

    /// A new token, and every link without a confirmed contact session
    /// closed, so no one connected to this device follows it across.
    fn rotate_token(&mut self) {
        let now = self.now_ms;
        if self.tokens.as_ref().is_none_or(|t| now < t.next_ms) {
            return;
        }
        let token = self.random_token();
        let next_ms = self.next_rotation();
        if let Some(t) = self.tokens.as_mut() {
            t.previous = Some(t.current);
            t.current = token;
            t.next_ms = next_ms;
        }
        self.radio.advertise(token);
        let drop: Vec<LinkId> = self
            .links
            .iter()
            .filter(|(_, l)| !l.has_confirmed_contact())
            .map(|(id, _)| *id)
            .collect();
        for id in drop {
            self.close_link(id);
        }
    }

    fn close_link(&mut self, id: LinkId) {
        self.radio.disconnect(id);
        self.link_gone(id);
    }

    fn link_gone(&mut self, id: LinkId) {
        let Some(gone) = self.links.remove(&id) else {
            return;
        };
        self.redial.retain(|_, r| r.link != id);
        if gone.exchanges.values().any(is_request_out) {
            self.notify(BleEvent::RequestFailed { link: id });
        }
    }

    // ---- receiving ----------------------------------------------------

    fn on_radio(&mut self, event: RadioEvent) {
        match event {
            RadioEvent::Up { link, token } => {
                if self.links.len() >= MAX_LINKS {
                    self.radio.disconnect(link);
                    return;
                }
                self.links.insert(link, Link::new(token, self.now_ms));
            }
            RadioEvent::Down(link) => self.link_gone(link),
            RadioEvent::Bytes(link, bytes) => {
                let now = self.now_ms;
                let Some(l) = self.links.get_mut(&link) else {
                    return;
                };
                if let Some(message) = l.reassembly.push(&bytes, now) {
                    if let Some(m) = LinkMessage::decode(&message) {
                        self.on_message(link, m);
                    }
                }
            }
            RadioEvent::Unavailable(why) => self.events.push(TransportEvent::Unavailable(why)),
        }
    }

    fn on_message(&mut self, link: LinkId, m: LinkMessage) {
        if m.body.len() < ID_LEN {
            return;
        }
        let xid = u32::from_be_bytes([m.body[0], m.body[1], m.body[2], m.body[3]]);
        let rest = &m.body[ID_LEN..];
        let side = Side::of_received(m.flags);
        match m.kind {
            Kind::Contact if side == Side::Theirs => self.answer_contact(link, xid, rest),
            Kind::Request if side == Side::Theirs => self.answer_request(link, xid, rest),
            Kind::Handshake => self.continue_handshake(link, (side, xid), rest),
            Kind::Sealed if rest.len() >= 8 => {
                let mut n = [0u8; 8];
                n.copy_from_slice(&rest[..8]);
                self.on_sealed(link, (side, xid), u64::from_be_bytes(n), &rest[8..]);
            }
            _ => {}
        }
    }

    /// Message 1 of a contact handshake. Answered only if it reads, names
    /// one of our tokens at a time near ours, comes from a contact, and
    /// its ephemeral key is new. Anything else gets no byte back.
    fn answer_contact(&mut self, id: LinkId, xid: u32, noise: &[u8]) {
        let now = self.now_ms;
        let unix = self.unix_now();
        let tokens = self.tokens();
        let me = self.me();
        let Some(keys) = self.keys.as_ref() else {
            return;
        };
        let Some(link) = self.links.get(&id) else {
            return;
        };
        if link.exchanges.contains_key(&(Side::Theirs, xid))
            || link.exchanges.len() >= EXCHANGES_CAP
        {
            return;
        }
        let mut hs = Session::contact_responder(keys, unix, &tokens);
        if hs.read_message(noise).is_err() {
            return;
        }
        let (Some(remote), Some(eph)) = (hs.remote_identity(), hs.remote_ephemeral()) else {
            return;
        };
        let rk = key_of(&remote);
        if Some(rk) == me || !self.contact_keys.contains(&rk) {
            return;
        }
        self.prune_seen();
        if self.seen_ephemerals.contains_key(&eph) {
            return;
        }
        self.remember_ephemeral(eph);
        let Ok(reply) = hs.write_message() else {
            return;
        };
        let Ok(cipher) = hs.into_transport() else {
            return;
        };
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        let mut unconfirmed: Vec<(u64, ExKey)> = link
            .exchanges
            .iter()
            .filter_map(|(k, e)| match e {
                Exchange::Session(s) if s.kind == SessKind::Contact && !s.confirmed() => {
                    Some((s.started_ms, *k))
                }
                _ => None,
            })
            .collect();
        unconfirmed.sort();
        while unconfirmed.len() >= UNCONFIRMED_CAP {
            let (_, k) = unconfirmed.remove(0);
            link.exchanges.remove(&k);
        }
        let k = (Side::Theirs, xid);
        let s = LinkSession::new(SessKind::Contact, cipher, Side::Theirs.flags(), xid, now);
        link.exchanges.insert(k, Exchange::Session(Box::new(s)));
        let wire = handshake_wire(Side::Theirs, xid, &reply);
        link.put(&mut self.radio, id, &wire);
    }

    /// Message 1 of a request to connect: answered in Open mode only. One
    /// per link (a new one replaces the last), and at most
    /// [`REQUESTS_CAP`] in all.
    fn answer_request(&mut self, id: LinkId, xid: u32, noise: &[u8]) {
        if self.mode != Some(Mode::Open) || self.keys.is_none() {
            return;
        }
        let now = self.now_ms;
        if self
            .links
            .get(&id)
            .is_none_or(|l| l.exchanges.contains_key(&(Side::Theirs, xid)))
        {
            return;
        }
        let mut hs = Session::request_responder();
        if hs.read_message(noise).is_err() {
            return;
        }
        let Ok(reply) = hs.write_message() else {
            return;
        };
        if let Some(link) = self.links.get_mut(&id) {
            link.exchanges.retain(|_, e| !is_request_in(e));
        }
        // the oldest anywhere goes if there are too many
        loop {
            let held: Vec<(u64, LinkId, ExKey)> = self
                .links
                .iter()
                .flat_map(|(lid, l)| {
                    l.exchanges.iter().filter_map(move |(k, e)| match e {
                        Exchange::Answering { since_ms, .. } => Some((*since_ms, *lid, *k)),
                        Exchange::Session(s) if s.kind == SessKind::RequestIn => {
                            Some((s.started_ms, *lid, *k))
                        }
                        _ => None,
                    })
                })
                .collect();
            if held.len() < REQUESTS_CAP {
                break;
            }
            if let Some((_, lid, k)) = held.into_iter().min() {
                if let Some(l) = self.links.get_mut(&lid) {
                    l.exchanges.remove(&k);
                }
            }
        }
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        link.exchanges.insert(
            (Side::Theirs, xid),
            Exchange::Answering { hs, since_ms: now },
        );
        let wire = handshake_wire(Side::Theirs, xid, &reply);
        link.put(&mut self.radio, id, &wire);
    }

    fn continue_handshake(&mut self, id: LinkId, k: ExKey, noise: &[u8]) {
        let now = self.now_ms;
        let me = self.me();
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        let Some(ex) = link.exchanges.remove(&k) else {
            return;
        };
        match ex {
            Exchange::Trial {
                mut hs, contact, ..
            } => {
                let Some(contact) = contact else { return };
                if hs.read_message(noise).is_err() {
                    return;
                }
                let Ok(cipher) = hs.into_transport() else {
                    return;
                };
                // the contact answered: no more hellos on this link
                link.exchanges
                    .retain(|_, e| !matches!(e, Exchange::Trial { .. }));
                let mut s =
                    LinkSession::new(SessKind::Contact, cipher, Side::Mine.flags(), k.1, now);
                // the first transport message confirms us to the responder
                let first = s.keepalive(now);
                link.exchanges.insert(k, Exchange::Session(Box::new(s)));
                if let Some(w) = first {
                    link.put(&mut self.radio, id, &w);
                }
                let ck = key_of(&contact);
                self.last_seen.insert(ck, now);
                self.redial.remove(&ck);
            }
            Exchange::Requesting {
                mut hs,
                hello,
                attempt,
                ..
            } => {
                if hs.read_message(noise).is_err() {
                    self.retry_request(id, hello, attempt);
                    return;
                }
                let Ok(third) = hs.write_message() else {
                    return;
                };
                let Ok(cipher) = hs.into_transport() else {
                    return;
                };
                let mut s =
                    LinkSession::new(SessKind::RequestOut, cipher, Side::Mine.flags(), k.1, now);
                link.put(
                    &mut self.radio,
                    id,
                    &handshake_wire(Side::Mine, k.1, &third),
                );
                let first = s.send_envelope(&hello, None, now);
                s.hello = Some((hello, attempt));
                link.exchanges.insert(k, Exchange::Session(Box::new(s)));
                if let Some(w) = first {
                    link.put(&mut self.radio, id, &w);
                }
            }
            Exchange::Answering { mut hs, .. } => {
                if hs.read_message(noise).is_err() {
                    return;
                }
                let Some(remote) = hs.remote_identity() else {
                    return;
                };
                if Some(key_of(&remote)) == me {
                    return;
                }
                let Ok(cipher) = hs.into_transport() else {
                    return;
                };
                let s =
                    LinkSession::new(SessKind::RequestIn, cipher, Side::Theirs.flags(), k.1, now);
                link.exchanges.insert(k, Exchange::Session(Box::new(s)));
            }
            // a late or repeated handshake message for a session: ignore
            session @ Exchange::Session(_) => {
                link.exchanges.insert(k, session);
            }
        }
    }

    fn on_sealed(&mut self, id: LinkId, k: ExKey, nonce: u64, ct: &[u8]) {
        let now = self.now_ms;
        let listening = self.listening;
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        let Some(s) = link.session(&k) else { return };
        let was_confirmed = s.confirmed();
        let take = listening || s.kind != SessKind::Contact;
        let Some(d) = s.receive(nonce, ct, take, now) else {
            return;
        };
        // an acknowledgement on its own waits a random moment
        s.hold_ack(now + self.rng.next_u64() % (ACK_DELAY_MAX_MS + 1));
        let kind = s.kind;
        let remote = s.remote.clone();
        let request_done = kind == SessKind::RequestOut && s.all_acked();
        let first_hello = kind == SessKind::RequestIn && s.heard_ms.is_none();
        if first_hello && !d.envelopes.is_empty() {
            s.heard_ms = Some(now);
        }
        if !was_confirmed && s.confirmed() && kind == SessKind::Contact {
            if let Some(r) = &remote {
                self.last_seen.insert(key_of(r), now);
            }
            // one confirmed session per contact per link: older ones go
            link.exchanges.retain(|other, e| match e {
                Exchange::Session(o) if *other != k && o.kind == SessKind::Contact => {
                    o.remote != remote
                }
                _ => true,
            });
        }
        match kind {
            SessKind::Contact => {
                if let Some(r) = &remote {
                    let (rk, fp) = (key_of(r), r.fingerprint());
                    for tag in d.acked {
                        self.queue.acked(tag, rk, fp);
                    }
                }
                for e in d.envelopes {
                    self.hand_over(&e);
                }
            }
            SessKind::RequestOut => {
                if request_done {
                    link.exchanges.remove(&k);
                    self.notify(BleEvent::RequestSent { link: id });
                }
            }
            SessKind::RequestIn => {
                let hello = d.envelopes.into_iter().next().filter(|_| first_hello);
                if let (Some(hello), Some(from)) = (hello, remote) {
                    if hello.len() <= MAX_ENVELOPE && Envelope::decode(&hello).is_ok() {
                        self.notify(BleEvent::Request {
                            link: id,
                            from,
                            hello,
                        });
                    } else {
                        link.exchanges.remove(&k);
                    }
                }
            }
        }
    }

    /// An envelope from a contact, to the node as the frames it reads.
    fn hand_over(&mut self, bytes: &[u8]) {
        if bytes.len() > MAX_ENVELOPE || Envelope::decode(bytes).is_err() {
            return;
        }
        let tag = self.next_tag;
        self.next_tag = tag.wrapping_add(1);
        self.events
            .extend(split(bytes, tag).into_iter().map(TransportEvent::Frame));
    }

    // ---- sending ------------------------------------------------------

    fn close_session(&mut self, id: LinkId, k: ExKey) {
        let now = self.now_ms;
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        let opened = link.token.is_some();
        let Some(Exchange::Session(s)) = link.exchanges.remove(&k) else {
            return;
        };
        match s.kind {
            SessKind::Contact => {
                if let (true, Side::Mine, Some(r)) = (opened, k.0, s.remote) {
                    self.redial.insert(
                        key_of(&r),
                        Redial {
                            link: id,
                            contact: r,
                            next_ms: now + REDIAL_FIRST_MS,
                            backoff_ms: REDIAL_FIRST_MS,
                        },
                    );
                }
            }
            SessKind::RequestOut => {
                if let Some((hello, attempt)) = s.hello {
                    self.retry_request(id, hello, attempt);
                }
            }
            SessKind::RequestIn => {}
        }
    }

    fn begin_request(&mut self, id: LinkId, hello: Vec<u8>, attempt: u8) {
        let now = self.now_ms;
        let ok = self.running
            && self.mode == Some(Mode::Open)
            && hello.len() <= MAX_ENVELOPE
            && self.links.contains_key(&id);
        let Some(keys) = self.keys.as_ref().filter(|_| ok) else {
            self.notify(BleEvent::RequestFailed { link: id });
            return;
        };
        let mut hs = Session::request_initiator(keys);
        let Ok(first) = hs.write_message() else {
            self.notify(BleEvent::RequestFailed { link: id });
            return;
        };
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        let xid = link.fresh_id(&mut *self.rng);
        link.exchanges.insert(
            (Side::Mine, xid),
            Exchange::Requesting {
                hs,
                hello,
                sent_ms: now,
                attempt,
            },
        );
        let wire = LinkMessage {
            kind: Kind::Request,
            flags: Side::Mine.flags(),
            body: body_with_id(xid, &first),
        }
        .encode();
        link.put(&mut self.radio, id, &wire);
    }

    fn retry_request(&mut self, id: LinkId, hello: Vec<u8>, attempt: u8) {
        if attempt < REQUEST_ATTEMPTS {
            self.begin_request(id, hello, attempt + 1);
        } else {
            self.notify(BleEvent::RequestFailed { link: id });
        }
    }

    /// One contact hello on a link: to `contact`, or a decoy of the same
    /// size to a key no one holds.
    fn dial(&mut self, id: LinkId, contact: Option<PublicIdentity>, attempt: u8, redial: bool) {
        let now = self.now_ms;
        let unix = self.unix_now();
        let them = match &contact {
            Some(c) => c.clone(),
            None => {
                let mut sign = [0u8; 32];
                let mut dh = [0u8; 32];
                self.rng.fill_bytes(&mut sign);
                self.rng.fill_bytes(&mut dh);
                PublicIdentity { sign, dh }
            }
        };
        let Some(keys) = self.keys.as_ref() else {
            return;
        };
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        let Some(token) = link.token else { return };
        let mut hs = Session::contact_initiator(keys, &them, unix, token);
        let Ok(first) = hs.write_message() else {
            return;
        };
        let xid = link.fresh_id(&mut *self.rng);
        link.exchanges.insert(
            (Side::Mine, xid),
            Exchange::Trial {
                contact,
                hs,
                sent_ms: now,
                attempt,
                redial,
            },
        );
        let wire = LinkMessage {
            kind: Kind::Contact,
            flags: Side::Mine.flags(),
            body: body_with_id(xid, &first),
        }
        .encode();
        link.put(&mut self.radio, id, &wire);
    }

    /// On every link this device opened and has no session on: send the
    /// token's one round of hellos, send it once more unanswered, and
    /// redial contacts whose sessions ended.
    fn trials(&mut self) {
        let now = self.now_ms;
        self.rounds
            .retain(|_, since| now.saturating_sub(*since) < ROUND_MEMORY_MS);
        let ids: Vec<LinkId> = self.links.keys().copied().collect();
        for id in ids {
            let Some(link) = self.links.get(&id) else {
                continue;
            };
            let Some(token) = link.token else { continue };
            if link.has_own_session() {
                continue;
            }
            // unanswered hellos: a round's go once more, afresh
            type Due = (ExKey, Option<PublicIdentity>, u8, bool);
            let due: Vec<Due> = link
                .exchanges
                .iter()
                .filter_map(|(k, e)| match e {
                    Exchange::Trial {
                        contact,
                        sent_ms,
                        attempt,
                        redial,
                        ..
                    } if now.saturating_sub(*sent_ms) >= TRIAL_RETRY_MS => {
                        Some((*k, contact.clone(), *attempt, *redial))
                    }
                    _ => None,
                })
                .collect();
            for (k, contact, attempt, redial) in due {
                if let Some(l) = self.links.get_mut(&id) {
                    l.exchanges.remove(&k);
                }
                if !redial && attempt < TRIAL_ATTEMPTS {
                    self.dial(id, contact, attempt + 1, false);
                }
            }
            // redials, with backoff, outside the rounds
            let pending: BTreeSet<Key> = self
                .links
                .get(&id)
                .map(|l| {
                    l.exchanges
                        .values()
                        .filter_map(|e| match e {
                            Exchange::Trial {
                                contact: Some(c), ..
                            } => Some(key_of(c)),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let due: Vec<PublicIdentity> = self
                .redial
                .iter_mut()
                .filter(|(k, r)| r.link == id && r.next_ms <= now && !pending.contains(*k))
                .map(|(_, r)| {
                    r.backoff_ms = (r.backoff_ms * 2).min(REDIAL_MAX_MS);
                    r.next_ms = now + r.backoff_ms;
                    r.contact.clone()
                })
                .collect();
            for c in due {
                self.dial(id, Some(c), 1, true);
            }
            // the token's one round
            if self.rounds.contains_key(&token) {
                continue;
            }
            if self.rounds.len() >= ROUNDS_CAP {
                let oldest = self.rounds.iter().min_by_key(|(_, s)| **s).map(|(t, _)| *t);
                if let Some(t) = oldest {
                    self.rounds.remove(&t);
                }
            }
            self.rounds.insert(token, now);
            let mut order: Vec<(usize, &PublicIdentity)> =
                self.contacts.iter().enumerate().collect();
            let (seen, tried) = (&self.last_seen, &self.last_tried);
            order.sort_by_key(|(i, c)| {
                let k = key_of(c);
                (
                    std::cmp::Reverse(seen.get(&k).copied()),
                    tried.get(&k).copied(),
                    *i,
                )
            });
            let chosen: Vec<PublicIdentity> = order
                .into_iter()
                .take(TRIAL_CAP)
                .map(|(_, c)| c.clone())
                .collect();
            for c in &chosen {
                self.last_tried.insert(key_of(c), now);
            }
            let decoys = TRIAL_CAP - chosen.len();
            for c in chosen {
                self.dial(id, Some(c), 1, false);
            }
            for _ in 0..decoys {
                self.dial(id, None, 1, false);
            }
        }
    }

    /// Expire half-done handshakes and held requests, run each session's
    /// timers, and close what has gone silent.
    fn upkeep(&mut self) {
        let now = self.now_ms;
        let mut close = Vec::new();
        let mut retry = Vec::new();
        let ids: Vec<LinkId> = self.links.keys().copied().collect();
        for id in ids {
            let Some(link) = self.links.get_mut(&id) else {
                continue;
            };
            let mut writes = Vec::new();
            link.exchanges.retain(|k, e| match e {
                Exchange::Answering { since_ms, .. } => {
                    now.saturating_sub(*since_ms) < HANDSHAKE_MS
                }
                Exchange::Requesting {
                    sent_ms,
                    hello,
                    attempt,
                    ..
                } => {
                    if now.saturating_sub(*sent_ms) >= TRIAL_RETRY_MS {
                        retry.push((id, std::mem::take(hello), *attempt));
                        false
                    } else {
                        true
                    }
                }
                Exchange::Session(s) => {
                    let age = now.saturating_sub(s.started_ms);
                    if !s.confirmed() && age >= HANDSHAKE_MS {
                        return false;
                    }
                    if s.kind == SessKind::RequestIn {
                        match s.heard_ms {
                            None if age >= HANDSHAKE_MS => return false,
                            Some(at)
                                if !s.accepted_by_person
                                    && now.saturating_sub(at) >= REQUEST_MS =>
                            {
                                return false
                            }
                            _ => {}
                        }
                    }
                    let (w, gone) = s.upkeep(now);
                    writes.extend(w);
                    if gone {
                        close.push((id, *k));
                    }
                    true
                }
                Exchange::Trial { .. } => true,
            });
            if !link.exchanges.is_empty() {
                link.busy_ms = now;
            }
            for w in writes {
                link.put(&mut self.radio, id, &w);
            }
        }
        for (id, k) in close {
            self.close_session(id, k);
        }
        for (id, hello, attempt) in retry {
            self.retry_request(id, hello, attempt);
        }
    }

    /// Close links that are idle, that this device opened more than a
    /// token's life ago, or, in Contacts mode, that showed a stranger's
    /// token whose round is over.
    fn close_links(&mut self) {
        let now = self.now_ms;
        let contacts_only = self.mode == Some(Mode::Contacts);
        let redialling: BTreeSet<LinkId> = self.redial.values().map(|r| r.link).collect();
        let doomed: Vec<LinkId> = self
            .links
            .iter()
            .filter(|(id, l)| {
                let idle = now.saturating_sub(l.busy_ms) >= IDLE_MS;
                let aged = l.token.is_some() && now.saturating_sub(l.up_ms) >= TOKEN_MS;
                let stranger = contacts_only
                    && l.token.is_some_and(|t| self.rounds.contains_key(&t))
                    && l.exchanges.is_empty()
                    && !redialling.contains(id);
                idle || aged || stranger
            })
            .map(|(id, _)| *id)
            .collect();
        for id in doomed {
            self.close_link(id);
        }
    }

    /// Take the node's frames in, as whole envelopes.
    fn collect(&mut self) {
        let now = self.now_ms;
        for frame in std::mem::take(&mut self.outbound_frames) {
            for e in self.outbound.push(&frame, now) {
                let Event::Complete { tag, bytes } = e else {
                    continue;
                };
                match targets(&bytes) {
                    Some(targets) if self.running => {
                        if let Some(dropped) = self.queue.add(tag, bytes, targets, now) {
                            self.events.push(TransportEvent::Dropped(dropped));
                        }
                    }
                    _ => self.events.push(TransportEvent::Dropped(tag)),
                }
            }
        }
        self.outbound.expire(now);
    }

    /// Hand waiting envelopes to the sessions of the people they are for.
    fn schedule(&mut self) {
        let now = self.now_ms;
        // confirmed contact sessions, oldest first: a contact's envelopes
        // keep to one session while it lives
        let mut sessions: Vec<(u64, Key, [u8; 8], LinkId, ExKey)> = Vec::new();
        let mut inflight: BTreeSet<(u16, Key)> = BTreeSet::new();
        for (id, link) in &self.links {
            for (k, e) in &link.exchanges {
                let Exchange::Session(s) = e else { continue };
                if s.kind != SessKind::Contact {
                    continue;
                }
                let Some(r) = &s.remote else { continue };
                let rk = key_of(r);
                for tag in s.in_flight() {
                    inflight.insert((tag, rk));
                }
                if s.confirmed() && self.contact_keys.contains(&rk) {
                    sessions.push((s.confirmed_ms.unwrap_or(0), rk, r.fingerprint(), *id, *k));
                }
            }
        }
        sessions.sort();
        let everyone: BTreeSet<Key> = sessions.iter().map(|s| s.1).collect();

        if self.sending {
            for i in 0..self.queue.items.len() {
                let mut wanted: Vec<Key> = Vec::new();
                for s in &sessions {
                    let o = &self.queue.items[i];
                    let fits = match &o.targets {
                        Targets::Everyone => true,
                        Targets::Only(fps) => fps.contains(&s.2),
                    };
                    if fits
                        && !o.done.contains(&s.1)
                        && !inflight.contains(&(o.tag, s.1))
                        && !wanted.contains(&s.1)
                    {
                        wanted.push(s.1);
                    }
                }
                for rk in wanted {
                    let route = sessions
                        .iter()
                        .filter(|s| s.1 == rk)
                        .map(|s| (s.3, s.4))
                        .find(|(id, k)| {
                            self.links
                                .get_mut(id)
                                .and_then(|l| l.session(k))
                                .is_some_and(|s| s.has_room())
                        });
                    let Some((id, k)) = route else { continue };
                    let tag = self.queue.items[i].tag;
                    let Some(link) = self.links.get_mut(&id) else {
                        continue;
                    };
                    let Some(s) = link.session(&k) else { continue };
                    let Some(wire) = s.send_envelope(&self.queue.items[i].envelope, Some(tag), now)
                    else {
                        continue;
                    };
                    link.put(&mut self.radio, id, &wire);
                    inflight.insert((tag, rk));
                    if !self.queue.items[i].playing {
                        self.queue.items[i].playing = true;
                        self.events.push(TransportEvent::Playing(tag));
                    }
                }
            }
        }
        let events = self.queue.retire(now, &everyone);
        self.events.extend(events);
    }

    fn update_peers(&mut self) {
        let now = self.now_ms;
        for (id, link) in &self.links {
            if link.token.is_some() {
                self.seen_links
                    .insert(*id, (now, link.has_confirmed_contact()));
            }
        }
        self.seen_links
            .retain(|_, (at, _)| now.saturating_sub(*at) < STRANGER_MS);
        while self.seen_links.len() > SEEN_LINKS_CAP {
            let oldest = self
                .seen_links
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(id, _)| *id);
            if let Some(id) = oldest {
                self.seen_links.remove(&id);
            }
        }
        let mut contacts: Vec<PublicIdentity> = Vec::new();
        for link in self.links.values() {
            for e in link.exchanges.values() {
                if let Exchange::Session(s) = e {
                    if s.kind == SessKind::Contact && s.confirmed() {
                        if let Some(r) = &s.remote {
                            if !contacts.contains(r) {
                                contacts.push(r.clone());
                            }
                        }
                    }
                }
            }
        }
        let strangers: Vec<LinkId> = self
            .seen_links
            .iter()
            .filter(|(_, (_, contact))| !contact)
            .map(|(id, _)| *id)
            .collect();
        self.shared.lock().unwrap().peers = Peers {
            contacts,
            strangers,
        };
    }

    fn prune_seen(&mut self) {
        let now = self.now_ms;
        while let Some((eph, at)) = self.seen_order.front().copied() {
            if now.saturating_sub(at) < SEEN_MS {
                break;
            }
            self.seen_order.pop_front();
            if self.seen_ephemerals.get(&eph) == Some(&at) {
                self.seen_ephemerals.remove(&eph);
            }
        }
    }

    fn remember_ephemeral(&mut self, eph: [u8; 32]) {
        while self.seen_ephemerals.len() >= SEEN_CAP {
            let Some((old, at)) = self.seen_order.pop_front() else {
                break;
            };
            if self.seen_ephemerals.get(&old) == Some(&at) {
                self.seen_ephemerals.remove(&old);
            }
        }
        self.seen_ephemerals.insert(eph, self.now_ms);
        self.seen_order.push_back((eph, self.now_ms));
    }
}

fn handshake_wire(side: Side, xid: u32, noise: &[u8]) -> Vec<u8> {
    LinkMessage {
        kind: Kind::Handshake,
        flags: side.flags(),
        body: body_with_id(xid, noise),
    }
    .encode()
}

fn is_request_in(e: &Exchange) -> bool {
    match e {
        Exchange::Answering { .. } => true,
        Exchange::Session(s) => s.kind == SessKind::RequestIn,
        _ => false,
    }
}

fn is_request_out(e: &Exchange) -> bool {
    match e {
        Exchange::Requesting { .. } => true,
        Exchange::Session(s) => s.kind == SessKind::RequestOut,
        _ => false,
    }
}

impl<R: Radio> Transport for BleTransport<R> {
    fn name(&self) -> Channel {
        Channel::Bluetooth
    }

    fn send(&mut self, frame: Vec<u8>) {
        self.outbound_frames.push(frame);
    }

    fn poll(&mut self, now_ms: u64) -> Vec<TransportEvent> {
        self.now_ms = self.now_ms.max(now_ms);
        self.apply_control();
        if self.running {
            self.rotate_token();
            for event in self.radio.poll() {
                self.on_radio(event);
            }
            let now = self.now_ms;
            for link in self.links.values_mut() {
                link.reassembly.expire(now);
            }
            self.prune_seen();
            self.trials();
        }
        self.collect();
        if self.running {
            self.schedule();
            self.upkeep();
            self.close_links();
        }
        self.update_peers();
        std::mem::take(&mut self.events)
    }

    fn set_listening(&mut self, on: bool) {
        self.listening = on;
    }

    fn set_sending(&mut self, on: bool) {
        self.sending = on;
    }

    fn delivered(&mut self, tag: u16) {
        self.queue.remove(tag);
    }

    fn withdraw(&mut self, tag: u16) {
        self.queue.remove(tag);
    }
}
