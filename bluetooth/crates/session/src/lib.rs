//! bombechat-session: live encrypted sessions between two devices, for the
//! Bluetooth link.
//!
//! A session is a Noise handshake, `Noise_XX_25519_ChaChaPoly_SHA256` (the
//! pattern Bitchat uses for live sessions), followed by a pair of transport
//! ciphers. Each side's Noise static key is its identity's X25519 key.
//!
//! # Handshake
//!
//! ```text
//! 1. initiator -> responder   e                    payload: empty
//! 2. responder -> initiator   e, ee, s, es         payload: proof
//! 3. initiator -> responder   s, se                payload: proof
//! ```
//!
//! Noise alone proves each side holds the X25519 secret it presented. The
//! proof adds the Ed25519 half, so the remote becomes a whole
//! [`PublicIdentity`] that can be matched against contacts and the safety
//! number:
//!
//! ```text
//! proof:  sign [32] ‖ dh [32] ‖ Ed25519 signature [64]       (128 bytes)
//! signed: "bombechat session v1" ‖ role u8 ‖ h [32] ‖ dh [32]
//! ```
//!
//! `h` is the Noise handshake hash just before the message carrying the
//! proof, which both sides know at that point, and `role` is 1 for the
//! responder's proof (message 2) and 2 for the initiator's (message 3).
//! `h` covers both sides' fresh ephemeral keys, so a proof can't be replayed
//! into another session, and `dh` binds the signature to the Noise static
//! key. A reader rejects a proof whose `dh` is not the static key Noise
//! presented, whose signature does not verify, or, for an initiator opened
//! to a known contact, whose identity is not that contact.
//!
//! # Failure
//!
//! Nothing here panics on any input. A handshake that fails at any step is
//! dead: every later call returns [`SessionError::Dead`], and the caller
//! starts a new one. A transport message that fails to decrypt leaves the
//! session as it was; the receive counter only moves on success, so a
//! replayed or reordered message never decrypts.
//!
//! # Sizes
//!
//! A Noise message is at most [`MAX_MESSAGE`] (65535) bytes, so one
//! [`Transport::encrypt`] call takes at most [`MAX_PLAINTEXT`] (65519)
//! bytes of plaintext and adds a [`TAG`]-byte tag. Nothing is split here:
//! the link layer frames messages for the radio.

use std::fmt;

use bombechat_core::{Identity, PublicIdentity};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use snow::error::{Error as NoiseError, StateProblem};
use snow::{Builder, HandshakeState, TransportState};
use zeroize::Zeroizing;

/// The Noise protocol every session runs. Kept exactly Bitchat's, so a
/// bridge between the two stays possible.
pub const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_SHA256";

/// The largest Noise message, handshake or transport, in bytes.
pub const MAX_MESSAGE: usize = 65535;

/// The authentication tag each encrypted message carries, in bytes.
pub const TAG: usize = 16;

/// The most plaintext one [`Transport::encrypt`] call accepts, in bytes.
pub const MAX_PLAINTEXT: usize = MAX_MESSAGE - TAG;

/// Domain separation for the identity proofs in messages 2 and 3.
const CONTEXT: &[u8] = b"bombechat session v1";

/// `sign [32] ‖ dh [32] ‖ signature [64]`.
const PROOF_LEN: usize = 32 + 32 + 64;

/// The handshake hash length for SHA-256.
const HASH_LEN: usize = 32;

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
    /// reordered, or belongs to another session.
    Tampered,
    /// The peer's Ed25519 signature over the handshake did not verify.
    BadSignature,
    /// The peer's Noise static key is not the X25519 key its proof names.
    KeyMismatch,
    /// The peer proved an identity other than the one this session was
    /// opened to.
    UnexpectedPeer,
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
            SessionError::OutOfTurn => {
                write!(f, "the handshake is not at a step where this side can do that")
            }
            SessionError::NotFinished => write!(f, "the handshake has not finished yet"),
            SessionError::Dead => {
                write!(f, "an earlier step of this handshake failed, so start a new one")
            }
            SessionError::Malformed => write!(f, "the message is not a well-formed session message"),
            SessionError::Tampered => write!(
                f,
                "the message failed authentication: it was altered, replayed or is not from this session"
            ),
            SessionError::BadSignature => {
                write!(f, "the peer's signature over the handshake did not verify")
            }
            SessionError::KeyMismatch => {
                write!(f, "the peer's Noise key is not the key its identity names")
            }
            SessionError::UnexpectedPeer => {
                write!(f, "the peer is not the identity this session was opened to")
            }
            SessionError::TooLarge => write!(
                f,
                "the plaintext is longer than the {MAX_PLAINTEXT} bytes one session message carries"
            ),
            SessionError::Exhausted => {
                write!(f, "the session has used up its message counter, so start a new one")
            }
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

/// Opens handshakes. Holds nothing itself.
pub struct Session;

impl Session {
    /// Start a handshake as the side that speaks first. With `expected`,
    /// the handshake fails with [`SessionError::UnexpectedPeer`] unless
    /// the responder proves exactly that identity.
    pub fn initiator(me: &Identity, expected: Option<&PublicIdentity>) -> Handshake {
        Handshake::new(me, Role::Initiator, expected.cloned(), None)
    }

    /// Start a handshake as the side that answers. Whoever it turns out to
    /// be is in [`Handshake::remote_identity`] once it has proved itself.
    pub fn responder(me: &Identity) -> Handshake {
        Handshake::new(me, Role::Responder, None, None)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Initiator,
    Responder,
}

/// The role byte in the signed proof: whose proof it is.
fn proof_role(prover: Role) -> u8 {
    match prover {
        Role::Responder => 1,
        Role::Initiator => 2,
    }
}

/// The bytes a proof signs.
fn proof_message(prover: Role, hash: &[u8], dh: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(CONTEXT.len() + 1 + hash.len() + dh.len());
    m.extend_from_slice(CONTEXT);
    m.push(proof_role(prover));
    m.extend_from_slice(hash);
    m.extend_from_slice(dh);
    m
}

/// A handshake in progress. Messages go out through
/// [`Handshake::write_message`] and come in through
/// [`Handshake::read_message`], three in all, then
/// [`Handshake::into_transport`] gives the session.
pub struct Handshake {
    /// `Err` once any step has failed (or the handshake could not be
    /// built); every later call then fails with [`SessionError::Dead`].
    noise: Result<Box<HandshakeState>, SessionError>,
    role: Role,
    /// Messages written or read so far, 0 to 3.
    step: u8,
    /// Taken (and so dropped and zeroized) once this side's proof is out.
    signing: Option<SigningKey>,
    me: PublicIdentity,
    expected: Option<PublicIdentity>,
    remote: Option<PublicIdentity>,
}

impl Handshake {
    /// `ephemeral` fixes this side's ephemeral secret, for the transcript
    /// test only; everything else passes `None` and gets a fresh one.
    fn new(
        me: &Identity,
        role: Role,
        expected: Option<PublicIdentity>,
        ephemeral: Option<&[u8; 32]>,
    ) -> Handshake {
        let secret = me.secret_bytes();
        let mut seed = Zeroizing::new([0u8; 32]);
        seed.copy_from_slice(&secret[0..32]);
        let noise = build(&secret[32..64], role, ephemeral);
        Handshake {
            noise,
            role,
            step: 0,
            signing: Some(SigningKey::from_bytes(&seed)),
            me: me.public(),
            expected,
            remote: None,
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

    /// Whether all three messages have passed and the peer has proved its
    /// identity.
    pub fn is_finished(&self) -> bool {
        matches!(&self.noise, Ok(noise) if noise.is_handshake_finished()) && self.remote.is_some()
    }

    /// The peer's identity, once it has proved it: after message 2 for the
    /// initiator, after message 3 for the responder.
    pub fn remote_identity(&self) -> Option<PublicIdentity> {
        self.remote.clone()
    }

    /// The session's transport ciphers, once the handshake has finished.
    pub fn into_transport(self) -> Result<Transport, SessionError> {
        if !self.is_finished() {
            return Err(match self.noise {
                Err(_) => SessionError::Dead,
                Ok(_) => SessionError::NotFinished,
            });
        }
        let (Ok(noise), Some(remote)) = (self.noise, self.remote) else {
            return Err(SessionError::NotFinished);
        };
        Ok(Transport {
            noise: Box::new(noise.into_transport_mode()?),
            remote,
        })
    }

    /// Mark the handshake dead on failure.
    fn settle<T>(&mut self, result: Result<T, SessionError>) -> Result<T, SessionError> {
        if let Err(e) = result {
            self.noise = Err(e);
            self.signing = None;
        }
        result
    }

    fn write_step(&mut self) -> Result<Vec<u8>, SessionError> {
        let noise = self.noise.as_mut().map_err(|_| SessionError::Dead)?;
        let payload = match (self.step, self.role) {
            (0, Role::Initiator) => Vec::new(),
            (1, Role::Responder) | (2, Role::Initiator) => {
                let signing = self.signing.take().ok_or(SessionError::Noise)?;
                let signed = proof_message(self.role, noise.get_handshake_hash(), &self.me.dh);
                let signature = signing.sign(&signed);
                let mut proof = Vec::with_capacity(PROOF_LEN);
                proof.extend_from_slice(&self.me.sign);
                proof.extend_from_slice(&self.me.dh);
                proof.extend_from_slice(&signature.to_bytes());
                proof
            }
            _ => return Err(SessionError::OutOfTurn),
        };
        let mut out = vec![0u8; MAX_MESSAGE];
        let len = noise.write_message(&payload, &mut out)?;
        out.truncate(len);
        self.step += 1;
        if self.step == 3 {
            self.signing = None;
        }
        Ok(out)
    }

    fn read_step(&mut self, message: &[u8]) -> Result<(), SessionError> {
        if message.len() > MAX_MESSAGE {
            return Err(SessionError::Malformed);
        }
        let noise = self.noise.as_mut().map_err(|_| SessionError::Dead)?;
        let hash_before = noise.get_handshake_hash().to_vec();
        let mut payload = vec![0u8; MAX_MESSAGE];
        let len = noise.read_message(message, &mut payload)?;
        let payload = payload.get(..len).ok_or(SessionError::Malformed)?;

        match (self.step, self.role) {
            (0, Role::Responder) => {
                if !payload.is_empty() {
                    return Err(SessionError::Malformed);
                }
            }
            (1, Role::Initiator) | (2, Role::Responder) => {
                let prover = match self.role {
                    Role::Initiator => Role::Responder,
                    Role::Responder => Role::Initiator,
                };
                let remote_static = noise.get_remote_static().ok_or(SessionError::Malformed)?;
                let remote = check_proof(prover, &hash_before, remote_static, payload)?;
                if let Some(expected) = &self.expected {
                    if *expected != remote {
                        return Err(SessionError::UnexpectedPeer);
                    }
                }
                self.remote = Some(remote);
            }
            _ => return Err(SessionError::OutOfTurn),
        }
        self.step += 1;
        Ok(())
    }
}

/// Verify a proof against the Noise static key the peer presented and the
/// handshake hash before its message, and return the identity it proves.
fn check_proof(
    prover: Role,
    hash_before: &[u8],
    remote_static: &[u8],
    proof: &[u8],
) -> Result<PublicIdentity, SessionError> {
    if proof.len() != PROOF_LEN || hash_before.len() != HASH_LEN {
        return Err(SessionError::Malformed);
    }
    let (sign, rest) = proof.split_at(32);
    let (dh, signature) = rest.split_at(32);
    let sign: [u8; 32] = sign.try_into().map_err(|_| SessionError::Malformed)?;
    let dh: [u8; 32] = dh.try_into().map_err(|_| SessionError::Malformed)?;
    let signature: [u8; 64] = signature.try_into().map_err(|_| SessionError::Malformed)?;

    if remote_static != dh.as_slice() {
        return Err(SessionError::KeyMismatch);
    }
    let key = VerifyingKey::from_bytes(&sign).map_err(|_| SessionError::BadSignature)?;
    key.verify_strict(
        &proof_message(prover, hash_before, &dh),
        &Signature::from_bytes(&signature),
    )
    .map_err(|_| SessionError::BadSignature)?;
    Ok(PublicIdentity { sign, dh })
}

/// Build the Noise state for one side.
fn build(
    static_secret: &[u8],
    role: Role,
    ephemeral: Option<&[u8; 32]>,
) -> Result<Box<HandshakeState>, SessionError> {
    let params = PATTERN.parse().map_err(|_| SessionError::Noise)?;
    let mut builder = Builder::new(params).local_private_key(static_secret)?;
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
    remote: PublicIdentity,
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
        Ok(out)
    }

    /// Who is on the other end, as proved in the handshake.
    pub fn remote_identity(&self) -> &PublicIdentity {
        &self.remote
    }
}

#[cfg(test)]
mod transcript;
