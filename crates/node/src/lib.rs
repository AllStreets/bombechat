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
//!   already handled, is skipped before anything is opened. An id only
//!   counts as handled once a signature covering it has verified, so
//!   garbage under a stolen id cannot suppress the real envelope. Control
//!   envelopes are deduped on their inner (signed) envelope's id, since
//!   the outer id is not signed.
//!
//! # Channel state and the outbox
//!
//! Besides what it hears, the node reports how each channel is doing and
//! how each of its sends is getting on. A channel is "receiving" while
//! any envelope is part way through arriving on it and "listening" when
//! none is; "failed" is reported when an arrival is given up (no frame
//! for ten minutes) or fails its checks, followed at once by the state
//! the channel is then in. Each message or broadcast this node sends is
//! reported as [`OutboxState::Waiting`] when it is queued,
//! [`OutboxState::Playing`] when its first frame goes out and
//! [`OutboxState::Sent`] when its last repeat has finished, or
//! [`OutboxState::Failed`] if it will never go out. Receipts and pairing
//! hellos are not reported.
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
//!
//! # The Sound switch
//!
//! [`Node::set_sound`]: at [`SoundSwitch::Listen`] the node hears but never
//! transmits; at [`SoundSwitch::Off`] it neither hears nor transmits.
//!
//! # Delivery kept in the store
//!
//! A message sent is stored waiting, with its sealed envelope, and marked
//! sent or failed as its carrier reports. One still waiting when the app
//! quits is queued again on the next [`Node::unlock`].

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
use bombechat_store::{Channel, Contact, ContactId, Delivery, Store, StoredMessage};
use rand_core::{OsRng, RngCore};

pub use sound::{SoundTransport, MAX_QUEUED_ENVELOPES, QUEUE_FULL};
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

/// A receipt for the same message is sent at most once in this long, so
/// a sender who missed the first one gets another when they resend, but a
/// replayed envelope cannot make this device transmit continuously.
const RECEIPT_GAP_MS: u64 = 60_000;

/// A channel's reception states, as reported in [`NodeEvent::ChannelState`].
const RECEIVING: &str = "receiving";
const LISTENING: &str = "listening";
/// Sound only, at [`SoundSwitch::Off`]: the microphone is closed, not
/// merely idle, so this is reported instead of [`LISTENING`].
const OFF: &str = "off";

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

/// Where one of this node's sends has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxState {
    /// Queued, waiting for the carrier (for sound, a quiet room).
    Waiting,
    /// Its first frame has gone out.
    Playing,
    /// Every frame, repeats included, has finished going out.
    Sent,
    /// It will not go out: no carrier took it, the carrier refused or
    /// could not play it, or the Sound switch does not allow sending.
    Failed,
}

impl OutboxState {
    /// The state as the interface names it: "waiting", "playing", "sent".
    pub fn as_str(self) -> &'static str {
        match self {
            OutboxState::Waiting => "waiting",
            OutboxState::Playing => "playing",
            OutboxState::Sent => "sent",
            OutboxState::Failed => "failed",
        }
    }
}

/// The Sound switch on the front of the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundSwitch {
    /// Not listening, never transmitting.
    Off,
    /// Listening through the microphone; never makes a sound: no
    /// messages, broadcasts, receipts or pairing hellos go out.
    Listen,
    /// Listening, and playing sends, receipts and hellos. The default.
    ListenAndSend,
}

impl SoundSwitch {
    /// The switch by its position, as the settings page numbers it: 0 Off,
    /// 1 Listen, 2 Listen and send.
    pub fn from_index(i: usize) -> Option<SoundSwitch> {
        match i {
            0 => Some(SoundSwitch::Off),
            1 => Some(SoundSwitch::Listen),
            2 => Some(SoundSwitch::ListenAndSend),
            _ => None,
        }
    }
}

/// The sound channel's state when a send is refused at Listen.
pub const LISTEN_ONLY: &str = "not sent: sound is set to listen only";

/// The sound channel's state when a send is refused with sound Off.
pub const SOUND_OFF: &str = "not sent: sound is off";

/// The channel state reported when a send asks for a channel this plan
/// does not build.
pub const NOT_IN_THIS_PLAN: &str = "unavailable: not in this plan";

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
    /// A channel changed state: "receiving", "listening", "failed", or
    /// e.g. "unavailable: microphone permission denied".
    ChannelState { channel: Channel, state: String },
    /// A message or broadcast this node sent, by envelope id, moved on.
    Outbox { id: [u8; 16], state: OutboxState },
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
            NodeEvent::Outbox { id, state } => f
                .debug_struct("Outbox")
                .field("id", id)
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
    /// When a receipt was last sent for each message id, by `now_ms`.
    receipts_sent: HashMap<[u8; 16], u64>,
    /// The `now_ms` of the current (or last) tick.
    now_ms: u64,
    /// Events raised outside a tick, reported on the next one.
    pending: Vec<NodeEvent>,
    /// The envelope id of each send still in the outbox, by frame tag.
    outbox: HashMap<u16, [u8; 16]>,
    /// Tags part way through arriving, by (transport, tag).
    arriving: HashSet<(usize, u16)>,
    /// Transports on which an arrival failed during this tick.
    failed: HashSet<usize>,
    /// The receiving/listening state last reported, by transport.
    reception: Vec<&'static str>,
    /// Where the Sound switch is.
    sound: SoundSwitch,
}

impl Node {
    /// A node over `transports`. With `store` `None` the node starts
    /// locked; see [`Node::unlock`].
    pub fn new(store: Option<Store>, transports: Vec<Box<dyn Transport>>) -> Node {
        let reassemblers = transports.iter().map(|_| Reassembler::new()).collect();
        let reception = transports.iter().map(|_| LISTENING).collect();
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
            receipts_sent: HashMap::new(),
            now_ms: 0,
            pending: Vec::new(),
            outbox: HashMap::new(),
            arriving: HashSet::new(),
            failed: HashSet::new(),
            reception,
            sound: SoundSwitch::ListenAndSend,
        };
        if let Some(store) = store {
            node.unlock(store);
        }
        node
    }

    /// Hand the node its opened store. Envelopes held while locked are
    /// opened on the next [`tick`](Node::tick). Messages that were still
    /// waiting to go out when the store was last open are queued again
    /// under the same envelope, or marked failed if it has expired.
    pub fn unlock(&mut self, store: Store) {
        self.me = Some(store.me());
        let waiting = store.waiting();
        self.store = Some(store);
        let now = now_min();
        for (m, bytes) in waiting {
            match Envelope::decode(&bytes) {
                Ok(env) if env.id == m.id && env.expires_min > now => {
                    self.sent_to.insert(env.id, vec![m.contact]);
                    let tag = self.transmit(&env);
                    self.track(tag, env.id);
                }
                _ => self.settle(m.id, OutboxState::Failed, None),
            }
        }
    }

    /// Set the Sound switch. Off, sound transports stop listening; at
    /// Listen and Off nothing more is queued on them (sends and
    /// broadcasts are refused, receipts and hellos are not sent) and what
    /// is already queued waits.
    pub fn set_sound(&mut self, switch: SoundSwitch) {
        self.sound = switch;
        for t in &mut self.transports {
            if t.name() == Channel::Sound {
                t.set_listening(switch != SoundSwitch::Off);
                t.set_sending(switch == SoundSwitch::ListenAndSend);
            }
        }
    }

    /// Play sound only while the room is quieter than `db`.
    pub fn set_send_under(&mut self, db: f32) {
        for t in &mut self.transports {
            t.set_send_under(db);
        }
    }

    /// Why nothing may be sent by sound right now, if so: the channel
    /// state to report.
    fn sound_refusal(&self) -> Option<&'static str> {
        match self.sound {
            SoundSwitch::Off => Some(SOUND_OFF),
            SoundSwitch::Listen => Some(LISTEN_ONLY),
            SoundSwitch::ListenAndSend => None,
        }
    }

    /// Report a send refused by the Sound switch: `why`, then the state
    /// the channel is in.
    fn report_refusal(&mut self, why: &'static str) {
        self.pending.push(NodeEvent::ChannelState {
            channel: Channel::Sound,
            state: why.to_string(),
        });
        for ti in 0..self.transports.len() {
            if self.channel(ti) == Channel::Sound {
                self.pending.push(NodeEvent::ChannelState {
                    channel: Channel::Sound,
                    state: self.reception[ti].to_string(),
                });
            }
        }
    }

    /// A send is over: record how it went in the store (if it is a stored
    /// message) and report it, now into `events` or else on the next tick.
    fn settle(&mut self, id: [u8; 16], state: OutboxState, events: Option<&mut Vec<NodeEvent>>) {
        let delivery = match state {
            OutboxState::Sent => Delivery::Sent,
            OutboxState::Failed => Delivery::Failed,
            OutboxState::Waiting | OutboxState::Playing => Delivery::Waiting,
        };
        if let Some(store) = &self.store {
            let _ = store.set_delivery(&id, delivery);
        }
        let event = NodeEvent::Outbox { id, state };
        match events {
            Some(events) => events.push(event),
            None => self.pending.push(event),
        }
    }

    /// Whether to send delivery receipts for messages from contacts. On by
    /// default; a receipt reveals that this device is nearby.
    pub fn set_receipts(&mut self, on: bool) {
        self.receipts = on;
    }

    /// This device's public identity, once unlocked.
    pub fn me(&self) -> Option<PublicIdentity> {
        self.me.as_ref().map(Identity::public)
    }

    /// The stored messages with `contact`, oldest first, or none while
    /// locked.
    pub fn conversation(&self, contact: ContactId) -> Vec<StoredMessage> {
        self.store
            .as_ref()
            .map(|s| s.conversation(contact))
            .unwrap_or_default()
    }

    /// Each channel's rate, in payload bytes a second, for the channels
    /// whose transport measures one ([`Transport::rate`]).
    pub fn rates(&self) -> Vec<(Channel, f32)> {
        self.transports
            .iter()
            .filter_map(|t| Some((t.name(), t.rate()?)))
            .collect()
    }

    /// The contacts in the store, or none while locked.
    pub fn contacts(&self) -> Vec<Contact> {
        self.store.as_ref().map(Store::contacts).unwrap_or_default()
    }

    /// Seal `text` to the given contacts and send it over `channel`.
    /// Returns the message's envelope id, or [`NOT_SENT`] if the node is
    /// locked, no listed contact can be sealed to, or the text is too
    /// long for one envelope.
    ///
    /// Only [`Channel::Sound`] is built in this plan. Any other channel
    /// sends nothing, returns [`NOT_SENT`] and reports the channel as
    /// [`NOT_IN_THIS_PLAN`] on the next tick.
    ///
    /// With the Sound switch at Listen or Off the message is sealed and
    /// stored but not sent: it is reported [`OutboxState::Failed`] and the
    /// channel as [`LISTEN_ONLY`] or [`SOUND_OFF`] on the next tick.
    ///
    /// A message to several contacts is one envelope; the store keeps one
    /// row per message id, so it is filed under the first of them.
    pub fn send(&mut self, to: &[ContactId], text: &str, channel: Channel) -> [u8; 16] {
        if channel != Channel::Sound {
            self.pending.push(NodeEvent::ChannelState {
                channel,
                state: NOT_IN_THIS_PLAN.to_string(),
            });
            return NOT_SENT;
        }
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
        let refused = self.sound_refusal();
        let mut stored = StoredMessage {
            id: env.id,
            contact: recipients[0].id,
            outgoing: true,
            text: text.to_string(),
            channel,
            at_min: now,
            heard: false,
            delivery: Delivery::Waiting,
        };
        if let Some(why) = refused {
            stored.delivery = Delivery::Failed;
            if store.put_message(&stored).is_err() {
                return NOT_SENT;
            }
            self.report_refusal(why);
            self.pending.push(NodeEvent::Outbox {
                id: env.id,
                state: OutboxState::Failed,
            });
            return env.id;
        }
        if store.put_outgoing(&stored, &env.encode()).is_err() {
            return NOT_SENT;
        }
        self.sent_to
            .insert(env.id, recipients.iter().map(|c| c.id).collect());
        let tag = self.transmit(&env);
        self.track(tag, env.id);
        env.id
    }

    /// Sign `text` as a public broadcast and send it. Returns its envelope
    /// id, or [`NOT_SENT`] if locked or the text is too long. Refused by
    /// the Sound switch as [`Node::send`] is.
    pub fn broadcast(&mut self, text: &str) -> [u8; 16] {
        let Some(me) = &self.me else {
            return NOT_SENT;
        };
        if ENVELOPE_HEADER + INNER_OVERHEAD + text.len() > MAX_ENVELOPE {
            return NOT_SENT;
        }
        let env = seal_public(me, text, now_min(), TTL_MIN);
        if let Some(why) = self.sound_refusal() {
            self.report_refusal(why);
            self.pending.push(NodeEvent::Outbox {
                id: env.id,
                state: OutboxState::Failed,
            });
            return env.id;
        }
        let tag = self.transmit(&env);
        self.track(tag, env.id);
        env.id
    }

    /// Put a send in the outbox: reported waiting on the next tick, then
    /// as its transport reports its tag playing and sent.
    fn track(&mut self, tag: u16, id: [u8; 16]) {
        if self.transports.is_empty() {
            self.settle(id, OutboxState::Failed, None);
        } else {
            self.outbox.insert(tag, id);
            self.pending.push(NodeEvent::Outbox {
                id,
                state: OutboxState::Waiting,
            });
        }
    }

    /// Announce this device for pairing: a signed hello carrying its
    /// identity and `my_name` (cut to 64 bytes). Anyone who hears it gets
    /// a [`NodeEvent::PairOffer`]. Refused with "locked" before
    /// [`Node::unlock`], and with the channel state it reports when the
    /// Sound switch is not at Listen and send.
    pub fn start_pairing(&mut self, my_name: &str) -> Result<(), &'static str> {
        let Some(me) = &self.me else {
            return Err("locked");
        };
        let env = control::pair_hello(me, my_name, random_id(), now_min(), TTL_MIN);
        if let Some(why) = self.sound_refusal() {
            self.report_refusal(why);
            return Err(why);
        }
        self.transmit(&env);
        Ok(())
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
            return match store.set_checked(c.id, true) {
                Ok(()) => c.id,
                Err(_) => NOT_STORED,
            };
        }
        store.add_contact(name, peer, true).unwrap_or(NOT_STORED)
    }

    /// Poll every transport and handle what they heard. `now_ms` is a
    /// monotonic millisecond clock.
    pub fn tick(&mut self, now_ms: u64) -> Vec<NodeEvent> {
        let mut events = std::mem::take(&mut self.pending);
        self.now_ms = now_ms;
        self.receipts_sent
            .retain(|_, at| now_ms.saturating_sub(*at) < RECEIPT_GAP_MS);

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
                        if channel == Channel::Sound && self.sound == SoundSwitch::Off {
                            continue;
                        }
                        for e in self.reassemblers[ti].push(&frame, now_ms) {
                            match e {
                                Event::FrameOk { tag, .. } => {
                                    if self.reassemblers[ti].prefix(tag).is_some() {
                                        self.arriving.insert((ti, tag));
                                    }
                                    self.on_frame(ti, tag, &mut events)
                                }
                                Event::Complete { tag, bytes } => {
                                    self.arriving.remove(&(ti, tag));
                                    self.on_complete(ti, tag, bytes, &mut events)
                                }
                                Event::FrameBad => {}
                            }
                        }
                    }
                    TransportEvent::Playing(tag) => {
                        if let Some(id) = self.outbox.get(&tag) {
                            events.push(NodeEvent::Outbox {
                                id: *id,
                                state: OutboxState::Playing,
                            });
                        }
                    }
                    TransportEvent::Sent(tag) => {
                        if let Some(id) = self.outbox.remove(&tag) {
                            self.settle(id, OutboxState::Sent, Some(&mut events));
                        }
                    }
                    TransportEvent::Dropped(tag) => {
                        if let Some(id) = self.outbox.remove(&tag) {
                            self.settle(id, OutboxState::Failed, Some(&mut events));
                        }
                    }
                    TransportEvent::RoomDb(db) => events.push(NodeEvent::RoomDb(db)),
                    TransportEvent::Unavailable(reason) => events.push(NodeEvent::ChannelState {
                        channel,
                        state: format!("unavailable: {reason}"),
                    }),
                    TransportEvent::Notice(state) => {
                        events.push(NodeEvent::ChannelState { channel, state })
                    }
                }
            }
            self.reassemblers[ti].expire(now_ms);
        }

        let reassemblers = &self.reassemblers;
        self.progress
            .retain(|(ti, tag), _| reassemblers[*ti].prefix(*tag).is_some());
        // An arrival the reassembler gave up on was abandoned.
        let failed = &mut self.failed;
        self.arriving.retain(|(ti, tag)| {
            let alive = reassemblers[*ti].prefix(*tag).is_some();
            if !alive {
                failed.insert(*ti);
            }
            alive
        });
        self.report_reception(&mut events);
        events
    }

    /// Report each transport's reception state if it changed, after a
    /// "failed" if an arrival failed on it this tick. Sound at
    /// [`SoundSwitch::Off`] reports [`OFF`], never [`LISTENING`] or
    /// [`RECEIVING`]: the microphone is closed, so nothing can arrive on
    /// it either.
    fn report_reception(&mut self, events: &mut Vec<NodeEvent>) {
        for ti in 0..self.transports.len() {
            let channel = self.channel(ti);
            let failed = self.failed.remove(&ti);
            if failed {
                events.push(NodeEvent::ChannelState {
                    channel,
                    state: "failed".to_string(),
                });
            }
            let now = if channel == Channel::Sound && self.sound == SoundSwitch::Off {
                OFF
            } else if self.arriving.iter().any(|(t, _)| *t == ti) {
                RECEIVING
            } else {
                LISTENING
            };
            if failed || self.reception[ti] != now {
                self.reception[ti] = now;
                events.push(NodeEvent::ChannelState {
                    channel,
                    state: now.to_string(),
                });
            }
        }
    }

    fn channel(&self, ti: usize) -> Channel {
        self.transports.get(ti).map_or(Channel::Sound, |t| t.name())
    }

    /// Split `env` under a fresh random tag and queue it on every
    /// transport. Remembers its id so this node ignores its own echo.
    /// Returns the tag.
    fn transmit(&mut self, env: &Envelope) -> u16 {
        self.mark_seen(env.id);
        let tag = OsRng.next_u32() as u16;
        let frames = split(&env.encode(), tag);
        for t in &mut self.transports {
            for f in &frames {
                t.send(f.clone());
            }
        }
        tag
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
            self.failed.insert(ti);
            return;
        };
        match env.kind {
            Kind::Private => self.advance(ti, tag, &bytes, events),
            Kind::Public => self.on_public(ti, &env, events),
            Kind::Control => self.on_control(ti, &env, events),
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
            if self.already_have(&layout.id) {
                self.resend_receipt(layout.id);
                self.progress.insert(key, Progress::Settled);
                return;
            }
            let me = self.me.as_ref().expect("checked by callers");
            let fresh = layout.expires_min >= now_min().saturating_sub(SKEW_MIN);
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
                    self.failed.insert(ti);
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
        match opener.finish() {
            Ok(opened) => self.deliver(ti, id, opened, events),
            Err(_) => {
                self.failed.insert(ti);
            }
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
            delivery: Delivery::Sent,
        };
        if store.put_message(&message).is_err() {
            return;
        }
        events.push(NodeEvent::Message(message));
        self.send_receipt(id, &contact);
    }

    /// A private envelope we already have arrived again. If it is a message
    /// a contact sent us, they may have missed our receipt: send another.
    fn resend_receipt(&mut self, id: [u8; 16]) {
        let Some(store) = &self.store else {
            return;
        };
        let Some(stored) = store.message(&id).filter(|m| !m.outgoing) else {
            return;
        };
        if let Some(contact) = store
            .contacts()
            .into_iter()
            .find(|c| c.id == stored.contact)
        {
            self.send_receipt(id, &contact);
        }
    }

    /// Send `contact` a receipt for message `id`, if receipts are on, the
    /// Sound switch is at Listen and send, and none went out for it in the
    /// last [`RECEIPT_GAP_MS`].
    fn send_receipt(&mut self, id: [u8; 16], contact: &Contact) {
        if !self.receipts || self.sound_refusal().is_some() || low_order(&contact.who.dh) {
            return;
        }
        if let Some(at) = self.receipts_sent.get(&id) {
            if self.now_ms.saturating_sub(*at) < RECEIPT_GAP_MS {
                return;
            }
        }
        self.receipts_sent.insert(id, self.now_ms);
        let me = self.me.as_ref().expect("unlocked");
        let receipt = control::heard(me, &contact.who, &id, random_id(), now_min(), TTL_MIN);
        self.transmit(&receipt);
    }

    fn on_public(&mut self, ti: usize, env: &Envelope, events: &mut Vec<NodeEvent>) {
        if self.already_have(&env.id) {
            return;
        }
        // Marked seen only once the signature covering the id verifies, so
        // garbage sent under a stolen id cannot suppress the real envelope.
        let Ok((signer, text, _)) = open_public(env, now_min()) else {
            self.failed.insert(ti);
            return;
        };
        self.mark_seen(env.id);
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

    fn on_control(&mut self, ti: usize, env: &Envelope, events: &mut Vec<NodeEvent>) {
        if self.already_have(&env.id) {
            return;
        }
        let me = self.me.as_ref().expect("unlocked");
        // The outer id is not signed; dedup on the inner envelope's id,
        // which is, and only once it has verified.
        let (control, inner_id) = match control::open(me, &env.body, now_min()) {
            Ok(opened) => opened,
            Err(control::Refused::NotForMe) => return,
            Err(control::Refused::Bad) => {
                self.failed.insert(ti);
                return;
            }
        };
        self.mark_seen(env.id);
        if self.seen.contains(&inner_id) {
            return;
        }
        self.mark_seen(inner_id);
        let me = self.me.as_ref().expect("unlocked");
        match control {
            control::Control::PairHello { identity, name } => {
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
            control::Control::Heard { id, by } => {
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
        /// Reported on the next poll, after the frames.
        news: Arc<Mutex<Vec<TransportEvent>>>,
    }

    impl Transport for Wire {
        fn name(&self) -> Channel {
            Channel::Sound
        }
        fn send(&mut self, frame: Vec<u8>) {
            self.sent.lock().unwrap().push(frame);
        }
        fn poll(&mut self, _now_ms: u64) -> Vec<TransportEvent> {
            let mut events: Vec<TransportEvent> = std::mem::take(&mut *self.inbox.lock().unwrap())
                .into_iter()
                .map(TransportEvent::Frame)
                .collect();
            events.extend(std::mem::take(&mut *self.news.lock().unwrap()));
            events
        }
    }

    impl Wire {
        fn hear(&self, env: &Envelope, tag: u16) {
            self.inbox.lock().unwrap().extend(split(&env.encode(), tag));
        }

        /// The tag of the frames last sent.
        fn last_tag(&self) -> u16 {
            let sent = self.sent.lock().unwrap();
            let f = sent.last().expect("something was sent");
            u16::from_be_bytes([f[0], f[1]])
        }

        /// The envelopes sent, reassembled from their frames.
        fn envelopes(&self) -> Vec<Envelope> {
            let mut r = Reassembler::new();
            self.sent
                .lock()
                .unwrap()
                .iter()
                .flat_map(|f| r.push(f, 0))
                .filter_map(|e| match e {
                    Event::Complete { bytes, .. } => Envelope::decode(&bytes).ok(),
                    _ => None,
                })
                .collect()
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
    fn a_transport_that_measures_no_rate_is_left_out_of_rates() {
        let node = Node::new(None, vec![Box::new(Wire::default())]);
        assert!(node.rates().is_empty());
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
        let id = node.send(&[ids[0]], "for b", Channel::Sound);
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
        assert_eq!(node.send(&[], "hi", Channel::Sound), NOT_SENT);
        assert_eq!(node.send(&[ContactId(999)], "hi", Channel::Sound), NOT_SENT);
        assert_eq!(node.send(&ids, &"x".repeat(4000), Channel::Sound), NOT_SENT);
        assert_eq!(node.broadcast(&"x".repeat(4000)), NOT_SENT);
        assert!(wire.sent.lock().unwrap().is_empty());

        let longest = (0..4000)
            .rev()
            .find(|n| private_envelope_len(1, *n) <= MAX_ENVELOPE)
            .unwrap();
        assert_ne!(
            node.send(&ids, &"x".repeat(longest), Channel::Sound),
            NOT_SENT
        );
    }

    fn offers(events: &[NodeEvent]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, NodeEvent::PairOffer { .. }))
            .count()
    }

    #[test]
    fn garbage_under_a_stolen_public_id_does_not_suppress_the_real_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let (b, c) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[]);
        let genuine = seal_public(&c, "Road north is open.", now_min(), 60);
        let forged = Envelope {
            body: vec![0; 140],
            ..genuine.clone()
        };

        wire.hear(&forged, 1);
        assert!(!node
            .tick(1)
            .iter()
            .any(|e| matches!(e, NodeEvent::Broadcast { .. })));
        wire.hear(&genuine, 2);
        let texts: Vec<_> = node
            .tick(2)
            .into_iter()
            .filter_map(|e| match e {
                NodeEvent::Broadcast { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["Road north is open."]);
    }

    #[test]
    fn garbage_under_a_stolen_control_id_does_not_suppress_the_real_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let (b, c) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[]);
        let genuine = control::pair_hello(&c, "Cy", random_id(), now_min(), 60);
        let forged = Envelope {
            body: vec![1, 1, 0, 0, 0],
            ..genuine.clone()
        };

        wire.hear(&forged, 1);
        assert_eq!(offers(&node.tick(1)), 0);
        wire.hear(&genuine, 2);
        assert_eq!(offers(&node.tick(2)), 1);
    }

    #[test]
    fn hello_with_a_bad_signature_offers_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let (b, c) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[]);
        let mut hello = control::pair_hello(&c, "Cy", random_id(), now_min(), 60);
        let last = hello.body.len() - 1;
        hello.body[last] ^= 1; // the inner signature's last byte

        wire.hear(&hello, 1);
        assert_eq!(offers(&node.tick(1)), 0);
    }

    #[test]
    fn a_hello_rewrapped_under_new_ids_offers_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let (b, c) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[]);
        let hello = control::pair_hello(&c, "Cy", random_id(), now_min(), 60);

        wire.hear(&hello, 1);
        assert_eq!(offers(&node.tick(1)), 1);
        let rewrapped = Envelope {
            id: random_id(),
            ..hello.clone()
        };
        wire.hear(&rewrapped, 2);
        assert_eq!(offers(&node.tick(2)), 0);
    }

    #[test]
    fn a_receipt_rewrapped_under_a_new_id_is_processed_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, ids) = node(&dir, &a, &[&b]);
        let id = node.send(&[ids[0]], "for b", Channel::Sound);
        let heard = |events: Vec<NodeEvent>| {
            events
                .iter()
                .filter(|e| matches!(e, NodeEvent::Heard { .. }))
                .count()
        };

        let receipt = control::heard(&b, &a.public(), &id, random_id(), now_min(), 60);
        wire.hear(&receipt, 1);
        assert_eq!(heard(node.tick(1)), 1);
        for tag in 2..5 {
            let rewrapped = Envelope {
                id: random_id(),
                ..receipt.clone()
            };
            wire.hear(&rewrapped, tag);
            assert_eq!(heard(node.tick(tag as u64)), 0);
        }
    }

    #[test]
    fn a_resent_message_gets_another_receipt_at_most_once_a_minute() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[&a]);
        let env = seal_private(&a, &[b.public()], "resent", now_min(), 60);
        let receipts_sent = |w: &Wire| std::mem::take(&mut *w.sent.lock().unwrap()).len();

        wire.hear(&env, 1);
        assert_eq!(messages(&node.tick(1_000)).len(), 1);
        assert!(receipts_sent(&wire) > 0, "first receipt");

        wire.hear(&env, 2);
        assert!(messages(&node.tick(30_000)).is_empty());
        assert_eq!(receipts_sent(&wire), 0, "too soon for another");

        wire.hear(&env, 3);
        assert!(messages(&node.tick(61_000)).is_empty(), "not shown twice");
        assert!(receipts_sent(&wire) > 0, "receipt resent after a minute");

        node.set_receipts(false);
        wire.hear(&env, 4);
        node.tick(200_000);
        assert_eq!(receipts_sent(&wire), 0, "receipts off");
    }

    #[test]
    fn confirming_a_known_contact_marks_it_checked() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let path = dir.path().join("a.db");
        let store = Store::create(&path, "pass", &a, "me").unwrap();
        let id = store.add_contact("Ben", &b.public(), false).unwrap();
        let mut node = Node::new(Some(store), vec![]);

        assert_eq!(node.confirm_pairing(&b.public(), "Ben"), id);
        let contacts = node.store.as_ref().unwrap().contacts();
        assert_eq!(contacts.len(), 1);
        assert!(contacts[0].checked);
    }

    #[test]
    fn locked_node_sends_nothing() {
        let wire = Wire::default();
        let mut node = Node::new(None, vec![Box::new(wire.clone())]);
        assert_eq!(node.send(&[ContactId(1)], "hi", Channel::Sound), NOT_SENT);
        assert_eq!(node.broadcast("hi"), NOT_SENT);
        assert_eq!(node.start_pairing("me"), Err("locked"));
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

    fn sound_states(events: &[NodeEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::ChannelState {
                    channel: Channel::Sound,
                    state,
                } => Some(state.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn channel_is_receiving_while_a_tag_arrives_then_listening() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[&a]);
        let env = seal_private(&a, &[b.public()], &"long ".repeat(40), now_min(), 60);
        let frames = split(&env.encode(), 9);
        let (last, rest) = frames.split_last().unwrap();

        assert!(
            sound_states(&node.tick(1)).is_empty(),
            "listening is the start"
        );
        wire.inbox.lock().unwrap().push(rest[0].clone());
        assert_eq!(sound_states(&node.tick(2)), vec!["receiving"]);
        wire.inbox.lock().unwrap().extend(rest[1..].iter().cloned());
        assert!(sound_states(&node.tick(3)).is_empty(), "still receiving");
        wire.inbox.lock().unwrap().push(last.clone());
        let done = node.tick(4);
        assert_eq!(messages(&done).len(), 1);
        assert_eq!(sound_states(&done), vec!["listening"]);
    }

    #[test]
    fn an_abandoned_arrival_reports_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[&a]);
        let env = seal_private(&a, &[b.public()], &"long ".repeat(40), now_min(), 60);
        wire.inbox
            .lock()
            .unwrap()
            .push(split(&env.encode(), 9).remove(0));
        assert_eq!(sound_states(&node.tick(1)), vec!["receiving"]);
        assert!(sound_states(&node.tick(60_000)).is_empty());
        // The reassembler gives a tag up after ten minutes without a frame.
        assert_eq!(
            sound_states(&node.tick(700_000)),
            vec!["failed", "listening"]
        );
    }

    #[test]
    fn an_arrival_that_fails_verification_reports_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[&a]);
        let env = seal_private(&a, &[b.public()], &"long ".repeat(40), now_min(), 60);
        let mut bytes = env.encode();
        let last = bytes.len() - 1;
        bytes[last] ^= 1; // the last chunk's tag
        let frames = split(&bytes, 9);
        let (end, rest) = frames.split_last().unwrap();

        wire.inbox.lock().unwrap().extend(rest.iter().cloned());
        assert_eq!(sound_states(&node.tick(1)), vec!["receiving"]);
        wire.inbox.lock().unwrap().push(end.clone());
        let done = node.tick(2);
        assert!(messages(&done).is_empty());
        assert_eq!(sound_states(&done), vec!["failed", "listening"]);

        // A forged broadcast fails its signature check the same way.
        let forged = Envelope {
            body: vec![0; 140],
            ..seal_public(&a, "Road north is open.", now_min(), 60)
        };
        wire.hear(&forged, 10);
        assert_eq!(sound_states(&node.tick(3)), vec!["failed", "listening"]);
    }

    #[test]
    fn sending_on_a_channel_not_in_this_plan_is_refused_and_reported() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, ids) = node(&dir, &a, &[&b]);
        for channel in [Channel::Light, Channel::Bluetooth] {
            assert_eq!(node.send(&ids, "hi", channel), NOT_SENT);
            let states: Vec<_> = node
                .tick(1)
                .into_iter()
                .filter_map(|e| match e {
                    NodeEvent::ChannelState { channel, state } => Some((channel, state)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                states,
                vec![(channel, "unavailable: not in this plan".to_string())]
            );
        }
        assert!(wire.sent.lock().unwrap().is_empty());
        assert!(!node
            .tick(2)
            .iter()
            .any(|e| matches!(e, NodeEvent::Outbox { .. })));
    }

    #[test]
    fn a_send_is_reported_waiting_but_a_receipt_is_not() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, ids) = node(&dir, &a, &[&b]);
        let outbox = |events: Vec<NodeEvent>| -> Vec<([u8; 16], OutboxState)> {
            events
                .into_iter()
                .filter_map(|e| match e {
                    NodeEvent::Outbox { id, state } => Some((id, state)),
                    _ => None,
                })
                .collect()
        };
        let id = node.send(&ids, "hi", Channel::Sound);
        let public = node.broadcast("Road north is open.");
        assert_eq!(
            outbox(node.tick(1)),
            vec![(id, OutboxState::Waiting), (public, OutboxState::Waiting)]
        );

        let env = seal_private(&b, &[a.public()], "back", now_min(), 60);
        wire.sent.lock().unwrap().clear();
        wire.hear(&env, 3);
        let events = node.tick(2);
        assert_eq!(messages(&events).len(), 1);
        assert!(!wire.sent.lock().unwrap().is_empty(), "a receipt went out");
        assert!(outbox(events).is_empty());
        assert!(outbox(node.tick(3)).is_empty());
    }

    #[test]
    fn a_send_with_no_transport_is_failed_at_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let store = Store::create(&dir.path().join("a.db"), "pass", &a, "me").unwrap();
        let c = store.add_contact("b", &b.public(), true).unwrap();
        let mut node = Node::new(Some(store), vec![]);
        let id = node.send(&[c], "hi", Channel::Sound);
        assert_ne!(id, NOT_SENT);
        let states: Vec<_> = node
            .tick(1)
            .into_iter()
            .filter_map(|e| match e {
                NodeEvent::Outbox { id, state } => Some((id, state)),
                _ => None,
            })
            .collect();
        assert_eq!(states, vec![(id, OutboxState::Failed)]);
    }

    #[test]
    fn a_control_envelope_that_fails_verification_reports_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        let (b, c) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[]);
        let mut hello = control::pair_hello(&c, "Cy", random_id(), now_min(), 60);
        let last = hello.body.len() - 1;
        hello.body[last] ^= 1;
        wire.hear(&hello, 1);
        assert_eq!(sound_states(&node.tick(1)), vec!["failed", "listening"]);

        // A receipt sealed to someone else is not a failure, just not ours.
        let other = Identity::generate();
        let receipt = control::heard(&c, &other.public(), &[1; 16], random_id(), now_min(), 60);
        wire.hear(&receipt, 2);
        assert!(sound_states(&node.tick(2)).is_empty());
    }

    #[test]
    fn conversation_lists_what_was_sent_and_nothing_while_locked() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, _, ids) = node(&dir, &a, &[&b]);
        let id = node.send(&ids, "first", Channel::Sound);
        let texts: Vec<_> = node
            .conversation(ids[0])
            .into_iter()
            .map(|m| (m.id, m.text, m.outgoing))
            .collect();
        assert_eq!(texts, vec![(id, "first".to_string(), true)]);
        assert!(Node::new(None, vec![]).conversation(ids[0]).is_empty());
    }

    #[test]
    fn me_is_the_public_identity_once_unlocked() {
        let dir = tempfile::TempDir::new().unwrap();
        let a = Identity::generate();
        let (node, _, _) = node(&dir, &a, &[]);
        assert_eq!(node.me(), Some(a.public()));
        assert_eq!(Node::new(None, vec![]).me(), None);
    }

    #[test]
    fn contacts_lists_the_store_and_nothing_while_locked() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (node, _, ids) = node(&dir, &a, &[&b]);
        let contacts = node.contacts();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].id, ids[0]);
        assert_eq!(contacts[0].who, b.public());

        assert!(Node::new(None, vec![]).contacts().is_empty());
    }

    #[test]
    fn low_order_points_are_caught() {
        assert!(low_order(&[0; 32]));
        let mut one = [0u8; 32];
        one[0] = 1;
        assert!(low_order(&one));
        assert!(!low_order(&Identity::generate().public().dh));
    }

    fn outbox(events: &[NodeEvent]) -> Vec<([u8; 16], OutboxState)> {
        events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::Outbox { id, state } => Some((*id, *state)),
                _ => None,
            })
            .collect()
    }

    fn states(events: &[NodeEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::ChannelState { state, .. } => Some(state.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn at_listen_it_hears_but_never_transmits() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, ids) = node(&dir, &b, &[&a]);
        node.set_sound(SoundSwitch::Listen);

        let env = seal_private(&a, &[b.public()], "heard, no receipt", now_min(), 60);
        wire.hear(&env, 1);
        assert_eq!(messages(&node.tick(1)), vec!["heard, no receipt"]);
        assert!(wire.sent.lock().unwrap().is_empty(), "no receipt");

        let id = node.send(&ids, "refused", Channel::Sound);
        assert_ne!(id, NOT_SENT, "kept, so the sheet can say it was not sent");
        let public = node.broadcast("refused too");
        assert_eq!(node.start_pairing("Ben"), Err(LISTEN_ONLY));
        let events = node.tick(2);
        assert_eq!(
            outbox(&events),
            vec![(id, OutboxState::Failed), (public, OutboxState::Failed)]
        );
        assert!(states(&events).contains(&LISTEN_ONLY.to_string()));
        assert!(wire.sent.lock().unwrap().is_empty(), "nothing transmitted");
        let kept = node.conversation(ids[0]);
        assert_eq!(kept.last().unwrap().delivery, Delivery::Failed);

        node.set_sound(SoundSwitch::ListenAndSend);
        let id = node.send(&ids, "now it goes", Channel::Sound);
        assert_eq!(outbox(&node.tick(3)), vec![(id, OutboxState::Waiting)]);
        assert!(!wire.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn at_off_it_hears_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, ids) = node(&dir, &b, &[&a]);
        node.set_sound(SoundSwitch::Off);
        wire.hear(&seal_private(&a, &[b.public()], "unheard", now_min(), 60), 1);
        let events = node.tick(1);
        assert!(messages(&events).is_empty());
        assert!(!events
            .iter()
            .any(|e| matches!(e, NodeEvent::ChunkLanded { .. })));
        // The channel reports itself off, not a stale "listening": the
        // microphone really is closed, not just letting what it hears go.
        assert_eq!(states(&events), vec!["off".to_string()]);
        node.send(&ids, "refused", Channel::Sound);
        let states = states(&node.tick(2));
        assert!(states.contains(&SOUND_OFF.to_string()));
        assert!(!states.contains(&"listening".to_string()));
        assert!(wire.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn a_send_is_stored_waiting_until_its_carrier_says_how_it_went() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, ids) = node(&dir, &a, &[&b]);
        let delivery = |node: &Node, id: [u8; 16]| {
            node.conversation(ids[0])
                .into_iter()
                .find(|m| m.id == id)
                .unwrap()
                .delivery
        };

        let sent = node.send(&ids, "goes out", Channel::Sound);
        assert_eq!(delivery(&node, sent), Delivery::Waiting);
        wire.news
            .lock()
            .unwrap()
            .push(TransportEvent::Sent(wire.last_tag()));
        node.tick(1);
        assert_eq!(delivery(&node, sent), Delivery::Sent);

        let dropped = node.send(&ids, "refused by the carrier", Channel::Sound);
        wire.news
            .lock()
            .unwrap()
            .push(TransportEvent::Dropped(wire.last_tag()));
        node.tick(2);
        assert_eq!(delivery(&node, dropped), Delivery::Failed);
    }

    #[test]
    fn what_was_still_waiting_at_quit_is_queued_again_on_unlock() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let path = dir.path().join("a.db");
        let store = Store::create(&path, "pass", &a, "me").unwrap();
        let c = store.add_contact("b", &b.public(), true).unwrap();
        let mut node = Node::new(Some(store), vec![Box::new(Wire::default())]);
        let id = node.send(&[c], "still waiting", Channel::Sound);
        // A message whose envelope ran out while the app was closed.
        let old = seal_private(&a, &[b.public()], "too old", now_min() - 3 * TTL_MIN, 60);
        let stale = StoredMessage {
            id: old.id,
            contact: c,
            outgoing: true,
            text: "too old".into(),
            channel: Channel::Sound,
            at_min: now_min() - 3 * TTL_MIN,
            heard: false,
            delivery: Delivery::Waiting,
        };
        node.store.as_ref().unwrap().put_outgoing(&stale, &old.encode()).unwrap();
        drop(node);

        let wire = Wire::default();
        let mut node = Node::new(None, vec![Box::new(wire.clone())]);
        assert!(wire.sent.lock().unwrap().is_empty());
        node.unlock(Store::open(&path, "pass").unwrap());
        let resent: Vec<_> = wire.envelopes().iter().map(|e| e.id).collect();
        assert_eq!(resent, vec![id], "the same envelope, and not the stale one");
        let events = node.tick(1);
        assert_eq!(
            outbox(&events),
            vec![(id, OutboxState::Waiting), (old.id, OutboxState::Failed)]
        );
        let kept = node.conversation(c);
        assert_eq!(kept[0].delivery, Delivery::Waiting);
        assert_eq!(kept[1].delivery, Delivery::Failed);
    }
}
