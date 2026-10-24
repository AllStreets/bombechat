//! Sealing private messages to one or more recipients, and signing public
//! broadcasts.
//!
//! # Private envelopes
//!
//! A random 32-byte content key encrypts the inner plaintext. For every
//! recipient a fresh X25519 ephemeral key agrees a secret with the
//! recipient's `dh` key; HKDF-SHA256 (salt `ephemeral public ‖ recipient dh`,
//! info `"bombechat seal v1"`) turns it into a one-time wrap key, and
//! XChaCha20-Poly1305 wraps the content key under it. The inner plaintext is
//! encrypted with the STREAM construction (`aead::stream`, big-endian 32-bit
//! counter, XChaCha20-Poly1305) in [`CHUNK`]-byte segments so each chunk can
//! be authenticated as it arrives over a slow carrier.
//!
//! Private body layout (all integers big-endian):
//!
//! ```text
//! header:
//!   version  u8 = 1
//!   count    u8          number of recipients, at least 1
//!   count x wrap (112 bytes each):
//!     fingerprint  [8]   recipient PublicIdentity::fingerprint()
//!     ephemeral    [32]  X25519 ephemeral public key
//!     nonce        [24]  XChaCha20-Poly1305 nonce for the wrap
//!     wrapped      [48]  content key [32] + Poly1305 tag [16]
//!   stream nonce prefix [19]   XChaCha20 24-byte nonce minus the 5 bytes
//!                              STREAM BE32 uses for counter and last flag
//! chunks:
//!   ceil(inner_len / CHUNK) STREAM segments, each CHUNK bytes of
//!   ciphertext + 16-byte tag, except the last, which carries 1..=CHUNK
//!   bytes of ciphertext + 16-byte tag.
//! ```
//!
//! The header is therefore exactly `21 + 112 * count` bytes, and
//! [`split_private_body`] cuts the rest into `CHUNK + 16`-byte pieces.
//!
//! Inner plaintext:
//!
//! ```text
//! sender sign [32] ‖ sender dh [32] ‖ sent_min u32 ‖ text utf8 ‖ Ed25519 signature [64]
//! ```
//!
//! # Binding to the envelope
//!
//! The envelope fields a carrier can see (kind, id, expiry) are not
//! encrypted, so they are bound to the body in two independent ways:
//!
//! * The **context** `kind u8 ‖ id [16] ‖ expires_min u32 ‖ count u8 ‖
//!   fingerprint [8] x count` is the associated data of every key wrap and
//!   every STREAM segment. A body moved into an envelope with a different
//!   id, expiry or kind, or with its recipient list edited, fails AEAD
//!   authentication (`Tampered`) before any plaintext is released.
//! * The sender's signature covers `"bombechat private sig v1" ‖ context ‖
//!   inner plaintext before the signature`. The AEAD alone only proves the
//!   chunk came from someone holding the content key, and every recipient
//!   holds it; the signature is what proves the named sender wrote it and
//!   stops one recipient re-encrypting a forged message to the others.
//!
//! `hops` is deliberately left out of both: carriers change it in transit.
//!
//! # Public envelopes
//!
//! The body is the inner plaintext layout above, unencrypted. The signature
//! covers `"bombechat public sig v1" ‖ kind u8 ‖ id [16] ‖ expires_min u32 ‖
//! body before the signature`.
//!
//! # Expiry
//!
//! An envelope is refused once `expires_min < now_min - 1440`: 24 hours of
//! clock skew are tolerated between devices without network time.

use chacha20poly1305::aead::generic_array::GenericArray;
use chacha20poly1305::aead::stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, VerifyingKey};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey as X25519PublicKey, SharedSecret};
use zeroize::{Zeroize, Zeroizing};

use crate::envelope::{Envelope, Kind, HEADER_LEN as ENVELOPE_HEADER_LEN, MAX_ENVELOPE};
use crate::identity::{Identity, PublicIdentity};

/// Plaintext bytes per STREAM segment.
pub const CHUNK: usize = 48;

/// Private body format version.
const VERSION: u8 = 1;
/// Poly1305 tag length.
const TAG: usize = 16;
/// One STREAM segment on the wire: ciphertext plus tag.
const SEGMENT: usize = CHUNK + TAG;
const FINGERPRINT_LEN: usize = 8;
const EPHEMERAL_LEN: usize = 32;
const WRAP_NONCE_LEN: usize = 24;
const CONTENT_KEY_LEN: usize = 32;
const WRAPPED_LEN: usize = CONTENT_KEY_LEN + TAG;
/// One recipient's entry in the header.
const WRAP_LEN: usize = FINGERPRINT_LEN + EPHEMERAL_LEN + WRAP_NONCE_LEN + WRAPPED_LEN;
/// XChaCha20's 24-byte nonce minus STREAM BE32's 5-byte overhead.
const STREAM_PREFIX_LEN: usize = 19;
/// Header bytes that do not depend on the recipient count.
const HEADER_FIXED: usize = 2 + STREAM_PREFIX_LEN;
const SIGNATURE_LEN: usize = 64;
/// sender sign ‖ sender dh ‖ sent_min.
const SENDER_LEN: usize = 32 + 32 + 4;
/// The shortest possible inner plaintext: empty text.
const INNER_MIN: usize = SENDER_LEN + SIGNATURE_LEN;
/// The largest envelope body that still fits in [`MAX_ENVELOPE`].
const MAX_BODY: usize = MAX_ENVELOPE - ENVELOPE_HEADER_LEN;
/// Clock skew tolerated when checking expiry: 24 hours.
const SKEW_MIN: u32 = 1440;

/// HKDF info for the per-recipient key-wrap key. The only HKDF derivation
/// in this module.
const SEAL_INFO: &[u8] = b"bombechat seal v1";
/// Signature domain separators, so a private signature can never be
/// replayed as a public one or the other way round.
const PRIVATE_SIG_DOMAIN: &[u8] = b"bombechat private sig v1";
const PUBLIC_SIG_DOMAIN: &[u8] = b"bombechat public sig v1";

/// A successfully opened and signature-checked message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// The sender, as named inside the ciphertext. The signature has been
    /// verified under exactly this key.
    pub sender: PublicIdentity,
    pub text: String,
    /// The sender's clock, in minutes since the Unix epoch, when sealed.
    pub sent_min: u32,
}

/// Why an envelope could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// No key wrap in the header carries this identity's fingerprint.
    NotForMe,
    /// An authenticated-encryption check failed: the ciphertext, the
    /// header or the envelope it sits in has been altered.
    Tampered,
    /// The sender's Ed25519 signature did not verify.
    BadSignature,
    /// The envelope expired more than 24 hours ago.
    Expired,
    /// The bytes do not have the expected structure.
    Malformed,
}

/// One chunk as it was heard and decrypted.
///
/// * `plain`: this chunk's decrypted bytes.
/// * `heard`: this chunk's ciphertext, tag excluded.
/// * `key`: the keystream, `heard XOR plain`.
///
/// `plain` from chunks before the last is authenticated as coming from a
/// holder of the content key (the sender or any co-recipient), not yet as
/// coming from the sender. Only once the last chunk has been accepted is
/// the sender's signature verified.
pub struct ChunkView {
    pub plain: Vec<u8>,
    pub heard: Vec<u8>,
    pub key: Vec<u8>,
}

/// Opens a private envelope chunk by chunk as the chunks arrive.
///
/// Feed every chunk in order with [`ChunkOpener::next`], passing
/// `last = true` for the final one; that call also verifies the sender's
/// signature and returns `Err(BadSignature)` if it fails. Then call
/// [`ChunkOpener::finish`] to get the [`Opened`] message.
///
/// Any error poisons the opener: every later `next` returns `Malformed`
/// and the plaintext gathered so far is wiped. Start again from
/// [`ChunkOpener::start`] to retry.
///
/// Expiry is not checked here, because the opener never sees the current
/// time; callers check it on the envelope before starting (as
/// [`open_private`] does).
pub struct ChunkOpener {
    decryptor: Option<DecryptorBE32<XChaCha20Poly1305>>,
    context: Vec<u8>,
    plain: Zeroizing<Vec<u8>>,
    opened: Option<Opened>,
}

impl ChunkOpener {
    /// Parse `header` (as returned by [`split_private_body`]), find this
    /// identity's key wrap and unwrap the content key. `env_id` and
    /// `expires_min` must be the enclosing envelope's; they are part of the
    /// authenticated context.
    pub fn start(
        me: &Identity,
        env_id: [u8; 16],
        expires_min: u32,
        header: &[u8],
    ) -> Result<ChunkOpener, OpenError> {
        let header = parse_header(header)?;
        let context = private_context(&env_id, expires_min, header.fingerprints());

        let mine = me.public();
        let my_fingerprint = mine.fingerprint();
        let mut addressed = false;
        let mut content_key = None;
        for wrap in header.wraps() {
            if wrap[..FINGERPRINT_LEN] != my_fingerprint {
                continue;
            }
            addressed = true;
            if let Some(key) = unwrap_content_key(me, &mine.dh, wrap, &context) {
                content_key = Some(key);
                break;
            }
        }
        if !addressed {
            return Err(OpenError::NotForMe);
        }
        let content_key = content_key.ok_or(OpenError::Tampered)?;

        let cipher = XChaCha20Poly1305::new(GenericArray::from_slice(&content_key[..]));
        let decryptor = DecryptorBE32::from_aead(cipher, GenericArray::from_slice(header.prefix));

        Ok(ChunkOpener {
            decryptor: Some(decryptor),
            context,
            // Reserved up front so the buffer never reallocates and leaves
            // unzeroized copies of plaintext behind.
            plain: Zeroizing::new(Vec::with_capacity(MAX_ENVELOPE)),
            opened: None,
        })
    }

    /// Authenticate and decrypt the next chunk. `last` must be true for the
    /// final chunk and only for it; a wrong flag fails authentication, which
    /// is what stops a carrier truncating or extending the message. On the
    /// last chunk the sender's signature is verified as well.
    pub fn next(&mut self, chunk: &[u8], last: bool) -> Result<ChunkView, OpenError> {
        let result = self.next_inner(chunk, last);
        if result.is_err() {
            self.decryptor = None;
            self.opened = None;
            self.plain.zeroize();
        }
        result
    }

    fn next_inner(&mut self, chunk: &[u8], last: bool) -> Result<ChunkView, OpenError> {
        if self.decryptor.is_none() {
            return Err(OpenError::Malformed);
        }
        if chunk.len() <= TAG || chunk.len() > SEGMENT || (!last && chunk.len() != SEGMENT) {
            return Err(OpenError::Malformed);
        }

        let payload = Payload {
            msg: chunk,
            aad: &self.context,
        };
        let decrypted = if last {
            self.decryptor
                .take()
                .expect("checked above")
                .decrypt_last(payload)
        } else {
            self.decryptor
                .as_mut()
                .expect("checked above")
                .decrypt_next(payload)
        };
        let plain = decrypted.map_err(|_| OpenError::Tampered)?;

        self.plain.extend_from_slice(&plain);
        if last {
            self.opened = Some(verify_inner(
                &self.plain,
                PRIVATE_SIG_DOMAIN,
                &self.context,
            )?);
        }

        let heard = chunk[..chunk.len() - TAG].to_vec();
        let key = heard.iter().zip(&plain).map(|(h, p)| h ^ p).collect();
        Ok(ChunkView { plain, heard, key })
    }

    /// The opened message, once the last chunk has been accepted and its
    /// signature verified. `Malformed` if that has not happened.
    pub fn finish(self) -> Result<Opened, OpenError> {
        self.opened.ok_or(OpenError::Malformed)
    }
}

/// Seal `text` from `from` to every identity in `to`.
///
/// The envelope gets a random id and expires at `now_min + ttl_min`.
///
/// # Panics
///
/// Panics if `to` is empty or has more than 255 entries, if the sealed body
/// would not fit in [`MAX_ENVELOPE`], or if a recipient's `dh` key is a
/// low-order point (sealing to it would expose the content key). Callers
/// are expected to size messages and validate contacts beforehand.
pub fn seal_private(
    from: &Identity,
    to: &[PublicIdentity],
    text: &str,
    now_min: u32,
    ttl_min: u32,
) -> Envelope {
    assert!(
        !to.is_empty() && to.len() <= u8::MAX as usize,
        "seal_private needs between 1 and 255 recipients"
    );
    let inner_len = INNER_MIN + text.len();
    let body_len = HEADER_FIXED + WRAP_LEN * to.len() + inner_len + TAG * inner_len.div_ceil(CHUNK);
    assert!(
        body_len <= MAX_BODY,
        "sealed body of {body_len} bytes does not fit in an envelope"
    );

    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    let expires_min = now_min.saturating_add(ttl_min);
    let context = private_context(&id, expires_min, to.iter().map(|r| r.fingerprint()));

    let mut content_key = Zeroizing::new([0u8; CONTENT_KEY_LEN]);
    OsRng.fill_bytes(&mut content_key[..]);
    let mut prefix = [0u8; STREAM_PREFIX_LEN];
    OsRng.fill_bytes(&mut prefix);

    let mut body = Vec::with_capacity(body_len);
    body.push(VERSION);
    body.push(to.len() as u8);
    for recipient in to {
        let ephemeral = EphemeralSecret::random_from_rng(OsRng);
        let ephemeral_public = X25519PublicKey::from(&ephemeral);
        let shared = ephemeral.diffie_hellman(&X25519PublicKey::from(recipient.dh));
        assert!(
            shared.was_contributory(),
            "recipient dh key is a low-order point"
        );
        let wrap_key = derive_wrap_key(&shared, ephemeral_public.as_bytes(), &recipient.dh);

        let mut nonce = [0u8; WRAP_NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let wrapped = XChaCha20Poly1305::new(GenericArray::from_slice(&wrap_key[..]))
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &content_key[..],
                    aad: &context,
                },
            )
            .expect("XChaCha20-Poly1305 encryption of 32 bytes cannot fail");

        body.extend_from_slice(&recipient.fingerprint());
        body.extend_from_slice(ephemeral_public.as_bytes());
        body.extend_from_slice(&nonce);
        body.extend_from_slice(&wrapped);
    }
    body.extend_from_slice(&prefix);

    let inner = signed_inner(from, text, now_min, PRIVATE_SIG_DOMAIN, &context);

    let cipher = XChaCha20Poly1305::new(GenericArray::from_slice(&content_key[..]));
    let mut encryptor = Some(EncryptorBE32::from_aead(
        cipher,
        GenericArray::from_slice(&prefix),
    ));
    let pieces: Vec<&[u8]> = inner.chunks(CHUNK).collect();
    for (i, piece) in pieces.iter().enumerate() {
        let payload = Payload {
            msg: piece,
            aad: &context,
        };
        let segment = if i + 1 == pieces.len() {
            encryptor
                .take()
                .expect("one last segment")
                .encrypt_last(payload)
        } else {
            encryptor
                .as_mut()
                .expect("before last segment")
                .encrypt_next(payload)
        }
        .expect("STREAM encryption of a short message cannot fail");
        body.extend_from_slice(&segment);
    }
    debug_assert_eq!(body.len(), body_len);

    Envelope {
        kind: Kind::Private,
        id,
        expires_min,
        hops: 0,
        body,
    }
}

/// Open a private envelope addressed to `me`: check expiry against
/// `now_min`, then run a [`ChunkOpener`] over every chunk and return the
/// signature-checked message.
pub fn open_private(me: &Identity, env: &Envelope, now_min: u32) -> Result<Opened, OpenError> {
    if env.kind != Kind::Private {
        return Err(OpenError::Malformed);
    }
    check_expiry(env.expires_min, now_min)?;
    let (header, chunks) = split_private_body(&env.body)?;
    let mut opener = ChunkOpener::start(me, env.id, env.expires_min, header)?;
    let count = chunks.len();
    for (i, chunk) in chunks.into_iter().enumerate() {
        opener.next(chunk, i + 1 == count)?;
    }
    opener.finish()
}

/// Split a private body into its header and its STREAM chunks, following
/// the layout in the module documentation. Only structure is checked
/// here; nothing is authenticated.
pub fn split_private_body(body: &[u8]) -> Result<(&[u8], Vec<&[u8]>), OpenError> {
    if body.len() < 2 || body[0] != VERSION || body[1] == 0 {
        return Err(OpenError::Malformed);
    }
    let header_len = HEADER_FIXED + WRAP_LEN * body[1] as usize;
    if body.len() <= header_len {
        return Err(OpenError::Malformed);
    }
    let (header, rest) = body.split_at(header_len);
    let chunks: Vec<&[u8]> = rest.chunks(SEGMENT).collect();
    if !matches!(chunks.last(), Some(c) if c.len() > TAG) {
        return Err(OpenError::Malformed);
    }
    Ok((header, chunks))
}

/// Sign `text` as a public broadcast from `from`.
///
/// # Panics
///
/// Panics if the body would not fit in [`MAX_ENVELOPE`].
pub fn seal_public(from: &Identity, text: &str, now_min: u32, ttl_min: u32) -> Envelope {
    let body_len = INNER_MIN + text.len();
    assert!(
        body_len <= MAX_BODY,
        "public body of {body_len} bytes does not fit in an envelope"
    );

    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    let expires_min = now_min.saturating_add(ttl_min);
    let context = public_context(&id, expires_min);
    let body = signed_inner(from, text, now_min, PUBLIC_SIG_DOMAIN, &context);

    Envelope {
        kind: Kind::Public,
        id,
        expires_min,
        hops: 0,
        // Public bodies are not secret; move out of the zeroizing wrapper.
        body: body.to_vec(),
    }
}

/// Check a public broadcast's expiry and signature, returning the sender,
/// the text and the sender's `sent_min`.
pub fn open_public(
    env: &Envelope,
    now_min: u32,
) -> Result<(PublicIdentity, String, u32), OpenError> {
    if env.kind != Kind::Public {
        return Err(OpenError::Malformed);
    }
    check_expiry(env.expires_min, now_min)?;
    let context = public_context(&env.id, env.expires_min);
    let opened = verify_inner(&env.body, PUBLIC_SIG_DOMAIN, &context)?;
    Ok((opened.sender, opened.text, opened.sent_min))
}

fn check_expiry(expires_min: u32, now_min: u32) -> Result<(), OpenError> {
    if expires_min < now_min.saturating_sub(SKEW_MIN) {
        Err(OpenError::Expired)
    } else {
        Ok(())
    }
}

/// `kind ‖ id ‖ expires_min ‖ count ‖ fingerprints`: associated data for
/// every wrap and segment, and part of the signed message.
fn private_context(
    id: &[u8; 16],
    expires_min: u32,
    fingerprints: impl ExactSizeIterator<Item = [u8; FINGERPRINT_LEN]>,
) -> Vec<u8> {
    let count = fingerprints.len();
    let mut out = Vec::with_capacity(1 + 16 + 4 + 1 + FINGERPRINT_LEN * count);
    out.push(Kind::Private as u8);
    out.extend_from_slice(id);
    out.extend_from_slice(&expires_min.to_be_bytes());
    out.push(count as u8);
    for fp in fingerprints {
        out.extend_from_slice(&fp);
    }
    out
}

/// `kind ‖ id ‖ expires_min` for public broadcasts.
fn public_context(id: &[u8; 16], expires_min: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 16 + 4);
    out.push(Kind::Public as u8);
    out.extend_from_slice(id);
    out.extend_from_slice(&expires_min.to_be_bytes());
    out
}

/// Build `sender sign ‖ sender dh ‖ sent_min ‖ text ‖ signature`, the
/// signature covering `domain ‖ context ‖ everything before it`.
fn signed_inner(
    from: &Identity,
    text: &str,
    sent_min: u32,
    domain: &[u8],
    context: &[u8],
) -> Zeroizing<Vec<u8>> {
    let sender = from.public();
    let mut inner = Zeroizing::new(Vec::with_capacity(INNER_MIN + text.len()));
    inner.extend_from_slice(&sender.sign);
    inner.extend_from_slice(&sender.dh);
    inner.extend_from_slice(&sent_min.to_be_bytes());
    inner.extend_from_slice(text.as_bytes());

    let message = signed_message(domain, context, &inner);
    let signature = from.sign(&message);
    inner.extend_from_slice(&signature.to_bytes());
    inner
}

fn signed_message(domain: &[u8], context: &[u8], unsigned: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut message = Zeroizing::new(Vec::with_capacity(
        domain.len() + context.len() + unsigned.len(),
    ));
    message.extend_from_slice(domain);
    message.extend_from_slice(context);
    message.extend_from_slice(unsigned);
    message
}

/// Parse and verify an inner plaintext. The sender returned is the key the
/// signature verified under.
fn verify_inner(inner: &[u8], domain: &[u8], context: &[u8]) -> Result<Opened, OpenError> {
    if inner.len() < INNER_MIN {
        return Err(OpenError::Malformed);
    }
    let (unsigned, signature) = inner.split_at(inner.len() - SIGNATURE_LEN);

    let mut sign = [0u8; 32];
    sign.copy_from_slice(&unsigned[0..32]);
    let mut dh = [0u8; 32];
    dh.copy_from_slice(&unsigned[32..64]);
    let sent_min = u32::from_be_bytes(unsigned[64..68].try_into().expect("4 bytes"));

    let verifying = VerifyingKey::from_bytes(&sign).map_err(|_| OpenError::BadSignature)?;
    let signature = Signature::from_bytes(signature.try_into().expect("64 bytes"));
    let message = signed_message(domain, context, unsigned);
    verifying
        .verify_strict(&message, &signature)
        .map_err(|_| OpenError::BadSignature)?;

    let text = std::str::from_utf8(&unsigned[SENDER_LEN..])
        .map_err(|_| OpenError::Malformed)?
        .to_owned();
    Ok(Opened {
        sender: PublicIdentity { sign, dh },
        text,
        sent_min,
    })
}

/// HKDF-SHA256 over the X25519 output, salted with both public keys so the
/// wrap key is bound to this exact ephemeral/recipient pair.
fn derive_wrap_key(
    shared: &SharedSecret,
    ephemeral_public: &[u8; 32],
    recipient_dh: &[u8; 32],
) -> Zeroizing<[u8; 32]> {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral_public);
    salt[32..].copy_from_slice(recipient_dh);
    let mut key = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes())
        .expand(SEAL_INFO, &mut key[..])
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

/// Try to unwrap the content key from one wrap entry. `None` on any
/// failure, including a low-order ephemeral key.
fn unwrap_content_key(
    me: &Identity,
    my_dh: &[u8; 32],
    wrap: &[u8],
    context: &[u8],
) -> Option<Zeroizing<[u8; CONTENT_KEY_LEN]>> {
    let rest = &wrap[FINGERPRINT_LEN..];
    let (ephemeral, rest) = rest.split_at(EPHEMERAL_LEN);
    let (nonce, wrapped) = rest.split_at(WRAP_NONCE_LEN);
    let ephemeral: [u8; 32] = ephemeral.try_into().expect("32 bytes");

    let shared = me.agree(&ephemeral);
    if !shared.was_contributory() {
        return None;
    }
    let wrap_key = derive_wrap_key(&shared, &ephemeral, my_dh);
    let unwrapped = Zeroizing::new(
        XChaCha20Poly1305::new(GenericArray::from_slice(&wrap_key[..]))
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: wrapped,
                    aad: context,
                },
            )
            .ok()?,
    );
    if unwrapped.len() != CONTENT_KEY_LEN {
        return None;
    }
    let mut key = Zeroizing::new([0u8; CONTENT_KEY_LEN]);
    key.copy_from_slice(&unwrapped);
    Some(key)
}

/// A header split into its recipient wraps and stream nonce prefix.
struct Header<'a> {
    wraps: &'a [u8],
    prefix: &'a [u8],
}

impl<'a> Header<'a> {
    fn wraps(&self) -> impl ExactSizeIterator<Item = &'a [u8]> {
        self.wraps.chunks_exact(WRAP_LEN)
    }

    fn fingerprints(&self) -> impl ExactSizeIterator<Item = [u8; FINGERPRINT_LEN]> + 'a {
        self.wraps
            .chunks_exact(WRAP_LEN)
            .map(|w| w[..FINGERPRINT_LEN].try_into().expect("8 bytes"))
    }
}

fn parse_header(header: &[u8]) -> Result<Header<'_>, OpenError> {
    if header.len() < 2 || header[0] != VERSION || header[1] == 0 {
        return Err(OpenError::Malformed);
    }
    let count = header[1] as usize;
    if header.len() != HEADER_FIXED + WRAP_LEN * count {
        return Err(OpenError::Malformed);
    }
    let wraps_end = 2 + WRAP_LEN * count;
    Ok(Header {
        wraps: &header[2..wraps_end],
        prefix: &header[wraps_end..],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A co-recipient holds the content key and can produce chunks that
    /// pass AEAD for every other recipient. It still cannot put words in
    /// the sender's mouth: the signature catches the edit.
    #[test]
    fn co_recipient_cannot_forge_sender() {
        let (alice, bob, carol) = (
            Identity::generate(),
            Identity::generate(),
            Identity::generate(),
        );
        let env = seal_private(
            &alice,
            &[bob.public(), carol.public()],
            "Pay the bearer 10 litres of fuel.",
            100,
            60,
        );

        // Carol recovers the content key and the inner plaintext.
        let (header, chunks) = split_private_body(&env.body).unwrap();
        let parsed = parse_header(header).unwrap();
        let context = private_context(&env.id, env.expires_min, parsed.fingerprints());
        let carol_wrap = parsed
            .wraps()
            .find(|w| w[..8] == carol.public().fingerprint())
            .unwrap();
        let key = unwrap_content_key(&carol, &carol.public().dh, carol_wrap, &context).unwrap();
        let cipher = XChaCha20Poly1305::new(GenericArray::from_slice(&key[..]));
        let mut dec = DecryptorBE32::from_aead(cipher, GenericArray::from_slice(parsed.prefix));
        let mut inner = Vec::new();
        for (i, c) in chunks.iter().enumerate() {
            let p = Payload {
                msg: c,
                aad: &context[..],
            };
            if i + 1 == chunks.len() {
                inner.extend(dec.decrypt_last(p).unwrap());
                break;
            }
            inner.extend(dec.decrypt_next(p).unwrap());
        }

        // She edits the amount, keeps Alice as the named sender and the old
        // signature, and re-encrypts under the same key for Bob.
        let at = inner.windows(2).position(|w| w == b"10").unwrap();
        inner[at..at + 2].copy_from_slice(b"90");
        let cipher = XChaCha20Poly1305::new(GenericArray::from_slice(&key[..]));
        let mut enc = Some(EncryptorBE32::from_aead(
            cipher,
            GenericArray::from_slice(parsed.prefix),
        ));
        let mut forged = header.to_vec();
        let pieces: Vec<&[u8]> = inner.chunks(CHUNK).collect();
        for (i, piece) in pieces.iter().enumerate() {
            let p = Payload {
                msg: piece,
                aad: &context[..],
            };
            let seg = if i + 1 == pieces.len() {
                enc.take().unwrap().encrypt_last(p).unwrap()
            } else {
                enc.as_mut().unwrap().encrypt_next(p).unwrap()
            };
            forged.extend(seg);
        }
        let forged = Envelope {
            body: forged,
            ..env.clone()
        };

        assert!(open_private(&bob, &env, 100).is_ok());
        assert_eq!(
            open_private(&bob, &forged, 100),
            Err(OpenError::BadSignature)
        );
    }
}
