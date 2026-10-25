//! bombechat-bluetooth: the Bluetooth link protocol, with no platform code.
//!
//! [`BleTransport`] is the node's [`Channel::Bluetooth`](bombechat_store::Channel)
//! transport over any [`Radio`]. The CoreBluetooth radio comes later;
//! [`FakeRadio`] is an in-memory one for tests.
//!
//! # The advert
//!
//! A device advertises our service ([`SERVICE_UUID`]) and a random 16-byte
//! token, replaced every 15 minutes ([`TOKEN_MS`]). Nothing in the advert is
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
//! | 4 data | `seq u32` and a transport message |
//! | 5 ack | `next u32`: every data message before it arrived |
//!
//! Any device sends any kind, so a kind tells an observer what a message
//! does, never who sent it.
//!
//! # Contacts (`Noise_IK`)
//!
//! A link opened to an advert knows the advert's token. On it this device
//! sends message 1 to each contact in turn as a candidate, most recently
//! seen first, at most [`TRIAL_CAP`] contacts per token per
//! [`TRIAL_WINDOW_MS`]: only the contact who owns the device can read it.
//! Unanswered, a candidate's message 1 goes again afresh after
//! [`TRIAL_RETRY_MS`], [`TRIAL_ATTEMPTS`] times in all. The cap is per
//! token rather than per link, so reconnecting doesn't buy a tracker more
//! trials.
//!
//! The answering side builds a contact responder for each message 1 with
//! its clock and its current and previous tokens. It answers only if the
//! message reads, was sent within two minutes of its own clock, names one
//! of its tokens, comes from a contact (not a stranger, not itself), and
//! carries an ephemeral key it hasn't answered in the last four minutes.
//! The ephemeral is added to that cache ([`SEEN_CAP`] keys, [`SEEN_MS`])
//! only after such a read. Everything else, stale, wrong target, stranger
//! or replay, gets no byte back.
//!
//! The initiator sends an empty transport message at once, so the
//! answering side's session is confirmed. A contact counts as present only
//! on a confirmed session.
//!
//! # Requests to connect (`Noise_XN`)
//!
//! Only in [`Mode::Open`], and only when asked ([`BleHandle::request`]):
//! the requester's hello envelope goes as the first transport message. The
//! recipient answers the handshake without any identity of its own, sends
//! nothing but handshake and acknowledgement bytes, and reports
//! [`BleEvent::Request`]. In [`Mode::Contacts`] a request's message 1 gets
//! no byte back.
//!
//! # Order and loss
//!
//! Noise counts its messages implicitly, so a session's transport messages
//! must be decrypted in order, each exactly once. Writes may be lost,
//! repeated or reordered. Each data message carries a sequence number in
//! the clear; the receiver decrypts in order, holds up to [`WINDOW`]
//! messages ahead of a gap, drops repeats, and acknowledges cumulatively.
//! The sender keeps what is unacknowledged (at most [`WINDOW`]) and sends
//! it all again every [`RESEND_MS`] without progress. This is simpler than
//! starting a new session on every gap, which would also need the
//! acknowledgements to know what to send again. A message that fails to
//! decrypt in its turn (tampered, or a forged sequence number), or a
//! session with no progress for [`STALL_MS`], closes the session; a
//! contact is then dialled afresh and its unacknowledged envelopes go again
//! on the new session.
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
mod radio;
mod route;
mod transport;
pub mod wire;

pub use fake::{FakeRadio, Room, FAKE_MAX_WRITE};
pub use radio::{LinkId, Mode, Radio, RadioEvent};
pub use transport::{
    BleEvent, BleHandle, BleTransport, Peers, EVENTS_CAP, HANDSHAKE_MS, HOLD_MS, QUEUE_CAP,
    RESEND_MS, SEEN_CAP, SEEN_MS, STALL_MS, STRANGER_MS, TOKEN_MS, TRIAL_ATTEMPTS, TRIAL_CAP,
    TRIAL_RETRY_MS, TRIAL_WINDOW_MS, WINDOW,
};

/// Our GATT service, generated once for Bombechat (never Bitchat's).
pub const SERVICE_UUID: &str = "8AAC3E33-A919-4002-96A8-14E1A873404F";

/// Our one characteristic in that service: write without response, and
/// notify.
pub const CHARACTERISTIC_UUID: &str = "CC26A52C-3F93-439B-83DC-2301C13CA327";
