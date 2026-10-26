//! The link layer as one of the node's transports: links and their
//! lifetimes, contact rounds and redials, requests, and envelopes to and
//! from the node. Sessions are in `session`, the waiting envelopes in
//! `queue`.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use bombechat_core::{Envelope, Identity, PublicIdentity, MAX_ENVELOPE};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_node::{
    read_hello, Transport, TransportEvent, NOT_IN_RANGE, REQUESTS_WAITING, REQUEST_GAP_MS,
};
use bombechat_session::{Handshake, Session, SessionKeys, TOKEN_LEN};
use bombechat_store::Channel;
use rand_core::{OsRng, RngCore};

use crate::clock::{SystemClock, WallClock};
use crate::handle::{BleHandle, Control, Peers};
use crate::link::{body_with_id, ExKey, Exchange, Link, Side};
use crate::queue::{Key, Queue};
use crate::radio::{Advert, LinkId, Mode, Radio, RadioEvent, TAG_LEN};
use crate::route::{targets, Targets};
use crate::session::{LinkSession, SessKind, ACK_DELAY_MAX_MS};
use crate::wire::{Kind, LinkMessage};

/// The advertising token changes at a random moment between this...
pub const TOKEN_MIN_MS: u64 = 12 * 60_000;
/// ...and this after the last change. A link this device opened is
/// closed once it is this old, so its token is never more than one
/// rotation stale.
pub const TOKEN_MS: u64 = 15 * 60_000;

/// A contact message 1 may name this device's current token or one of
/// this many before it (about an hour's worth), for a contact that
/// learned it inside a session.
pub const PAST_TOKENS: usize = 4;
/// This device draws its tokens this many rotations ahead, and tells
/// contacts them in order with the current one, so a contact recognises
/// its advert through that many rotations made while the two were apart.
/// A hello may name one of them too: a future token is told only to
/// contacts, and becomes current only when this device rotates to it.
pub const FUTURE_TOKENS: usize = 3;
/// A contact's token schedule learned inside a session is kept this long
/// after it was last heard: as long as that contact could still answer to
/// one of its tokens.
pub const CONTACT_TOKEN_MS: u64 = (PAST_TOKENS + FUTURE_TOKENS) as u64 * TOKEN_MS + TOKEN_MS;

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
/// unless the person accepted it; an accepted one, this long after the
/// accept if the proof hasn't finished; a request sent waits this long
/// after it was delivered for an answer.
pub const REQUEST_MS: u64 = 10 * 60_000;
/// At most this many requests received are held, one per link; one more
/// pushes out the oldest the person hasn't accepted. Accepted ones never
/// go this way. The node owns the number ([`REQUESTS_WAITING`]): the
/// transport holds no more than the node will show. The gap between two
/// requests from one link or identity is the node's too
/// ([`REQUEST_GAP_MS`]).
pub const REQUESTS_CAP: usize = REQUESTS_WAITING;
/// At most this many requests received are part way (handshake not done,
/// or no hello yet); one more drops the oldest of them, never one already
/// reported.
pub const REQUESTS_PART_WAY_CAP: usize = 4;
/// A request this device sends is begun afresh, unanswered or broken
/// off, until it has been tried this many times; then it has failed.
pub const REQUEST_ATTEMPTS: u8 = 3;

/// A link with no session and no handshake for this long is closed.
pub const IDLE_MS: u64 = 120_000;
/// At most this many links other devices opened to this one (their
/// subscriptions to its advert); one more is closed at once. Counted
/// apart from the links this device opens, so strangers' subscriptions,
/// which a peripheral cannot drop, never take the room that dialling a
/// contact needs.
pub const MAX_LINKS_IN: usize = 32;
/// At most this many links this device opened to adverts; one more is
/// closed at once...
pub const MAX_LINKS_OUT: usize = 32;
/// ...and this many of them are kept for adverts it knows as a
/// contact's (by a token that contact told it in a session, or one it
/// lost a contact's link on): links to other adverts stop short of them.
pub const CONTACT_LINKS: usize = 8;

/// A stranger's advert counts towards the strangers seen for this long
/// after a link to it was last up: fifteen minutes, a token's life, so a
/// stranger whose idle link was closed (and isn't connected to again
/// until its token changes) is still counted...
pub const STRANGER_MS: u64 = TOKEN_MS;
/// ...and at most this many are remembered.
const SEEN_LINKS_CAP: usize = 64;
/// At most this many links and identities are remembered for the gap
/// between two requests.
const REQUEST_MEMORY_CAP: usize = 256;

/// Why a send is refused with the switch at Off.
pub const BLUETOOTH_SEND_OFF: &str = "not sent: bluetooth is off";
/// Why a request to connect cannot be sent unless the switch is at Open.
pub const REQUESTS_NEED_OPEN: &str = "Requests go out only with Bluetooth set to Open.";

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
    /// The tokens before it, newest first, at most [`PAST_TOKENS`].
    past: VecDeque<[u8; TOKEN_LEN]>,
    /// The next [`FUTURE_TOKENS`], in the order they will be current.
    future: VecDeque<[u8; TOKEN_LEN]>,
    next_ms: u64,
}

struct Redial {
    link: LinkId,
    contact: PublicIdentity,
    next_ms: u64,
    backoff_ms: u64,
}

/// The Bluetooth transport: [`Channel::Bluetooth`] for the node, over any
/// [`Radio`]. Its timers run on the `now_ms` the node passes to
/// [`Transport::poll`]; contact handshakes and request hellos are stamped
/// and judged by its [`WallClock`], read afresh each time.
pub struct BleTransport<R: Radio> {
    radio: R,
    rng: Box<dyn RngCore + Send>,
    shared: Arc<Mutex<Control>>,
    clock: Box<dyn WallClock>,
    keys: Option<SessionKeys>,
    contacts: Vec<PublicIdentity>,
    contact_keys: BTreeSet<Key>,
    mode: Option<Mode>,
    running: bool,
    tokens: Option<Tokens>,
    /// Each contact's token schedule (current, then the next ones), as it
    /// last said inside a session, and when: memory only.
    contact_tokens: BTreeMap<Key, (Vec<[u8; TOKEN_LEN]>, u64)>,
    links: BTreeMap<LinkId, Link>,
    /// Adverts this device opened links to, by what their round is
    /// remembered by: when a link to it was last up, and whether it
    /// carried a confirmed contact session.
    seen_links: BTreeMap<[u8; TOKEN_LEN], (u64, bool)>,
    /// When a request was last reported from each link, and from each
    /// identity: at most one each in [`REQUEST_GAP_MS`].
    request_links: BTreeMap<LinkId, u64>,
    request_ids: BTreeMap<Key, u64>,
    /// Links whose request received the node was told of and not yet told
    /// is gone.
    reported: BTreeSet<LinkId>,
    /// Links the node asked to send a request on, not yet told how it
    /// ended, and whether the request was delivered: one that was ends
    /// [`TransportEvent::RequestUnanswered`], one that never was
    /// [`TransportEvent::RequestFailed`].
    asked: BTreeMap<LinkId, bool>,
    /// What [`TransportEvent::Peers`] last said.
    said: Option<Peers>,
    last_seen: BTreeMap<Key, u64>,
    last_tried: BTreeMap<Key, u64>,
    rounds: BTreeMap<[u8; TOKEN_LEN], u64>,
    redial: BTreeMap<Key, Redial>,
    /// Contacts whose link (to an advert with this token) the radio lost:
    /// redialled at once on the next link to the same token, which gets
    /// no new round of its own.
    lost: BTreeMap<[u8; TOKEN_LEN], (Vec<PublicIdentity>, u64)>,
    seen_ephemerals: HashMap<[u8; 32], u64>,
    seen_order: VecDeque<([u8; 32], u64)>,
    /// Proofs of an accept, answered only once the node has taken the one
    /// who accepted as a contact ([`BleTransport::release_proofs`]).
    proofs: Vec<HeldProof>,
    outbound_frames: Vec<Vec<u8>>,
    outbound: Reassembler,
    queue: Queue,
    events: Vec<TransportEvent>,
    next_tag: u16,
    listening: bool,
    sending: bool,
    now_ms: u64,
}

/// A contact handshake's message 1 from the one who accepted a request
/// this device sent, read and not yet answered: the node decides first.
struct HeldProof {
    link: LinkId,
    xid: u32,
    hs: Handshake,
    remote: PublicIdentity,
    since_ms: u64,
}

impl<R: Radio> BleTransport<R> {
    /// A transport over `radio`, off until it has keys and a mode.
    /// Contact handshakes carry the system's wall clock as it reads when
    /// each is built, and a device more than two minutes off answers none.
    pub fn new(radio: R) -> BleTransport<R> {
        BleTransport::with_rng(radio, SystemClock, Box::new(OsRng))
    }

    /// As [`BleTransport::new`], reading the Unix time from `clock` and
    /// drawing exchange numbers, tokens, decoys and rotation times from
    /// `rng`: tests pass a [`crate::TestClock`] and a seeded rng.
    pub fn with_rng(
        radio: R,
        clock: impl WallClock + 'static,
        rng: Box<dyn RngCore + Send>,
    ) -> BleTransport<R> {
        BleTransport {
            radio,
            rng,
            shared: Arc::new(Mutex::new(Control::default())),
            clock: Box::new(clock),
            keys: None,
            contacts: Vec::new(),
            contact_keys: BTreeSet::new(),
            mode: None,
            running: false,
            tokens: None,
            contact_tokens: BTreeMap::new(),
            links: BTreeMap::new(),
            seen_links: BTreeMap::new(),
            request_links: BTreeMap::new(),
            request_ids: BTreeMap::new(),
            reported: BTreeSet::new(),
            asked: BTreeMap::new(),
            said: None,
            last_seen: BTreeMap::new(),
            last_tried: BTreeMap::new(),
            rounds: BTreeMap::new(),
            redial: BTreeMap::new(),
            lost: BTreeMap::new(),
            seen_ephemerals: HashMap::new(),
            seen_order: VecDeque::new(),
            proofs: Vec::new(),
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

    /// Forget every schedule contacts told, as if none had been.
    #[cfg(test)]
    pub(crate) fn forget_told(&mut self) {
        self.contact_tokens.clear();
    }

    /// Links this device opened that carry a contact session it began.
    #[cfg(test)]
    pub(crate) fn own_sessions(&self) -> usize {
        self.links
            .values()
            .filter(|l| l.opened && l.has_own_session())
            .count()
    }

    /// When the token next changes, while running.
    #[cfg(test)]
    pub(crate) fn next_rotation_ms(&self) -> Option<u64> {
        self.tokens.as_ref().map(|t| t.next_ms)
    }

    /// The tokens a contact message 1 may name: current, then the
    /// [`PAST_TOKENS`] before it, newest first, then the
    /// [`FUTURE_TOKENS`] after it, in order.
    pub fn tokens(&self) -> Vec<[u8; TOKEN_LEN]> {
        match &self.tokens {
            Some(t) => std::iter::once(t.current)
                .chain(t.past.iter().copied())
                .chain(t.future.iter().copied())
                .collect(),
            None => Vec::new(),
        }
    }

    /// The schedule contacts are told: current, then the
    /// [`FUTURE_TOKENS`] after it, in order.
    pub fn token_schedule(&self) -> Vec<[u8; TOKEN_LEN]> {
        match &self.tokens {
            Some(t) => std::iter::once(t.current)
                .chain(t.future.iter().copied())
                .collect(),
            None => Vec::new(),
        }
    }

    /// The current token `contact` last gave this device inside a
    /// session, while it is kept.
    pub fn contact_token(&self, contact: &PublicIdentity) -> Option<[u8; TOKEN_LEN]> {
        self.contact_schedule(contact).first().copied()
    }

    /// The whole schedule `contact` last gave, current first.
    pub fn contact_schedule(&self, contact: &PublicIdentity) -> Vec<[u8; TOKEN_LEN]> {
        self.contact_tokens
            .get(&key_of(contact))
            .map(|(t, _)| t.clone())
            .unwrap_or_default()
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

    /// Requests received and reported to the node, still held.
    pub fn requests_reported(&self) -> usize {
        self.links
            .values()
            .flat_map(|l| l.exchanges.values())
            .filter(|e| shown(e))
            .count()
    }

    /// Requests received and held: answering, shown and waiting, or held
    /// quietly after a decline until their time is up.
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

    /// The Unix time in milliseconds, read now from the wall clock: never
    /// worked out from `now_ms`, which stops while the Mac sleeps.
    fn unix_now(&self) -> u64 {
        self.clock.unix_ms(self.now_ms)
    }

    fn me(&self) -> Option<Key> {
        self.keys.as_ref().map(|k| key_of(k.public()))
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
            self.contact_tokens.clear();
            restart = true;
        }
        let mut mode_changed = false;
        if let Some(mode) = mode {
            mode_changed = mode != self.mode;
            self.mode = mode;
        }
        if let Some(contacts) = contacts {
            self.use_contacts(contacts);
            self.release_proofs();
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
        for (link, answer) in answers {
            self.answer(link, answer);
        }
    }

    /// The person answered the request received on `link`. Accepted: our
    /// hello goes back inside the request session, and once the requester
    /// holds it, this device proves it sent it by a contact handshake
    /// naming the requester's token ([`BleTransport::prove_accepted`]).
    /// Declined: the request is no longer shown and nothing of ours goes
    /// back, and its session is held exactly as an ignored one is
    /// ([`LinkSession::hush`]), so the requester can't tell the two apart.
    fn answer(&mut self, link: LinkId, answer: Option<Vec<u8>>) {
        let now = self.now_ms;
        let Some(l) = self.links.get_mut(&link) else {
            return;
        };
        let Some(hello) = answer.filter(|h| h.len() <= MAX_ENVELOPE) else {
            for e in l.exchanges.values_mut() {
                if let Exchange::Session(s) = e {
                    if s.kind == SessKind::RequestIn && s.accepted_ms.is_none() {
                        s.hush();
                    }
                }
            }
            return;
        };
        let mut writes = Vec::new();
        for e in l.exchanges.values_mut() {
            if let Exchange::Session(s) = e {
                if s.kind == SessKind::RequestIn
                    && s.heard_ms.is_some()
                    && s.accepted_ms.is_none()
                    && !s.quiet
                {
                    s.accepted_ms = Some(now);
                    writes.extend(s.send_envelope(&hello, None, now));
                }
            }
        }
        for w in writes {
            l.put(&mut self.radio, link, &w);
        }
    }

    fn use_contacts(&mut self, contacts: Vec<PublicIdentity>) {
        let me = self.me();
        let before = std::mem::take(&mut self.contact_keys);
        self.contacts = contacts
            .into_iter()
            .filter(|c| Some(key_of(c)) != me)
            .collect();
        self.contact_keys = self.contacts.iter().map(key_of).collect();
        let keys = &self.contact_keys;
        self.last_seen.retain(|k, _| keys.contains(k));
        self.last_tried.retain(|k, _| keys.contains(k));
        self.redial.retain(|k, _| keys.contains(k));
        // a removed contact is told nothing more: its sessions go below,
        // and what it told this device is forgotten
        self.contact_tokens.retain(|k, _| keys.contains(k));
        // and the tokens it was told ahead are drawn again, so it can't
        // recognise this device's later adverts; the contacts that remain
        // are told the new schedule in their sessions
        if before.iter().any(|k| !keys.contains(k)) && self.tokens.is_some() {
            let fresh: VecDeque<[u8; TOKEN_LEN]> =
                (0..FUTURE_TOKENS).map(|_| self.random_token()).collect();
            if let Some(t) = self.tokens.as_mut() {
                t.future = fresh;
            }
        }
        let keys = &self.contact_keys;
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
        let future = (0..FUTURE_TOKENS).map(|_| self.random_token()).collect();
        let next_ms = self.next_rotation();
        self.tokens = Some(Tokens {
            current,
            past: VecDeque::new(),
            future,
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
        self.lost.clear();
        self.proofs.clear();
        for tag in self.queue.drain_unreported() {
            self.events.push(TransportEvent::Dropped(tag));
        }
    }

    /// Every request, either way, ends: the node is told at the end of
    /// the poll ([`BleTransport::settle_requests`]).
    fn drop_requests(&mut self) {
        for link in self.links.values_mut() {
            link.exchanges
                .retain(|_, e| !is_request_in(e) && !is_request_out(e));
        }
    }

    /// A new token, and every link without a confirmed contact session
    /// closed, so no one connected to this device follows it across.
    /// Contacts in session are told the new token inside it (see
    /// [`BleTransport::share_tokens`]).
    fn rotate_token(&mut self) {
        let now = self.now_ms;
        if self.tokens.as_ref().is_none_or(|t| now < t.next_ms) {
            return;
        }
        let drawn = self.random_token();
        let next_ms = self.next_rotation();
        let Some(t) = self.tokens.as_mut() else {
            return;
        };
        // the next token in the schedule becomes current; a new one is
        // drawn at the end
        let token = t.future.pop_front().unwrap_or(drawn);
        if t.future.len() < FUTURE_TOKENS {
            t.future.push_back(drawn);
        }
        t.past.push_front(t.current);
        t.past.truncate(PAST_TOKENS);
        t.current = token;
        t.next_ms = next_ms;
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

    /// The radio lost a link this device opened: remember the contacts it
    /// carried (or was redialling), so a new link to the same advert
    /// dials them again at once. The token's one round is spent, so
    /// without this they would wait for the next token.
    fn remember_lost(&mut self, id: LinkId) {
        let now = self.now_ms;
        let Some(link) = self.links.get(&id) else {
            return;
        };
        let Some(token) = link.round else { return };
        let mut who: Vec<PublicIdentity> = link
            .exchanges
            .values()
            .filter_map(|e| match e {
                Exchange::Session(s) if s.kind == SessKind::Contact => s.remote.clone(),
                _ => None,
            })
            .collect();
        who.extend(
            self.redial
                .values()
                .filter(|r| r.link == id)
                .map(|r| r.contact.clone()),
        );
        who.sort_by_key(key_of);
        who.dedup();
        if who.is_empty() {
            return;
        }
        self.lost
            .retain(|_, (_, at)| now.saturating_sub(*at) < ROUND_MEMORY_MS);
        if self.lost.len() >= ROUNDS_CAP {
            let oldest = self
                .lost
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(t, _)| *t);
            if let Some(t) = oldest {
                self.lost.remove(&t);
            }
        }
        self.lost.insert(token, (who, now));
    }

    /// Whether a new link would pass its cap: one the other device
    /// opened against [`MAX_LINKS_IN`], one this device opened against
    /// [`MAX_LINKS_OUT`], less the [`CONTACT_LINKS`] kept back unless it
    /// is to a contact's advert.
    fn full(&self, opened: bool, contact: bool) -> bool {
        let mine = self.links.values().filter(|l| l.opened).count();
        if !opened {
            return self.links.len() - mine >= MAX_LINKS_IN;
        }
        let room = if contact {
            MAX_LINKS_OUT
        } else {
            MAX_LINKS_OUT - CONTACT_LINKS
        };
        mine >= room
    }

    fn link_gone(&mut self, id: LinkId) {
        if self.links.remove(&id).is_some() {
            self.redial.retain(|_, r| r.link != id);
        }
    }

    // ---- receiving ----------------------------------------------------

    fn on_radio(&mut self, event: RadioEvent) {
        match event {
            RadioEvent::Up { link, advert } => {
                // a contact's schedule names the advert by its tag first,
                // then a token this device was given for it on a link it
                // lost, then the token this link was given, if any
                let (tag, given) = match advert {
                    Advert::Unseen => (None, None),
                    Advert::Token(t) => {
                        let mut tag = [0u8; TAG_LEN];
                        tag.copy_from_slice(&t[..TAG_LEN]);
                        (Some(tag), Some(t))
                    }
                    Advert::Withheld(tag) => (tag, None),
                };
                let opened = advert != Advert::Unseen;
                let told = tag.and_then(|tag| self.contact_token_by_tag(tag));
                let lost =
                    tag.and_then(|tag| self.lost.keys().copied().find(|k| k[..TAG_LEN] == tag));
                if self.full(opened, told.is_some() || lost.is_some()) {
                    self.radio.disconnect(link);
                    return;
                }
                let token = told.or(lost).or(given);
                // a link with no token to name is remembered by its tag,
                // or, with none, by itself
                let round = match (token, tag) {
                    (Some(t), _) => Some(t),
                    (None, Some(tag)) => {
                        let mut r = [0u8; TOKEN_LEN];
                        r[..TAG_LEN].copy_from_slice(&tag);
                        Some(r)
                    }
                    (None, None) if opened => Some(self.random_token()),
                    (None, None) => None,
                };
                self.links
                    .insert(link, Link::new(opened, tag, token, round, self.now_ms));
                if let Some((who, _)) = lost.and_then(|t| self.lost.remove(&t)) {
                    for contact in who {
                        let k = key_of(&contact);
                        if self.contact_keys.contains(&k) {
                            self.redial.insert(
                                k,
                                Redial {
                                    link,
                                    contact,
                                    next_ms: self.now_ms,
                                    backoff_ms: REDIAL_FIRST_MS,
                                },
                            );
                        }
                    }
                }
            }
            RadioEvent::Down(link) => {
                self.remember_lost(link);
                self.link_gone(link);
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
            RadioEvent::Available => self.events.push(TransportEvent::Recovered),
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
            Kind::Request if side == Side::Theirs => self.request_message(link, xid, rest),
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
    /// our current token, one of the [`PAST_TOKENS`] before it or one of
    /// the [`FUTURE_TOKENS`] told to contacts, at a
    /// time near ours, comes from a contact (or, on a link carrying a
    /// request this device sent, from the one whose answer claimed to be
    /// them: the proof of an accept), and its ephemeral key is new.
    /// Anything else gets no byte back.
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
        let awaited = link.awaits(&remote);
        if Some(rk) == me || !(self.contact_keys.contains(&rk) || awaited) {
            return;
        }
        self.prune_seen();
        if self.seen_ephemerals.contains_key(&eph) {
            return;
        }
        self.remember_ephemeral(eph);
        if !self.contact_keys.contains(&rk) {
            // the proof of an accept, from the one this device asked: the
            // node is told and decides whether to take them as a contact;
            // only if it does does the answer go, so the answer itself
            // shows the one who accepted that they were added
            let hello = self
                .links
                .get(&id)
                .and_then(|l| l.answer_from(&remote))
                .unwrap_or_default();
            if let Some(link) = self.links.get_mut(&id) {
                link.exchanges.retain(|_, e| !is_request_out(e));
            }
            self.asked.remove(&id);
            self.proofs.push(HeldProof {
                link: id,
                xid,
                hs,
                remote: remote.clone(),
                since_ms: now,
            });
            self.events.push(TransportEvent::RequestAccepted {
                stranger: id.0,
                from: remote,
                hello,
            });
            return;
        }
        self.answer_contact_with(id, xid, hs);
    }

    /// The node has said who its contacts are: each proof held for one of
    /// them is answered, and the rest are let go unanswered.
    fn release_proofs(&mut self) {
        for p in std::mem::take(&mut self.proofs) {
            if self.contact_keys.contains(&key_of(&p.remote)) {
                self.answer_contact_with(p.link, p.xid, p.hs);
            }
        }
    }

    /// Answer a contact handshake's message 1, read from a contact into
    /// `hs`: the session is theirs from now on.
    fn answer_contact_with(&mut self, id: LinkId, xid: u32, mut hs: Handshake) {
        let now = self.now_ms;
        let Ok(reply) = hs.write_message() else {
            return;
        };
        let Ok(cipher) = hs.into_transport() else {
            return;
        };
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        if link.exchanges.contains_key(&(Side::Theirs, xid)) {
            return;
        }
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

    /// Message 1 of a request to connect: answered in Open mode only, and
    /// not on a link that holds a request whose hello arrived (shown,
    /// accepted or held quietly): one per link, so a stranger can't
    /// replace theirs to push others out. Whether it will be shown is
    /// decided only when its hello arrives, and it is answered the same
    /// way whatever is decided. A request part way on the link is
    /// replaced; past [`REQUESTS_PART_WAY_CAP`] part way in all, the
    /// oldest of those goes, never one whose hello arrived.
    fn request_message(&mut self, id: LinkId, xid: u32, noise: &[u8]) {
        if self.mode != Some(Mode::Open) || self.keys.is_none() {
            return;
        }
        let now = self.now_ms;
        let Some(link) = self.links.get(&id) else {
            return;
        };
        let held = link
            .exchanges
            .values()
            .any(|e| is_request_in(e) && !part_way(e));
        if held || link.exchanges.contains_key(&(Side::Theirs, xid)) {
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
        // the oldest part way anywhere goes if there are too many
        loop {
            let held: Vec<(u64, LinkId, ExKey)> = self
                .links
                .iter()
                .flat_map(|(lid, l)| {
                    l.exchanges.iter().filter_map(move |(k, e)| match e {
                        Exchange::Answering { since_ms, .. } => Some((*since_ms, *lid, *k)),
                        Exchange::Session(s) if part_way(e) => Some((s.started_ms, *lid, *k)),
                        _ => None,
                    })
                })
                .collect();
            if held.len() < REQUESTS_PART_WAY_CAP {
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
        let schedule = self.token_schedule();
        let current = schedule.first().copied();
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
                // the contact answered: no more hellos on this link, and a
                // request from them this device accepted is proved
                link.exchanges.retain(|_, e| match e {
                    Exchange::Trial { .. } => false,
                    Exchange::Session(s) if s.kind == SessKind::RequestIn => {
                        s.remote.as_ref() != Some(&contact)
                    }
                    _ => true,
                });
                let mut s =
                    LinkSession::new(SessKind::Contact, cipher, Side::Mine.flags(), k.1, now);
                // the first transport message confirms us to the
                // responder, and tells it our token
                let first = if schedule.is_empty() {
                    s.keepalive(now)
                } else {
                    s.send_token(&schedule, now)
                };
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
                // the hello, then our current token alone: an accept is
                // proved by a contact handshake naming it, on this link
                let mut first: Vec<Vec<u8>> =
                    s.send_envelope(&hello, None, now).into_iter().collect();
                if let Some(t) = current {
                    first.extend(s.send_token(&[t], now));
                }
                s.hello = Some((hello, attempt));
                link.exchanges.insert(k, Exchange::Session(Box::new(s)));
                for w in first {
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
        let delivered = kind == SessKind::RequestOut && s.all_acked() && s.delivered_ms.is_none();
        if delivered {
            s.delivered_ms = Some(now);
        }
        let first_hello = kind == SessKind::RequestIn && s.heard_ms.is_none();
        if first_hello && !d.envelopes.is_empty() {
            s.heard_ms = Some(now);
        }
        if kind == SessKind::RequestIn && !s.quiet {
            if let Some(t) = d.tokens.first() {
                s.peer_token = Some(*t);
            }
        }
        // the answer to a request this device sent: who it claims to be
        let unix_min = (self.clock.unix_ms(now) / 60_000) as u32;
        if kind == SessKind::RequestOut && s.answer.is_none() {
            if let Some(hello) = d.envelopes.first() {
                let mine = self.keys.as_ref().map(|k| k.public().clone());
                if let Some((who, _)) = read_hello(hello, unix_min) {
                    if Some(&who) != mine.as_ref() {
                        s.answer = Some((who, hello.clone()));
                    }
                }
            }
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
                    if !d.tokens.is_empty() && self.contact_keys.contains(&rk) {
                        self.contact_tokens.insert(rk, (d.tokens, now));
                    }
                }
                for e in d.envelopes {
                    self.hand_over(&e);
                }
            }
            SessKind::RequestOut => {
                // told once, the first time it is delivered
                if delivered && self.asked.get(&id) == Some(&false) {
                    self.asked.insert(id, true);
                    self.events
                        .push(TransportEvent::RequestDelivered { stranger: id.0 });
                }
            }
            SessKind::RequestIn => {
                let hello = d.envelopes.into_iter().next().filter(|_| first_hello);
                if let (Some(hello), Some(from)) = (hello, remote) {
                    self.take_request(id, k, from, hello);
                }
            }
        }
    }

    /// A request's hello arrived: report it, unless the link or the
    /// requester brought one in the last [`REQUEST_GAP_MS`], or it is no
    /// envelope; then it is held quietly, never shown. Past
    /// [`REQUESTS_CAP`] shown, the oldest the person hasn't accepted is
    /// held quietly from then on. Either way the requester sees what an
    /// ignored request shows: its hello acknowledged, the session held
    /// until its time is up.
    fn take_request(&mut self, id: LinkId, k: ExKey, from: PublicIdentity, hello: Vec<u8>) {
        let now = self.now_ms;
        let fresh =
            |at: Option<&u64>| at.is_none_or(|at| now.saturating_sub(*at) >= REQUEST_GAP_MS);
        let fk = key_of(&from);
        let ok = hello.len() <= MAX_ENVELOPE
            && Envelope::decode(&hello).is_ok()
            && fresh(self.request_links.get(&id))
            && fresh(self.request_ids.get(&fk));
        if !ok {
            if let Some(s) = self.links.get_mut(&id).and_then(|l| l.session(&k)) {
                s.hush();
            }
            return;
        }
        self.request_links
            .retain(|_, at| now.saturating_sub(*at) < REQUEST_GAP_MS);
        self.request_ids
            .retain(|_, at| now.saturating_sub(*at) < REQUEST_GAP_MS);
        for _ in REQUEST_MEMORY_CAP..=self.request_links.len() {
            let oldest = self
                .request_links
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(l, _)| *l);
            oldest.map(|l| self.request_links.remove(&l));
        }
        for _ in REQUEST_MEMORY_CAP..=self.request_ids.len() {
            let oldest = self
                .request_ids
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(i, _)| *i);
            oldest.map(|i| self.request_ids.remove(&i));
        }
        self.request_links.insert(id, now);
        self.request_ids.insert(fk, now);
        // the oldest reported and not accepted goes if there are too many
        let mut held: Vec<(u64, LinkId, ExKey)> = self
            .links
            .iter()
            .flat_map(|(lid, l)| {
                l.exchanges.iter().filter_map(move |(ek, e)| match e {
                    Exchange::Session(s) if shown(e) && s.accepted_ms.is_none() => {
                        s.heard_ms.map(|at| (at, *lid, *ek))
                    }
                    _ => None,
                })
            })
            .filter(|(_, lid, ek)| (*lid, *ek) != (id, k))
            .collect();
        held.sort();
        while held.len() >= REQUESTS_CAP {
            let (_, lid, ek) = held.remove(0);
            if let Some(s) = self.links.get_mut(&lid).and_then(|l| l.session(&ek)) {
                s.hush();
            }
        }
        self.reported.insert(id);
        self.events.push(TransportEvent::Request {
            request: id.0,
            from,
            hello,
        });
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
        // a request that can't begin has failed: the node is told at the
        // end of the poll
        let Some(keys) = self.keys.as_ref().filter(|_| ok) else {
            return;
        };
        let mut hs = Session::request_initiator(keys);
        let Ok(first) = hs.write_message() else {
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
        }
    }

    /// Requests this device accepted whose hello the requester now holds:
    /// prove who sent it by a contact handshake on the same link, naming
    /// the token the requester gave. It is redialled, with backoff, until
    /// it is answered or the request ends.
    fn prove_accepted(&mut self) {
        let now = self.now_ms;
        let mut due = Vec::new();
        for (id, link) in &mut self.links {
            let mut token = None;
            for e in link.exchanges.values_mut() {
                let Exchange::Session(s) = e else { continue };
                if s.kind != SessKind::RequestIn
                    || s.accepted_ms.is_none()
                    || s.proving
                    || !s.all_acked()
                {
                    continue;
                }
                if let (Some(t), Some(r)) = (s.peer_token, s.remote.clone()) {
                    s.proving = true;
                    token = Some(t);
                    due.push((*id, r));
                }
            }
            if link.token.is_none() {
                link.token = token;
            }
        }
        for (link, contact) in due {
            self.redial.insert(
                key_of(&contact),
                Redial {
                    link,
                    contact,
                    next_ms: now,
                    backoff_ms: REDIAL_FIRST_MS,
                },
            );
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
        // a decoy on a link with no token to name names the round's key
        let token = match &contact {
            Some(_) => link.token,
            None => link.token.or(link.round),
        };
        let Some(token) = token else { return };
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
            // a link to an advert whose tag a contact's schedule (told
            // since it came up) now matches: that token is the one to
            // name, whatever the link was given, and it gets its own round
            let told = self.links.get(&id).and_then(|l| {
                let m = self.contact_token_by_tag(l.tag?)?;
                (Some(m) != l.token).then_some(m)
            });
            let Some(link) = self.links.get_mut(&id) else {
                continue;
            };
            if told.is_some() {
                link.token = told;
                link.round = told;
            }
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
            // the token's one round, on a link this device opened
            let Some((round, named)) = self.links.get(&id).and_then(|l| Some((l.round?, l.token)))
            else {
                continue;
            };
            if self.rounds.contains_key(&round) {
                continue;
            }
            if self.rounds.len() >= ROUNDS_CAP {
                let oldest = self.rounds.iter().min_by_key(|(_, s)| **s).map(|(t, _)| *t);
                if let Some(t) = oldest {
                    self.rounds.remove(&t);
                }
            }
            self.rounds.insert(round, now);
            // a contact who gave this link's token inside a session goes
            // first
            let (seen, tried, given) = (&self.last_seen, &self.last_tried, &self.contact_tokens);
            let gave = |c: &PublicIdentity| {
                named.is_some_and(|n| given.get(&key_of(c)).is_some_and(|(ts, _)| ts.contains(&n)))
            };
            let mut order: Vec<(usize, &PublicIdentity)> = self
                .contacts
                .iter()
                .enumerate()
                .filter(|_| named.is_some())
                .collect();
            order.sort_by_key(|(i, c)| {
                let k = key_of(c);
                (
                    std::cmp::Reverse(gave(c)),
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
                    let over =
                        |at: Option<u64>| at.is_some_and(|at| now.saturating_sub(at) >= REQUEST_MS);
                    match s.kind {
                        SessKind::RequestIn => match (s.heard_ms, s.accepted_ms) {
                            (None, _) if age >= HANDSHAKE_MS => return false,
                            (heard, None) if over(heard) => return false,
                            (_, accepted) if over(accepted) => return false,
                            _ => {}
                        },
                        // delivered, and no answer in time
                        SessKind::RequestOut if over(s.delivered_ms) => return false,
                        _ => {}
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
                let aged = l.opened && now.saturating_sub(l.up_ms) >= TOKEN_MS;
                let stranger = contacts_only
                    && l.round.is_some_and(|t| self.rounds.contains_key(&t))
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

    /// Tell every contact in a confirmed session this device's token
    /// schedule (current, then the next [`FUTURE_TOKENS`]), inside the
    /// session: at once when the session is confirmed and at every
    /// rotation, and with each keepalive. A schedule that changed only
    /// ahead (a contact was removed) waits for each session's next
    /// keepalive, so a removal sends nothing out of rhythm. A contact
    /// whose advert scan meets this device again, even after it rotated
    /// up to three times out of range, names that token in its hello,
    /// even where the radio withheld it.
    fn share_tokens(&mut self) {
        let now = self.now_ms;
        let schedule = self.token_schedule();
        if schedule.is_empty() {
            return;
        }
        for (id, link) in self.links.iter_mut() {
            let mut writes = Vec::new();
            for e in link.exchanges.values_mut() {
                let Exchange::Session(s) = e else { continue };
                let contact = s
                    .remote
                    .as_ref()
                    .is_some_and(|r| self.contact_keys.contains(&key_of(r)));
                if s.kind != SessKind::Contact || !s.confirmed() || !contact {
                    continue;
                }
                match &s.shared {
                    Some(told) if told == &schedule => {}
                    // only the tokens ahead changed: the next keepalive
                    // carries them
                    Some(told) if told.first() == schedule.first() => {
                        s.shared = Some(schedule.clone());
                    }
                    _ => writes.extend(s.send_token(&schedule, now)),
                }
            }
            for w in writes {
                link.put(&mut self.radio, *id, &w);
            }
        }
    }

    /// The token a contact gave inside a session (current or one of the
    /// next ones in its schedule) whose first bytes are `tag`, while it is
    /// kept.
    fn contact_token_by_tag(&self, tag: [u8; TAG_LEN]) -> Option<[u8; TOKEN_LEN]> {
        let now = self.now_ms;
        self.contact_tokens
            .iter()
            .filter(|(k, (_, at))| {
                self.contact_keys.contains(*k) && now.saturating_sub(*at) < CONTACT_TOKEN_MS
            })
            .flat_map(|(_, (ts, _))| ts.iter().copied())
            .find(|t| t[..TAG_LEN] == tag)
    }

    /// Who is near: contacts on confirmed sessions; strangers a request
    /// can go to (live links this device opened, without a confirmed
    /// contact session, in Open mode); and how many strangers' adverts a
    /// link was up to in the last [`STRANGER_MS`]. Told to the node when
    /// it changes.
    fn update_peers(&mut self) {
        let now = self.now_ms;
        for link in self.links.values() {
            if let (true, Some(r)) = (link.opened, link.round) {
                let contact = link.has_confirmed_contact();
                let seen = self.seen_links.entry(r).or_insert((now, contact));
                *seen = (now, seen.1 || contact);
            }
        }
        self.seen_links
            .retain(|_, (at, _)| now.saturating_sub(*at) < STRANGER_MS);
        while self.seen_links.len() > SEEN_LINKS_CAP {
            let oldest = self
                .seen_links
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(r, _)| *r);
            if let Some(r) = oldest {
                self.seen_links.remove(&r);
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
        let open = self.running && self.mode == Some(Mode::Open);
        let strangers: Vec<LinkId> = self
            .links
            .iter()
            .filter(|(_, l)| open && l.opened && !l.has_confirmed_contact())
            .map(|(id, _)| *id)
            .collect();
        let strangers_seen = self.seen_links.values().filter(|(_, c)| !c).count();
        let peers = Peers {
            contacts,
            strangers,
            strangers_seen,
        };
        if self.said.as_ref() != Some(&peers) {
            self.events.push(TransportEvent::Peers {
                contacts: peers.contacts.clone(),
                strangers: peers.strangers.iter().map(|l| l.0).collect(),
                strangers_seen,
            });
            self.said = Some(peers.clone());
        }
        self.shared.lock().unwrap().peers = peers;
    }

    /// Tell the node which requests ended this poll, however they ended:
    /// a request received it was told of and that is no longer held is
    /// gone; a request sent that is no longer under way, and wasn't
    /// accepted, failed.
    fn settle_requests(&mut self) {
        let holds = |l: &Link, pred: fn(&Exchange) -> bool| l.exchanges.values().any(pred);
        let gone: Vec<LinkId> = self
            .reported
            .iter()
            .copied()
            .filter(|id| !self.links.get(id).is_some_and(|l| holds(l, shown)))
            .collect();
        for id in gone {
            self.reported.remove(&id);
            self.events
                .push(TransportEvent::RequestGone { request: id.0 });
        }
        let ended: Vec<(LinkId, bool)> = self
            .asked
            .iter()
            .map(|(id, delivered)| (*id, *delivered))
            .filter(|(id, _)| !self.links.get(id).is_some_and(|l| holds(l, is_request_out)))
            .collect();
        for (id, delivered) in ended {
            self.asked.remove(&id);
            let stranger = id.0;
            self.events.push(if delivered {
                TransportEvent::RequestUnanswered { stranger }
            } else {
                TransportEvent::RequestFailed { stranger }
            });
        }
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

/// A request received whose hello arrived and that is shown (or accepted),
/// not held quietly.
fn shown(e: &Exchange) -> bool {
    matches!(e, Exchange::Session(s) if s.kind == SessKind::RequestIn && s.heard_ms.is_some() && !s.quiet)
}

/// A request received that hasn't been reported: its handshake isn't done,
/// or its hello hasn't arrived.
fn part_way(e: &Exchange) -> bool {
    match e {
        Exchange::Answering { .. } => true,
        Exchange::Session(s) => s.kind == SessKind::RequestIn && s.heard_ms.is_none(),
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
            self.proofs
                .retain(|p| now.saturating_sub(p.since_ms) < HANDSHAKE_MS);
            self.contact_tokens
                .retain(|_, (_, at)| now.saturating_sub(*at) < CONTACT_TOKEN_MS);
            self.prove_accepted();
            self.trials();
        }
        self.collect();
        if self.running {
            self.schedule();
            self.upkeep();
            self.close_links();
            self.share_tokens();
        }
        self.settle_requests();
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

    fn set_identity(&mut self, me: Option<&Identity>) {
        self.shared.lock().unwrap().keys = Some(me.map(SessionKeys::new));
    }

    fn set_contacts(&mut self, contacts: &[PublicIdentity]) {
        self.shared.lock().unwrap().contacts = Some(contacts.to_vec());
    }

    /// [`BLUETOOTH_SEND_OFF`] with the switch at Off, or locked; the
    /// switch as last set, even before the next poll applies it.
    fn refusal(&self) -> Option<&'static str> {
        let c = self.shared.lock().unwrap();
        let mode = c.mode.unwrap_or(self.mode);
        let keyed = c.keys.as_ref().map_or(self.keys.is_some(), Option::is_some);
        (mode.is_none() || !keyed).then_some(BLUETOOTH_SEND_OFF)
    }

    /// Ask the stranger on link `stranger` (one listed in
    /// [`TransportEvent::Peers`]) to connect, carrying `hello`. Refused
    /// with [`REQUESTS_NEED_OPEN`] unless the switch is at Open, and with
    /// [`NOT_IN_RANGE`] for a link no longer listed. Asking again on a
    /// link already asked does nothing more.
    fn request(&mut self, stranger: u64, hello: Vec<u8>) -> Result<(), &'static str> {
        let link = LinkId(stranger);
        let mut c = self.shared.lock().unwrap();
        if c.mode.unwrap_or(self.mode) != Some(Mode::Open) {
            return Err(REQUESTS_NEED_OPEN);
        }
        if !self
            .said
            .as_ref()
            .is_some_and(|p| p.strangers.contains(&link))
        {
            return Err(NOT_IN_RANGE);
        }
        if let std::collections::btree_map::Entry::Vacant(e) = self.asked.entry(link) {
            e.insert(false);
            c.requests.push((link, hello));
        }
        Ok(())
    }

    fn answer_request(&mut self, request: u64, hello: Option<Vec<u8>>) {
        self.shared
            .lock()
            .unwrap()
            .answers
            .push((LinkId(request), hello));
    }
}
