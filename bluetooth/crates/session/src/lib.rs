//! bombechat-session: live encrypted sessions between two devices, for the
//! Bluetooth link.
//!
//! A session is a Noise handshake followed by a pair of transport ciphers.
//! A side's Noise static key is its identity's X25519 key. There are three
//! kinds of session, one Noise pattern each:
//!
//! | kind    | pattern                            | prologue         | used for |
//! |---------|------------------------------------|------------------|----------|
//! | contact | `Noise_IK_25519_ChaChaPoly_SHA256` | `"bombechat v1"` | talking to someone already in the contacts |
//! | request | `Noise_XN_25519_ChaChaPoly_SHA256` | `"bombechat v1"` | a stranger's request to connect |
//! | bridge  | `Noise_XX_25519_ChaChaPoly_SHA256` | none             | kept for a future Bitchat bridge; the link layer doesn't use it |
//!
//! ```text
//! contact (IK), the initiator already knows the responder's static key:
//!   1. initiator -> responder   e, es, s, ss     payload: hello (below)
//!   2. responder -> initiator   e, ee, se        payload: empty
//!
//! request (XN), the recipient never sends a static key:
//!   1. requester -> recipient   e                payload: empty
//!   2. recipient -> requester   e, ee            payload: empty
//!   3. requester -> recipient   s, se            payload: requester's certificate
//!
//! bridge (XX), Bitchat's pattern for live sessions:
//!   1. initiator -> responder   e                payload: empty
//!   2. responder -> initiator   e, ee, s, es     payload: responder's certificate
//!   3. initiator -> responder   s, se            payload: initiator's certificate
//! ```
//!
//! # Identity
//!
//! Noise proves that each side holding a static key holds its secret. The
//! certificate adds the Ed25519 half, so the remote becomes a whole
//! [`PublicIdentity`] that can be matched against contacts and the safety
//! number. It is made once per identity, in [`SessionKeys`] (Ed25519 is
//! deterministic, so it is the same bytes every time), and travels as the payload of whichever
//! message carries the sender's static key:
//!
//! ```text
//! certificate: sign [32] ‖ dh [32] ‖ Ed25519 signature [64]   (128 bytes)
//! signed:      "bombechat static key v1" ‖ dh [32]
//! ```
//!
//! The contact initiator's message 1 carries a hello, not just the
//! certificate, so its responder can refuse replays and relays:
//!
//! ```text
//! hello: certificate [128] ‖ sent_at u64 BE, Unix ms [8] ‖ target token [16]   (152 bytes)
//! ```
//!
//! `sent_at` is the initiator's clock; the responder refuses it if it is
//! more than [`CONTACT_WINDOW_MS`] (120 s) from its own clock either way
//! ([`SessionError::Stale`]). The target token is the advertising token of
//! the device the initiator meant to reach; the responder refuses it unless
//! it is one of its own current or previous tokens
//! ([`SessionError::WrongTarget`]), so a message 1 relayed to another device
//! fails there. Both live inside the encrypted, authenticated payload, so a
//! relay or replay can't rewrite them.
//!
//! A reader rejects a certificate whose `dh` is not the static key Noise
//! presented, or whose signature doesn't verify. The certificate signs
//! nothing about the session, so a session leaves its peer no transferable
//! proof that this device took part: as deniable as plain Noise. Replaying
//! someone's certificate gains nothing, because Noise's DH steps still need
//! that static key's secret.
//!
//! The contact responder learns the initiator's identity from message 1 and
//! then decides: [`Handshake::remote_identity`] after
//! [`Handshake::read_message`], then either [`Handshake::write_message`] or
//! drop the handshake. Dropping it sends nothing; nor does any failure.
//!
//! Within the window, Noise can't tell a replayed message 1. The answer is
//! useless to whoever replayed it, but answering shows this device is in
//! range, so the link layer keeps a cache of message 1 ephemerals it has
//! answered ([`Handshake::remote_ephemeral`]) for the window and refuses a
//! repeat. It must add an ephemeral only after `read_message` succeeded and
//! the initiator is one of its contacts; otherwise anyone who knows this
//! device's public key could fill the cache with valid message 1s and push
//! real entries out.
//!
//! The contact responder's view of its peer is unconfirmed until the first
//! transport message from the initiator decrypts ([`Transport::confirmed`]):
//! message 1 may be a replay inside the window, or a forgery by someone
//! holding this device's static secret (IK's key-compromise impersonation).
//! Neither can read or write the session, but neither proves the initiator
//! is here now. The link layer must not show the contact as nearby before
//! then.
//!
//! The request recipient learns the requester at message 3 and never
//! reveals itself in the handshake. When it accepts, it proves who it is by
//! running a contact (IK) handshake to the requester. A certificate sent
//! inside the request session would prove nothing: certificates are public,
//! the same bytes every time, and XN does no Diffie-Hellman with the
//! recipient's static key, so anyone holding the recipient's certificate
//! could send it.
//!
//! Remote X25519 keys, static and ephemeral, are refused if they are weak
//! (low order, so a Diffie-Hellman with them is all zeros), the same rule as
//! core's `was_contributory` checks.
//!
//! # Failure
//!
//! Nothing here panics on any input. A handshake that fails at any step is
//! dead: every later call returns [`SessionError::Dead`], it no longer
//! reports a remote identity, and the caller starts a new one. A transport
//! message that fails to decrypt leaves the session as it was; the receive
//! counter only moves on success, so a replayed or reordered message never
//! decrypts.
//!
//! # Sizes
//!
//! A Noise message is at most [`MAX_MESSAGE`] (65535) bytes, so one
//! [`Transport::encrypt`] call takes at most [`MAX_PLAINTEXT`] (65519)
//! bytes of plaintext and adds a [`TAG`]-byte tag. Nothing is split here:
//! the link layer frames messages for the radio.
//!
//! # Key material in memory
//!
//! This crate's own copies of secrets are zeroized: [`SessionKeys`] holds
//! the X25519 static secret boxed in `Zeroizing`, so moving the keys moves
//! a pointer, and the Ed25519 signing key exists only, boxed, while the
//! certificate is made. (The constructors of those types may pass their
//! values through the stack on the way into the box; that is beyond this
//! crate's reach, as it is for `Identity`.) snow 0.10 does not zeroize: its
//! copies of the static secret, the ephemeral secret, the chaining key and
//! the transport keys are freed without being wiped when a handshake or
//! transport is dropped. Someone who can later read this process's memory
//! (or a core dump) might recover a finished session's keys from freed
//! heap. The fix, when it is worth it, is a small snow resolver wrapping the
//! same primitives in zeroize-on-drop types.

use std::fmt;

use bombechat_core::{Identity, PublicIdentity};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use snow::error::{Error as NoiseError, StateProblem};
use snow::{Builder, HandshakeState, TransportState};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};
use zeroize::Zeroizing;

/// The contact pattern.
pub const CONTACT_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_SHA256";

/// The request pattern.
pub const REQUEST_PATTERN: &str = "Noise_XN_25519_ChaChaPoly_SHA256";

/// The bridge pattern, exactly Bitchat's, so a bridge stays possible.
pub const BRIDGE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_SHA256";

/// The prologue of contact and request handshakes. It keeps their
/// transcripts apart from every other deployment of the same patterns.
pub const PROLOGUE: &[u8] = b"bombechat v1";

/// The largest Noise message, handshake or transport, in bytes.
pub const MAX_MESSAGE: usize = 65535;

/// The authentication tag each encrypted message carries, in bytes.
pub const TAG: usize = 16;

/// The most plaintext one [`Transport::encrypt`] call accepts, in bytes.
pub const MAX_PLAINTEXT: usize = MAX_MESSAGE - TAG;

/// The length of a static key certificate.
pub const CERTIFICATE_LEN: usize = 32 + 32 + 64;

/// The length of an advertising token.
pub const TOKEN_LEN: usize = 16;

/// The length of the contact initiator's hello in message 1: certificate,
/// sent-at time, target token.
pub const CONTACT_HELLO_LEN: usize = CERTIFICATE_LEN + 8 + TOKEN_LEN;

/// How far a contact message 1's sent-at time may be from the responder's
/// clock, either way, in milliseconds.
pub const CONTACT_WINDOW_MS: u64 = 120_000;

/// Domain separation for the static key certificate.
const CERTIFICATE_CONTEXT: &[u8] = b"bombechat static key v1";

/// Length of an X25519 public key, and of the cleartext ephemeral key that
/// starts any message carrying one.
const KEY_LEN: usize = 32;

/// Why a session step failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionError {
    /// The call does not fit where the handshake is: writing when it is the
    /// other side's turn, reading when it is ours, or either once finished.
    OutOfTurn,
    /// `into_transport` was called before the handshake finished.
    NotFinished,
    /// An earlier step of this handshake failed.
    Dead,
    /// The bytes do not have the shape of a session message.
    Malformed,
    /// Authenticated decryption failed: the message was altered, replayed,
    /// reordered, or belongs to another session or another key.
    Tampered,
    /// The peer's certificate signature did not verify.
    BadSignature,
    /// The peer's Noise static key is not the X25519 key its certificate
    /// names.
    KeyMismatch,
    /// The peer proved an identity other than the one this session was
    /// opened to.
    UnexpectedPeer,
    /// A key is low order, so a Diffie-Hellman with it gives nothing secret.
    WeakKey,
    /// A contact message 1 was sent more than [`CONTACT_WINDOW_MS`] from
    /// this device's clock: a replay, or a clock too far off.
    Stale,
    /// A contact message 1 was meant for a device advertising another
    /// token: relayed here from elsewhere.
    WrongTarget,
    /// The plaintext is longer than [`MAX_PLAINTEXT`].
    TooLarge,
    /// The session's message counter is used up.
    Exhausted,
    /// The system's random number source failed.
    Random,
    /// The Noise library refused a step for a reason none of the above
    /// covers.
    Noise,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::OutOfTurn => write!(
                f,
                "the handshake is not at a step where this side can do that"
            ),
            SessionError::NotFinished => write!(f, "the handshake has not finished yet"),
            SessionError::Dead => write!(
                f,
                "an earlier step of this handshake failed, so start a new one"
            ),
            SessionError::Malformed => {
                write!(f, "the message is not a well-formed session message")
            }
            SessionError::Tampered => write!(
                f,
                "the message failed authentication: it was altered, replayed or is not from this session"
            ),
            SessionError::BadSignature => {
                write!(f, "the peer's key certificate did not verify")
            }
            SessionError::KeyMismatch => {
                write!(f, "the peer's Noise key is not the key its certificate names")
            }
            SessionError::UnexpectedPeer => {
                write!(f, "the peer is not the identity this session was opened to")
            }
            SessionError::WeakKey => {
                write!(f, "a key is weak and would leave the session readable")
            }
            SessionError::Stale => write!(
                f,
                "the message was sent more than two minutes from this device's clock"
            ),
            SessionError::WrongTarget => {
                write!(f, "the message was meant for another device")
            }
            SessionError::TooLarge => write!(
                f,
                "the plaintext is longer than the {MAX_PLAINTEXT} bytes one session message carries"
            ),
            SessionError::Exhausted => write!(
                f,
                "the session has used up its message counter, so start a new one"
            ),
            SessionError::Random => write!(f, "the system's random number source failed"),
            SessionError::Noise => write!(f, "the Noise library refused the step"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<NoiseError> for SessionError {
    fn from(e: NoiseError) -> SessionError {
        match e {
            NoiseError::Decrypt => SessionError::Tampered,
            NoiseError::Input | NoiseError::Dh => SessionError::Malformed,
            NoiseError::Rng => SessionError::Random,
            NoiseError::State(StateProblem::Exhausted) => SessionError::Exhausted,
            NoiseError::State(
                StateProblem::NotTurnToRead
                | StateProblem::NotTurnToWrite
                | StateProblem::HandshakeAlreadyFinished,
            ) => SessionError::OutOfTurn,
            NoiseError::State(StateProblem::HandshakeNotFinished) => SessionError::NotFinished,
            _ => SessionError::Noise,
        }
    }
}

/// This identity's static key certificate: `sign ‖ dh ‖ signature`, the
/// signature over `"bombechat static key v1" ‖ dh`. The same bytes every
/// time for the same identity.
pub fn certificate(me: &Identity) -> [u8; CERTIFICATE_LEN] {
    let public = me.public();
    let secret = me.secret_bytes();
    let mut seed = Box::new(Zeroizing::new([0u8; 32]));
    seed.copy_from_slice(&secret[0..32]);
    // Lives only here, boxed so it isn't copied about, and is zeroized when
    // dropped at the end of this function.
    let signing = Box::new(SigningKey::from_bytes(&seed));
    let signature = signing.sign(&certificate_message(&public.dh));
    let mut out = [0u8; CERTIFICATE_LEN];
    out[0..32].copy_from_slice(&public.sign);
    out[32..64].copy_from_slice(&public.dh);
    out[64..].copy_from_slice(&signature.to_bytes());
    out
}

fn certificate_message(dh: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(CERTIFICATE_CONTEXT.len() + dh.len());
    m.extend_from_slice(CERTIFICATE_CONTEXT);
    m.extend_from_slice(dh);
    m
}

/// This device's keys for sessions, made once per identity and reused for
/// every handshake: its X25519 static secret (zeroized on drop), its public
/// identity and its certificate.
pub struct SessionKeys {
    static_secret: Box<Zeroizing<[u8; 32]>>,
    public: PublicIdentity,
    certificate: [u8; CERTIFICATE_LEN],
}

impl SessionKeys {
    /// Make the keys for `me`, signing its certificate once.
    pub fn new(me: &Identity) -> SessionKeys {
        let secret = me.secret_bytes();
        let mut static_secret = Box::new(Zeroizing::new([0u8; 32]));
        static_secret.copy_from_slice(&secret[32..64]);
        SessionKeys {
            static_secret,
            public: me.public(),
            certificate: certificate(me),
        }
    }

    /// The identity these keys prove.
    pub fn public(&self) -> &PublicIdentity {
        &self.public
    }

    /// The certificate every handshake sends.
    pub fn certificate(&self) -> &[u8; CERTIFICATE_LEN] {
        &self.certificate
    }
}

/// Verify a certificate against the static key Noise presented and return
/// the identity it names.
fn check_certificate(
    certificate: &[u8],
    remote_static: &[u8],
) -> Result<PublicIdentity, SessionError> {
    if certificate.len() != CERTIFICATE_LEN {
        return Err(SessionError::Malformed);
    }
    let (sign, rest) = certificate.split_at(32);
    let (dh, signature) = rest.split_at(32);
    let sign: [u8; 32] = sign.try_into().map_err(|_| SessionError::Malformed)?;
    let dh: [u8; 32] = dh.try_into().map_err(|_| SessionError::Malformed)?;
    let signature: [u8; 64] = signature.try_into().map_err(|_| SessionError::Malformed)?;

    if remote_static != dh.as_slice() {
        return Err(SessionError::KeyMismatch);
    }
    let key = VerifyingKey::from_bytes(&sign).map_err(|_| SessionError::BadSignature)?;
    key.verify_strict(
        &certificate_message(&dh),
        &Signature::from_bytes(&signature),
    )
    .map_err(|_| SessionError::BadSignature)?;
    Ok(PublicIdentity { sign, dh })
}

/// Whether an X25519 public key is weak: the wrong length, or low order so
/// that a Diffie-Hellman with it is all zeros whatever the secret. Any
/// clamped scalar is a multiple of the cofactor, so one probe decides it.
fn is_weak(key: &[u8]) -> bool {
    let Ok(bytes) = <[u8; KEY_LEN]>::try_from(key) else {
        return true;
    };
    let probe = StaticSecret::from([0x5a; 32]);
    !probe
        .diffie_hellman(&X25519PublicKey::from(bytes))
        .was_contributory()
}

/// Opens handshakes. Holds nothing itself.
pub struct Session;

impl Session {
    /// A session with a contact whose identity this side already holds
    /// (`Noise_IK`). The first message carries this side's hello, encrypted
    /// to `them`: a device that isn't `them` can't read it. `now_ms` is
    /// this device's clock (Unix ms) and `their_token` the advertising
    /// token of the device this message 1 is for.
    pub fn contact_initiator(
        me: &SessionKeys,
        them: &PublicIdentity,
        now_ms: u64,
        their_token: [u8; TOKEN_LEN],
    ) -> Handshake {
        Handshake::new(
            Kind::Contact,
            Role::Initiator,
            Some(me),
            Some(them.clone()),
            Freshness::Send {
                now_ms,
                token: their_token,
            },
            None,
        )
    }

    /// Answer a contact session. `now_ms` is this device's clock (Unix ms)
    /// and `my_tokens` its current and previous advertising tokens; a
    /// message 1 sent at another time or to another token fails with
    /// [`SessionError::Stale`] or [`SessionError::WrongTarget`]. After
    /// reading message 1, look at [`Handshake::remote_identity`] and either
    /// write message 2 or drop the handshake, which sends nothing.
    ///
    /// The initiator named there is unconfirmed: message 1 may be a replay
    /// within the window, or forged by someone holding this device's static
    /// secret. It is confirmed once the first transport message from it
    /// decrypts ([`Transport::confirmed`]).
    pub fn contact_responder(
        me: &SessionKeys,
        now_ms: u64,
        my_tokens: &[[u8; TOKEN_LEN]],
    ) -> Handshake {
        Handshake::new(
            Kind::Contact,
            Role::Responder,
            Some(me),
            None,
            Freshness::Check {
                now_ms,
                tokens: my_tokens.to_vec(),
            },
            None,
        )
    }

    /// Send a request to connect to a device whose identity isn't known
    /// (`Noise_XN`). This side's identity reaches the recipient in message
    /// 3; the recipient's never comes back in the handshake. If the
    /// recipient accepts, it proves who it is with a contact handshake to
    /// this side; nothing said inside this session proves it.
    pub fn request_initiator(me: &SessionKeys) -> Handshake {
        Handshake::new(
            Kind::Request,
            Role::Initiator,
            Some(me),
            None,
            Freshness::None,
            None,
        )
    }

    /// Receive a request to connect. Uses no identity: this side stays
    /// anonymous throughout the handshake, and learns the requester's
    /// identity from message 3.
    pub fn request_responder() -> Handshake {
        Handshake::new(
            Kind::Request,
            Role::Responder,
            None,
            None,
            Freshness::None,
            None,
        )
    }

    /// A Bitchat-pattern session (`Noise_XX`), kept for a future bridge
    /// and not used by the link layer. With `expected`, the handshake fails
    /// with [`SessionError::UnexpectedPeer`] unless the responder proves
    /// exactly that identity.
    pub fn bridge_initiator(me: &SessionKeys, expected: Option<&PublicIdentity>) -> Handshake {
        Handshake::new(
            Kind::Bridge,
            Role::Initiator,
            Some(me),
            expected.cloned(),
            Freshness::None,
            None,
        )
    }

    /// Answer a Bitchat-pattern session. Reveals this side's identity to
    /// whoever sends message 1.
    pub fn bridge_responder(me: &SessionKeys) -> Handshake {
        Handshake::new(
            Kind::Bridge,
            Role::Responder,
            Some(me),
            None,
            Freshness::None,
            None,
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Contact,
    Request,
    Bridge,
}

impl Kind {
    fn pattern(self) -> &'static str {
        match self {
            Kind::Contact => CONTACT_PATTERN,
            Kind::Request => REQUEST_PATTERN,
            Kind::Bridge => BRIDGE_PATTERN,
        }
    }

    fn messages(self) -> u8 {
        match self {
            Kind::Contact => 2,
            Kind::Request | Kind::Bridge => 3,
        }
    }

    /// Whether message `step` (0-based) carries its sender's static key,
    /// and so its certificate.
    fn sends_static(self, step: u8) -> bool {
        match self {
            Kind::Contact => step == 0,
            Kind::Request => step == 2,
            Kind::Bridge => step == 1 || step == 2,
        }
    }
}

/// The contact hello's time and target, on whichever side.
enum Freshness {
    /// Not a contact handshake.
    None,
    /// Contact initiator: what message 1 says.
    Send { now_ms: u64, token: [u8; TOKEN_LEN] },
    /// Contact responder: what message 1 must match.
    Check {
        now_ms: u64,
        tokens: Vec<[u8; TOKEN_LEN]>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Initiator,
    Responder,
}

/// A handshake in progress. Messages go out through
/// [`Handshake::write_message`] and come in through
/// [`Handshake::read_message`], then [`Handshake::into_transport`] gives
/// the session.
pub struct Handshake {
    /// `Err` once any step has failed (or the handshake could not be
    /// built); every later call then fails with [`SessionError::Dead`].
    noise: Result<Box<HandshakeState>, SessionError>,
    kind: Kind,
    role: Role,
    /// Messages written or read so far.
    step: u8,
    /// This side's certificate; `None` for the request recipient, which
    /// has no static key.
    certificate: Option<[u8; CERTIFICATE_LEN]>,
    /// The contact a contact initiator dials, or the identity a bridge
    /// initiator insists on.
    expected: Option<PublicIdentity>,
    freshness: Freshness,
    remote: Option<PublicIdentity>,
    /// The peer's ephemeral key, once a message carrying it has been read.
    remote_ephemeral: Option<[u8; KEY_LEN]>,
}

impl Handshake {
    /// `ephemeral` fixes this side's ephemeral secret, for the transcript
    /// tests only; everything else passes `None` and gets a fresh one.
    fn new(
        kind: Kind,
        role: Role,
        me: Option<&SessionKeys>,
        expected: Option<PublicIdentity>,
        freshness: Freshness,
        ephemeral: Option<&[u8; 32]>,
    ) -> Handshake {
        let remote_static = match (kind, role, &expected) {
            (Kind::Contact, Role::Initiator, Some(them)) => Some(them.dh),
            _ => None,
        };
        let noise = match remote_static {
            Some(key) if is_weak(&key) => Err(SessionError::WeakKey),
            _ => build(kind, role, me, remote_static.as_ref(), ephemeral),
        };
        Handshake {
            noise,
            kind,
            role,
            step: 0,
            certificate: me.map(|me| me.certificate),
            expected,
            freshness,
            remote: None,
            remote_ephemeral: None,
        }
    }

    /// The next handshake message to send.
    pub fn write_message(&mut self) -> Result<Vec<u8>, SessionError> {
        let my_turn = match &self.noise {
            Err(_) => return Err(SessionError::Dead),
            Ok(noise) => noise.is_my_turn() && !noise.is_handshake_finished(),
        };
        if !my_turn {
            return Err(SessionError::OutOfTurn);
        }
        let result = self.write_step();
        self.settle(result)
    }

    /// Take in the peer's next handshake message. Any failure ends the
    /// handshake.
    pub fn read_message(&mut self, message: &[u8]) -> Result<(), SessionError> {
        let their_turn = match &self.noise {
            Err(_) => return Err(SessionError::Dead),
            Ok(noise) => !noise.is_my_turn() && !noise.is_handshake_finished(),
        };
        if !their_turn {
            return Err(SessionError::OutOfTurn);
        }
        let result = self.read_step(message);
        self.settle(result)
    }

    /// Whether every message has passed and the peer has proved what it
    /// has to: its identity, except for the request recipient, which
    /// proves only that it holds the session's keys.
    pub fn is_finished(&self) -> bool {
        let finished = matches!(&self.noise, Ok(noise) if noise.is_handshake_finished());
        let anonymous_peer = self.kind == Kind::Request && self.role == Role::Initiator;
        finished && (self.remote.is_some() || anonymous_peer)
    }

    /// The peer's identity, once it has proved it. Contact: after message
    /// 1 for the responder (before it answers), after message 2 for the
    /// initiator. Request: after message 3 for the recipient, never for
    /// the requester. Bridge: after message 2 for the initiator, after
    /// message 3 for the responder. Always `None` once the handshake has
    /// failed.
    ///
    /// For the contact responder this identity is unconfirmed until the
    /// first transport message from it decrypts: see
    /// [`Transport::confirmed`].
    pub fn remote_identity(&self) -> Option<PublicIdentity> {
        self.remote.clone()
    }

    /// The peer's ephemeral key, once a message carrying it (message 1 or
    /// 2) has been read successfully; `None` once the handshake has failed.
    /// The contact responder's link layer keys its seen cache on it. Add it
    /// to the cache only after `read_message` succeeded and the initiator
    /// is a contact.
    pub fn remote_ephemeral(&self) -> Option<[u8; KEY_LEN]> {
        self.remote_ephemeral
    }

    /// The session's transport ciphers, once the handshake has finished.
    pub fn into_transport(self) -> Result<Transport, SessionError> {
        if !self.is_finished() {
            return Err(match self.noise {
                Err(_) => SessionError::Dead,
                Ok(_) => SessionError::NotFinished,
            });
        }
        let Ok(noise) = self.noise else {
            return Err(SessionError::Dead);
        };
        // Every side but the contact responder has already seen its peer
        // use a fresh ephemeral key of this session; the contact responder
        // waits for the first transport message.
        let confirmed = !(self.kind == Kind::Contact && self.role == Role::Responder);
        Ok(Transport {
            noise: Box::new(noise.into_transport_mode()?),
            remote: self.remote,
            confirmed,
        })
    }

    /// Mark the handshake dead on failure.
    fn settle<T>(&mut self, result: Result<T, SessionError>) -> Result<T, SessionError> {
        if let Err(e) = result {
            self.noise = Err(e);
            self.remote = None;
            self.remote_ephemeral = None;
        }
        result
    }

    fn write_step(&mut self) -> Result<Vec<u8>, SessionError> {
        if self.step >= self.kind.messages() {
            return Err(SessionError::OutOfTurn);
        }
        let mut payload = Vec::new();
        if self.kind.sends_static(self.step) {
            let certificate = self.certificate.as_ref().ok_or(SessionError::Noise)?;
            payload.extend_from_slice(certificate);
            if let Freshness::Send { now_ms, token } = &self.freshness {
                payload.extend_from_slice(&now_ms.to_be_bytes());
                payload.extend_from_slice(token);
            }
        }
        let noise = self.noise.as_mut().map_err(|_| SessionError::Dead)?;
        let mut out = vec![0u8; MAX_MESSAGE];
        let len = noise.write_message(&payload, &mut out)?;
        out.truncate(len);
        self.step += 1;
        Ok(out)
    }

    fn read_step(&mut self, message: &[u8]) -> Result<(), SessionError> {
        if message.len() > MAX_MESSAGE || self.step >= self.kind.messages() {
            return Err(SessionError::Malformed);
        }
        // Messages 1 and 2 of every pattern open with the sender's
        // ephemeral key in the clear. Refuse a weak one before snow mixes
        // it into anything.
        let mut ephemeral = None;
        if self.step < 2 {
            let key = message.get(..KEY_LEN).ok_or(SessionError::Malformed)?;
            if is_weak(key) {
                return Err(SessionError::WeakKey);
            }
            ephemeral = <[u8; KEY_LEN]>::try_from(key).ok();
        }
        let noise = self.noise.as_mut().map_err(|_| SessionError::Dead)?;
        let mut payload = vec![0u8; MAX_MESSAGE];
        let len = noise.read_message(message, &mut payload)?;
        let payload = payload.get(..len).ok_or(SessionError::Malformed)?;

        if self.kind.sends_static(self.step) {
            let remote_static = noise.get_remote_static().ok_or(SessionError::Malformed)?;
            if is_weak(remote_static) {
                return Err(SessionError::WeakKey);
            }
            let remote = match &self.freshness {
                Freshness::Check { now_ms, tokens } => {
                    if payload.len() != CONTACT_HELLO_LEN {
                        return Err(SessionError::Malformed);
                    }
                    let (certificate, rest) = payload.split_at(CERTIFICATE_LEN);
                    let (sent_at, token) = rest.split_at(8);
                    let sent_at: [u8; 8] =
                        sent_at.try_into().map_err(|_| SessionError::Malformed)?;
                    let remote = check_certificate(certificate, remote_static)?;
                    if u64::from_be_bytes(sent_at).abs_diff(*now_ms) > CONTACT_WINDOW_MS {
                        return Err(SessionError::Stale);
                    }
                    if !tokens.iter().any(|t| t.as_slice() == token) {
                        return Err(SessionError::WrongTarget);
                    }
                    remote
                }
                _ => check_certificate(payload, remote_static)?,
            };
            if let Some(expected) = &self.expected {
                if *expected != remote {
                    return Err(SessionError::UnexpectedPeer);
                }
            }
            self.remote = Some(remote);
        } else {
            if !payload.is_empty() {
                return Err(SessionError::Malformed);
            }
            // The contact initiator dialled a known identity; message 2
            // decrypting proves the responder holds that identity's static
            // secret (es and se are both in the keys).
            if self.kind == Kind::Contact && self.role == Role::Initiator {
                self.remote = self.expected.clone();
            }
        }
        if ephemeral.is_some() {
            self.remote_ephemeral = ephemeral;
        }
        self.step += 1;
        Ok(())
    }
}

/// Build the Noise state for one side.
fn build(
    kind: Kind,
    role: Role,
    me: Option<&SessionKeys>,
    remote_static: Option<&[u8; 32]>,
    ephemeral: Option<&[u8; 32]>,
) -> Result<Box<HandshakeState>, SessionError> {
    let params = kind.pattern().parse().map_err(|_| SessionError::Noise)?;
    let mut builder = Builder::new(params);
    if let Some(me) = me {
        builder = builder.local_private_key(&me.static_secret[..])?;
    }
    if kind != Kind::Bridge {
        builder = builder.prologue(PROLOGUE)?;
    }
    if let Some(key) = remote_static {
        builder = builder.remote_public_key(key)?;
    }
    if let Some(ephemeral) = ephemeral {
        builder = builder.fixed_ephemeral_key_for_testing_only(ephemeral);
    }
    let state = match role {
        Role::Initiator => builder.build_initiator()?,
        Role::Responder => builder.build_responder()?,
    };
    Ok(Box::new(state))
}

/// An established session: one cipher each way, with implicit counters.
/// Messages must be decrypted in the order they were encrypted.
pub struct Transport {
    noise: Box<TransportState>,
    remote: Option<PublicIdentity>,
    confirmed: bool,
}

impl Transport {
    /// Encrypt one message of at most [`MAX_PLAINTEXT`] bytes. The result
    /// is [`TAG`] bytes longer than `plaintext`.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, SessionError> {
        if plaintext.len() > MAX_PLAINTEXT {
            return Err(SessionError::TooLarge);
        }
        let mut out = vec![0u8; plaintext.len() + TAG];
        let len = self.noise.write_message(plaintext, &mut out)?;
        out.truncate(len);
        Ok(out)
    }

    /// Decrypt the next message from the peer. A failure leaves the session
    /// unchanged, so the next genuine message still decrypts.
    pub fn decrypt(&mut self, message: &[u8]) -> Result<Vec<u8>, SessionError> {
        if message.len() < TAG || message.len() > MAX_MESSAGE {
            return Err(SessionError::Malformed);
        }
        let mut out = vec![0u8; message.len() - TAG];
        let len = self.noise.read_message(message, &mut out)?;
        out.truncate(len);
        self.confirmed = true;
        Ok(out)
    }

    /// Who is on the other end, as proved in the handshake. `None` only for
    /// a requester, whose recipient stays anonymous; if it accepts, it
    /// proves who it is with a contact handshake, not inside this session.
    /// For a contact responder, unconfirmed until [`Transport::confirmed`].
    pub fn remote_identity(&self) -> Option<&PublicIdentity> {
        self.remote.as_ref()
    }

    /// Whether the peer is known to be live in this session: true once a
    /// transport message from it has decrypted. Every side but the contact
    /// responder starts confirmed, having already decrypted a handshake
    /// message keyed to this session's fresh ephemerals (the contact
    /// initiator after message 2). The contact responder starts
    /// unconfirmed, because its message 1 may be a replay or a
    /// key-compromise forgery, and must not show the contact as nearby
    /// until this is true.
    pub fn confirmed(&self) -> bool {
        self.confirmed
    }
}

#[cfg(test)]
mod transcript;

#[cfg(test)]
mod tests {
    use super::*;

    /// A failure after the peer has proved itself still leaves no remote
    /// identity behind. (No outside input can fail a step at that point,
    /// so the failure is injected.)
    #[test]
    fn a_failure_clears_the_remote_identity() {
        let alice = SessionKeys::new(&Identity::generate());
        let bob = SessionKeys::new(&Identity::generate());
        let mut a = Session::contact_initiator(&alice, bob.public(), 0, [0; TOKEN_LEN]);
        let mut b = Session::contact_responder(&bob, 0, &[[0; TOKEN_LEN]]);
        b.read_message(&a.write_message().unwrap()).unwrap();
        assert_eq!(b.remote_identity().as_ref(), Some(alice.public()));

        assert_eq!(
            b.settle::<()>(Err(SessionError::Noise)),
            Err(SessionError::Noise)
        );
        assert_eq!(b.remote_identity(), None);
        assert_eq!(b.remote_ephemeral(), None);
        assert_eq!(b.write_message(), Err(SessionError::Dead));
        assert!(!b.is_finished());
    }
}
