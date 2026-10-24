//! Device identities and Signal-style 60-digit safety numbers.
//!
//! An [`Identity`] holds the long-term Ed25519 signing key and X25519
//! Diffie-Hellman key for a device. Only the derived [`PublicIdentity`]
//! (the two public keys) ever leaves the device; `safety_number` lets two
//! devices verify out of band that they hold the identities they think
//! they hold.

use ed25519_dalek::SigningKey;
use rand_core::OsRng;
use sha2::{Digest, Sha256, Sha512};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};
use zeroize::Zeroizing;

/// Number of SHA-512 rounds used to derive the digits for one side of a
/// safety number, matching Signal's fingerprint scheme.
const FINGERPRINT_ITERATIONS: usize = 5200;

/// A device's long-term identity: an Ed25519 signing key and an X25519
/// Diffie-Hellman key. The secret material never leaves this type except
/// through [`Identity::secret_bytes`], which returns it zeroized on drop.
pub struct Identity {
    signing: SigningKey,
    dh: StaticSecret,
}

impl Identity {
    /// Generate a fresh identity from the operating system's CSPRNG.
    pub fn generate() -> Identity {
        let signing = SigningKey::generate(&mut OsRng);
        let dh = StaticSecret::random_from_rng(OsRng);
        Identity { signing, dh }
    }

    /// Reconstruct an identity from its secret bytes: the 32-byte Ed25519
    /// seed followed by the 32-byte X25519 secret scalar.
    pub fn from_secret_bytes(bytes: &[u8; 64]) -> Identity {
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes[0..32]);
        let mut dh_bytes = [0u8; 32];
        dh_bytes.copy_from_slice(&bytes[32..64]);

        let signing = SigningKey::from_bytes(&seed);
        let dh = StaticSecret::from(dh_bytes);
        Identity { signing, dh }
    }

    /// The identity's secret bytes: the Ed25519 seed (32 bytes) followed by
    /// the X25519 secret (32 bytes). Zeroized when dropped.
    pub fn secret_bytes(&self) -> Zeroizing<[u8; 64]> {
        let mut out = [0u8; 64];
        out[0..32].copy_from_slice(self.signing.as_bytes());
        out[32..64].copy_from_slice(self.dh.as_bytes());
        Zeroizing::new(out)
    }

    /// The public half of this identity, safe to share with other devices.
    pub fn public(&self) -> PublicIdentity {
        PublicIdentity {
            sign: self.signing.verifying_key().to_bytes(),
            dh: *X25519PublicKey::from(&self.dh).as_bytes(),
        }
    }
}

/// The public keys that identify a device: an Ed25519 verifying key and an
/// X25519 public key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicIdentity {
    pub sign: [u8; 32],
    pub dh: [u8; 32],
}

impl PublicIdentity {
    /// The first 8 bytes of SHA-256 over `sign ‖ dh`, used to order the two
    /// sides of a safety number deterministically.
    pub fn fingerprint(&self) -> [u8; 8] {
        let mut hasher = Sha256::new();
        hasher.update(self.sign);
        hasher.update(self.dh);
        let digest = hasher.finalize();

        let mut out = [0u8; 8];
        out.copy_from_slice(&digest[0..8]);
        out
    }

    /// A 4-character uppercase hex rendering of the fingerprint's first two
    /// bytes, e.g. "7F3A".
    pub fn short_hex(&self) -> String {
        let fp = self.fingerprint();
        format!("{:02X}{:02X}", fp[0], fp[1])
    }
}

/// Hash `version(0u16 BE) ‖ sign ‖ dh` with SHA-512, then iterate
/// `h = SHA-512(h ‖ sign ‖ dh)` until `FINGERPRINT_ITERATIONS` hashes have
/// been computed in total.
fn iterated_hash(id: &PublicIdentity) -> [u8; 64] {
    let mut hasher = Sha512::new();
    hasher.update(0u16.to_be_bytes());
    hasher.update(id.sign);
    hasher.update(id.dh);

    let mut h = [0u8; 64];
    h.copy_from_slice(&hasher.finalize());

    for _ in 1..FINGERPRINT_ITERATIONS {
        let mut hasher = Sha512::new();
        hasher.update(h);
        hasher.update(id.sign);
        hasher.update(id.dh);
        h.copy_from_slice(&hasher.finalize());
    }

    h
}

/// Turn the first 30 bytes of an iterated hash into 30 decimal digits: six
/// 5-byte big-endian chunks, each taken mod 100000 and zero-padded to 5
/// digits.
fn digits_for(h: &[u8; 64]) -> String {
    let mut out = String::with_capacity(30);
    for chunk in h[0..30].chunks_exact(5) {
        let mut buf = [0u8; 8];
        buf[3..8].copy_from_slice(chunk);
        let value = u64::from_be_bytes(buf) % 100_000;
        out.push_str(&format!("{value:05}"));
    }
    out
}

/// The 60-digit safety number for a pair of identities, grouped into 12
/// groups of 5 digits separated by single spaces. Symmetric in `a` and `b`:
/// the side with the lexicographically lower fingerprint always contributes
/// the first 30 digits.
pub fn safety_number(a: &PublicIdentity, b: &PublicIdentity) -> String {
    let (first, second) = if a.fingerprint() <= b.fingerprint() {
        (a, b)
    } else {
        (b, a)
    };

    let digits = format!(
        "{}{}",
        digits_for(&iterated_hash(first)),
        digits_for(&iterated_hash(second))
    );

    digits
        .as_bytes()
        .chunks_exact(5)
        .map(|group| std::str::from_utf8(group).expect("ASCII digits"))
        .collect::<Vec<_>>()
        .join(" ")
}
