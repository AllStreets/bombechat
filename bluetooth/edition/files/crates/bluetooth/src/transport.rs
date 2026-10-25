//! The link layer: sessions over a [`Radio`], as one of the node's
//! transports.
//!
//! See the crate documentation for the protocol. This file is the state
//! machine: links, the exchanges on each (handshakes and sessions), the
//! envelopes waiting to go, and what the node and the app are told.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use bombechat_core::{Envelope, PublicIdentity, MAX_ENVELOPE};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_node::{Transport, TransportEvent};
use bombechat_session::{Handshake, Session, SessionKeys, Transport as Cipher, TOKEN_LEN};
use bombechat_store::Channel;
use rand_core::{OsRng, RngCore};

use crate::radio::{LinkId, Mode, Radio, RadioEvent};
use crate::route::{targets, Targets};
use crate::wire::{fragment, Kind, LinkMessage, Reassembly, FROM_INITIATOR};

/// A new advertising token this often.
pub const TOKEN_MS: u64 = 15 * 60_000;

/// At most this many contacts are tried on one advertising token...
pub const TRIAL_CAP: usize = 8;
/// ...in this long.
pub const TRIAL_WINDOW_MS: u64 = 10 * 60_000;
/// A contact handshake message 1 unanswered this long is sent again,
/// afresh...
pub const TRIAL_RETRY_MS: u64 = 4_000;
/// ...this many times in all, then given up for the window.
pub const TRIAL_ATTEMPTS: u8 = 3;

/// The seen-ephemeral cache holds at most this many keys...
pub const SEEN_CAP: usize = 4096;
/// ...each for this long: the contact window either way.
pub const SEEN_MS: u64 = 4 * 60_000;

/// A handshake half done, or a contact session the other side has not
/// yet confirmed, is dropped after this long.
pub const HANDSHAKE_MS: u64 = 30_000;

/// At most this many transport messages in flight on a session, and at
/// most this many held ahead of a gap.
pub const WINDOW: usize = 32;
/// Unacknowledged transport messages go again this often...
pub const RESEND_MS: u64 = 1_500;
/// ...and a session that acknowledges nothing for this long is closed.
/// A contact session is then dialled afresh.
pub const STALL_MS: u64 = 15_000;

/// An envelope waits this long for the people it is for to come in range.
pub const HOLD_MS: u64 = 10 * 60_000;
/// At most this many envelopes wait; one more drops the oldest.
pub const QUEUE_CAP: usize = 64;

/// A link counts towards the strangers in range for this long after it
/// was last seen.
pub const STRANGER_MS: u64 = 120_000;

/// At most this many Bluetooth events wait for the app.
pub const EVENTS_CAP: usize = 64;

/// At most this many exchanges (handshakes and sessions) on one link.
const EXCHANGES_CAP: usize = 24;
/// At most this many request handshakes answered and waiting for
/// message 3, and unconfirmed contact sessions, on one link.
const ANSWERING_CAP: usize = 4;

const PING: u8 = 0;
const ENVELOPE: u8 = 1;

const ID_LEN: usize = 4;

/// What the app can know about who is near.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Peers {
    /// Contacts with a confirmed session now.
    pub contacts: Vec<PublicIdentity>,
    /// Links seen in the last [`STRANGER_MS`] that carry no confirmed
    /// contact session. Only links this device opened to an advert are
    /// counted, so a device that also opened a link back is counted once.
    pub strangers: Vec<LinkId>,
}

impl Peers {
    /// How many strangers are in range.
    pub fn strangers_in_range(&self) -> usize {
        self.strangers.len()
    }
}

/// Bluetooth news for the app, beyond what the node's transport events
/// carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BleEvent {
    /// In Open mode, a stranger's request to connect arrived on `link`:
    /// `from` is proved by the handshake; `hello` is the first envelope
    /// the requester sent inside the session. Nothing was sent back but
    /// handshake and acknowledgement bytes, which carry nothing of ours.
    Request {
        link: LinkId,
        from: PublicIdentity,
        hello: Vec<u8>,
    },
    /// The request sent on `link` was received.
    RequestSent { link: LinkId },
    /// The request for `link` could not be sent: the link is gone, the
    /// mode is not Open, or it went unanswered.
    RequestFailed { link: LinkId },
}

#[derive(Default)]
struct Control {
    keys: Option<Option<SessionKeys>>,
    contacts: Option<Vec<PublicIdentity>>,
    mode: Option<Option<Mode>>,
    requests: Vec<(LinkId, Vec<u8>)>,
    events: VecDeque<BleEvent>,
    peers: Peers,
}

/// The app's hold on a [`BleTransport`] once the node owns it. Changes
/// take effect at the transport's next poll.
#[derive(Clone)]
pub struct BleHandle(Arc<Mutex<Control>>);

impl BleHandle {
    /// This device's session keys, or `None` while locked. Without keys
    /// the radio stays off.
    pub fn set_identity(&self, keys: Option<SessionKeys>) {
        self.0.lock().unwrap().keys = Some(keys);
    }

    /// The people a contact session may be with.
    pub fn set_contacts(&self, contacts: Vec<PublicIdentity>) {
        self.0.lock().unwrap().contacts = Some(contacts);
    }

    /// The Bluetooth switch: `None` is Off, the radio stopped.
    pub fn set_mode(&self, mode: Option<Mode>) {
        self.0.lock().unwrap().mode = Some(mode);
    }

    /// Send a request to connect over `link` (one of
    /// [`Peers::strangers`]), carrying `hello`, an envelope. Only in Open
    /// mode, and only from an explicit press.
    pub fn request(&self, link: LinkId, hello: Vec<u8>) {
        self.0.lock().unwrap().requests.push((link, hello));
    }

    /// Who was near at the last poll.
    pub fn peers(&self) -> Peers {
        self.0.lock().unwrap().peers.clone()
    }

    /// Bluetooth news since the last call.
    pub fn take_events(&self) -> Vec<BleEvent> {
        self.0.lock().unwrap().events.drain(..).collect()
    }
}

type Key = [u8; 64];

fn key_of(p: &PublicIdentity) -> Key {
    let mut k = [0u8; 64];
    k[..32].copy_from_slice(&p.sign);
    k[32..].copy_from_slice(&p.dh);
    k
}

/// Who began an exchange: this device, or the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Side {
    Mine,
    Theirs,
}

impl Side {
    fn flags(self) -> u8 {
        match self {
            Side::Mine => FROM_INITIATOR,
            Side::Theirs => 0,
        }
    }

    /// The side a received message belongs to, by its flags.
    fn of_received(flags: u8) -> Side {
        if flags & FROM_INITIATOR != 0 {
            Side::Theirs
        } else {
            Side::Mine
        }
    }
}

type ExKey = (Side, u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessKind {
    Contact,
    /// A request this device sent.
    RequestOut,
    /// A request this device received.
    RequestIn,
}

struct Unacked {
    seq: u32,
    wire: Vec<u8>,
    tag: Option<u16>,
}

struct Sess {
    kind: SessKind,
    cipher: Cipher,
    remote: Option<PublicIdentity>,
    started_ms: u64,
    confirmed_ms: Option<u64>,
    send_next: u32,
    unacked: VecDeque<Unacked>,
    last_progress_ms: u64,
    last_resend_ms: u64,
    recv_next: u32,
    ahead: BTreeMap<u32, Vec<u8>>,
    ack_due: bool,
    /// For a request sent: the hello and which attempt this is.
    hello: Option<(Vec<u8>, u8)>,
    request_heard: bool,
}

impl Sess {
    fn new(kind: SessKind, cipher: Cipher, now_ms: u64) -> Sess {
        let remote = cipher.remote_identity().cloned();
        let confirmed_ms = cipher.confirmed().then_some(now_ms);
        Sess {
            kind,
            cipher,
            remote,
            started_ms: now_ms,
            confirmed_ms,
            send_next: 0,
            unacked: VecDeque::new(),
            last_progress_ms: now_ms,
            last_resend_ms: now_ms,
            recv_next: 0,
            ahead: BTreeMap::new(),
            ack_due: false,
            hello: None,
            request_heard: false,
        }
    }

    fn confirmed(&self) -> bool {
        self.cipher.confirmed()
    }

    fn has_room(&self) -> bool {
        self.unacked.len() < WINDOW
    }

    /// Encrypt one payload as the next transport message; its wire bytes.
    fn seal(
        &mut self,
        side: Side,
        xid: u32,
        payload: &[u8],
        tag: Option<u16>,
        now_ms: u64,
    ) -> Option<Vec<u8>> {
        if self.send_next == u32::MAX {
            return None;
        }
        let ct = self.cipher.encrypt(payload).ok()?;
        let seq = self.send_next;
        self.send_next += 1;
        let mut body = Vec::with_capacity(2 * ID_LEN + ct.len());
        body.extend_from_slice(&xid.to_be_bytes());
        body.extend_from_slice(&seq.to_be_bytes());
        body.extend_from_slice(&ct);
        let wire = LinkMessage {
            kind: Kind::Data,
            flags: side.flags(),
            body,
        }
        .encode();
        if self.unacked.is_empty() {
            self.last_progress_ms = now_ms;
            self.last_resend_ms = now_ms;
        }
        self.unacked.push_back(Unacked {
            seq,
            wire: wire.clone(),
            tag,
        });
        Some(wire)
    }

    /// Take transport message `seq`. The payloads now in order, or `Err`
    /// if one would not decrypt: the session is then broken.
    fn receive(&mut self, seq: u32, ct: &[u8], now_ms: u64) -> Result<Vec<Vec<u8>>, ()> {
        self.ack_due = true;
        if seq < self.recv_next {
            return Ok(Vec::new());
        }
        if seq > self.recv_next {
            if (seq - self.recv_next) as usize <= WINDOW {
                self.ahead.entry(seq).or_insert_with(|| ct.to_vec());
            }
            return Ok(Vec::new());
        }
        let mut out = vec![self.open(ct, now_ms)?];
        while let Some(next) = self.ahead.remove(&self.recv_next) {
            out.push(self.open(&next, now_ms)?);
        }
        Ok(out)
    }

    fn open(&mut self, ct: &[u8], now_ms: u64) -> Result<Vec<u8>, ()> {
        let plain = self.cipher.decrypt(ct).map_err(|_| ())?;
        self.recv_next += 1;
        self.confirmed_ms.get_or_insert(now_ms);
        Ok(plain)
    }

    /// The other side has every message before `next`. The tags whose
    /// envelopes it now holds.
    fn acknowledge(&mut self, next: u32, now_ms: u64) -> Vec<u16> {
        if next > self.send_next {
            return Vec::new();
        }
        let mut tags = Vec::new();
        while self.unacked.front().is_some_and(|u| u.seq < next) {
            if let Some(u) = self.unacked.pop_front() {
                tags.extend(u.tag);
            }
            self.last_progress_ms = now_ms;
            self.last_resend_ms = now_ms;
        }
        tags
    }

    fn ack_wire(&self, side: Side, xid: u32) -> Vec<u8> {
        let mut body = xid.to_be_bytes().to_vec();
        body.extend_from_slice(&self.recv_next.to_be_bytes());
        LinkMessage {
            kind: Kind::Ack,
            flags: side.flags(),
            body,
        }
        .encode()
    }
}

enum Exchange {
    /// A contact handshake this device began: message 1 sent, waiting for
    /// message 2.
    Trial {
        contact: PublicIdentity,
        hs: Handshake,
        sent_ms: u64,
        attempt: u8,
    },
    /// A request this device began: message 1 sent, waiting for message 2.
    Requesting {
        hs: Handshake,
        hello: Vec<u8>,
        sent_ms: u64,
        attempt: u8,
    },
    /// A request the other side began: message 2 sent, waiting for
    /// message 3.
    Answering {
        hs: Handshake,
        since_ms: u64,
    },
    Session(Box<Sess>),
}

struct Link {
    token: Option<[u8; TOKEN_LEN]>,
    reassembly: Reassembly,
    next_fragment: u16,
    exchanges: HashMap<ExKey, Exchange>,
}

impl Link {
    fn session(&mut self, k: &ExKey) -> Option<&mut Sess> {
        match self.exchanges.get_mut(k) {
            Some(Exchange::Session(s)) => Some(s),
            _ => None,
        }
    }

    fn has_confirmed_contact(&self) -> bool {
        self.exchanges.values().any(
            |e| matches!(e, Exchange::Session(s) if s.kind == SessKind::Contact && s.confirmed()),
        )
    }

    /// A fresh exchange number for this device's side.
    fn fresh_id(&self) -> u32 {
        loop {
            let id = OsRng.next_u32();
            if !self.exchanges.contains_key(&(Side::Mine, id)) {
                return id;
            }
        }
    }
}

/// Write one link message to `link`, in fragments the link takes.
fn put<R: Radio>(radio: &mut R, id: LinkId, link: &mut Link, message: &[u8]) {
    let n = link.next_fragment;
    link.next_fragment = n.wrapping_add(1);
    for f in fragment(message, n, radio.max_write(id)) {
        radio.write(id, &f);
    }
}

fn body_with_id(xid: u32, rest: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(ID_LEN + rest.len());
    b.extend_from_slice(&xid.to_be_bytes());
    b.extend_from_slice(rest);
    b
}

struct Outgoing {
    tag: u16,
    envelope: Vec<u8>,
    targets: Targets,
    done: HashSet<Key>,
    done_fps: HashSet<[u8; 8]>,
    since_ms: u64,
    playing: bool,
    reported: bool,
}

struct TokenTrials {
    since_ms: u64,
    tried: Vec<Key>,
}

struct Tokens {
    current: [u8; TOKEN_LEN],
    previous: Option<[u8; TOKEN_LEN]>,
    since_ms: u64,
}

/// The Bluetooth transport: [`Channel::Bluetooth`] for the node, over any
/// [`Radio`]. Its clock is the `now_ms` the node passes to
/// [`Transport::poll`]; it reads none of its own.
pub struct BleTransport<R: Radio> {
    radio: R,
    shared: Arc<Mutex<Control>>,
    unix_base_ms: u64,
    keys: Option<SessionKeys>,
    contacts: Vec<PublicIdentity>,
    contact_keys: HashSet<Key>,
    mode: Option<Mode>,
    running: bool,
    tokens: Option<Tokens>,
    links: HashMap<LinkId, Link>,
    seen_links: HashMap<LinkId, (u64, bool)>,
    last_seen: HashMap<Key, u64>,
    token_trials: HashMap<[u8; TOKEN_LEN], TokenTrials>,
    seen_ephemerals: HashMap<[u8; 32], u64>,
    seen_order: VecDeque<([u8; 32], u64)>,
    outbound_frames: Vec<Vec<u8>>,
    outbound: Reassembler,
    queue: Vec<Outgoing>,
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
        BleTransport {
            radio,
            shared: Arc::new(Mutex::new(Control::default())),
            unix_base_ms,
            keys: None,
            contacts: Vec::new(),
            contact_keys: HashSet::new(),
            mode: None,
            running: false,
            tokens: None,
            links: HashMap::new(),
            seen_links: HashMap::new(),
            last_seen: HashMap::new(),
            token_trials: HashMap::new(),
            seen_ephemerals: HashMap::new(),
            seen_order: VecDeque::new(),
            outbound_frames: Vec::new(),
            outbound: Reassembler::new(),
            queue: Vec::new(),
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
        self.queue.len()
    }

    fn unix_now(&self) -> u64 {
        self.unix_base_ms.saturating_add(self.now_ms)
    }

    fn me(&self) -> Option<Key> {
        self.keys.as_ref().map(|k| key_of(k.public()))
    }

    fn notify(&self, event: BleEvent) {
        let mut c = self.shared.lock().unwrap();
        if c.events.len() >= EVENTS_CAP {
            c.events.pop_front();
        }
        c.events.push_back(event);
    }

    // ---- switching ----------------------------------------------------

    fn apply_control(&mut self) {
        let (keys, contacts, mode, requests) = {
            let mut c = self.shared.lock().unwrap();
            (
                c.keys.take(),
                c.contacts.take(),
                c.mode.take(),
                std::mem::take(&mut c.requests),
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
    }

    fn set_contacts(&mut self, contacts: Vec<PublicIdentity>) {
        let me = self.me();
        self.contacts = contacts
            .into_iter()
            .filter(|c| Some(key_of(c)) != me)
            .collect();
        self.contact_keys = self.contacts.iter().map(key_of).collect();
        let keys = &self.contact_keys;
        for link in self.links.values_mut() {
            link.exchanges.retain(|_, e| match e {
                Exchange::Trial { contact, .. } => keys.contains(&key_of(contact)),
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
        let token = random_token();
        self.tokens = Some(Tokens {
            current: token,
            previous: None,
            since_ms: self.now_ms,
        });
        self.radio.advertise(token);
    }

    fn halt(&mut self) {
        self.radio.stop();
        self.running = false;
        self.tokens = None;
        self.links.clear();
        self.seen_links.clear();
        self.token_trials.clear();
        for o in self.queue.drain(..) {
            if !o.reported {
                self.events.push(TransportEvent::Dropped(o.tag));
            }
        }
    }

    fn drop_requests(&mut self) {
        let mut failed = Vec::new();
        for (id, link) in &mut self.links {
            link.exchanges.retain(|_, e| match e {
                Exchange::Requesting { .. } => {
                    failed.push(*id);
                    false
                }
                Exchange::Answering { .. } => false,
                Exchange::Session(s) => {
                    if s.kind == SessKind::RequestOut {
                        failed.push(*id);
                    }
                    s.kind == SessKind::Contact
                }
                Exchange::Trial { .. } => true,
            });
        }
        for link in failed {
            self.notify(BleEvent::RequestFailed { link });
        }
    }

    fn rotate_token(&mut self) {
        let now = self.now_ms;
        let Some(t) = self.tokens.as_mut() else {
            return;
        };
        if now.saturating_sub(t.since_ms) < TOKEN_MS {
            return;
        }
        t.previous = Some(t.current);
        t.current = random_token();
        t.since_ms = now;
        let token = t.current;
        self.radio.advertise(token);
    }

    // ---- receiving ----------------------------------------------------

    fn on_radio(&mut self, event: RadioEvent) {
        match event {
            RadioEvent::Up { link, token } => {
                self.links.insert(
                    link,
                    Link {
                        token,
                        reassembly: Reassembly::new(),
                        next_fragment: 0,
                        exchanges: HashMap::new(),
                    },
                );
            }
            RadioEvent::Down(link) => {
                let Some(gone) = self.links.remove(&link) else {
                    return;
                };
                let mut request_failed = false;
                for e in gone.exchanges.values() {
                    match e {
                        // a contact reached here may be dialled again on its token
                        Exchange::Session(s) if s.kind == SessKind::Contact => {
                            self.may_dial_again(gone.token, s.remote.as_ref())
                        }
                        Exchange::Session(s) if s.kind == SessKind::RequestOut => {
                            request_failed = true
                        }
                        Exchange::Requesting { .. } => request_failed = true,
                        _ => {}
                    }
                }
                if request_failed {
                    self.notify(BleEvent::RequestFailed { link });
                }
            }
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
            Kind::Data if rest.len() >= ID_LEN => {
                let seq = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
                self.on_data(link, (side, xid), seq, &rest[ID_LEN..]);
            }
            Kind::Ack if rest.len() == ID_LEN => {
                let next = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
                self.on_ack(link, (side, xid), next);
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
        // at most a few unconfirmed contact sessions on one link
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
        while unconfirmed.len() >= ANSWERING_CAP {
            let (_, k) = unconfirmed.remove(0);
            link.exchanges.remove(&k);
        }
        link.exchanges.insert(
            (Side::Theirs, xid),
            Exchange::Session(Box::new(Sess::new(SessKind::Contact, cipher, now))),
        );
        let wire = LinkMessage {
            kind: Kind::Handshake,
            flags: Side::Theirs.flags(),
            body: body_with_id(xid, &reply),
        }
        .encode();
        put(&mut self.radio, id, link, &wire);
    }

    /// Message 1 of a request to connect: answered in Open mode only.
    fn answer_request(&mut self, id: LinkId, xid: u32, noise: &[u8]) {
        if self.mode != Some(Mode::Open) || self.keys.is_none() {
            return;
        }
        let now = self.now_ms;
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        if link.exchanges.contains_key(&(Side::Theirs, xid))
            || link.exchanges.len() >= EXCHANGES_CAP
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
        let mut answering: Vec<(u64, ExKey)> = link
            .exchanges
            .iter()
            .filter_map(|(k, e)| match e {
                Exchange::Answering { since_ms, .. } => Some((*since_ms, *k)),
                _ => None,
            })
            .collect();
        answering.sort();
        while answering.len() >= ANSWERING_CAP {
            let (_, k) = answering.remove(0);
            link.exchanges.remove(&k);
        }
        link.exchanges.insert(
            (Side::Theirs, xid),
            Exchange::Answering { hs, since_ms: now },
        );
        let wire = LinkMessage {
            kind: Kind::Handshake,
            flags: Side::Theirs.flags(),
            body: body_with_id(xid, &reply),
        }
        .encode();
        put(&mut self.radio, id, link, &wire);
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
                if hs.read_message(noise).is_err() {
                    return;
                }
                let Ok(cipher) = hs.into_transport() else {
                    return;
                };
                // the contact answered: no more trials on this link
                link.exchanges
                    .retain(|_, e| !matches!(e, Exchange::Trial { .. }));
                link.exchanges.insert(
                    k,
                    Exchange::Session(Box::new(Sess::new(SessKind::Contact, cipher, now))),
                );
                self.last_seen.insert(key_of(&contact), now);
                // the first transport message confirms us to the responder
                self.send_payload(id, k, &[PING], None);
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
                let mut s = Sess::new(SessKind::RequestOut, cipher, now);
                s.hello = Some((hello.clone(), attempt));
                link.exchanges.insert(k, Exchange::Session(Box::new(s)));
                let wire = LinkMessage {
                    kind: Kind::Handshake,
                    flags: Side::Mine.flags(),
                    body: body_with_id(k.1, &third),
                }
                .encode();
                put(&mut self.radio, id, link, &wire);
                let mut payload = vec![ENVELOPE];
                payload.extend_from_slice(&hello);
                self.send_payload(id, k, &payload, None);
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
                link.exchanges.insert(
                    k,
                    Exchange::Session(Box::new(Sess::new(SessKind::RequestIn, cipher, now))),
                );
            }
            // a late or repeated handshake message for a session: ignore
            session @ Exchange::Session(_) => {
                link.exchanges.insert(k, session);
            }
        }
    }

    fn on_data(&mut self, id: LinkId, k: ExKey, seq: u32, ct: &[u8]) {
        let now = self.now_ms;
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        let Some(s) = link.session(&k) else { return };
        let was_confirmed = s.confirmed();
        let Ok(payloads) = s.receive(seq, ct, now) else {
            self.close_session(id, k);
            return;
        };
        let kind = s.kind;
        let remote = s.remote.clone();
        if !was_confirmed && s.confirmed() {
            if let Some(r) = &remote {
                self.last_seen.insert(key_of(r), now);
            }
            // one confirmed session per contact per link: older ones go
            if kind == SessKind::Contact {
                if let Some(link) = self.links.get_mut(&id) {
                    link.exchanges.retain(|other, e| match e {
                        Exchange::Session(o) if *other != k && o.kind == SessKind::Contact => {
                            o.remote != remote
                        }
                        _ => true,
                    });
                }
            }
        }
        for p in payloads {
            if p.first() != Some(&ENVELOPE) {
                continue;
            }
            let bytes = &p[1..];
            match kind {
                SessKind::Contact => self.hand_over(bytes),
                SessKind::RequestIn => {
                    let Some(link) = self.links.get_mut(&id) else {
                        return;
                    };
                    let Some(s) = link.session(&k) else { return };
                    if s.request_heard {
                        continue;
                    }
                    s.request_heard = true;
                    if let Some(from) = remote.clone() {
                        self.notify(BleEvent::Request {
                            link: id,
                            from,
                            hello: bytes.to_vec(),
                        });
                    }
                }
                SessKind::RequestOut => {}
            }
        }
    }

    /// An envelope from a contact, to the node as the frames it reads.
    fn hand_over(&mut self, bytes: &[u8]) {
        if !self.listening || bytes.len() > MAX_ENVELOPE || Envelope::decode(bytes).is_err() {
            return;
        }
        let tag = self.next_tag;
        self.next_tag = tag.wrapping_add(1);
        self.events
            .extend(split(bytes, tag).into_iter().map(TransportEvent::Frame));
    }

    fn on_ack(&mut self, id: LinkId, k: ExKey, next: u32) {
        let now = self.now_ms;
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        let Some(s) = link.session(&k) else { return };
        let tags = s.acknowledge(next, now);
        let request_done = s.kind == SessKind::RequestOut && s.unacked.is_empty();
        if let Some(remote) = s.remote.clone() {
            let rk = key_of(&remote);
            let fp = remote.fingerprint();
            for tag in tags {
                if let Some(o) = self.queue.iter_mut().find(|o| o.tag == tag) {
                    o.done.insert(rk);
                    o.done_fps.insert(fp);
                }
            }
        }
        if request_done {
            link.exchanges.remove(&k);
            self.notify(BleEvent::RequestSent { link: id });
        }
    }

    // ---- sending ------------------------------------------------------

    fn send_payload(&mut self, id: LinkId, k: ExKey, payload: &[u8], tag: Option<u16>) -> bool {
        let now = self.now_ms;
        let Some(link) = self.links.get_mut(&id) else {
            return false;
        };
        let Some(s) = link.session(&k) else {
            return false;
        };
        let Some(wire) = s.seal(k.0, k.1, payload, tag, now) else {
            return false;
        };
        put(&mut self.radio, id, link, &wire);
        true
    }

    fn close_session(&mut self, id: LinkId, k: ExKey) {
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        let token = link.token;
        let Some(Exchange::Session(s)) = link.exchanges.remove(&k) else {
            return;
        };
        match s.kind {
            SessKind::Contact => self.may_dial_again(token, s.remote.as_ref()),
            SessKind::RequestOut => {
                if let Some((hello, attempt)) = s.hello {
                    self.retry_request(id, hello, attempt);
                }
            }
            SessKind::RequestIn => {}
        }
    }

    /// A session with `remote` on a link showing `token` ended: the
    /// contact may be dialled on that token again within the window.
    fn may_dial_again(&mut self, token: Option<[u8; TOKEN_LEN]>, remote: Option<&PublicIdentity>) {
        if let (Some(token), Some(r)) = (token, remote) {
            if let Some(t) = self.token_trials.get_mut(&token) {
                let rk = key_of(r);
                t.tried.retain(|k| *k != rk);
            }
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
        let xid = link.fresh_id();
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
        put(&mut self.radio, id, link, &wire);
    }

    fn retry_request(&mut self, id: LinkId, hello: Vec<u8>, attempt: u8) {
        if attempt < TRIAL_ATTEMPTS {
            self.begin_request(id, hello, attempt + 1);
        } else {
            self.notify(BleEvent::RequestFailed { link: id });
        }
    }

    /// Dial a contact on a link: message 1 of a contact handshake.
    fn dial(&mut self, id: LinkId, contact: PublicIdentity, attempt: u8) {
        let now = self.now_ms;
        let unix = self.unix_now();
        let Some(keys) = self.keys.as_ref() else {
            return;
        };
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        let Some(token) = link.token else { return };
        let mut hs = Session::contact_initiator(keys, &contact, unix, token);
        let Ok(first) = hs.write_message() else {
            return;
        };
        let xid = link.fresh_id();
        link.exchanges.insert(
            (Side::Mine, xid),
            Exchange::Trial {
                contact,
                hs,
                sent_ms: now,
                attempt,
            },
        );
        let wire = LinkMessage {
            kind: Kind::Contact,
            flags: Side::Mine.flags(),
            body: body_with_id(xid, &first),
        }
        .encode();
        put(&mut self.radio, id, link, &wire);
    }

    /// Start, repeat and give up contact trials on every link that shows a
    /// token and has no session of ours yet.
    fn trials(&mut self) {
        let now = self.now_ms;
        self.token_trials
            .retain(|_, t| now.saturating_sub(t.since_ms) < TRIAL_WINDOW_MS);
        let ids: Vec<LinkId> = self.links.keys().copied().collect();
        for id in ids {
            let Some(link) = self.links.get(&id) else {
                continue;
            };
            let Some(token) = link.token else { continue };
            let ours = link.exchanges.iter().any(|(k, e)| {
                k.0 == Side::Mine
                    && matches!(e, Exchange::Session(s) if s.kind == SessKind::Contact)
            });
            if ours {
                continue;
            }
            // repeat unanswered trials afresh, or give them up
            let due: Vec<(ExKey, PublicIdentity, u8)> = link
                .exchanges
                .iter()
                .filter_map(|(k, e)| match e {
                    Exchange::Trial {
                        contact,
                        sent_ms,
                        attempt,
                        ..
                    } if now.saturating_sub(*sent_ms) >= TRIAL_RETRY_MS => {
                        Some((*k, contact.clone(), *attempt))
                    }
                    _ => None,
                })
                .collect();
            for (k, contact, attempt) in due {
                if let Some(l) = self.links.get_mut(&id) {
                    l.exchanges.remove(&k);
                }
                if attempt < TRIAL_ATTEMPTS {
                    self.dial(id, contact, attempt + 1);
                }
            }
            // new candidates, most recently seen first, within the cap
            let t = self.token_trials.entry(token).or_insert(TokenTrials {
                since_ms: now,
                tried: Vec::new(),
            });
            let mut candidates: Vec<&PublicIdentity> = self
                .contacts
                .iter()
                .filter(|c| !t.tried.contains(&key_of(c)))
                .collect();
            let last_seen = &self.last_seen;
            candidates.sort_by_key(|c| std::cmp::Reverse(last_seen.get(&key_of(c)).copied()));
            let room = TRIAL_CAP.saturating_sub(t.tried.len());
            let chosen: Vec<PublicIdentity> = candidates.into_iter().take(room).cloned().collect();
            t.tried.extend(chosen.iter().map(key_of));
            for c in chosen {
                self.dial(id, c, 1);
            }
        }
    }

    /// Expire half-done handshakes, resend what is unacknowledged, close
    /// what has stalled, and send the acknowledgements due.
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
                    if !s.confirmed() && now.saturating_sub(s.started_ms) >= HANDSHAKE_MS {
                        return false;
                    }
                    if !s.unacked.is_empty() {
                        if now.saturating_sub(s.last_progress_ms) >= STALL_MS {
                            close.push((id, *k));
                            return true;
                        }
                        if now.saturating_sub(s.last_resend_ms) >= RESEND_MS {
                            s.last_resend_ms = now;
                            writes.extend(s.unacked.iter().map(|u| u.wire.clone()));
                        }
                    }
                    if s.ack_due {
                        s.ack_due = false;
                        writes.push(s.ack_wire(k.0, k.1));
                    }
                    true
                }
                Exchange::Trial { .. } => true,
            });
            for w in writes {
                put(&mut self.radio, id, link, &w);
            }
        }
        for (id, k) in close {
            self.close_session(id, k);
        }
        for (id, hello, attempt) in retry {
            self.retry_request(id, hello, attempt);
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
                let route = targets(&bytes);
                match route {
                    Some(targets) if self.running => {
                        self.queue.retain(|o| o.tag != tag);
                        if self.queue.len() >= QUEUE_CAP {
                            let old = self.queue.remove(0);
                            if !old.reported {
                                self.events.push(TransportEvent::Dropped(old.tag));
                            }
                        }
                        self.queue.push(Outgoing {
                            tag,
                            envelope: bytes,
                            targets,
                            done: HashSet::new(),
                            done_fps: HashSet::new(),
                            since_ms: now,
                            playing: false,
                            reported: false,
                        });
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
        // confirmed contact sessions, oldest first, by who is on the end:
        // a contact's envelopes keep to one session while it lives
        let mut sessions: Vec<(u64, Key, [u8; 8], LinkId, ExKey)> = Vec::new();
        let mut inflight: HashSet<(u16, Key)> = HashSet::new();
        for (id, link) in &self.links {
            for (k, e) in &link.exchanges {
                let Exchange::Session(s) = e else { continue };
                if s.kind != SessKind::Contact {
                    continue;
                }
                let Some(r) = &s.remote else { continue };
                let rk = key_of(r);
                for u in &s.unacked {
                    if let Some(tag) = u.tag {
                        inflight.insert((tag, rk));
                    }
                }
                if s.confirmed() && self.contact_keys.contains(&rk) {
                    sessions.push((s.confirmed_ms.unwrap_or(0), rk, r.fingerprint(), *id, *k));
                }
            }
        }
        sessions.sort_by_key(|s| s.0);
        let everyone: HashSet<Key> = sessions.iter().map(|s| s.1).collect();

        if self.sending {
            for i in 0..self.queue.len() {
                let mut wanted: Vec<Key> = Vec::new();
                for s in &sessions {
                    let o = &self.queue[i];
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
                    let tag = self.queue[i].tag;
                    let mut payload = vec![ENVELOPE];
                    payload.extend_from_slice(&self.queue[i].envelope);
                    if self.send_payload(id, k, &payload, Some(tag)) {
                        inflight.insert((tag, rk));
                        if !self.queue[i].playing {
                            self.queue[i].playing = true;
                            self.events.push(TransportEvent::Playing(tag));
                        }
                    }
                }
            }
        }

        // report and retire
        let mut events = Vec::new();
        self.queue.retain_mut(|o| {
            if !o.reported {
                let complete = match &o.targets {
                    Targets::Only(fps) => fps.iter().all(|fp| o.done_fps.contains(fp)),
                    Targets::Everyone => !o.done.is_empty() && everyone.is_subset(&o.done),
                };
                if complete {
                    o.reported = true;
                    events.push(TransportEvent::Sent(o.tag));
                }
            }
            if now.saturating_sub(o.since_ms) >= HOLD_MS {
                if !o.reported {
                    events.push(if o.done.is_empty() {
                        TransportEvent::Dropped(o.tag)
                    } else {
                        TransportEvent::Sent(o.tag)
                    });
                }
                return false;
            }
            !(o.reported && matches!(o.targets, Targets::Only(_)))
        });
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
        let mut strangers: Vec<LinkId> = self
            .seen_links
            .iter()
            .filter(|(_, (_, contact))| !contact)
            .map(|(id, _)| *id)
            .collect();
        strangers.sort();
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

fn random_token() -> [u8; TOKEN_LEN] {
    let mut t = [0u8; TOKEN_LEN];
    OsRng.fill_bytes(&mut t);
    t
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
        self.queue.retain(|o| o.tag != tag);
    }

    fn withdraw(&mut self, tag: u16) {
        self.queue.retain(|o| o.tag != tag);
    }
}
