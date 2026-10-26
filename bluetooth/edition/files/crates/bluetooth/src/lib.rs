//! bombechat-bluetooth: the Bluetooth link protocol, with no platform code.
//!
//! [`BleTransport`] is the node's [`Channel::Bluetooth`](bombechat_store::Channel)
//! transport over any [`Radio`]. [`CoreBluetoothRadio`] is the real one on
//! macOS (on other systems it only says Bluetooth isn't supported yet);
//! [`FakeRadio`] is an in-memory one for tests.
//!
//! # The advert
//!
//! A device advertises our service ([`SERVICE_UUID`]) and a random 16-byte
//! token, replaced at a random moment between 12 and 15 minutes
//! ([`TOKEN_MIN_MS`], [`TOKEN_MS`]). Nothing in the advert is
//! stable. On macOS the advert can hold only the token's first six bytes
//! (as the local name, its tag); the radio hands the whole token to each
//! device that connects, before anything else on the link, unless it heard
//! that device under an earlier token, when it withholds it (see
//! [`CoreBluetoothRadio`] and [`Advert`]). Links carry writes on one characteristic ([`CHARACTERISTIC_UUID`]),
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
//! whole number of 4 KiB, is what it is (envelope, acknowledgement,
//! keepalive or token), so only the two ends can tell those apart or tell
//! how long an envelope is. Any device sends any kind: a device with no contacts
//! sends decoy hellos.
//!
//! # Contacts (`Noise_IK`)
//!
//! A hello names the token of the device it is for, so only a link this
//! device opened can carry one. A token a contact gave inside a session
//! ([`BleTransport::contact_schedule`]) whose tag the advert showed comes
//! first; then one this device was given for that advert on a link it
//! lost; then the one this link was given. The other device withholds its
//! token from a device it heard under an earlier one, so without the first
//! two such a link has no token to name, and its round is all decoys.
//!
//! Each token gets one round of exactly
//! [`TRIAL_CAP`] hellos: a contact who gave this token first, then
//! contacts most recently seen (then least recently tried), and decoys of
//! the same size, to keys no one holds, for the rest. So the number of hellos says nothing about
//! the number of contacts. A round's hellos unanswered after
//! [`TRIAL_RETRY_MS`] go once more, afresh, and that is all for that token:
//! reconnecting gives a tracker nothing more.
//!
//! When a contact session on such a link ends, the contact is redialled
//! outside the rounds, after [`REDIAL_FIRST_MS`], doubling to
//! [`REDIAL_MAX_MS`], for as long as the link lives. When the radio loses
//! such a link, its contacts are redialled at once on the next link to
//! the same advert, which gets no round of its own.
//!
//! Each side draws its tokens [`FUTURE_TOKENS`] rotations ahead. Every
//! confirmed contact session carries each side's token schedule (current,
//! then the next ones in order), in a token message inside the
//! encryption, padded like every other: once when it is confirmed, once
//! whenever the schedule changes, and in place of every keepalive. Each
//! side keeps, in memory, the latest schedule each contact gave
//! ([`CONTACT_TOKEN_MS`]), so it recognises that contact's advert through
//! up to three rotations made apart. A removed contact's sessions end at
//! once, so it is told nothing more, what it gave is forgotten, and the
//! tokens it was told ahead are drawn again.
//!
//! The answering side builds a contact responder for each message 1 with
//! its clock, its current token, the [`PAST_TOKENS`] before it (about
//! an hour) and the [`FUTURE_TOKENS`] told ahead. It answers only if the
//! message reads, was sent within two minutes of its own clock, names one
//! of those tokens, comes from a contact (not a stranger, not itself), and
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
//! Only in [`Mode::Open`] ([`BluetoothSwitch::Open`]), and only when the
//! node asks ([`bombechat_node::Node::request`], one stranger chosen by
//! label): the requester's hello envelope goes as the first transport
//! message, then its current token alone. The recipient answers the
//! handshake without any identity of its own, sends nothing but handshake
//! and acknowledgement bytes, and reports the request to the node once the
//! hello decodes and fits. It is held [`REQUEST_MS`] after the hello
//! arrived, one per link, at most [`REQUESTS_CAP`] (one more pushes out
//! the oldest the person hasn't accepted; accepted ones never go this
//! way), and at most [`REQUESTS_PART_WAY_CAP`] part way, whose eviction
//! never touches one reported. At most one request is reported per link
//! and per requester identity in [`bombechat_node::REQUEST_GAP_MS`]: a
//! link that brought one gets no answer to another message 1 in that
//! time, so one stranger can't push out other people's requests by
//! asking again and again. In [`Mode::Contacts`] a request's message 1
//! gets no byte back, requests held end, and no stranger is offered.
//!
//! Accepting sends this device's hello inside the request session. Once
//! the requester acknowledges it, this device proves it sent it with a
//! contact handshake on the same link, naming the token the requester
//! gave, redialled with backoff until answered. The requester answers that
//! handshake from the identity the hello claimed, and only on that link,
//! though it is not yet a contact; when the session confirms, the
//! requester reports the accept and takes them as a contact. A hello
//! claiming someone else proves nothing: its proof comes from another key.
//!
//! Declining forgets the request and sends nothing. The link is held as
//! long as the request would have been, so the requester can't tell a
//! decline from no answer by when the link closes; either way its request
//! ends unanswered [`REQUEST_MS`] after it was delivered.
//!
//! # Who is near
//!
//! Contacts count as present only on a confirmed session. Strangers are
//! offered for a request while a link this device opened to their advert
//! is up, in Open mode. They are counted as seen for [`STRANGER_MS`]
//! (fifteen minutes) after such a link was last up, keyed by the advert's
//! token: an idle link is closed after [`IDLE_MS`] and not made again until
//! the token changes, so a count of live links alone would drop a stranger
//! still sitting nearby for up to a token's life. The count is an upper
//! bound: a stranger who rotates within the fifteen minutes is counted
//! twice, because nothing links its two tokens (by design), and erring
//! high is the safe side for a person deciding whether strangers are near.
//!
//! # Links
//!
//! A link is closed ([`Radio::disconnect`]) when it has had no session and
//! no handshake for [`IDLE_MS`]; when this device opened it [`TOKEN_MS`]
//! ago, so its token is never more than one rotation stale (the radio then
//! connects to the fresh advert); in Contacts mode, when it shows a
//! stranger's token whose round is over; and, on this device's own
//! rotation, when it carries no confirmed contact session, so no stranger
//! connected to this device follows it from one token to the next.
//!
//! A link with a confirmed contact session is kept across this device's
//! rotation, for at most one more token (the contact closes a link it
//! opened once it is that old). Someone sniffing it can link the two
//! tokens; closing and redialling wouldn't stop that (the new connection
//! between the same radios follows at once) and would cost a round of
//! hellos and a gap in delivery at every rotation, so it is accepted, and
//! only concerns two devices that are each other's contacts.
//!
//! # Order and loss
//!
//! Transport messages are sealed under explicit nonces, a counter of each
//! side's sends, so they decrypt in any order; a replay window refuses any
//! nonce accepted before. Envelopes carry a sequence number inside the
//! encryption and are delivered in order, once each, with up to [`WINDOW`]
//! messages or 64 KiB held ahead of a gap. Every message carries the
//! receiver's cumulative acknowledgement, so acknowledgements are as
//! authenticated as the data. Envelopes and keepalives both draw an
//! acknowledgement, which waits a random moment up to
//! [`ACK_DELAY_MAX_MS`] unless something else carries it sooner, so
//! whether and how soon a reply follows doesn't tell the two apart. The
//! sender keeps what is unacknowledged (at most [`WINDOW`]) and sends it
//! all again, newly sealed, every [`RESEND_MS`]. A confirmed contact
//! session sends a keepalive every [`KEEPALIVE_MS`] and closes after [`SILENCE_MS`] without hearing
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

// The radio's logic is platform-free and tested everywhere, but only
// macOS drives it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod adapter;
mod fake;
mod handle;
mod link;
mod queue;
mod radio;
mod route;
mod session;
#[cfg(test)]
mod sim;
mod transport;
mod unsupported;
pub use unsupported::{UnsupportedRadio, BLUETOOTH_NOT_SUPPORTED};
pub mod wire;

/// The Bluetooth radio: CoreBluetooth on macOS. Bombechat has no Bluetooth
/// code for other systems yet, so there it reports
/// [`BLUETOOTH_NOT_SUPPORTED`] at the first start and does nothing else.
#[cfg(not(target_os = "macos"))]
pub type CoreBluetoothRadio = UnsupportedRadio;

#[cfg(target_os = "macos")]
mod macos;

pub use adapter::{
    BLUETOOTH_DENIED, BLUETOOTH_FAILED, BLUETOOTH_OFF, BLUETOOTH_UNSUPPORTED, CENTRALS_CAP,
    CENTRAL_MEMORY_MS, CLOSED_MS, CONNECT_MS, QUEUE_BYTES, REFUSED_MS, RETRY_MS,
};
pub use fake::{FakeRadio, Room, SeededRng, FAKE_MAX_WRITE};
pub use handle::{BleHandle, BluetoothSwitch, Peers};
#[cfg(target_os = "macos")]
pub use macos::CoreBluetoothRadio;
pub use queue::{HOLD_MS, QUEUE_CAP};
pub use radio::{Advert, LinkId, Mode, Radio, RadioEvent, TAG_LEN};
pub use session::{
    bucket, ACK_DELAY_MAX_MS, AHEAD_BYTES, KEEPALIVE_MS, RESEND_MS, SILENCE_MS, WINDOW,
};
pub use transport::{
    BleTransport, BLUETOOTH_SEND_OFF, CONTACT_TOKEN_MS, FUTURE_TOKENS, HANDSHAKE_MS, IDLE_MS,
    MAX_LINKS, PAST_TOKENS, REDIAL_FIRST_MS, REDIAL_MAX_MS, REQUESTS_CAP, REQUESTS_NEED_OPEN,
    REQUESTS_PART_WAY_CAP, REQUEST_ATTEMPTS, REQUEST_MS, ROUNDS_CAP, ROUND_MEMORY_MS, SEEN_CAP,
    SEEN_MS, STRANGER_MS, TOKEN_MIN_MS, TOKEN_MS, TRIAL_ATTEMPTS, TRIAL_CAP, TRIAL_RETRY_MS,
};

/// Our GATT service, generated once for Bombechat (never Bitchat's).
pub const SERVICE_UUID: &str = "8AAC3E33-A919-4002-96A8-14E1A873404F";

/// Our one characteristic in that service: write without response, and
/// notify.
pub const CHARACTERISTIC_UUID: &str = "CC26A52C-3F93-439B-83DC-2301C13CA327";
