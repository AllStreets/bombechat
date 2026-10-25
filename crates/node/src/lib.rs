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
//! # The Light switch
//!
//! [`Node::set_light`] does the same for light: at [`LightSwitch::Read`]
//! the camera reads but nothing is ever shown; at [`LightSwitch::Off`] the
//! camera is closed and nothing is shown either. It starts at Off, so no
//! camera opens until the device sets the switch.
//!
//! A message goes out on the channel it is sent on, and only there. A
//! pairing hello goes out on every channel whose switch allows sending.
//! A receipt goes back on the channel its message came in on if that
//! channel may send, else on another that may. A broadcast goes out on
//! the channel it is sent on, as a message does.
//! Light shows each envelope for a sending window
//! ([`light::SEND_WINDOW`] times the frames a receiver needs at least),
//! cut short once everyone a message was for has sent a receipt.
//!
//! # Delivery kept in the store
//!
//! A message sent is stored waiting, with its sealed envelope, and marked
//! sent or failed as its carrier reports. One still waiting when the app
//! quits is queued again on the next [`Node::unlock`].
//!
//! # The self-test
//!
//! [`Node::start_self_test`] seals [`SELF_TEST_TEXT`] to this machine's
//! own key and sends it like any message (by sound, through the quiet
//! gate), so the machine hears itself: its chunks land as for any
//! message, and once the last one verifies the node reports
//! [`SelfTestState::Heard`]. It is never stored, never in the outbox,
//! never answered with a receipt. What marks it is its envelope id: drawn
//! at random for each run, held only in memory, bound to the body by the
//! seal and covered by the signature. Only an envelope under that id,
//! opened with this machine's key and signed by this machine, is taken,
//! and only once: it is then remembered as handled like any other, so a
//! replay is ignored. Everything else this machine sends is remembered as
//! handled the moment it goes out, so its own echo is never heard.

mod control;
mod light;
mod sound;
mod transport;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::ops::Range;
use std::time::{SystemTime, UNIX_EPOCH};

use bombechat_core::{
    open_public, safety_number, seal_private, seal_public, ChunkOpener, ChunkView, Envelope,
    Identity, Kind, OpenError, Opened, PublicIdentity, CHUNK, MAX_ENVELOPE,
};
use bombechat_modem::{split, Band, Event, Reassembler};
use bombechat_store::{
    Channel, Contact, ContactId, Delivery, Store, StoredBroadcast, StoredMessage,
};
use rand_core::{OsRng, RngCore};

pub use light::{LightTransport, ReaderWatch, PICTURES_HELD, SEND_WINDOW, STILL_MS};
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
/// Sound at [`SoundSwitch::Off`] and light at [`LightSwitch::Off`]: the
/// microphone or camera is closed, not merely idle, so this is reported
/// instead of [`LISTENING`].
const OFF: &str = "off";
/// Not a state a channel can be in: what the last report is taken to
/// have said once it no longer holds (a hint given way, a camera back),
/// so the next report says the state the channel is in.
const UNSAID: &str = "";

/// The light channel's state while a code is in view but cannot be read:
/// the screen is too bright, at too steep an angle or too far away.
/// Reported at most once in [`HINT_GAP_MS`]; it gives way to the
/// channel's state once a code reads, or once no unreadable code has been
/// in view for [`HINT_HOLD_MS`].
pub const TILT_OR_DIM: &str = "seeing a code: tilt or dim the screen";

/// The shortest time between two [`TILT_OR_DIM`] reports on a channel.
const HINT_GAP_MS: u64 = 5_000;

/// [`TILT_OR_DIM`] gives way this long after the last unreadable code.
const HINT_HOLD_MS: u64 = 3_000;

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

/// The longest text a broadcast can carry, in UTF-8 bytes: what is left
/// of an envelope after its header and the signed inner layout. Longer
/// is refused by [`Node::broadcast`]; the page is told this so it can
/// say so before asking.
pub const MAX_BROADCAST: usize = MAX_ENVELOPE - ENVELOPE_HEADER - INNER_OVERHEAD;

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

/// The Light switch on the front of the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightSwitch {
    /// The camera closed, nothing shown. Where the node starts.
    Off,
    /// Reading through the camera; never shows anything: no messages,
    /// receipts or pairing hellos go out by light.
    Read,
    /// Reading, and showing sends, receipts and hellos.
    ReadAndFlash,
}

impl LightSwitch {
    /// The switch by its position, as the settings page numbers it: 0 Off,
    /// 1 Read, 2 Read and flash.
    pub fn from_index(i: usize) -> Option<LightSwitch> {
        match i {
            0 => Some(LightSwitch::Off),
            1 => Some(LightSwitch::Read),
            2 => Some(LightSwitch::ReadAndFlash),
            _ => None,
        }
    }
}

/// The light channel's state when a send is refused at Read.
pub const READ_ONLY: &str = "not sent: light is set to read only";

/// The light channel's state when a send is refused with light Off.
pub const LIGHT_OFF: &str = "not sent: light is off";

/// How long pairing stays in progress at most, in ms: three minutes.
pub const PAIRING_CAP_MS: u64 = 180_000;

/// Returned by [`Node::start_pairing`] when there is no sound or light
/// transport at all to send the hello on.
pub const NOTHING_TO_SEND_ON: &str = "not sent: no sound or light to send on";

/// The channel state reported when a send asks for a channel this plan
/// does not build.
pub const NOT_IN_THIS_PLAN: &str = "unavailable: not in this plan";

/// What a self-test seals and sends ([`Node::start_self_test`]): short,
/// so it plays quickly, and in letters, so the drums spell it.
pub const SELF_TEST_TEXT: &str = "The machine hears itself.";

/// A self-test is given up as not heard this long, in ms, after its
/// envelope should have finished going out: from its first play, the
/// envelope's length at the channel's rate, and then this.
pub const SELF_TEST_MS: u64 = 30_000;

/// A self-test by sound still waiting for a quiet room this long, in ms,
/// is given up ([`SelfTestState::RoomNeverQuiet`]): two minutes.
pub const SELF_TEST_QUIET_MS: u64 = 120_000;

/// How a self-test is getting on ([`NodeEvent::SelfTest`]).
#[derive(Clone, Debug, PartialEq)]
pub enum SelfTestState {
    /// Queued: for sound, waiting for a quiet room like any send.
    Waiting,
    /// It went out under the event's tag: by sound, its first frame is
    /// playing; by light, it is on screen (shown again under a fresh tag
    /// each time its sending window ends, until it is heard).
    Playing,
    /// It came back and opened: `text` as sealed, `bytes` the whole
    /// envelope, heard `ms` after it began to play, on `band` (sound).
    /// `ms` is never less than the envelope takes to go out at the
    /// carrier's rate, and is `None` when the time was not measured (it
    /// opened before its carrier said it was playing), so no rate made of
    /// it is ever faster than the carrier.
    Heard {
        text: String,
        bytes: usize,
        ms: Option<u64>,
        band: Option<Band>,
    },
    /// It went out, and nothing came back in time ([`SELF_TEST_MS`]).
    NotHeard,
    /// By sound: the room never went quieter than Send under in
    /// [`SELF_TEST_QUIET_MS`], so it never played.
    RoomNeverQuiet,
    /// The carrier would not take it (a full queue, a speaker that would
    /// not play).
    NotPlayed,
    /// Ended before it was heard: [`Node::stop_self_test`], or the
    /// channel's switch turned away from sending.
    Stopped,
}

/// What the node reports from a [`Node::tick`].
pub enum NodeEvent {
    /// One chunk of a private message decrypted, in order. For the drums
    /// only: until the [`NodeEvent::Message`] that follows, the text is
    /// not known to come from the named sender. `channel` is the one it
    /// arrived on.
    ChunkLanded {
        tag: u16,
        index: u16,
        of: u16,
        view: ChunkView,
        channel: Channel,
    },
    /// A private message from a contact, verified and stored.
    Message(StoredMessage),
    /// A contact reports hearing a message this node sent.
    Heard { id: [u8; 16], by: ContactId },
    /// A signed public broadcast, heard and kept ([`Node::broadcasts`]).
    /// `verified` is true when the signer is a contact whose safety
    /// number has been checked; `hops` is how many carriers it came
    /// through; `at_min` is when it was kept, as messages are stamped.
    Broadcast {
        id: [u8; 16],
        signer: PublicIdentity,
        text: String,
        verified: bool,
        hops: u8,
        at_min: u32,
    },
    /// Someone nearby offered to pair. Nothing is stored until
    /// [`Node::confirm_pairing`].
    PairOffer {
        peer: PublicIdentity,
        name: String,
        safety: String,
        /// The channel the hello came in on.
        channel: Channel,
    },
    /// The room's loudness in approximate dB.
    RoomDb(f32),
    /// A channel changed state: "receiving", "listening", "failed", or
    /// e.g. "unavailable: microphone permission denied".
    ChannelState { channel: Channel, state: String },
    /// A message or broadcast this node sent, by envelope id, moved on.
    Outbox { id: [u8; 16], state: OutboxState },
    /// Light: put this code frame on screen now, in place of the last.
    /// `bits` is the whole grid, one bit a cell (dark 1), row-major from
    /// the top left, most significant bit first, as
    /// [`bombechat_light::grid_bits`] packs it; the grid is `cells` a side
    /// for preset `spec_id` (0 slow, 1 normal, 2 fast; see
    /// [`bombechat_light::CodeSpec::from_preset`]). `index` counts the
    /// frames of envelope `tag` shown so far. Public code bits only:
    /// they carry the sealed envelope, never a key.
    Display {
        tag: u16,
        bits: Vec<u8>,
        spec_id: u8,
        index: u32,
        /// The frame is of this machine's pairing hello, not a message.
        hello: bool,
        /// The frame is of this machine's self-test.
        test: bool,
    },
    /// Light: clear the screen; nothing is being shown. `tag` is the
    /// envelope whose frame was on it.
    DisplayStopped { tag: u16 },
    /// The self-test in progress moved on ([`Node::start_self_test`]).
    /// `tag` is the one its envelope goes out under (by light, the latest
    /// showing), so its chunks can be told from a message's.
    SelfTest {
        tag: u16,
        channel: Channel,
        state: SelfTestState,
    },
    /// Light: what the camera sees, a small grey picture `width` by
    /// `height` (one byte a pixel, row-major from the top left), about
    /// four times a second while it reads and [`Node::set_eyepiece`] is
    /// on. For the page's eyepiece only; never stored.
    Eyepiece {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
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
            NodeEvent::Display {
                tag,
                spec_id,
                index,
                ..
            } => f
                .debug_struct("Display")
                .field("tag", tag)
                .field("spec_id", spec_id)
                .field("index", index)
                .finish_non_exhaustive(),
            NodeEvent::DisplayStopped { tag } => {
                f.debug_struct("DisplayStopped").field("tag", tag).finish()
            }
            NodeEvent::SelfTest {
                tag,
                channel,
                state,
            } => f
                .debug_struct("SelfTest")
                .field("tag", tag)
                .field("channel", channel)
                .field("state", state)
                .finish(),
            NodeEvent::Eyepiece { width, height, .. } => f
                .debug_struct("Eyepiece")
                .field("width", width)
                .field("height", height)
                .finish_non_exhaustive(),
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

/// The longest text a private message to `recipients` people can carry,
/// in UTF-8 bytes: longer does not fit one envelope, and [`Node::send`]
/// refuses it. The page is told this for one recipient (a reply) so it
/// can say so before asking.
pub fn max_private_text(recipients: usize) -> usize {
    let mut len = MAX_ENVELOPE;
    while len > 0 && private_envelope_len(recipients, len) > MAX_ENVELOPE {
        len -= 1;
    }
    len
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

/// A pairing in progress: see [`Node::start_pairing`].
struct Pairing {
    hello: Envelope,
    /// When it started, by `now_ms`.
    since_ms: u64,
    /// The channel the hello goes out on.
    channel: Channel,
    /// The tag the hello is showing under by light, if it is.
    light_tag: Option<u16>,
    /// Their hello is in ([`Node::finish_pairing`]): the one on screen
    /// finishes its window and is not shown again.
    finishing: bool,
}

/// A self-test in progress: see [`Node::start_self_test`].
struct SelfTest {
    /// The envelope's id: this run's marker, held only here.
    id: [u8; 16],
    /// The envelope, to show again by light.
    envelope: Envelope,
    /// The tag it goes out under (by light, the latest showing).
    tag: u16,
    channel: Channel,
    /// When it was started, by `now_ms` (the last tick's).
    since_ms: u64,
    /// When it first went out, by `now_ms`.
    playing_ms: Option<u64>,
    /// How long the envelope takes to go out at the carrier's rate.
    airtime_ms: u64,
    /// How long after it first went out it is given up as not heard.
    limit_ms: u64,
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
    /// Of those, the ones a transport holds itself (light collects whole
    /// envelopes) until it hands their frames over or gives them up.
    held: HashSet<(usize, u16)>,
    /// Who has reported hearing each message still in the outbox.
    heard_by: HashMap<[u8; 16], HashSet<ContactId>>,
    /// Transports on which an arrival failed during this tick.
    failed: HashSet<usize>,
    /// The receiving/listening state last reported, by transport.
    reception: Vec<&'static str>,
    /// Whether [`TILT_OR_DIM`] is the state last reported, by transport.
    hinting: Vec<bool>,
    /// When [`TILT_OR_DIM`] was last reported, by transport.
    hinted_at: Vec<Option<u64>>,
    /// When a code was last in view and could not be read, by transport.
    unreadable_at: Vec<u64>,
    /// Envelopes that arrived and would not open, by (transport, tag):
    /// once handed over whole, their transport is asked to collect them
    /// afresh ([`Transport::rejected`]).
    unopened: HashSet<(usize, u16)>,
    /// Where the Sound switch is.
    sound: SoundSwitch,
    /// Where the Light switch is.
    light: LightSwitch,
    /// The pairing in progress, if any.
    pairing: Option<Pairing>,
    /// The self-test in progress, if any.
    self_test: Option<SelfTest>,
    /// The machine's clock for stamping stored messages: the wall minute
    /// read once, at the `now_ms` it was read, and moved on only by
    /// `now_ms` after that. See [`Node::stamp`].
    clock: Option<(u64, u32)>,
    /// The wall clock, in minutes since the Unix epoch.
    wall: fn() -> u32,
}

impl Node {
    /// A node over `transports`. With `store` `None` the node starts
    /// locked; see [`Node::unlock`].
    pub fn new(store: Option<Store>, transports: Vec<Box<dyn Transport>>) -> Node {
        let reassemblers = transports.iter().map(|_| Reassembler::new()).collect();
        let reception: Vec<&'static str> = transports.iter().map(|_| LISTENING).collect();
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
            held: HashSet::new(),
            heard_by: HashMap::new(),
            failed: HashSet::new(),
            hinting: vec![false; reception.len()],
            hinted_at: vec![None; reception.len()],
            unreadable_at: vec![0; reception.len()],
            unopened: HashSet::new(),
            reception,
            sound: SoundSwitch::ListenAndSend,
            light: LightSwitch::Off,
            pairing: None,
            self_test: None,
            clock: None,
            wall: now_min,
        };
        node.set_light(LightSwitch::Off);
        if let Some(store) = store {
            node.unlock(store);
        }
        node
    }

    /// Hand the node its opened store. Envelopes held while locked are
    /// opened on the next [`tick`](Node::tick). Messages that were still
    /// waiting to go out when the store was last open (messages, then
    /// broadcasts) are queued again under the same envelope, or marked
    /// failed if it has expired.
    pub fn unlock(&mut self, store: Store) {
        self.me = Some(store.me());
        let waiting = store.waiting();
        let notices = store.waiting_broadcasts();
        self.store = Some(store);
        let now = now_min();
        for (m, bytes) in waiting {
            match Envelope::decode(&bytes) {
                Ok(env) if env.id == m.id && env.expires_min > now => {
                    self.sent_to.insert(env.id, vec![m.contact]);
                    let tag = self.transmit(&env, &[m.channel]);
                    self.track(tag, env.id, m.channel);
                }
                _ => self.settle(m.id, OutboxState::Failed, None),
            }
        }
        for (b, bytes) in notices {
            match Envelope::decode(&bytes) {
                Ok(env) if env.id == b.id && env.expires_min > now => {
                    let tag = self.transmit(&env, &[b.channel]);
                    self.track(tag, env.id, b.channel);
                }
                _ => self.settle(b.id, OutboxState::Failed, None),
            }
        }
    }

    /// Set the Sound switch. Off, sound transports stop listening; at
    /// Listen and Off nothing more is queued on them (sends and
    /// broadcasts are refused, receipts and hellos are not sent) and what
    /// is already queued waits.
    ///
    /// A self-test by sound ends ([`SelfTestState::Stopped`]) when the
    /// switch leaves Listen and send.
    pub fn set_sound(&mut self, switch: SoundSwitch) {
        self.sound = switch;
        if switch != SoundSwitch::ListenAndSend {
            self.end_self_test_on(Channel::Sound);
        }
        for t in &mut self.transports {
            if t.name() == Channel::Sound {
                t.set_listening(switch != SoundSwitch::Off);
                t.set_sending(switch == SoundSwitch::ListenAndSend);
            }
        }
    }

    /// Set the Light switch. Off, light transports close the camera; at
    /// Read and Off nothing more is queued on them and nothing is shown:
    /// an envelope part way through showing stops at once and waits
    /// (reported [`OutboxState::Waiting`]), carrying on from where it
    /// stopped once the switch is back at Read and flash.
    ///
    /// Pairing by light ends when the switch leaves Read and flash: its
    /// hello stops, as by [`Node::stop_pairing`], rather than waiting.
    ///
    /// A self-test by light ends the same way.
    pub fn set_light(&mut self, switch: LightSwitch) {
        self.light = switch;
        if switch != LightSwitch::ReadAndFlash {
            self.end_self_test_on(Channel::Light);
        }
        if switch != LightSwitch::ReadAndFlash
            && self
                .pairing
                .as_ref()
                .is_some_and(|p| p.channel == Channel::Light)
        {
            self.stop_pairing();
        }
        for t in &mut self.transports {
            if t.name() == Channel::Light {
                t.set_listening(switch != LightSwitch::Off);
                t.set_sending(switch == LightSwitch::ReadAndFlash);
            }
        }
    }

    /// Whether light transports report what the camera sees
    /// ([`NodeEvent::Eyepiece`]) while it reads. Off until turned on: a
    /// page that hides its eyepiece is sent no picture at all.
    pub fn set_eyepiece(&mut self, on: bool) {
        for t in &mut self.transports {
            t.set_eyepiece(on);
        }
    }

    /// Show light codes in `spec` (the flash speed: slow, normal or fast)
    /// from the next envelope on. Not a preset: ignored.
    pub fn set_light_preset(&mut self, spec: bombechat_light::CodeSpec) {
        for t in &mut self.transports {
            t.set_preset(spec);
        }
    }

    /// Play sound only while the room is quieter than `db`.
    pub fn set_send_under(&mut self, db: f32) {
        for t in &mut self.transports {
            t.set_send_under(db);
        }
    }

    /// Play sound at `gain`, 0 (silent) to 1 (full scale).
    pub fn set_volume(&mut self, gain: f32) {
        for t in &mut self.transports {
            t.set_volume(gain);
        }
    }

    /// Play each sound frame again `repeats` times after its first play.
    pub fn set_repeats(&mut self, repeats: u8) {
        for t in &mut self.transports {
            t.set_repeats(repeats);
        }
    }

    /// Move the sound transports to `band` (or as near as each can go).
    /// A transport whose band changes gives up whatever was part way
    /// through arriving on it: that arrival is reported "failed" on the
    /// next tick, as one that stopped arriving is, so the page marks it
    /// as not checked.
    pub fn set_band(&mut self, band: Band) {
        for ti in 0..self.transports.len() {
            let before = self.transports[ti].band();
            self.transports[ti].set_band(band);
            if self.transports[ti].band() == before {
                continue;
            }
            self.reassemblers[ti] = Reassembler::new();
            self.progress.retain(|(t, _), _| *t != ti);
            let was_arriving = self.arriving.iter().any(|(t, _)| *t == ti);
            self.arriving.retain(|(t, _)| *t != ti);
            if was_arriving {
                self.failed.insert(ti);
            }
        }
    }

    /// The band the sound transport is on, if there is one.
    pub fn band(&self) -> Option<Band> {
        self.transports
            .iter()
            .filter(|t| t.name() == Channel::Sound)
            .find_map(|t| t.band())
    }

    /// Tell the store the machine's age, in ms of running time, to stamp
    /// on messages it stores from now on. See [`Store::set_age`].
    pub fn set_age(&self, age_ms: u64) {
        if let Some(store) = &self.store {
            store.set_age(age_ms);
        }
    }

    /// Erase stored messages kept longer than `keep_ms` of running time
    /// at the machine's age `age_ms`, except ones still waiting to go
    /// out. Returns how many went; none while locked.
    pub fn forget_older_than(&self, age_ms: u64, keep_ms: u64) -> usize {
        self.store
            .as_ref()
            .and_then(|s| s.forget_older_than(age_ms, keep_ms).ok())
            .unwrap_or(0)
    }

    /// The minute to stamp on a message stored now, by the machine's own
    /// clock: the wall clock as it read the first time this session
    /// stamped anything, moved on since only by the monotonic `now_ms`.
    /// A wall clock that jumps forward or back while the app runs moves
    /// no stamp, so stamps keep the order messages came in, and none
    /// is lifted by a jump into the next session (each session reads the
    /// wall clock afresh). It is for showing and ordering only: "Delete
    /// messages after" never reads it, but the machine's running age
    /// ([`Node::set_age`]).
    fn stamp(&mut self) -> u32 {
        let now_ms = self.now_ms;
        let wall = self.wall;
        let (at_ms, at_min) = *self.clock.get_or_insert_with(|| (now_ms, wall()));
        let elapsed = (now_ms.saturating_sub(at_ms) / 60_000).min(u64::from(u32::MAX)) as u32;
        at_min.saturating_add(elapsed)
    }

    /// Why nothing may be sent on `channel` right now, if so: the
    /// channel state to report.
    fn refusal(&self, channel: Channel) -> Option<&'static str> {
        match channel {
            Channel::Sound => match self.sound {
                SoundSwitch::Off => Some(SOUND_OFF),
                SoundSwitch::Listen => Some(LISTEN_ONLY),
                SoundSwitch::ListenAndSend => None,
            },
            Channel::Light => match self.light {
                LightSwitch::Off => Some(LIGHT_OFF),
                LightSwitch::Read => Some(READ_ONLY),
                LightSwitch::ReadAndFlash => None,
            },
            Channel::Bluetooth => Some(NOT_IN_THIS_PLAN),
        }
    }

    /// Whether `channel`'s switch has it closed: not hearing at all.
    fn closed(&self, channel: Channel) -> bool {
        (channel == Channel::Sound && self.sound == SoundSwitch::Off)
            || (channel == Channel::Light && self.light == LightSwitch::Off)
    }

    /// Report a send on `channel` refused by its switch: `why`, then the
    /// state the channel is in.
    fn report_refusal(&mut self, channel: Channel, why: &'static str) {
        self.pending.push(NodeEvent::ChannelState {
            channel,
            state: why.to_string(),
        });
        for ti in 0..self.transports.len() {
            if self.channel(ti) == channel {
                self.pending.push(NodeEvent::ChannelState {
                    channel,
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

    /// The broadcasts kept, sent and heard, oldest first, or none while
    /// locked.
    pub fn broadcasts(&self) -> Vec<StoredBroadcast> {
        self.store
            .as_ref()
            .map(Store::broadcasts)
            .unwrap_or_default()
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
    /// [`Channel::Sound`] and [`Channel::Light`] are built in this plan.
    /// [`Channel::Bluetooth`] sends nothing, returns [`NOT_SENT`] and
    /// reports the channel as [`NOT_IN_THIS_PLAN`] on the next tick.
    ///
    /// With the channel's switch not allowing sends (Sound at Listen or
    /// Off, Light at Read or Off) the message is sealed and stored but not
    /// sent: it is reported [`OutboxState::Failed`] and the channel as
    /// [`LISTEN_ONLY`], [`SOUND_OFF`], [`READ_ONLY`] or [`LIGHT_OFF`] on
    /// the next tick.
    ///
    /// A message to several contacts is one envelope; the store keeps one
    /// row per message id, so it is filed under the first of them.
    pub fn send(&mut self, to: &[ContactId], text: &str, channel: Channel) -> [u8; 16] {
        if channel == Channel::Bluetooth {
            self.pending.push(NodeEvent::ChannelState {
                channel,
                state: NOT_IN_THIS_PLAN.to_string(),
            });
            return NOT_SENT;
        }
        if self.me.is_none() || self.store.is_none() {
            return NOT_SENT;
        }
        let at_min = self.stamp();
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
        let refused = self.refusal(channel);
        let mut stored = StoredMessage {
            id: env.id,
            contact: recipients[0].id,
            outgoing: true,
            text: text.to_string(),
            channel,
            at_min,
            heard: false,
            delivery: Delivery::Waiting,
        };
        if let Some(why) = refused {
            stored.delivery = Delivery::Failed;
            if store.put_message(&stored).is_err() {
                return NOT_SENT;
            }
            self.report_refusal(channel, why);
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
        let tag = self.transmit(&env, &[channel]);
        self.track(tag, env.id, channel);
        env.id
    }

    /// Sign `text` as a public broadcast, keep it ([`Node::broadcasts`];
    /// while waiting, with its envelope and channel, so an unlock queues
    /// it again the same way) and send it over `channel`. Returns its envelope id, or [`NOT_SENT`] if locked or
    /// the text is longer than [`MAX_BROADCAST`]. Refused by the
    /// channel's switch as [`Node::send`] is, and on
    /// [`Channel::Bluetooth`] likewise.
    pub fn broadcast(&mut self, text: &str, channel: Channel) -> [u8; 16] {
        if channel == Channel::Bluetooth {
            self.pending.push(NodeEvent::ChannelState {
                channel,
                state: NOT_IN_THIS_PLAN.to_string(),
            });
            return NOT_SENT;
        }
        if self.me.is_none() || self.store.is_none() || text.len() > MAX_BROADCAST {
            return NOT_SENT;
        }
        let at_min = self.stamp();
        let (Some(me), Some(store)) = (&self.me, &self.store) else {
            return NOT_SENT;
        };
        let env = seal_public(me, text, now_min(), TTL_MIN);
        let refused = self.refusal(channel);
        let mut kept = StoredBroadcast {
            id: env.id,
            outgoing: true,
            signer: me.public().sign,
            verified: true,
            hops: 0,
            text: text.to_string(),
            at_min,
            delivery: Delivery::Waiting,
            channel,
        };
        if refused.is_some() {
            kept.delivery = Delivery::Failed;
        }
        if store.put_broadcast(&kept, Some(&env.encode())).is_err() {
            return NOT_SENT;
        }
        if let Some(why) = refused {
            self.report_refusal(channel, why);
            self.pending.push(NodeEvent::Outbox {
                id: env.id,
                state: OutboxState::Failed,
            });
            return env.id;
        }
        let tag = self.transmit(&env, &[channel]);
        self.track(tag, env.id, channel);
        env.id
    }

    /// Put a send on `channel` in the outbox: reported waiting on the next
    /// tick, then as its transport reports its tag playing and sent.
    fn track(&mut self, tag: u16, id: [u8; 16], channel: Channel) {
        if !self.transports.iter().any(|t| t.name() == channel) {
            self.settle(id, OutboxState::Failed, None);
        } else {
            self.outbox.insert(tag, id);
            self.pending.push(NodeEvent::Outbox {
                id,
                state: OutboxState::Waiting,
            });
        }
    }

    /// Announce this device for pairing on `channel` (sound or light): a
    /// signed hello carrying its identity and `my_name` (cut to 64 bytes).
    /// Anyone who hears it gets a [`NodeEvent::PairOffer`]. Refused with
    /// "locked" before [`Node::unlock`]; with that channel's state unless
    /// its switch allows sending (light at Off or Read, sound at Off or
    /// Listen: a switch is never turned for pairing, so the camera and the
    /// screen are only ever as the Light switch has them); and with
    /// [`NOTHING_TO_SEND_ON`] when there is no transport for it.
    ///
    /// Pairing is then in progress until [`Node::stop_pairing`],
    /// [`Node::finish_pairing`], [`Node::confirm_pairing`], the Light
    /// switch leaving Read and flash (pairing by light), or
    /// [`PAIRING_CAP_MS`] later, whichever comes first. While it is, the
    /// hello keeps showing by light (each time its sending window ends it
    /// goes on screen again, and [`NodeEvent::Display`] says `hello`);
    /// sound plays it once. Starting again ends the pairing in progress
    /// first.
    pub fn start_pairing(&mut self, my_name: &str, channel: Channel) -> Result<(), &'static str> {
        let Some(me) = &self.me else {
            return Err("locked");
        };
        let env = control::pair_hello(me, my_name, random_id(), now_min(), TTL_MIN);
        let carried = matches!(channel, Channel::Sound | Channel::Light)
            && self.transports.iter().any(|t| t.name() == channel);
        if !carried {
            return Err(NOTHING_TO_SEND_ON);
        }
        if let Some(why) = self.refusal(channel) {
            self.report_refusal(channel, why);
            return Err(why);
        }
        self.stop_pairing();
        let tag = self.transmit(&env, &[channel]);
        self.pairing = Some(Pairing {
            since_ms: self.now_ms,
            channel,
            light_tag: (channel == Channel::Light).then_some(tag),
            finishing: false,
            hello: env,
        });
        Ok(())
    }

    /// End the pairing in progress, if any: the hello stops showing.
    pub fn stop_pairing(&mut self) {
        let Some(pairing) = self.pairing.take() else {
            return;
        };
        if let Some(tag) = pairing.light_tag {
            for t in &mut self.transports {
                if t.name() == Channel::Light {
                    t.delivered(tag);
                }
            }
        }
    }

    /// Their hello is in: pairing ends once the hello on screen has shown
    /// to the end of its sending window, so the other person, who may
    /// have started later, still gets a whole window to read it. With no
    /// hello showing by light (sound plays it once), it ends now.
    pub fn finish_pairing(&mut self) {
        match self.pairing.as_mut() {
            Some(pairing) if pairing.light_tag.is_some() => pairing.finishing = true,
            _ => self.stop_pairing(),
        }
    }

    /// Whether pairing is in progress (see [`Node::start_pairing`]).
    pub fn pairing(&self) -> bool {
        self.pairing.is_some()
    }

    /// The light transport finished showing the hello under `tag`: show
    /// it again under a fresh tag while pairing is in progress, unless it
    /// was dropped rather than shown.
    fn hello_shown(&mut self, tag: u16, dropped: bool) {
        let Some(pairing) = self.pairing.as_mut() else {
            return;
        };
        if pairing.light_tag != Some(tag) {
            return;
        }
        pairing.light_tag = None;
        if pairing.finishing {
            self.pairing = None;
            return;
        }
        let hello = pairing.hello.clone();
        if dropped || self.refusal(Channel::Light).is_some() {
            return;
        }
        let tag = self.transmit(&hello, &[Channel::Light]);
        if let Some(pairing) = self.pairing.as_mut() {
            pairing.light_tag = Some(tag);
        }
    }

    /// The light transport finished showing the self-test under `tag`:
    /// while the test is in progress, show it again under a fresh tag, as
    /// a hello is, so whoever holds the mirror has time to.
    fn self_test_shown(&mut self, tag: u16) {
        let Some(test) = self.self_test.as_ref() else {
            return;
        };
        if test.channel != Channel::Light
            || test.tag != tag
            || self.refusal(Channel::Light).is_some()
        {
            return;
        }
        let envelope = test.envelope.clone();
        let tag = self.put_on_air(&envelope, &[Channel::Light]);
        if let Some(test) = self.self_test.as_mut() {
            test.tag = tag;
        }
    }

    /// Store `peer` as a contact named `name`, marked checked (the user
    /// has compared safety numbers). Returns the contact's id: the
    /// existing one if `peer` is already a contact, or [`NOT_STORED`] if
    /// locked, if `peer`'s key cannot be sealed to, or if the store fails.
    ///
    /// Ends the pairing in progress, if any.
    pub fn confirm_pairing(&mut self, peer: &PublicIdentity, name: &str) -> ContactId {
        self.stop_pairing();
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

    /// Test the machine on `channel` (sound or light): seal
    /// [`SELF_TEST_TEXT`] to this machine's own key and send it, so it is
    /// heard by this machine's own microphone (or, by light, its camera
    /// reading the screen in a mirror) and opened as any message is. By
    /// sound it waits for a quiet room like any send; by light it is
    /// shown again each time its sending window ends. Reported as
    /// [`NodeEvent::SelfTest`]: waiting, playing, then heard, or not
    /// heard once [`SELF_TEST_MS`] have passed after it should have
    /// finished going out. It is never stored, never in the outbox, never
    /// answered with a receipt, and nothing else this machine sends is
    /// heard because of it.
    ///
    /// Refused as [`Node::start_pairing`] is: "locked"; the channel's
    /// state unless its switch allows sending (no switch is turned for
    /// it); [`NOTHING_TO_SEND_ON`] without a transport for it. Starting
    /// again ends the test in progress first.
    pub fn start_self_test(&mut self, channel: Channel) -> Result<(), &'static str> {
        if self.me.is_none() {
            return Err("locked");
        }
        let carried = matches!(channel, Channel::Sound | Channel::Light)
            && self.transports.iter().any(|t| t.name() == channel);
        if !carried {
            return Err(NOTHING_TO_SEND_ON);
        }
        if let Some(why) = self.refusal(channel) {
            self.report_refusal(channel, why);
            return Err(why);
        }
        self.stop_self_test();
        let me = self.me.as_ref().expect("checked above");
        let envelope = seal_private(me, &[me.public()], SELF_TEST_TEXT, now_min(), TTL_MIN);
        // the time the envelope takes to go out at the channel's rate
        let bytes = envelope.encode().len();
        let airtime_ms = self
            .transports
            .iter()
            .filter(|t| t.name() == channel)
            .find_map(|t| t.rate())
            .filter(|r| *r > 0.0)
            .map_or(0, |r| (bytes as f32 * 1000.0 / r).ceil() as u64);
        let tag = self.put_on_air(&envelope, &[channel]);
        self.self_test = Some(SelfTest {
            id: envelope.id,
            envelope,
            tag,
            channel,
            since_ms: self.now_ms,
            playing_ms: None,
            airtime_ms,
            limit_ms: airtime_ms + SELF_TEST_MS,
        });
        self.pending.push(NodeEvent::SelfTest {
            tag,
            channel,
            state: SelfTestState::Waiting,
        });
        Ok(())
    }

    /// End the self-test in progress, if any, as
    /// [`SelfTestState::Stopped`]: what is still queued of it does not go
    /// out, and what is still arriving of it is let go.
    pub fn stop_self_test(&mut self) {
        self.end_self_test(SelfTestState::Stopped, None);
    }

    /// Whether a self-test is in progress (see [`Node::start_self_test`]).
    pub fn self_testing(&self) -> bool {
        self.self_test.is_some()
    }

    /// The self-test in progress, if it is on `channel`, ends as stopped.
    fn end_self_test_on(&mut self, channel: Channel) {
        if self
            .self_test
            .as_ref()
            .is_some_and(|t| t.channel == channel)
        {
            self.stop_self_test();
        }
    }

    /// End the self-test in progress as `state`, reported now into
    /// `events` or else on the next tick. Its id is remembered as
    /// handled, so it is heard at most once; what is still queued of it
    /// is withdrawn, and an arrival of it part way through is let go.
    fn end_self_test(&mut self, state: SelfTestState, events: Option<&mut Vec<NodeEvent>>) {
        let Some(test) = self.self_test.take() else {
            return;
        };
        self.mark_seen(test.id);
        for progress in self.progress.values_mut() {
            if matches!(progress, Progress::Opening { id, .. } if *id == test.id) {
                *progress = Progress::Settled;
            }
        }
        for t in &mut self.transports {
            if t.name() == test.channel {
                t.withdraw(test.tag);
            }
        }
        let event = NodeEvent::SelfTest {
            tag: test.tag,
            channel: test.channel,
            state,
        };
        match events {
            Some(events) => events.push(event),
            None => self.pending.push(event),
        }
    }

    /// Poll every transport and handle what they heard. `now_ms` is a
    /// monotonic millisecond clock.
    pub fn tick(&mut self, now_ms: u64) -> Vec<NodeEvent> {
        let mut events = std::mem::take(&mut self.pending);
        self.now_ms = now_ms;
        self.receipts_sent
            .retain(|_, at| now_ms.saturating_sub(*at) < RECEIPT_GAP_MS);
        if self
            .pairing
            .as_ref()
            .is_some_and(|p| now_ms.saturating_sub(p.since_ms) >= PAIRING_CAP_MS)
        {
            self.stop_pairing();
        }

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
                        if self.closed(channel) {
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
                                    self.held.remove(&(ti, tag));
                                    self.on_complete(ti, tag, bytes, &mut events)
                                }
                                Event::FrameBad => {}
                            }
                        }
                    }
                    TransportEvent::Playing(tag) => {
                        if let Some(test) = self.self_test.as_mut().filter(|t| t.tag == tag) {
                            test.playing_ms.get_or_insert(now_ms);
                            events.push(NodeEvent::SelfTest {
                                tag,
                                channel: test.channel,
                                state: SelfTestState::Playing,
                            });
                        }
                        if let Some(id) = self.outbox.get(&tag) {
                            events.push(NodeEvent::Outbox {
                                id: *id,
                                state: OutboxState::Playing,
                            });
                        }
                    }
                    TransportEvent::Sent(tag) => {
                        if channel == Channel::Light {
                            self.hello_shown(tag, false);
                            self.self_test_shown(tag);
                        }
                        if let Some(id) = self.outbox.remove(&tag) {
                            self.heard_by.remove(&id);
                            self.settle(id, OutboxState::Sent, Some(&mut events));
                        }
                    }
                    TransportEvent::Dropped(tag) => {
                        if channel == Channel::Light {
                            self.hello_shown(tag, true);
                        }
                        if self.self_test.as_ref().is_some_and(|t| t.tag == tag) {
                            self.end_self_test(SelfTestState::NotPlayed, Some(&mut events));
                        }
                        if let Some(id) = self.outbox.remove(&tag) {
                            self.heard_by.remove(&id);
                            self.settle(id, OutboxState::Failed, Some(&mut events));
                        }
                    }
                    TransportEvent::Waiting(tag) => {
                        if let Some(id) = self.outbox.get(&tag) {
                            events.push(NodeEvent::Outbox {
                                id: *id,
                                state: OutboxState::Waiting,
                            });
                        }
                    }
                    TransportEvent::Arriving(tag) => {
                        if !self.closed(channel) {
                            self.arriving.insert((ti, tag));
                            self.held.insert((ti, tag));
                        }
                    }
                    TransportEvent::Abandoned(tag) => {
                        self.held.remove(&(ti, tag));
                    }
                    TransportEvent::Display {
                        tag,
                        index,
                        spec,
                        bits,
                    } => events.push(NodeEvent::Display {
                        hello: self
                            .pairing
                            .as_ref()
                            .is_some_and(|p| p.light_tag == Some(tag)),
                        test: self
                            .self_test
                            .as_ref()
                            .is_some_and(|t| t.channel == Channel::Light && t.tag == tag),
                        tag,
                        bits,
                        spec_id: spec,
                        index,
                    }),
                    TransportEvent::DisplayStopped { tag } => {
                        events.push(NodeEvent::DisplayStopped { tag })
                    }
                    TransportEvent::Eyepiece {
                        width,
                        height,
                        pixels,
                    } => {
                        if !self.closed(channel) {
                            events.push(NodeEvent::Eyepiece {
                                width,
                                height,
                                pixels,
                            })
                        }
                    }
                    TransportEvent::CodeUnreadable => self.unreadable(ti, now_ms, &mut events),
                    TransportEvent::CodeRead => {
                        if self.hinting[ti] {
                            self.hinting[ti] = false;
                            self.reception[ti] = UNSAID;
                        }
                    }
                    TransportEvent::RoomDb(db) => events.push(NodeEvent::RoomDb(db)),
                    TransportEvent::Unavailable(reason) => events.push(NodeEvent::ChannelState {
                        channel,
                        state: format!("unavailable: {reason}"),
                    }),
                    TransportEvent::Recovered => self.reception[ti] = UNSAID,
                    TransportEvent::Notice(state) => {
                        events.push(NodeEvent::ChannelState { channel, state })
                    }
                }
            }
            self.reassemblers[ti].expire(now_ms);
        }

        if self.self_test.as_ref().is_some_and(|t| {
            t.playing_ms
                .is_some_and(|at| now_ms.saturating_sub(at) >= t.limit_ms)
        }) {
            self.end_self_test(SelfTestState::NotHeard, Some(&mut events));
        }
        if self.self_test.as_ref().is_some_and(|t| {
            t.channel == Channel::Sound
                && t.playing_ms.is_none()
                && now_ms.saturating_sub(t.since_ms) >= SELF_TEST_QUIET_MS
        }) {
            self.end_self_test(SelfTestState::RoomNeverQuiet, Some(&mut events));
        }

        let reassemblers = &self.reassemblers;
        self.progress
            .retain(|(ti, tag), _| reassemblers[*ti].prefix(*tag).is_some());
        self.unopened
            .retain(|(ti, tag)| reassemblers[*ti].prefix(*tag).is_some());
        // An arrival the reassembler (or the transport holding it) gave up
        // on was abandoned.
        let failed = &mut self.failed;
        let held = &self.held;
        self.arriving.retain(|(ti, tag)| {
            let alive = reassemblers[*ti].prefix(*tag).is_some() || held.contains(&(*ti, *tag));
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
    /// [`SoundSwitch::Off`] and light at [`LightSwitch::Off`] report
    /// [`OFF`], never [`LISTENING`] or [`RECEIVING`]: the microphone or
    /// camera is closed, so nothing can arrive on it either.
    fn report_reception(&mut self, events: &mut Vec<NodeEvent>) {
        for ti in 0..self.transports.len() {
            let channel = self.channel(ti);
            let failed = self.failed.remove(&ti);
            if self.hinting[ti] {
                let stale = self.now_ms.saturating_sub(self.unreadable_at[ti]) >= HINT_HOLD_MS;
                if failed || stale || self.closed(channel) {
                    self.hinting[ti] = false;
                    self.reception[ti] = UNSAID;
                } else {
                    continue;
                }
            }
            if failed {
                events.push(NodeEvent::ChannelState {
                    channel,
                    state: "failed".to_string(),
                });
            }
            let now = if self.closed(channel) {
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
    /// transport of the given channels. Remembers its id so this node
    /// ignores its own echo. Returns the tag.
    fn transmit(&mut self, env: &Envelope, channels: &[Channel]) -> u16 {
        self.mark_seen(env.id);
        self.put_on_air(env, channels)
    }

    /// Split `env` under a fresh random tag and queue it on every
    /// transport of the given channels, without remembering its id: only
    /// the self-test is sent this way, to be heard. Returns the tag.
    fn put_on_air(&mut self, env: &Envelope, channels: &[Channel]) -> u16 {
        // a tag no send in the outbox (nor the self-test) has: two envelopes on one tag would share a queue slot in
        // the transport and one outbox entry, so a refusal or a sent report would land on the wrong message
        let tag = loop {
            let tag = OsRng.next_u32() as u16;
            if !self.outbox.contains_key(&tag)
                && self.self_test.as_ref().is_none_or(|t| t.tag != tag)
            {
                break tag;
            }
        };
        let frames = split(&env.encode(), tag);
        for t in &mut self.transports {
            if !channels.contains(&t.name()) {
                continue;
            }
            for f in &frames {
                t.send(f.clone());
            }
        }
        tag
    }

    /// Remember `id` as handled. Never the live self-test's id: anyone may
    /// put a notice or a control envelope under it (the id is chosen by
    /// whoever seals, and a control envelope's outer id is not even
    /// signed), and that must not keep the test out. The test's own end
    /// remembers it, once the test is over.
    fn mark_seen(&mut self, id: [u8; 16]) {
        if self.self_test.as_ref().is_some_and(|t| t.id == id) {
            return;
        }
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
        match Envelope::decode(&bytes) {
            Ok(env) => match env.kind {
                Kind::Private => self.advance(ti, tag, &bytes, events),
                Kind::Public => self.on_public(ti, tag, &env, events),
                Kind::Control => self.on_control(ti, tag, &env, events),
            },
            Err(_) => {
                self.failed.insert(ti);
                self.unopened.insert((ti, tag));
            }
        }
        self.progress.remove(&(ti, tag));
        if self.unopened.remove(&(ti, tag)) && self.transports[ti].rejected(tag) {
            self.reassemblers[ti].forget(tag);
        }
    }

    /// A code was in view on transport `ti` and could not be read: say
    /// [`TILT_OR_DIM`], unless it was said less than [`HINT_GAP_MS`] ago.
    fn unreadable(&mut self, ti: usize, now_ms: u64, events: &mut Vec<NodeEvent>) {
        self.unreadable_at[ti] = now_ms;
        let channel = self.channel(ti);
        let recent = self.hinted_at[ti].is_some_and(|at| now_ms.saturating_sub(at) < HINT_GAP_MS);
        if self.hinting[ti] || recent || self.closed(channel) {
            return;
        }
        self.hinting[ti] = true;
        self.hinted_at[ti] = Some(now_ms);
        events.push(NodeEvent::ChannelState {
            channel,
            state: TILT_OR_DIM.to_string(),
        });
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
                    self.unopened.insert(key);
                    return;
                }
                Some(Ok(layout)) => layout,
            };
            // the live self-test's id is never taken as handled: only its
            // own opening, from this machine, settles it (self_tested)
            let testing = self.self_test.as_ref().is_some_and(|t| t.id == layout.id);
            if !testing && self.already_have(&layout.id) {
                self.resend_receipt(layout.id, self.channel(ti));
                self.progress.insert(key, Progress::Settled);
                return;
            }
            let me = self.me.as_ref().expect("checked by callers");
            let fresh = layout.expires_min >= now_min().saturating_sub(SKEW_MIN);
            let started = fresh.then(|| {
                ChunkOpener::start(
                    me,
                    layout.id,
                    layout.expires_min,
                    &bytes[layout.header.clone()],
                )
            });
            let opener = match started {
                Some(Ok(opener)) => Some(opener),
                // Damaged or tampered with: worth collecting again. Not
                // sealed to this machine: it never will open here.
                Some(Err(OpenError::Tampered | OpenError::BadSignature | OpenError::Malformed)) => {
                    self.unopened.insert(key);
                    None
                }
                Some(Err(_)) | None => None,
            };
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
        let channel = self.transports.get(ti).map_or(Channel::Sound, |t| t.name());
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
                    channel,
                }),
                Err(_) => {
                    *progress = Progress::Settled;
                    self.failed.insert(ti);
                    self.unopened.insert(key);
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
                self.unopened.insert(key);
            }
        }
    }

    /// Store and announce a verified private message, and send a receipt.
    fn deliver(&mut self, ti: usize, id: [u8; 16], opened: Opened, events: &mut Vec<NodeEvent>) {
        if self.self_test.as_ref().is_some_and(|t| t.id == id) {
            self.self_tested(opened, events);
            return;
        }
        self.mark_seen(id);
        let Some(contact) = self.contact_for(&opened.sender) else {
            return;
        };
        let now = self.stamp();
        let store = self.store.as_ref().expect("unlocked");
        if store.has_message(&id) {
            return;
        }
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
        self.send_receipt(id, &contact, self.channel(ti));
    }

    /// The self-test's envelope opened. It is the test only if this
    /// machine signed it: one sealed to this machine under the test's id by
    /// anyone else is dropped without counting the id as handled, so it
    /// cannot keep the real one out. Not stored, no receipt.
    fn self_tested(&mut self, opened: Opened, events: &mut Vec<NodeEvent>) {
        let mine = self.me.as_ref().map(Identity::public);
        let Some(test) = self.self_test.as_ref() else {
            return;
        };
        if mine.as_ref() != Some(&opened.sender) {
            return;
        }
        let state = SelfTestState::Heard {
            text: opened.text,
            bytes: test.envelope.encode().len(),
            // never sooner than it takes to go out at the carrier's rate:
            // a carrier that reports playing late cannot make it look
            // faster than the modem is
            ms: test
                .playing_ms
                .map(|at| self.now_ms.saturating_sub(at).max(test.airtime_ms))
                .filter(|ms| *ms > 0),
            band: (test.channel == Channel::Sound)
                .then(|| self.band())
                .flatten(),
        };
        self.end_self_test(state, Some(events));
    }

    /// A private envelope we already have arrived again, on `channel`. If
    /// it is a message a contact sent us, they may have missed our
    /// receipt: send another.
    fn resend_receipt(&mut self, id: [u8; 16], channel: Channel) {
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
            self.send_receipt(id, &contact, channel);
        }
    }

    /// Send `contact` a receipt for message `id`, which came in on
    /// `came_on`, if receipts are on, a switch allows sending and none
    /// went out for it in the last [`RECEIPT_GAP_MS`]. It goes back on
    /// `came_on` if that channel may send and has a transport, else on the
    /// first other channel that may.
    fn send_receipt(&mut self, id: [u8; 16], contact: &Contact, came_on: Channel) {
        let usable =
            |c: Channel| self.refusal(c).is_none() && self.transports.iter().any(|t| t.name() == c);
        let Some(channel) = [came_on, Channel::Sound, Channel::Light]
            .into_iter()
            .find(|c| usable(*c))
        else {
            return;
        };
        if !self.receipts || low_order(&contact.who.dh) {
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
        self.transmit(&receipt, &[channel]);
    }

    /// `by` reports hearing message `id`. Once everyone it was sent to
    /// has, a transport still sending it (light) can stop.
    fn heard(&mut self, id: [u8; 16], by: ContactId) {
        let Some(tag) = self
            .outbox
            .iter()
            .find(|(_, v)| **v == id)
            .map(|(tag, _)| *tag)
        else {
            return;
        };
        let heard = self.heard_by.entry(id).or_default();
        heard.insert(by);
        let everyone = self
            .sent_to
            .get(&id)
            .is_none_or(|to| to.iter().all(|c| heard.contains(c)));
        if everyone {
            self.heard_by.remove(&id);
            for t in &mut self.transports {
                t.delivered(tag);
            }
        }
    }

    fn on_public(&mut self, ti: usize, tag: u16, env: &Envelope, events: &mut Vec<NodeEvent>) {
        if self.already_have(&env.id) {
            return;
        }
        // Marked seen only once the signature covering the id verifies, so
        // garbage sent under a stolen id cannot suppress the real envelope.
        let Ok((signer, text, _)) = open_public(env, now_min()) else {
            self.failed.insert(ti);
            self.unopened.insert((ti, tag));
            return;
        };
        self.mark_seen(env.id);
        let me = self.me.as_ref().expect("unlocked");
        if signer == me.public() || text.starts_with(control::HELLO_PREFIX) {
            return;
        }
        let verified = self.contact_for(&signer).is_some_and(|c| c.checked);
        let at_min = self.stamp();
        let kept = StoredBroadcast {
            id: env.id,
            outgoing: false,
            signer: signer.sign,
            verified,
            hops: env.hops,
            text: text.clone(),
            at_min,
            delivery: Delivery::Sent,
            channel: self.channel(ti),
        };
        // kept once: already_have, above, finds it in the store next time
        let _ = self
            .store
            .as_ref()
            .expect("unlocked")
            .put_broadcast(&kept, None);
        events.push(NodeEvent::Broadcast {
            id: env.id,
            signer,
            text,
            verified,
            hops: env.hops,
            at_min,
        });
    }

    fn on_control(&mut self, ti: usize, tag: u16, env: &Envelope, events: &mut Vec<NodeEvent>) {
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
                self.unopened.insert((ti, tag));
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
                    channel: self.channel(ti),
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
                    self.heard(id, contact.id);
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
        /// The band it was last moved to, if ever.
        band: Arc<Mutex<Option<Band>>>,
        /// Tags it was told were delivered.
        delivered: Arc<Mutex<Vec<u16>>>,
        /// Tags it was told would not open.
        rejected: Arc<Mutex<Vec<u16>>>,
        /// Light, not sound.
        light: bool,
    }

    impl Transport for Wire {
        fn name(&self) -> Channel {
            if self.light {
                Channel::Light
            } else {
                Channel::Sound
            }
        }
        fn delivered(&mut self, tag: u16) {
            self.delivered.lock().unwrap().push(tag);
        }
        /// Light collects afresh, as the light transport does.
        fn rejected(&mut self, tag: u16) -> bool {
            self.rejected.lock().unwrap().push(tag);
            self.light
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
        fn set_band(&mut self, band: Band) {
            *self.band.lock().unwrap() = Some(band);
        }
        fn band(&self) -> Option<Band> {
            *self.band.lock().unwrap()
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
        assert_eq!(node.broadcast(&"x".repeat(4000), Channel::Sound), NOT_SENT);
        assert_eq!(
            node.broadcast(&"x".repeat(MAX_BROADCAST + 1), Channel::Sound),
            NOT_SENT
        );
        assert!(wire.sent.lock().unwrap().is_empty());

        let longest = (0..4000)
            .rev()
            .find(|n| private_envelope_len(1, *n) <= MAX_ENVELOPE)
            .unwrap();
        assert_ne!(
            node.send(&ids, &"x".repeat(longest), Channel::Sound),
            NOT_SENT
        );
        // the longest broadcast the page is told of still fits in an envelope
        assert_ne!(
            node.broadcast(&"x".repeat(MAX_BROADCAST), Channel::Sound),
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

    /// A node for `me` knowing `contact`, with a sound wire and a light
    /// wire, both switches allowing sends.
    fn two_channel_node(
        dir: &tempfile::TempDir,
        me: &Identity,
        contact: &Identity,
    ) -> (Node, Wire, Wire, ContactId) {
        let store = Store::create(&dir.path().join("two.db"), "pass", me, "me").unwrap();
        let id = store.add_contact("c", &contact.public(), true).unwrap();
        let (sound, light) = (
            Wire::default(),
            Wire {
                light: true,
                ..Wire::default()
            },
        );
        let mut node = Node::new(
            Some(store),
            vec![Box::new(sound.clone()), Box::new(light.clone())],
        );
        node.set_light(LightSwitch::ReadAndFlash);
        (node, sound, light, id)
    }

    #[test]
    fn a_message_goes_only_on_its_own_channel() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, sound, light, b_id) = two_channel_node(&dir, &a, &b);
        let id = node.send(&[b_id], "by light", Channel::Light);
        assert_eq!(light.envelopes().len(), 1);
        assert!(sound.sent.lock().unwrap().is_empty());
        let tag = light.last_tag();
        node.tick(1);

        // The light transport reports it playing, then sent.
        light
            .news
            .lock()
            .unwrap()
            .push(TransportEvent::Playing(tag));
        assert_eq!(outbox(&node.tick(2)), vec![(id, OutboxState::Playing)]);
        light
            .news
            .lock()
            .unwrap()
            .push(TransportEvent::Waiting(tag));
        assert_eq!(outbox(&node.tick(3)), vec![(id, OutboxState::Waiting)]);
        light.news.lock().unwrap().push(TransportEvent::Sent(tag));
        assert_eq!(outbox(&node.tick(4)), vec![(id, OutboxState::Sent)]);
        let kept = node.conversation(b_id);
        assert_eq!(kept.last().unwrap().channel, Channel::Light);
        assert_eq!(kept.last().unwrap().delivery, Delivery::Sent);
    }

    #[test]
    fn a_receipt_goes_back_the_way_its_message_came_when_it_can() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, sound, light, _) = two_channel_node(&dir, &b, &a);

        light.hear(&seal_private(&a, &[b.public()], "one", now_min(), 60), 1);
        let events = node.tick(1);
        assert_eq!(messages(&events), vec!["one"]);
        assert_eq!(node.conversation(ContactId(1))[0].channel, Channel::Light);
        assert_eq!(light.envelopes().len(), 1, "receipt by light");
        assert!(sound.sent.lock().unwrap().is_empty());

        sound.hear(&seal_private(&a, &[b.public()], "two", now_min(), 60), 2);
        assert_eq!(messages(&node.tick(2)), vec!["two"]);
        assert_eq!(sound.envelopes().len(), 1, "receipt by sound");
        assert_eq!(light.envelopes().len(), 1);

        // Light may not flash: a message that came by light is answered
        // by sound instead.
        node.set_light(LightSwitch::Read);
        light.hear(&seal_private(&a, &[b.public()], "three", now_min(), 60), 3);
        assert_eq!(messages(&node.tick(3)), vec!["three"]);
        assert_eq!(sound.envelopes().len(), 2);
        assert_eq!(light.envelopes().len(), 1);

        // Neither may send: no receipt at all.
        node.set_sound(SoundSwitch::Listen);
        light.hear(&seal_private(&a, &[b.public()], "four", now_min(), 60), 4);
        assert_eq!(messages(&node.tick(4)), vec!["four"]);
        assert_eq!((sound.envelopes().len(), light.envelopes().len()), (2, 1));
    }

    #[test]
    fn a_receipt_from_everyone_tells_the_carrier_it_was_delivered() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b, c) = (
            Identity::generate(),
            Identity::generate(),
            Identity::generate(),
        );
        let (mut node, wire, ids) = node(&dir, &a, &[&b, &c]);
        let id = node.send(&ids, "for both", Channel::Sound);
        let tag = wire.last_tag();
        node.tick(1);

        let from_b = control::heard(&b, &a.public(), &id, random_id(), now_min(), 60);
        wire.hear(&from_b, 100);
        node.tick(2);
        assert!(wire.delivered.lock().unwrap().is_empty(), "one of two");

        let from_c = control::heard(&c, &a.public(), &id, random_id(), now_min(), 60);
        wire.hear(&from_c, 101);
        node.tick(3);
        assert_eq!(*wire.delivered.lock().unwrap(), vec![tag]);
    }

    #[test]
    fn light_off_reports_off_and_hears_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, _, light, _) = two_channel_node(&dir, &b, &a);
        node.set_light(LightSwitch::Off);
        light.hear(&seal_private(&a, &[b.public()], "unseen", now_min(), 60), 1);
        let events = node.tick(1);
        assert!(messages(&events).is_empty());
        assert!(events.iter().any(|e| matches!(
            e,
            NodeEvent::ChannelState { channel: Channel::Light, state } if state == "off"
        )));
    }

    #[test]
    fn a_tag_light_holds_is_receiving_until_it_lands_or_is_given_up() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, _, light, _) = two_channel_node(&dir, &b, &a);
        node.tick(0);
        let light_states = |events: Vec<NodeEvent>| -> Vec<String> {
            events
                .into_iter()
                .filter_map(|e| match e {
                    NodeEvent::ChannelState {
                        channel: Channel::Light,
                        state,
                    } => Some(state),
                    _ => None,
                })
                .collect()
        };
        light.news.lock().unwrap().push(TransportEvent::Arriving(5));
        assert_eq!(light_states(node.tick(1)), vec!["receiving"]);
        assert!(light_states(node.tick(2)).is_empty(), "still receiving");
        light.hear(&seal_private(&a, &[b.public()], "landed", now_min(), 60), 5);
        let events = node.tick(3);
        assert_eq!(messages(&events), vec!["landed"]);
        assert_eq!(light_states(events), vec!["listening"]);

        light.news.lock().unwrap().push(TransportEvent::Arriving(6));
        assert_eq!(light_states(node.tick(4)), vec!["receiving"]);
        light
            .news
            .lock()
            .unwrap()
            .push(TransportEvent::Abandoned(6));
        assert_eq!(light_states(node.tick(5)), vec!["failed", "listening"]);
    }

    fn light_states(events: &[NodeEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::ChannelState {
                    channel: Channel::Light,
                    state,
                } => Some(state.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_broadcast_goes_on_the_channel_it_is_sent_on_and_its_switch_refuses_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, sound, light, _) = two_channel_node(&dir, &a, &b);
        let id = node.broadcast("By light.", Channel::Light);
        assert_ne!(id, NOT_SENT);
        assert_eq!(light.envelopes().len(), 1);
        assert_eq!(light.envelopes()[0].kind, Kind::Public);
        assert!(sound.sent.lock().unwrap().is_empty());
        let tag = light.last_tag();
        assert_eq!(outbox(&node.tick(1)), vec![(id, OutboxState::Waiting)]);
        light.news.lock().unwrap().push(TransportEvent::Sent(tag));
        assert_eq!(outbox(&node.tick(2)), vec![(id, OutboxState::Sent)]);

        node.set_light(LightSwitch::Read);
        let refused = node.broadcast("Not by light.", Channel::Light);
        let events = node.tick(3);
        assert_eq!(outbox(&events), vec![(refused, OutboxState::Failed)]);
        assert_eq!(light_states(&events)[0], READ_ONLY);
        assert_eq!(light.envelopes().len(), 1, "nothing more shown");
        assert_eq!(
            node.broadcasts().last().map(|b| b.delivery),
            Some(Delivery::Failed)
        );

        assert_eq!(node.broadcast("No.", Channel::Bluetooth), NOT_SENT);
    }

    #[test]
    fn an_unreadable_code_says_tilt_or_dim_at_most_every_few_seconds_until_one_reads() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, _, light, _) = two_channel_node(&dir, &b, &a);
        node.tick(0);
        let unreadable = |node: &mut Node, now: u64| {
            light
                .news
                .lock()
                .unwrap()
                .push(TransportEvent::CodeUnreadable);
            light_states(&node.tick(now))
        };
        assert_eq!(unreadable(&mut node, 100), vec![TILT_OR_DIM]);
        assert!(unreadable(&mut node, 200).is_empty(), "said once");
        // Arriving while it says so: the hint stays until a code reads.
        light.news.lock().unwrap().push(TransportEvent::Arriving(9));
        assert!(light_states(&node.tick(300)).is_empty());
        light.news.lock().unwrap().push(TransportEvent::CodeRead);
        assert_eq!(light_states(&node.tick(400)), vec!["receiving"]);
        // Unreadable again soon after: not said again yet.
        assert!(unreadable(&mut node, 1_000).is_empty());
        assert!(unreadable(&mut node, 4_000).is_empty());
        assert_eq!(unreadable(&mut node, 5_100), vec![TILT_OR_DIM]);
        // No unreadable code in view for a while: it gives way by itself.
        assert!(light_states(&node.tick(7_000)).is_empty());
        assert_eq!(light_states(&node.tick(8_200)), vec!["receiving"]);

        // Off says off, whatever the hint.
        assert_eq!(unreadable(&mut node, 20_000), vec![TILT_OR_DIM]);
        node.set_light(LightSwitch::Off);
        assert_eq!(light_states(&node.tick(20_100)), vec!["off"]);
        assert!(unreadable(&mut node, 30_000).is_empty(), "never while off");
    }

    #[test]
    fn a_light_envelope_that_will_not_open_is_collected_afresh() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b, c) = (
            Identity::generate(),
            Identity::generate(),
            Identity::generate(),
        );
        let (mut node, sound, light, _) = two_channel_node(&dir, &b, &a);
        node.tick(0);

        // Damaged in a chunk: it fails, the transport is told, and the
        // same envelope handed over again under the same tag opens.
        let env = seal_private(&a, &[b.public()], "second try", now_min(), 60);
        let mut bytes = env.encode();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        light.inbox.lock().unwrap().extend(split(&bytes, 40));
        let events = node.tick(1);
        assert!(messages(&events).is_empty());
        assert_eq!(light_states(&events), vec!["failed", "listening"]);
        assert_eq!(*light.rejected.lock().unwrap(), vec![40]);
        light.hear(&env, 40);
        assert_eq!(messages(&node.tick(2)), vec!["second try"]);

        // Sealed to someone else: it will never open here, so it is not
        // collected again.
        light.hear(
            &seal_private(&a, &[c.public()], "not ours", now_min(), 60),
            41,
        );
        assert!(messages(&node.tick(3)).is_empty());
        assert_eq!(*light.rejected.lock().unwrap(), vec![40]);

        // Its key wrap tampered with: rejected, and the real one opens.
        let env = seal_private(&a, &[b.public()], "wrap", now_min(), 60);
        let mut bytes = env.encode();
        bytes[ENVELOPE_HEADER + PRIVATE_HEADER_FIXED + WRAP_LEN - 1] ^= 1;
        light.inbox.lock().unwrap().extend(split(&bytes, 44));
        assert!(messages(&node.tick(4)).is_empty());
        assert_eq!(*light.rejected.lock().unwrap(), vec![40, 44]);
        light.hear(&env, 44);
        assert_eq!(messages(&node.tick(5)), vec!["wrap"]);

        // A tampered public notice.
        let mut notice = seal_public(&a, "Road open.", now_min(), 60);
        notice.body[0] ^= 1;
        light.hear(&notice, 42);
        node.tick(6);
        assert_eq!(*light.rejected.lock().unwrap(), vec![40, 44, 42]);

        // Sound keeps what it completed: told, but nothing is collected
        // afresh.
        let env = seal_private(&a, &[b.public()], "by sound", now_min(), 60);
        let mut bytes = env.encode();
        let end = bytes.len() - 1;
        bytes[end] ^= 1;
        sound.inbox.lock().unwrap().extend(split(&bytes, 43));
        node.tick(7);
        assert_eq!(*sound.rejected.lock().unwrap(), vec![43]);
        sound.hear(&env, 43);
        assert!(
            messages(&node.tick(8)).is_empty(),
            "the tag is spent on sound"
        );
    }

    #[test]
    fn pairing_by_one_channel_sends_the_hello_there_only() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, sound, light, _) = two_channel_node(&dir, &a, &b);
        assert_eq!(node.start_pairing("Ann", Channel::Light), Ok(()));
        assert_eq!((sound.envelopes().len(), light.envelopes().len()), (0, 1));
        assert!(node.pairing());
        assert_eq!(node.start_pairing("Ann", Channel::Sound), Ok(()));
        assert_eq!((sound.envelopes().len(), light.envelopes().len()), (1, 1));
        // Starting by sound ended the pairing by light: its hello is done.
        assert!(light.delivered.lock().unwrap().contains(&light.last_tag()));
    }

    #[test]
    fn pairing_by_light_is_refused_unless_light_may_flash() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, sound, light, _) = two_channel_node(&dir, &a, &b);
        node.tick(0);
        for (switch, why) in [
            (LightSwitch::Off, LIGHT_OFF),
            (LightSwitch::Read, READ_ONLY),
        ] {
            node.set_light(switch);
            // Sound may send, but this pairing is by light only.
            assert_eq!(node.start_pairing("Ann", Channel::Light), Err(why));
            assert!(!node.pairing());
            let states: Vec<_> = node
                .tick(1)
                .into_iter()
                .filter_map(|e| match e {
                    NodeEvent::ChannelState { channel, state } => Some((channel, state)),
                    _ => None,
                })
                .collect();
            assert!(states.contains(&(Channel::Light, why.to_string())));
            assert!(!states.iter().any(|(c, _)| *c == Channel::Sound));
        }
        assert_eq!((sound.envelopes().len(), light.envelopes().len()), (0, 0));

        // Pairing by sound with Sound off is refused the same way.
        node.set_light(LightSwitch::ReadAndFlash);
        node.set_sound(SoundSwitch::Off);
        assert_eq!(node.start_pairing("Ann", Channel::Sound), Err(SOUND_OFF));
        assert_eq!((sound.envelopes().len(), light.envelopes().len()), (0, 0));
    }

    /// An unlocked node for `me` over a real light transport (its camera
    /// never shows anything), Light at Read and flash.
    fn light_node(dir: &tempfile::TempDir, me: &Identity) -> Node {
        let store = Store::create(&dir.path().join("light.db"), "pass", me, "me").unwrap();
        let camera = bombechat_light::FakeCamera::new(Vec::new());
        let light = LightTransport::new(Box::new(camera), bombechat_light::CodeSpec::normal());
        let mut node = Node::new(Some(store), vec![Box::new(light)]);
        node.set_light(LightSwitch::ReadAndFlash);
        node
    }

    /// Tick `node` every 20 ms over `from..to`: the tags shown, in order
    /// of first showing, how many frames, and whether the screen ended
    /// clear.
    fn shown(node: &mut Node, from: u64, to: u64) -> (Vec<u16>, usize, bool) {
        let (mut tags, mut frames, mut lit) = (Vec::new(), 0, false);
        for now in (from..to).step_by(20) {
            for e in node.tick(now) {
                match e {
                    NodeEvent::Display { tag, .. } => {
                        frames += 1;
                        lit = true;
                        if !tags.contains(&tag) {
                            tags.push(tag);
                        }
                    }
                    NodeEvent::DisplayStopped { .. } => lit = false,
                    _ => {}
                }
            }
        }
        (tags, frames, !lit)
    }

    #[test]
    fn the_hello_keeps_showing_while_pairing_is_in_progress() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let mut node = light_node(&dir, &a);
        node.tick(0);
        assert_eq!(node.start_pairing("Ann", Channel::Light), Ok(()));
        assert!(node.pairing());

        // Far past one 4 s window, it is still showing, under fresh tags.
        let (tags, frames, _) = shown(&mut node, 20, 13_000);
        assert!(tags.len() >= 3, "shown again each window: {tags:?}");
        assert!(frames >= 120, "10 a second throughout, got {frames}");
        let (_, frames, _) = shown(&mut node, 13_000, 14_000);
        assert_eq!(frames, 10);

        // Confirming ends it: the screen clears at once and stays clear.
        node.confirm_pairing(&b.public(), "Ben");
        assert!(!node.pairing());
        let (_, frames, clear) = shown(&mut node, 14_000, 14_020);
        assert_eq!((frames, clear), (0, true));
        let (_, frames, clear) = shown(&mut node, 14_020, 30_000);
        assert_eq!((frames, clear), (0, true));

        // Stopping ends it too.
        node.start_pairing("Ann", Channel::Light).unwrap();
        let (_, frames, _) = shown(&mut node, 30_000, 36_000);
        assert_eq!(frames, 60);
        node.stop_pairing();
        let (_, frames, clear) = shown(&mut node, 36_000, 40_000);
        assert_eq!((frames, clear), (0, true));
    }

    #[test]
    fn a_finished_pairing_shows_its_hello_to_the_end_of_the_window_then_ends() {
        let dir = tempfile::TempDir::new().unwrap();
        let a = Identity::generate();
        let mut node = light_node(&dir, &a);
        node.tick(0);
        node.start_pairing("Ann", Channel::Light).unwrap();
        // A second into the first window, their hello is in.
        let (tags, frames, _) = shown(&mut node, 20, 1_000);
        assert_eq!((tags.len(), frames), (1, 10));
        node.finish_pairing();
        assert!(node.pairing(), "still in progress until the window ends");
        // The rest of the window (the page's still moment and 4 s), and
        // no more.
        let (more, frames, clear) = shown(&mut node, 1_000, 20_000);
        assert_eq!((more, frames, clear), (tags, 42, true));
        assert!(!node.pairing());

        // Finished with no hello on screen (sound only), it ends at once.
        let (b, c) = (Identity::generate(), Identity::generate());
        let (mut two, _, light, _) = two_channel_node(&dir, &b, &c);
        two.start_pairing("Ben", Channel::Sound).unwrap();
        two.finish_pairing();
        assert!(!two.pairing());
        assert!(light.envelopes().is_empty());
    }

    #[test]
    fn hello_frames_say_so_and_a_message_by_light_does_not() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let mut node = light_node(&dir, &a);
        let ben = node
            .store
            .as_ref()
            .unwrap()
            .add_contact("Ben", &b.public(), true)
            .unwrap();
        node.tick(0);
        // A message goes on screen first, the hello queues behind it.
        node.send(&[ben], "Road north is open.", Channel::Light);
        node.start_pairing("Ann", Channel::Light).unwrap();
        let mut frames = Vec::new();
        for now in (20..12_000).step_by(20) {
            for e in node.tick(now) {
                if let NodeEvent::Display { tag, hello, .. } = e {
                    frames.push((tag, hello));
                }
            }
        }
        let first = frames[0].0;
        assert!(frames
            .iter()
            .take_while(|(t, _)| *t == first)
            .all(|(_, h)| !h));
        assert!(frames
            .iter()
            .skip_while(|(t, _)| *t == first)
            .all(|(_, h)| *h));
        assert!(frames.iter().any(|(_, h)| *h));
    }

    #[test]
    fn light_leaving_read_and_flash_ends_pairing_by_light() {
        let a = Identity::generate();
        for switch in [LightSwitch::Read, LightSwitch::Off] {
            let dir = tempfile::TempDir::new().unwrap();
            let mut node = light_node(&dir, &a);
            node.tick(0);
            node.start_pairing("Ann", Channel::Light).unwrap();
            let (_, frames, _) = shown(&mut node, 20, 1_000);
            assert!(frames > 0);
            node.set_light(switch);
            assert!(!node.pairing(), "{switch:?}");
            let (_, frames, clear) = shown(&mut node, 1_000, 20_000);
            assert_eq!((frames, clear), (0, true));
            // Back at Read and flash, nothing comes back on screen.
            node.set_light(LightSwitch::ReadAndFlash);
            let (_, frames, _) = shown(&mut node, 20_000, 30_000);
            assert_eq!(frames, 0, "{switch:?}");
        }
    }

    #[test]
    fn pairing_ends_by_itself_after_three_minutes() {
        let dir = tempfile::TempDir::new().unwrap();
        let a = Identity::generate();
        let mut node = light_node(&dir, &a);
        node.tick(0);
        node.start_pairing("Ann", Channel::Light).unwrap();
        let (_, frames, _) = shown(&mut node, 20, PAIRING_CAP_MS - 1_000);
        assert!(frames > 1_700, "shown all along: {frames}");
        assert!(node.pairing());
        let (_, _, clear) = shown(&mut node, PAIRING_CAP_MS - 1_000, PAIRING_CAP_MS + 200);
        assert!(clear);
        assert!(!node.pairing());
        let (_, frames, _) = shown(&mut node, PAIRING_CAP_MS + 200, PAIRING_CAP_MS + 10_000);
        assert_eq!(frames, 0);
    }

    #[test]
    fn pairing_with_no_channel_that_can_send_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let a = Identity::generate();
        let mut node = light_node(&dir, &a);
        node.set_light(LightSwitch::Read);
        // Sound is at Listen and send, but there is no sound to send on.
        assert_eq!(node.start_pairing("Ann", Channel::Light), Err(READ_ONLY));
        assert!(!node.pairing());
        let states: Vec<_> = node
            .tick(1)
            .into_iter()
            .filter_map(|e| match e {
                NodeEvent::ChannelState { channel, state } => Some((channel, state)),
                _ => None,
            })
            .collect();
        assert!(states.contains(&(Channel::Light, READ_ONLY.to_string())));
        assert!(!states.iter().any(|(c, _)| *c == Channel::Sound));

        let store = Store::create(&dir.path().join("none.db"), "pass", &a, "me").unwrap();
        let mut bare = Node::new(Some(store), vec![]);
        assert_eq!(
            bare.start_pairing("Ann", Channel::Light),
            Err(NOTHING_TO_SEND_ON)
        );
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
        assert_eq!(node.broadcast("hi", Channel::Sound), NOT_SENT);
        assert_eq!(node.start_pairing("me", Channel::Sound), Err("locked"));
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
    fn a_band_change_abandons_an_arrival_as_failed_and_the_next_one_lands() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &b, &[&a]);
        let env = seal_private(&a, &[b.public()], &"long ".repeat(40), now_min(), 60);
        wire.inbox
            .lock()
            .unwrap()
            .push(split(&env.encode(), 9).remove(0));
        assert_eq!(sound_states(&node.tick(1)), vec!["receiving"]);

        node.set_band(Band::Audible);
        assert_eq!(node.band(), Some(Band::Audible));
        let after = node.tick(2);
        assert_eq!(sound_states(&after), vec!["failed", "listening"]);
        assert!(messages(&after).is_empty());

        // The same band again changes nothing, and abandons nothing.
        wire.inbox
            .lock()
            .unwrap()
            .push(split(&env.encode(), 10).remove(0));
        assert_eq!(sound_states(&node.tick(3)), vec!["receiving"]);
        node.set_band(Band::Audible);
        assert!(sound_states(&node.tick(4)).is_empty(), "still receiving");

        // A whole message on the new band arrives and is delivered.
        let next = seal_private(&a, &[b.public()], "On the new band.", now_min(), 60);
        wire.hear(&next, 11);
        assert_eq!(
            messages(&node.tick(5)),
            vec!["On the new band.".to_string()]
        );
    }

    #[test]
    fn a_band_change_with_nothing_arriving_reports_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, _wire, _) = node(&dir, &b, &[&a]);
        node.tick(1);
        node.set_band(Band::Audible);
        assert!(sound_states(&node.tick(2)).is_empty());
    }

    /// The wall clock the stamp tests set, in minutes.
    static WALL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    fn test_wall() -> u32 {
        WALL.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn stamped(node: &Node, contact: ContactId) -> Vec<u32> {
        node.conversation(contact)
            .iter()
            .map(|m| m.at_min)
            .collect()
    }

    #[test]
    fn a_wall_clock_that_jumps_ahead_and_back_moves_no_stamp() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, _wire, ids) = node(&dir, &b, &[&a]);
        node.wall = test_wall;
        WALL.store(1_000_000, std::sync::atomic::Ordering::SeqCst);
        node.tick(0);
        node.send(&ids, "before", Channel::Sound);
        // the clock jumps a month ahead
        WALL.store(1_000_000 + 30 * 1440, std::sync::atomic::Ordering::SeqCst);
        node.tick(60_000);
        node.send(&ids, "during", Channel::Sound);
        // and is put right
        WALL.store(1_000_002, std::sync::atomic::Ordering::SeqCst);
        node.tick(120_000);
        node.send(&ids, "after", Channel::Sound);
        assert_eq!(
            stamped(&node, ids[0]),
            vec![1_000_000, 1_000_001, 1_000_002]
        );
    }

    #[test]
    fn a_stamp_from_a_fast_clock_does_not_lift_the_next_session() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let path = dir.path().join("b.db");
        let store = Store::create(&path, "pass", &b, "me").unwrap();
        let contact = store.add_contact("a", &a.public(), true).unwrap();
        // stored while the clock ran a month fast; it has since been put right
        let ahead = now_min() + 30 * 1440;
        store
            .put_message(&StoredMessage {
                id: [1; 16],
                contact,
                outgoing: false,
                text: "From the fast clock.".into(),
                channel: Channel::Sound,
                at_min: ahead,
                heard: false,
                delivery: Delivery::Sent,
            })
            .unwrap();
        let mut node = Node::new(Some(store), vec![Box::new(Wire::default())]);
        let before = now_min();
        let sent = node.send(&[contact], "Now.", Channel::Sound);
        let at = node
            .conversation(contact)
            .iter()
            .find(|m| m.id == sent)
            .unwrap()
            .at_min;
        assert!(
            at >= before && at <= now_min(),
            "the wall clock's minute, not {ahead}"
        );
    }

    #[test]
    fn forgetting_while_locked_erases_nothing() {
        let node = Node::new(None, Vec::new());
        node.set_age(u64::MAX);
        assert_eq!(node.forget_older_than(u64::MAX, 0), 0);
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
        {
            let channel = Channel::Bluetooth;
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
    fn a_light_send_is_refused_by_the_light_switch_and_kept_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, wire, ids) = node(&dir, &a, &[&b]);
        for (switch, why) in [
            (LightSwitch::Off, LIGHT_OFF),
            (LightSwitch::Read, READ_ONLY),
        ] {
            node.set_light(switch);
            let id = node.send(&ids, "hi", Channel::Light);
            assert_ne!(id, NOT_SENT, "kept, so the sheet can say it was not sent");
            let events = node.tick(1);
            assert_eq!(outbox(&events), vec![(id, OutboxState::Failed)]);
            assert!(events.iter().any(|e| matches!(
                e,
                NodeEvent::ChannelState { channel: Channel::Light, state } if state == why
            )));
            let kept = node.conversation(ids[0]);
            assert_eq!(kept.last().unwrap().delivery, Delivery::Failed);
            assert_eq!(kept.last().unwrap().channel, Channel::Light);
        }
        assert!(
            wire.sent.lock().unwrap().is_empty(),
            "nothing went by sound"
        );

        // Allowed, but there is no light transport: failed at once, and
        // still nothing by sound.
        node.set_light(LightSwitch::ReadAndFlash);
        let id = node.send(&ids, "hi", Channel::Light);
        assert_eq!(outbox(&node.tick(2)), vec![(id, OutboxState::Failed)]);
        assert!(wire.sent.lock().unwrap().is_empty());
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
        let public = node.broadcast("Road north is open.", Channel::Sound);
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
        let public = node.broadcast("refused too", Channel::Sound);
        assert_eq!(node.start_pairing("Ben", Channel::Sound), Err(LISTEN_ONLY));
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
        wire.hear(
            &seal_private(&a, &[b.public()], "unheard", now_min(), 60),
            1,
        );
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
        node.store
            .as_ref()
            .unwrap()
            .put_outgoing(&stale, &old.encode())
            .unwrap();
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
    #[test]
    fn a_broadcast_is_kept_with_its_delivery_and_one_heard_is_kept_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, c) = (Identity::generate(), Identity::generate());
        let (mut node, wire, _) = node(&dir, &a, &[]);
        let id = node.broadcast("Water at the church.", Channel::Sound);
        let kept = node.broadcasts();
        assert_eq!(kept.len(), 1);
        assert!(kept[0].outgoing && kept[0].verified);
        assert_eq!((kept[0].id, kept[0].signer), (id, a.public().sign));
        assert_eq!(
            (kept[0].text.as_str(), kept[0].delivery),
            ("Water at the church.", Delivery::Waiting)
        );
        wire.news
            .lock()
            .unwrap()
            .push(TransportEvent::Sent(wire.last_tag()));
        node.tick(1);
        assert_eq!(node.broadcasts()[0].delivery, Delivery::Sent);

        // at Listen it is kept, as failed, and nothing plays
        node.set_sound(SoundSwitch::Listen);
        let before = wire.sent.lock().unwrap().len();
        let refused = node.broadcast("refused", Channel::Sound);
        assert_eq!(wire.sent.lock().unwrap().len(), before);
        assert_eq!(node.broadcasts()[1].id, refused);
        assert_eq!(node.broadcasts()[1].delivery, Delivery::Failed);

        // one heard, twice: kept once, reported once, with its id and hops
        let mut heard = seal_public(&c, "Road north is open.", now_min(), 60);
        heard.hops = 2;
        wire.hear(&heard, 7);
        let events = node.tick(2);
        wire.hear(&heard, 8);
        let again = node.tick(3);
        let reported: Vec<_> = events
            .iter()
            .chain(again.iter())
            .filter_map(|e| match e {
                NodeEvent::Broadcast {
                    id, hops, verified, ..
                } => Some((*id, *hops, *verified)),
                _ => None,
            })
            .collect();
        assert_eq!(reported, vec![(heard.id, 2, false)]);
        let kept = node.broadcasts();
        assert_eq!(kept.len(), 3);
        assert!(!kept[2].outgoing);
        assert_eq!(
            (kept[2].signer, kept[2].hops, kept[2].verified),
            (c.public().sign, 2, false)
        );
        assert_eq!(kept[2].text, "Road north is open.");
    }

    #[test]
    fn a_broadcast_still_waiting_at_quit_is_queued_again_on_unlock() {
        let dir = tempfile::TempDir::new().unwrap();
        let a = Identity::generate();
        let path = dir.path().join("a.db");
        let store = Store::create(&path, "pass", &a, "me").unwrap();
        let mut node = Node::new(Some(store), vec![Box::new(Wire::default())]);
        let id = node.broadcast("Shelter open until dawn.", Channel::Sound);
        drop(node);

        let wire = Wire::default();
        let mut node = Node::new(None, vec![Box::new(wire.clone())]);
        node.unlock(Store::open(&path, "pass").unwrap());
        let resent: Vec<_> = wire.envelopes().iter().map(|e| e.id).collect();
        assert_eq!(resent, vec![id], "the same signed envelope");
        assert_eq!(outbox(&node.tick(1)), vec![(id, OutboxState::Waiting)]);
        wire.news
            .lock()
            .unwrap()
            .push(TransportEvent::Sent(wire.last_tag()));
        node.tick(2);
        assert_eq!(node.broadcasts()[0].delivery, Delivery::Sent);
    }

    #[test]
    fn a_light_broadcast_still_waiting_at_quit_goes_again_by_light() {
        let dir = tempfile::TempDir::new().unwrap();
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut node, _, _, _) = two_channel_node(&dir, &a, &b);
        let id = node.broadcast("By light, again.", Channel::Light);
        drop(node);

        let (sound, light) = (
            Wire::default(),
            Wire {
                light: true,
                ..Wire::default()
            },
        );
        let mut node = Node::new(None, vec![Box::new(sound.clone()), Box::new(light.clone())]);
        node.set_light(LightSwitch::ReadAndFlash);
        node.unlock(Store::open(&dir.path().join("two.db"), "pass").unwrap());
        let resent: Vec<_> = light.envelopes().iter().map(|e| e.id).collect();
        assert_eq!(resent, vec![id], "by light");
        assert!(sound.sent.lock().unwrap().is_empty(), "not by sound");
        assert_eq!(node.broadcasts()[0].channel, Channel::Light);
    }

    fn self_tests(events: &[NodeEvent]) -> Vec<(u16, SelfTestState)> {
        events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::SelfTest { tag, state, .. } => Some((*tag, state.clone())),
                _ => None,
            })
            .collect()
    }

    /// A sound test started, with its tag and the envelope it sent.
    fn testing(me: &Identity, dir: &tempfile::TempDir) -> (Node, Wire, u16, Envelope) {
        let (mut node, wire, _) = node(dir, me, &[]);
        node.start_self_test(Channel::Sound).unwrap();
        // its waiting is reported on the first tick
        assert_eq!(self_tests(&node.tick(0)).len(), 1);
        let tag = wire.last_tag();
        let real = wire.envelopes().remove(0);
        (node, wire, tag, real)
    }

    /// The real test's envelope arrives: it must be heard.
    fn the_real_one_lands(node: &mut Node, wire: &Wire, real: &Envelope, tag: u16, now: u64) {
        wire.hear(real, tag);
        let ev = node.tick(now);
        assert!(
            matches!(
                self_tests(&ev).last(),
                Some((t, SelfTestState::Heard { text, .. })) if *t == tag && text == SELF_TEST_TEXT
            ),
            "the real test was kept out: {ev:?}"
        );
        assert!(messages(&ev).is_empty());
        assert!(!node.self_testing());
        assert!(node.seen.contains(&real.id), "used up once heard");
    }

    #[test]
    fn a_private_envelope_another_identity_seals_under_the_tests_id_does_not_settle_it() {
        let dir = tempfile::tempdir().unwrap();
        let (me, them) = (Identity::generate(), Identity::generate());
        let (mut node, wire, tag, real) = testing(&me, &dir);

        // someone who heard the id seals their own message to this machine
        // under it: it opens, but it is not this machine's
        let forged = bombechat_core::seal_private_under(
            &them,
            &[me.public()],
            SELF_TEST_TEXT,
            real.id,
            now_min(),
            TTL_MIN,
        );
        assert_eq!(forged.id, real.id);
        wire.hear(&forged, 0x0f0f);
        let ev = node.tick(1);
        assert!(self_tests(&ev).is_empty() && messages(&ev).is_empty());
        assert!(node.self_testing(), "still waiting for the real one");
        assert!(!node.seen.contains(&real.id), "the id is not used up");
        the_real_one_lands(&mut node, &wire, &real, tag, 2);
    }

    #[test]
    fn a_strangers_notice_under_the_tests_id_does_not_keep_the_test_out() {
        let dir = tempfile::tempdir().unwrap();
        let (me, them) = (Identity::generate(), Identity::generate());
        let (mut node, wire, tag, real) = testing(&me, &dir);

        // a notice, validly signed by a stranger, under the test's id: it
        // is kept as any notice is, and the test is untouched
        let notice = bombechat_core::seal_public_under(
            &them,
            "Road north is open.",
            real.id,
            now_min(),
            TTL_MIN,
        );
        wire.hear(&notice, 0x0a0a);
        let ev = node.tick(1);
        assert!(ev.iter().any(|e| matches!(e, NodeEvent::Broadcast { .. })));
        assert!(self_tests(&ev).is_empty());
        assert!(!node.seen.contains(&real.id), "the id is not used up");
        the_real_one_lands(&mut node, &wire, &real, tag, 2);
    }

    #[test]
    fn a_strangers_control_envelope_under_the_tests_id_does_not_keep_the_test_out() {
        let dir = tempfile::tempdir().unwrap();
        let (me, them) = (Identity::generate(), Identity::generate());
        let (mut node, wire, tag, real) = testing(&me, &dir);

        // a hello whose outer id (not signed) is the test's
        let hello = control::pair_hello(&them, "Ben", real.id, now_min(), TTL_MIN);
        wire.hear(&hello, 0x0b0b);
        let ev = node.tick(1);
        assert!(ev.iter().any(|e| matches!(e, NodeEvent::PairOffer { .. })));
        assert!(!node.seen.contains(&real.id), "the id is not used up");
        the_real_one_lands(&mut node, &wire, &real, tag, 2);
    }

    #[test]
    fn a_test_heard_with_no_time_measured_reports_no_time() {
        let dir = tempfile::tempdir().unwrap();
        let me = Identity::generate();
        // the carrier never says it is playing: no time to measure from
        let (mut node, wire, tag, real) = testing(&me, &dir);
        wire.hear(&real, tag);
        let ev = node.tick(1);
        assert!(matches!(
            self_tests(&ev).last(),
            Some((_, SelfTestState::Heard { ms: None, .. }))
        ));

        // and one heard in the same tick it began to play, on a carrier that
        // measures no rate to floor it at, has no time either
        let (mut node, wire, tag, real) = testing(&me, &tempfile::tempdir().unwrap());
        wire.news.lock().unwrap().push(TransportEvent::Playing(tag));
        wire.hear(&real, tag);
        let ev = node.tick(5);
        assert!(matches!(
            self_tests(&ev).last(),
            Some((_, SelfTestState::Heard { ms: None, .. }))
        ));
    }

    #[test]
    fn a_test_that_never_plays_for_a_loud_room_is_given_up_after_two_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let me = Identity::generate();
        let (mut node, _wire, tag, _) = testing(&me, &dir);
        let mut now = 0;
        let mut ended = None;
        // counted in ticks of 500 ms: nothing ever says it is playing
        for round in 1..=1000u64 {
            now = round * 500;
            if let Some(s) = self_tests(&node.tick(now))
                .into_iter()
                .find(|(_, s)| *s != SelfTestState::Waiting)
            {
                ended = Some(s);
                break;
            }
        }
        assert_eq!(ended, Some((tag, SelfTestState::RoomNeverQuiet)));
        assert_eq!(
            now, SELF_TEST_QUIET_MS,
            "given up at two minutes, not before"
        );
        assert!(!node.self_testing());
    }

    #[test]
    fn a_self_test_by_light_shows_again_until_it_is_read_and_says_so_on_screen() {
        let dir = tempfile::tempdir().unwrap();
        let me = Identity::generate();
        let store = Store::create(&dir.path().join("me.db"), "pass", &me, "me").unwrap();
        let wire = Wire {
            light: true,
            ..Wire::default()
        };
        let mut node = Node::new(Some(store), vec![Box::new(wire.clone())]);
        assert_eq!(
            node.start_self_test(Channel::Light),
            Err(LIGHT_OFF),
            "Light starts at Off"
        );
        node.set_light(LightSwitch::ReadAndFlash);
        node.start_self_test(Channel::Light).unwrap();
        let first = wire.last_tag();
        wire.news.lock().unwrap().extend([
            TransportEvent::Playing(first),
            TransportEvent::Display {
                tag: first,
                index: 0,
                spec: 1,
                bits: vec![0; 392],
            },
        ]);
        let ev = node.tick(10);
        assert_eq!(
            self_tests(&ev),
            vec![
                (first, SelfTestState::Waiting),
                (first, SelfTestState::Playing)
            ]
        );
        assert!(ev.iter().any(|e| matches!(
            e,
            NodeEvent::Display {
                test: true,
                hello: false,
                ..
            }
        )));

        // its window ends unread: on screen again under a fresh tag
        wire.news.lock().unwrap().push(TransportEvent::Sent(first));
        node.tick(20);
        let again = wire.last_tag();
        assert_ne!(again, first);
        wire.news
            .lock()
            .unwrap()
            .push(TransportEvent::Playing(again));
        let ev = node.tick(30);
        assert_eq!(self_tests(&ev), vec![(again, SelfTestState::Playing)]);

        // read in the mirror: heard, and taken off the screen
        let envelope = wire.envelopes().remove(0);
        wire.hear(&envelope, again);
        let ev = node.tick(40);
        assert!(matches!(
            self_tests(&ev).last(),
            Some((t, SelfTestState::Heard { band: None, .. })) if *t == again
        ));
        assert_eq!(*wire.delivered.lock().unwrap(), vec![again]);

        // and Light turned to Read stops one in progress
        node.start_self_test(Channel::Light).unwrap();
        node.set_light(LightSwitch::Read);
        let ev = node.tick(50);
        assert_eq!(self_tests(&ev).last().unwrap().1, SelfTestState::Stopped);
    }
}

#[cfg(test)]
mod reply_max_tests {
    use super::*;

    #[test]
    fn the_longest_reply_is_the_most_one_envelope_holds() {
        let max = max_private_text(1);
        assert!(private_envelope_len(1, max) <= MAX_ENVELOPE);
        assert!(private_envelope_len(1, max + 1) > MAX_ENVELOPE);
        assert!(max_private_text(2) < max);
    }
}
