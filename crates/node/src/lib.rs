//! bombechat-node: the runtime that ties identity, sealing, framing, the
//! transports and the encrypted store together.
//!
//! A [`Node`] is driven entirely by [`Node::tick`]: each tick polls every
//! transport, reassembles the frames it heard, opens private messages
//! chunk by chunk as they land, and returns what happened as
//! [`NodeEvent`]s. Sending ([`Node::send`], [`Node::broadcast`],
//! [`Node::start_pairing`]) seals an envelope, splits it into frames under
//! a fresh random tag and hands the frames to every transport.
//!
//! # What is trusted, and when
//!
//! * A chunk of a private message is shown ([`NodeEvent::ChunkLanded`])
//!   as soon as it decrypts, but at that point it is only known to come
//!   from a holder of the content key. Nothing is stored or shown as a
//!   message until the last chunk has decrypted and the sender's
//!   signature over the whole message has verified.
//! * A private message is delivered only if its (signature-checked)
//!   sender is a contact; the [`NodeEvent::Message`] names that contact.
//!   A sealed message from an identity that is not a contact is dropped,
//!   not held: there is no contact to show it under, and holding it
//!   would let strangers fill memory.
//! * An envelope whose id is already in the store, or that this session
//!   already handled, is skipped before anything is opened.
//!
//! # Clocks
//!
//! `now_ms` (passed to [`Node::tick`]) is a monotonic millisecond clock
//! used only for scheduling: reassembly expiry and the quiet gate. The
//! minutes stamped into envelopes and checked for expiry come from the
//! system wall clock, because they are compared with other devices'.
//!
//! # Locked
//!
//! A node created without a store (before the passphrase is entered)
//! still listens. Envelopes that finish arriving are held in memory,
//! still sealed, up to [`LOCKED_INBOX`] of them (the oldest is dropped
//! first), and opened on the first tick after [`Node::unlock`]. While
//! locked, [`Node::send`], [`Node::broadcast`] and [`Node::start_pairing`]
//! do nothing, and [`Node::confirm_pairing`] stores nothing.

mod control;
mod sound;
mod transport;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::ops::Range;
use std::time::{SystemTime, UNIX_EPOCH};

use bombechat_core::{
    open_public, safety_number, seal_private, seal_public, ChunkOpener, ChunkView, Envelope,
    Identity, Kind, Opened, PublicIdentity, CHUNK, MAX_ENVELOPE,
};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_store::{Channel, Contact, ContactId, Store, StoredMessage};
use rand_core::{OsRng, RngCore};

pub use sound::SoundTransport;
pub use transport::{Transport, TransportEvent};

/// Most complete envelopes held while the node is locked.
pub const LOCKED_INBOX: usize = 256;

/// How long envelopes this node sends stay valid: one day.
const TTL_MIN: u32 = 1440;

/// Clock skew tolerated on expiry, as in `bombechat_core::seal`.
const SKEW_MIN: u32 = 1440;

/// How many envelope ids this session remembers having handled, beyond
/// what the store remembers. Oldest forgotten first.
const SEEN_CAP: usize = 4096;

/// Returned by [`Node::send`] and [`Node::broadcast`] when nothing was
/// sent. No real envelope has this id.
pub const NOT_SENT: [u8; 16] = [0; 16];

/// Returned by [`Node::confirm_pairing`] when nothing was stored. SQLite
/// row ids start at 1, so no real contact has it.
pub const NOT_STORED: ContactId = ContactId(0);

// Envelope and private-body layout, as documented in
// `bombechat_core::envelope` and `bombechat_core::seal`. Needed to find
// chunk boundaries in a partially received envelope.
const ENVELOPE_HEADER: usize = 25;
const PRIVATE_HEADER_FIXED: usize = 21;
const WRAP_LEN: usize = 112;
const AEAD_TAG: usize = 16;
const SEGMENT: usize = CHUNK + AEAD_TAG;
/// sender sign, sender dh, sent_min and signature around the text.
const INNER_OVERHEAD: usize = 32 + 32 + 4 + 64;

/// What the node reports from a [`Node::tick`].
pub enum NodeEvent {
    /// One chunk of a private message decrypted, in order. For the drums
    /// only: until the [`NodeEvent::Message`] that follows, the text is
    /// not known to come from the named sender.
    ChunkLanded {
        tag: u16,
        index: u16,
        of: u16,
        view: ChunkView,
    },
    /// A private message from a contact, verified and stored.
    Message(StoredMessage),
    /// A contact reports hearing a message this node sent.
    Heard { id: [u8; 16], by: ContactId },
    /// A signed public broadcast. `verified` is true when the signer is a
    /// contact whose safety number has been checked.
    Broadcast {
        signer: PublicIdentity,
        text: String,
        verified: bool,
    },
    /// Someone nearby offered to pair. Nothing is stored until
    /// [`Node::confirm_pairing`].
    PairOffer {
        peer: PublicIdentity,
        name: String,
        safety: String,
    },
    /// The room's loudness in approximate dB.
    RoomDb(f32),
    /// A channel changed state, e.g. "unavailable: microphone permission
    /// denied".
    ChannelState { channel: Channel, state: String },
}

impl fmt::Debug for NodeEvent {
    /// Chunk contents are left out, so a log line never carries them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeEvent::ChunkLanded { tag, index, of, .. } => f
                .debug_struct("ChunkLanded")
                .field("tag", tag)
                .field("index", index)
                .field("of", of)
                .finish_non_exhaustive(),
            NodeEvent::Message(m) => f
                .debug_struct("Message")
                .field("id", &m.id)
                .field("contact", &m.contact)
                .finish_non_exhaustive(),
            NodeEvent::Heard { id, by } => f
                .debug_struct("Heard")
                .field("id", id)
                .field("by", by)
                .finish(),
            NodeEvent::Broadcast {
                signer, verified, ..
            } => f
                .debug_struct("Broadcast")
                .field("signer", &signer.short_hex())
                .field("verified", verified)
                .finish_non_exhaustive(),
            NodeEvent::PairOffer { peer, .. } => f
                .debug_struct("PairOffer")
                .field("peer", &peer.short_hex())
                .finish_non_exhaustive(),
            NodeEvent::RoomDb(db) => f.debug_tuple("RoomDb").field(db).finish(),
            NodeEvent::ChannelState { channel, state } => f
                .debug_struct("ChannelState")
                .field("channel", channel)
                .field("state", state)
                .finish(),
        }
    }
}

/// Progressive opening of one private envelope, by (transport, tag).
enum Progress {
    Opening {
        id: [u8; 16],
        opener: Box<ChunkOpener>,
        /// Offset of the first chunk in the envelope bytes.
        chunks_at: usize,
        /// Length of the whole encoded envelope.
        end: usize,
        of: u16,
        next: u16,
    },
    /// Nothing more to do for this tag's chunks: opened, a duplicate, not
    /// for this node, not private, or failed.
    Settled,
}

/// Where a private envelope's parts sit, read from its first bytes.
struct PrivateLayout {
    id: [u8; 16],
    expires_min: u32,
    header: Range<usize>,
    end: usize,
}

/// Read a private envelope's layout from its first bytes. `None` if more
/// bytes are needed; `Some(Err(()))` if it is not a well-formed private
/// envelope.
fn private_layout(bytes: &[u8]) -> Option<Result<PrivateLayout, ()>> {
    if bytes.len() < ENVELOPE_HEADER {
        return None;
    }
    if bytes[0] != 1 || bytes[1] != Kind::Private as u8 {
        return Some(Err(()));
    }
    let id: [u8; 16] = bytes[2..18].try_into().expect("16 bytes");
    let expires_min = u32::from_be_bytes(bytes[18..22].try_into().expect("4 bytes"));
    let body_len = u16::from_be_bytes([bytes[23], bytes[24]]) as usize;
    let end = ENVELOPE_HEADER + body_len;
    if end > MAX_ENVELOPE {
        return Some(Err(()));
    }
    if bytes.len() < ENVELOPE_HEADER + 2 {
        return None;
    }
    let (version, count) = (bytes[ENVELOPE_HEADER], bytes[ENVELOPE_HEADER + 1]);
    if version != 1 || count == 0 {
        return Some(Err(()));
    }
    let header_len = PRIVATE_HEADER_FIXED + WRAP_LEN * count as usize;
    let chunks_len = body_len.saturating_sub(header_len);
    // The last chunk must carry at least one byte besides its tag.
    let last_len = chunks_len % SEGMENT;
    if chunks_len == 0 || (last_len != 0 && last_len <= AEAD_TAG) {
        return Some(Err(()));
    }
    if bytes.len() < ENVELOPE_HEADER + header_len {
        return None;
    }
    Some(Ok(PrivateLayout {
        id,
        expires_min,
        header: ENVELOPE_HEADER..ENVELOPE_HEADER + header_len,
        end,
    }))
}

/// Encoded size of a private envelope to `recipients` carrying `text_len`
/// bytes of text, following the layout in `bombechat_core::seal`.
fn private_envelope_len(recipients: usize, text_len: usize) -> usize {
    let inner = INNER_OVERHEAD + text_len;
    ENVELOPE_HEADER
        + PRIVATE_HEADER_FIXED
        + WRAP_LEN * recipients
        + inner
        + AEAD_TAG * inner.div_ceil(CHUNK)
}

/// Whether an X25519 public key is a low-order point. Sealing to one
/// would expose the content key, so `seal_private` refuses (panics).
fn low_order(dh: &[u8; 32]) -> bool {
    let probe = x25519_dalek::EphemeralSecret::random_from_rng(OsRng);
    !probe
        .diffie_hellman(&x25519_dalek::PublicKey::from(*dh))
        .was_contributory()
}

fn now_min() -> u32 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    u32::try_from(secs / 60).unwrap_or(u32::MAX)
}

fn random_id() -> [u8; 16] {
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    id
}

/// The node: one device's view of the network.
pub struct Node {
    store: Option<Store>,
    /// This device's identity, read from the store on unlock.
    me: Option<Identity>,
    transports: Vec<Box<dyn Transport>>,
    /// One per transport, by index: tags are only unique per carrier.
    reassemblers: Vec<Reassembler>,
    progress: HashMap<(usize, u16), Progress>,
    /// Complete envelopes heard while locked: (transport, tag, bytes).
    locked_inbox: VecDeque<(usize, u16, Vec<u8>)>,
    seen: HashSet<[u8; 16]>,
    seen_order: VecDeque<[u8; 16]>,
    /// Who each message sent this session went to, for checking receipts.
    sent_to: HashMap<[u8; 16], Vec<ContactId>>,
    receipts: bool,
}

impl Node {
    /// A node over `transports`. With `store` `None` the node starts
    /// locked; see [`Node::unlock`].
    pub fn new(store: Option<Store>, transports: Vec<Box<dyn Transport>>) -> Node {
        let reassemblers = transports.iter().map(|_| Reassembler::new()).collect();
        let mut node = Node {
            store: None,
            me: None,
            transports,
            reassemblers,
            progress: HashMap::new(),
            locked_inbox: VecDeque::new(),
            seen: HashSet::new(),
            seen_order: VecDeque::new(),
            sent_to: HashMap::new(),
            receipts: true,
        };
        if let Some(store) = store {
            node.unlock(store);
        }
        node
    }

    /// Hand the node its opened store. Envelopes held while locked are
    /// opened on the next [`tick`](Node::tick).
    pub fn unlock(&mut self, store: Store) {
        self.me = Some(store.me());
        self.store = Some(store);
    }

    /// Whether to send delivery receipts for messages from contacts. On by
    /// default; a receipt reveals that this device is nearby.
    pub fn set_receipts(&mut self, on: bool) {
        self.receipts = on;
    }

    /// Seal `text` to the given contacts and send it. Returns the
    /// message's envelope id, or [`NOT_SENT`] if the node is locked, no
    /// listed contact can be sealed to, or the text is too long for one
    /// envelope.
    ///
    /// A message to several contacts is one envelope; the store keeps one
    /// row per message id, so it is filed under the first of them.
    pub fn send(&mut self, to: &[ContactId], text: &str) -> [u8; 16] {
        let (Some(me), Some(store)) = (&self.me, &self.store) else {
            return NOT_SENT;
        };
        let contacts = store.contacts();
        let mut recipients: Vec<&Contact> = Vec::new();
        for id in to {
            if let Some(c) = contacts.iter().find(|c| c.id == *id) {
                if !low_order(&c.who.dh) && !recipients.iter().any(|r| r.id == c.id) {
                    recipients.push(c);
                }
            }
        }
        if recipients.is_empty()
            || recipients.len() > u8::MAX as usize
            || private_envelope_len(recipients.len(), text.len()) > MAX_ENVELOPE
        {
            return NOT_SENT;
        }

        let now = now_min();
        let who: Vec<PublicIdentity> = recipients.iter().map(|c| c.who.clone()).collect();
        let env = seal_private(me, &who, text, now, TTL_MIN);
        let stored = StoredMessage {
            id: env.id,
            contact: recipients[0].id,
            outgoing: true,
            text: text.to_string(),
            channel: self.channel(0),
            at_min: now,
            heard: false,
        };
        if store.put_message(&stored).is_err() {
            return NOT_SENT;
        }
        self.sent_to
            .insert(env.id, recipients.iter().map(|c| c.id).collect());
        self.transmit(&env);
        env.id
    }

    /// Sign `text` as a public broadcast and send it. Returns its envelope
    /// id, or [`NOT_SENT`] if locked or the text is too long.
    pub fn broadcast(&mut self, text: &str) -> [u8; 16] {
        let Some(me) = &self.me else {
            return NOT_SENT;
        };
        if ENVELOPE_HEADER + INNER_OVERHEAD + text.len() > MAX_ENVELOPE {
            return NOT_SENT;
        }
        let env = seal_public(me, text, now_min(), TTL_MIN);
        self.transmit(&env);
        env.id
    }

    /// Announce this device for pairing: a signed hello carrying its
    /// identity and `my_name` (cut to 64 bytes). Anyone who hears it gets
    /// a [`NodeEvent::PairOffer`].
    pub fn start_pairing(&mut self, my_name: &str) {
        let Some(me) = &self.me else {
            return;
        };
        let env = control::pair_hello(me, my_name, random_id(), now_min(), TTL_MIN);
        self.transmit(&env);
    }

    /// Store `peer` as a contact named `name`, marked checked (the user
    /// has compared safety numbers). Returns the contact's id: the
    /// existing one if `peer` is already a contact, or [`NOT_STORED`] if
    /// locked, if `peer`'s key cannot be sealed to, or if the store fails.
    pub fn confirm_pairing(&mut self, peer: &PublicIdentity, name: &str) -> ContactId {
        let Some(store) = &self.store else {
            return NOT_STORED;
        };
        if low_order(&peer.dh) {
            return NOT_STORED;
        }
        if let Some(c) = store.contacts().into_iter().find(|c| &c.who == peer) {
            return c.id;
        }
        store.add_contact(name, peer, true).unwrap_or(NOT_STORED)
    }

    /// Poll every transport and handle what they heard. `now_ms` is a
    /// monotonic millisecond clock.
    pub fn tick(&mut self, now_ms: u64) -> Vec<NodeEvent> {
        let mut events = Vec::new();

        if self.me.is_some() {
            while let Some((ti, tag, bytes)) = self.locked_inbox.pop_front() {
                self.on_complete(ti, tag, bytes, &mut events);
            }
        }

        for ti in 0..self.transports.len() {
            let channel = self.channel(ti);
            for heard in self.transports[ti].poll(now_ms) {
                match heard {
                    TransportEvent::Frame(frame) => {
                        for e in self.reassemblers[ti].push(&frame, now_ms) {
                            match e {
                                Event::FrameOk { tag, .. } => self.on_frame(ti, tag, &mut events),
                                Event::Complete { tag, bytes } => {
                                    self.on_complete(ti, tag, bytes, &mut events)
                                }
                                Event::FrameBad => {}
                            }
                        }
                    }
                    TransportEvent::RoomDb(db) => events.push(NodeEvent::RoomDb(db)),
                    TransportEvent::Unavailable(reason) => events.push(NodeEvent::ChannelState {
                        channel,
                        state: format!("unavailable: {reason}"),
                    }),
                }
            }
            self.reassemblers[ti].expire(now_ms);
        }

        let reassemblers = &self.reassemblers;
        self.progress
            .retain(|(ti, tag), _| reassemblers[*ti].prefix(*tag).is_some());
        events
    }

    fn channel(&self, ti: usize) -> Channel {
        self.transports.get(ti).map_or(Channel::Sound, |t| t.name())
    }

    /// Split `env` under a fresh random tag and queue it on every
    /// transport. Remembers its id so this node ignores its own echo.
    fn transmit(&mut self, env: &Envelope) {
        self.mark_seen(env.id);
        let tag = OsRng.next_u32() as u16;
        let frames = split(&env.encode(), tag);
        for t in &mut self.transports {
            for f in &frames {
                t.send(f.clone());
            }
        }
    }

    fn mark_seen(&mut self, id: [u8; 16]) {
        if self.seen.insert(id) {
            self.seen_order.push_back(id);
            if self.seen_order.len() > SEEN_CAP {
                let old = self.seen_order.pop_front().expect("non-empty");
                self.seen.remove(&old);
            }
        }
    }

    fn already_have(&self, id: &[u8; 16]) -> bool {
        self.seen.contains(id) || self.store.as_ref().is_some_and(|s| s.has_message(id))
    }

    fn contact_for(&self, who: &PublicIdentity) -> Option<Contact> {
        self.store
            .as_ref()?
            .contacts()
            .into_iter()
            .find(|c| &c.who == who)
    }

    /// A frame of `tag` was accepted: open any private chunks it
    /// completed.
    fn on_frame(&mut self, ti: usize, tag: u16, events: &mut Vec<NodeEvent>) {
        if self.me.is_none() || matches!(self.progress.get(&(ti, tag)), Some(Progress::Settled)) {
            return;
        }
        let Some(prefix) = self.reassemblers[ti].prefix(tag) else {
            return;
        };
        let prefix = prefix.to_vec();
        self.advance(ti, tag, &prefix, events);
    }

    /// Every frame of `tag` has arrived.
    fn on_complete(&mut self, ti: usize, tag: u16, bytes: Vec<u8>, events: &mut Vec<NodeEvent>) {
        if self.me.is_none() {
            self.progress.remove(&(ti, tag));
            if self.locked_inbox.len() == LOCKED_INBOX {
                self.locked_inbox.pop_front();
            }
            self.locked_inbox.push_back((ti, tag, bytes));
            return;
        }
        let Ok(env) = Envelope::decode(&bytes) else {
            self.progress.remove(&(ti, tag));
            return;
        };
        match env.kind {
            Kind::Private => self.advance(ti, tag, &bytes, events),
            Kind::Public => self.on_public(&env, events),
            Kind::Control => self.on_control(&env, events),
        }
        self.progress.remove(&(ti, tag));
    }

    /// Open as many chunks of a private envelope as `bytes` (a prefix of
    /// it, or all of it) now covers, emitting each as it decrypts, and
    /// deliver the message once the last chunk verifies.
    fn advance(&mut self, ti: usize, tag: u16, bytes: &[u8], events: &mut Vec<NodeEvent>) {
        let key = (ti, tag);
        if !self.progress.contains_key(&key) {
            let layout = match private_layout(bytes) {
                None => return,
                Some(Err(())) => {
                    self.progress.insert(key, Progress::Settled);
                    return;
                }
                Some(Ok(layout)) => layout,
            };
            let me = self.me.as_ref().expect("checked by callers");
            let fresh = !self.already_have(&layout.id)
                && layout.expires_min >= now_min().saturating_sub(SKEW_MIN);
            let opener = fresh
                .then(|| {
                    ChunkOpener::start(
                        me,
                        layout.id,
                        layout.expires_min,
                        &bytes[layout.header.clone()],
                    )
                    .ok()
                })
                .flatten();
            let progress = match opener {
                Some(opener) => Progress::Opening {
                    id: layout.id,
                    opener: Box::new(opener),
                    chunks_at: layout.header.end,
                    end: layout.end,
                    of: (layout.end - layout.header.end).div_ceil(SEGMENT) as u16,
                    next: 0,
                },
                None => Progress::Settled,
            };
            self.progress.insert(key, progress);
        }

        let progress = self.progress.get_mut(&key).expect("inserted above");
        let Progress::Opening {
            id,
            opener,
            chunks_at,
            end,
            of,
            next,
        } = progress
        else {
            return;
        };
        let id = *id;
        while *next < *of {
            let start = *chunks_at + *next as usize * SEGMENT;
            let stop = (start + SEGMENT).min(*end);
            if bytes.len() < stop {
                return;
            }
            let last = *next + 1 == *of;
            match opener.next(&bytes[start..stop], last) {
                Ok(view) => events.push(NodeEvent::ChunkLanded {
                    tag,
                    index: *next,
                    of: *of,
                    view,
                }),
                Err(_) => {
                    *progress = Progress::Settled;
                    return;
                }
            }
            *next += 1;
        }

        // The last chunk verified. Settle before finishing so `next` is
        // never called on this opener again.
        let Progress::Opening { opener, .. } = std::mem::replace(progress, Progress::Settled)
        else {
            unreachable!("matched as Opening above");
        };
        if let Ok(opened) = opener.finish() {
            self.deliver(ti, id, opened, events);
        }
    }

    /// Store and announce a verified private message, and send a receipt.
    fn deliver(&mut self, ti: usize, id: [u8; 16], opened: Opened, events: &mut Vec<NodeEvent>) {
        self.mark_seen(id);
        let Some(contact) = self.contact_for(&opened.sender) else {
            return;
        };
        let store = self.store.as_ref().expect("unlocked");
        if store.has_message(&id) {
            return;
        }
        let now = now_min();
        let message = StoredMessage {
            id,
            contact: contact.id,
            outgoing: false,
            text: opened.text,
            channel: self.channel(ti),
            at_min: now,
            heard: false,
        };
        if store.put_message(&message).is_err() {
            return;
        }
        events.push(NodeEvent::Message(message));

        if self.receipts && !low_order(&contact.who.dh) {
            let me = self.me.as_ref().expect("unlocked");
            let receipt = control::heard(me, &contact.who, &id, random_id(), now, TTL_MIN);
            self.transmit(&receipt);
        }
    }

    fn on_public(&mut self, env: &Envelope, events: &mut Vec<NodeEvent>) {
        if self.already_have(&env.id) {
            return;
        }
        self.mark_seen(env.id);
        let Ok((signer, text, _)) = open_public(env, now_min()) else {
            return;
        };
        let me = self.me.as_ref().expect("unlocked");
        if signer == me.public() || text.starts_with(control::HELLO_PREFIX) {
            return;
        }
        let verified = self.contact_for(&signer).is_some_and(|c| c.checked);
        events.push(NodeEvent::Broadcast {
            signer,
            text,
            verified,
        });
    }

    fn on_control(&mut self, env: &Envelope, events: &mut Vec<NodeEvent>) {
        if self.already_have(&env.id) {
            return;
        }
        self.mark_seen(env.id);
        let me = self.me.as_ref().expect("unlocked");
        match control::open(me, &env.body, now_min()) {
            Some(control::Control::PairHello { identity, name }) => {
                let mine = me.public();
                if identity == mine || low_order(&identity.dh) {
                    return;
                }
                events.push(NodeEvent::PairOffer {
                    safety: safety_number(&mine, &identity),
                    peer: identity,
                    name,
                });
            }
            Some(control::Control::Heard { id, by }) => {
                let Some(contact) = self.contact_for(&by) else {
                    return;
                };
                let store = self.store.as_ref().expect("unlocked");
                let sent_to_them = self
                    .sent_to
                    .get(&id)
                    .is_some_and(|to| to.contains(&contact.id))
                    || store
                        .conversation(contact.id)
                        .iter()
                        .any(|m| m.id == id && m.outgoing);
                if sent_to_them && store.mark_heard(&id).is_ok() {
                    events.push(NodeEvent::Heard { id, by: contact.id });
                }
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A transport the test scripts by hand: frames pushed into `inbox`
    /// are heard on the next poll; frames the node sends land in `sent`.
    #[derive(Clone, Default)]
    struct Wire {
        inbox: Arc<Mutex<Vec<Vec<u8>>>>,
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl Transport for Wire {
        fn name(&self) -> Channel {
            Channel::Sound
        }
        fn send(&mut self, frame: Vec<u8>) {
            self.sent.lock().unwrap().push(frame);
        }
        fn poll(&mut self, _now_ms: u64) -> Vec<TransportEvent> {
            std::mem::take(&mut *self.inbox.lock().unwrap())
                .into_iter()
                .map(TransportEvent::Frame)
                .collect()
        }
    }

    impl Wire {
        fn hear(&self, env: &Envelope, tag: u16) {
            self.inbox.lock().unwrap().extend(split(&env.encode(), tag));
        }
    }

    /// A node for `me`, unlocked, knowing each of `contacts` (checked).
    fn node(
        dir: &tempfile::TempDir,
        me: &Identity,
        contacts: &[&Identity],
    ) -> (Node, Wire, Vec<ContactId>) {
        let path = dir.path().join(format!("{}.db", me.public().short_hex()));
        let store = Store::create(&path, "pass", me, "me").unwrap();
        let ids = contacts
            .iter()
            .map(|c| store.add_contact("c", &c.public(), true).unwrap())
            .collect();
        let wire = Wire::default();
        (
            Node::new(Some(store), vec![Box::new(wire.clone())]),
            wire,
            ids,
        )
    }

    fn messages(events: &[NodeEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::Message(m) => Some(m.text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn node_can_move_to_a_tick_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<Node>();
    }

    #[test]
    fn chunks_land_before_the_envelope_completes() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[&a]);
        let env = seal_private(&a, &[b.public()], &"long ".repeat(40), now_min(), 60);
        let frames = split(&env.encode(), 9);
        let (last, rest) = frames.split_last().unwrap();

        wire.inbox.lock().unwrap().extend(rest.iter().cloned());
        let early = node.tick(1);
        let landed = early
            .iter()
            .filter(|e| matches!(e, NodeEvent::ChunkLanded { .. }))
            .count();
        assert!(landed >= 2, "only {landed} chunks landed early");
        assert!(messages(&early).is_empty(), "no message before the end");

        wire.inbox.lock().unwrap().push(last.clone());
        let done = node.tick(2);
        assert_eq!(messages(&done), vec!["long ".repeat(40)]);
    }

    #[test]
    fn duplicate_envelope_under_a_new_tag_is_delivered_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[&a]);
        let env = seal_private(&a, &[b.public()], "once", now_min(), 60);

        wire.hear(&env, 1);
        assert_eq!(messages(&node.tick(1)), vec!["once"]);
        wire.hear(&env, 2);
        let again = node.tick(2);
        assert!(messages(&again).is_empty());
        assert!(!again
            .iter()
            .any(|e| matches!(e, NodeEvent::ChunkLanded { .. })));
    }

    #[test]
    fn private_message_from_a_stranger_is_dropped() {
        let dir = tempfile::TempDir::new().unwrap();
        let (stranger, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[]);
        let env = seal_private(&stranger, &[b.public()], "hello", now_min(), 60);

        wire.hear(&env, 1);
        assert!(messages(&node.tick(1)).is_empty());
        assert!(wire.sent.lock().unwrap().is_empty(), "no receipt either");
    }

    #[test]
    fn receipts_can_be_turned_off() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[&a]);

        let env = seal_private(&a, &[b.public()], "with receipt", now_min(), 60);
        wire.hear(&env, 1);
        assert_eq!(messages(&node.tick(1)).len(), 1);
        assert!(!wire.sent.lock().unwrap().is_empty(), "receipt sent");

        wire.sent.lock().unwrap().clear();
        node.set_receipts(false);
        let env = seal_private(&a, &[b.public()], "without", now_min(), 60);
        wire.hear(&env, 2);
        assert_eq!(messages(&node.tick(2)).len(), 1);
        assert!(wire.sent.lock().unwrap().is_empty(), "no receipt");
    }

    #[test]
    fn receipt_for_a_message_sent_to_someone_else_is_ignored() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b, c) = (
            Identity::generate(),
            Identity::generate(),
            Identity::generate(),
        );
        let (mut node, wire, ids) = node(&dir, &a, &[&b, &c]);
        let id = node.send(&[ids[0]], "for b");
        assert_ne!(id, NOT_SENT);

        let from_c = control::heard(&c, &a.public(), &id, random_id(), now_min(), 60);
        wire.hear(&from_c, 1);
        assert!(!node
            .tick(1)
            .iter()
            .any(|e| matches!(e, NodeEvent::Heard { .. })));

        let from_b = control::heard(&b, &a.public(), &id, random_id(), now_min(), 60);
        wire.hear(&from_b, 2);
        let heard: Vec<_> = node
            .tick(2)
            .into_iter()
            .filter_map(|e| match e {
                NodeEvent::Heard { id, by } => Some((id, by)),
                _ => None,
            })
            .collect();
        assert_eq!(heard, vec![(id, ids[0])]);
    }

    #[test]
    fn locked_inbox_keeps_the_newest_256() {
        let dir = tempfile::TempDir::new().unwrap();
        let wire = Wire::default();
        let mut node = Node::new(None, vec![Box::new(wire.clone())]);
        let c = Identity::generate();
        for i in 0..(LOCKED_INBOX as u16 + 4) {
            wire.hear(&seal_public(&c, &format!("post {i}"), now_min(), 60), i);
        }
        node.tick(1);
        assert_eq!(node.locked_inbox.len(), LOCKED_INBOX);

        let me = Identity::generate();
        node.unlock(Store::create(&dir.path().join("m.db"), "pass", &me, "me").unwrap());
        let texts: Vec<_> = node
            .tick(2)
            .into_iter()
            .filter_map(|e| match e {
                NodeEvent::Broadcast { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), LOCKED_INBOX);
        assert_eq!(texts[0], "post 4");
    }

    #[test]
    fn sends_too_long_or_to_nobody_are_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, ids) = node(&dir, &a, &[&b]);
        assert_eq!(node.send(&[], "hi"), NOT_SENT);
        assert_eq!(node.send(&[ContactId(999)], "hi"), NOT_SENT);
        assert_eq!(node.send(&ids, &"x".repeat(4000)), NOT_SENT);
        assert_eq!(node.broadcast(&"x".repeat(4000)), NOT_SENT);
        assert!(wire.sent.lock().unwrap().is_empty());

        let longest = (0..4000)
            .rev()
            .find(|n| private_envelope_len(1, *n) <= MAX_ENVELOPE)
            .unwrap();
        assert_ne!(node.send(&ids, &"x".repeat(longest)), NOT_SENT);
    }

    #[test]
    fn locked_node_sends_nothing() {
        let wire = Wire::default();
        let mut node = Node::new(None, vec![Box::new(wire.clone())]);
        assert_eq!(node.send(&[ContactId(1)], "hi"), NOT_SENT);
        assert_eq!(node.broadcast("hi"), NOT_SENT);
        node.start_pairing("me");
        let peer = Identity::generate().public();
        assert_eq!(node.confirm_pairing(&peer, "p"), NOT_STORED);
        assert!(wire.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn size_formula_matches_the_sealer() {
        let (a, b, c) = (
            Identity::generate(),
            Identity::generate(),
            Identity::generate(),
        );
        for (to, len) in [(1, 0), (1, 47), (2, 48), (1, 200), (2, 1000)] {
            let who: Vec<_> = [&b, &c][..to].iter().map(|i| i.public()).collect();
            let env = seal_private(&a, &who, &"x".repeat(len), 0, 1);
            assert_eq!(env.encode().len(), private_envelope_len(to, len));
        }
    }

    #[test]
    fn layout_finds_the_chunks_of_a_sealed_envelope() {
        let (a, b) = (Identity::generate(), Identity::generate());
        let env = seal_private(&a, &[b.public()], "Water is at the church on Fifth.", 0, 1);
        let bytes = env.encode();
        assert!(private_layout(&bytes[..10]).is_none());
        assert!(private_layout(&bytes[..ENVELOPE_HEADER + 50]).is_none());
        let layout = private_layout(&bytes).unwrap().unwrap();
        assert_eq!(layout.id, env.id);
        assert_eq!(layout.end, bytes.len());
        let (header, chunks) = bombechat_core::split_private_body(&env.body).unwrap();
        assert_eq!(&bytes[layout.header.clone()], header);
        assert_eq!(
            (layout.end - layout.header.end).div_ceil(SEGMENT),
            chunks.len()
        );

        let public = seal_public(&a, "hi", 0, 1).encode();
        assert!(matches!(private_layout(&public), Some(Err(()))));
    }

    #[test]
    fn low_order_points_are_caught() {
        assert!(low_order(&[0; 32]));
        let mut one = [0u8; 32];
        one[0] = 1;
        assert!(low_order(&one));
        assert!(!low_order(&Identity::generate().public().dh));
    }
}
