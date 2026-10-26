//! bombechat-bluetooth: the Bluetooth link protocol, with no platform code.
//!
//! [`BleTransport`] is the node's [`Channel::Bluetooth`](bombechat_store::Channel)
//! transport over any [`Radio`]. The CoreBluetooth radio comes later;
//! [`FakeRadio`] is an in-memory one for tests.
//!
//! # The advert
//!
//! A device advertises our service ([`SERVICE_UUID`]) and a random 16-byte
//! token, replaced at a random moment between 12 and 15 minutes
//! ([`TOKEN_MIN_MS`], [`TOKEN_MS`]). Nothing in the advert is
//! stable. Links carry writes on one characteristic ([`CHARACTERISTIC_UUID`]),
//! write without response one way and notify the other.
//!
//! # Link messages
//!
//! `kind u8 | flags u8 | len u16 BE | body`, cut into fragments no longer
//! than the link's largest write: `message u16 | index u16 | count u16 |
//! data`. Reassembly is capped per link at 32 messages part way, 64 KiB
//! each, 30 seconds each; past a cap the message is dropped. The sizes
//! follow the link and are padded to nothing, so they don't line up with
//! Bitchat's fragments or padding buckets.
//!
//! Every body begins with a 4-byte exchange number, random, chosen by the
//! side that began the exchange. The flag `FROM_INITIATOR` says the writer
//! began it, so each side keys exchanges by (who began, number). Kinds:
//!
//! | kind | body after the number |
//! |---|---|
//! | 1 contact | `Noise_IK` message 1 |
//! | 2 request | `Noise_XN` message 1 |
//! | 3 handshake | any later handshake message |
//! | 4 sealed | `nonce u64` and a transport message |
//!
//! Inside a sealed message, padded to 256, 512, 1024 or 2048 bytes or a
//! whole number of 4 KiB, is what it is (envelope, acknowledgement or
//! keepalive), so only the two ends can tell those apart or tell how long
//! an envelope is. Any device sends any kind: a device with no contacts
//! sends decoy hellos.
//!
//! # Contacts (`Noise_IK`)
//!
//! A link this device opened knows the advert's token. Each token gets one
//! round of exactly [`TRIAL_CAP`] hellos: contacts most recently seen
//! first (then least recently tried), and decoys of the same size, to keys
//! no one holds, for the rest. So the number of hellos says nothing about
//! the number of contacts. A round's hellos unanswered after
//! [`TRIAL_RETRY_MS`] go once more, afresh, and that is all for that token:
//! reconnecting gives a tracker nothing more.
//!
//! When a contact session on such a link ends, the contact is redialled
//! outside the rounds, after [`REDIAL_FIRST_MS`], doubling to
//! [`REDIAL_MAX_MS`], for as long as the link lives.
//!
//! The answering side builds a contact responder for each message 1 with
//! its clock and its current and previous tokens. It answers only if the
//! message reads, was sent within two minutes of its own clock, names one
//! of its tokens, comes from a contact (not a stranger, not itself), and
//! carries an ephemeral key it hasn't answered in the last four minutes.
//! The ephemeral is added to that cache ([`SEEN_CAP`] keys, [`SEEN_MS`])
//! only after such a read. Everything else, stale, wrong target, stranger,
//! decoy or replay, gets no byte back.
//!
//! The initiator sends a keepalive at once, so the answering side's
//! session is confirmed. A contact counts as present only on a confirmed
//! session.
//!
//! # Requests to connect (`Noise_XN`)
//!
//! Only in [`Mode::Open`], and only when asked ([`BleHandle::request`]):
//! the requester's hello envelope goes as the first transport message. The
//! recipient answers the handshake without any identity of its own, sends
//! nothing but handshake and acknowledgement bytes, and reports
//! [`BleEvent::Request`] once the hello decodes and fits. It holds one
//! request per link, a new one replacing the last, and [`REQUESTS_CAP`] in
//! all; each is forgotten [`REQUEST_MS`] after its hello unless the person
//! accepts. In [`Mode::Contacts`] a request's message 1 gets no byte back.
//!
//! # Links
//!
//! A link is closed ([`Radio::disconnect`]) when it has had no session and
//! no handshake for [`IDLE_MS`]; when this device opened it [`TOKEN_MS`]
//! ago, so its token is never more than one rotation stale (the radio then
//! connects to the fresh advert); in Contacts mode, when it shows a
//! stranger's token whose round is over; and, on this device's own
//! rotation, when it carries no confirmed contact session, so nobody
//! connected to this device follows it from one token to the next.
//!
//! # Order and loss
//!
//! Transport messages are sealed under explicit nonces, a counter of each
//! side's sends, so they decrypt in any order; a replay window refuses any
//! nonce accepted before. Envelopes carry a sequence number inside the
//! encryption and are delivered in order, once each, with up to [`WINDOW`]
//! messages or 64 KiB held ahead of a gap. Every message carries the
//! receiver's cumulative acknowledgement, so acknowledgements are as
//! authenticated as the data. The sender keeps what is unacknowledged (at
//! most [`WINDOW`]) and sends it all again, newly sealed, every
//! [`RESEND_MS`]. A confirmed contact session sends a keepalive every
//! [`KEEPALIVE_MS`] and closes after [`SILENCE_MS`] without hearing
//! anything, or without progress; it is then redialled, and its
//! unacknowledged envelopes go again on the next session.
//!
//! # Envelopes
//!
//! Inside a session travel whole envelopes (`bombechat_core::Envelope`
//! wire bytes), not the sound modem's Reed-Solomon frames: the session
//! already authenticates and orders, and a future Bitchat bridge translates
//! envelopes, so it never needs the modem's framing. The transport takes
//! the node's frames in, rebuilds each envelope, and hands received
//! envelopes back to the node as frames, as every transport does.
//!
//! A private envelope (or a receipt) goes only to confirmed sessions whose
//! contact's fingerprint is among its recipients. A public envelope goes
//! to every confirmed contact session. Strangers in Open mode get no
//! broadcasts: whether they should is a later decision.
//!
//! # Clocks
//!
//! The transport reads no clock. The node's `now_ms` drives every timer,
//! and contact handshakes add the Unix time at which that clock read zero
//! (given to [`BleTransport::new`]).
//!
//! A contact more than two minutes off gets no answer, and the answering
//! side can't tell the stale message came from a contact, so neither side
//! can tell the person what is wrong. The initiator can't tell a contact
//! with a wrong clock from a stranger: both stay silent. A hint ("No
//! answer. If this keeps happening, check both Macs' clocks.") is left for
//! later, when a signal that tells the two apart exists.

mod fake;
mod handle;
mod link;
mod queue;
mod radio;
mod route;
mod session;
mod transport;
pub mod wire;

pub use fake::{FakeRadio, Room, SeededRng, FAKE_MAX_WRITE};
pub use handle::{BleEvent, BleHandle, Peers, EVENTS_CAP};
pub use queue::{HOLD_MS, QUEUE_CAP};
pub use radio::{LinkId, Mode, Radio, RadioEvent};
pub use session::{bucket, AHEAD_BYTES, KEEPALIVE_MS, RESEND_MS, SILENCE_MS, WINDOW};
pub use transport::{
    BleTransport, HANDSHAKE_MS, IDLE_MS, MAX_LINKS, REDIAL_FIRST_MS, REDIAL_MAX_MS, REQUESTS_CAP,
    REQUEST_MS, ROUNDS_CAP, ROUND_MEMORY_MS, SEEN_CAP, SEEN_MS, STRANGER_MS, TOKEN_MIN_MS,
    TOKEN_MS, TRIAL_ATTEMPTS, TRIAL_CAP, TRIAL_RETRY_MS,
};

/// Our GATT service, generated once for Bombechat (never Bitchat's).
pub const SERVICE_UUID: &str = "8AAC3E33-A919-4002-96A8-14E1A873404F";

/// Our one characteristic in that service: write without response, and
/// notify.
pub const CHARACTERISTIC_UUID: &str = "CC26A52C-3F93-439B-83DC-2301C13CA327";
