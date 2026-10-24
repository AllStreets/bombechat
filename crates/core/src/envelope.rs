//! Envelope wire format: the framing every message, control and public
//! post is carried in once it leaves the device.
//!
//! `hops` and `expires_min` sit outside the encryption on purpose, so
//! carriers can enforce hop limits and expiry without decrypting anything.
//!
//! Wire layout (big-endian, 25-byte header):
//! `version u8 = 1 | kind u8 | id [16] | expires_min u32 | hops u8 | body_len u16 | body`.

/// The largest number of bytes a single encoded envelope may occupy,
/// header included.
pub const MAX_ENVELOPE: usize = 4096;

/// The wire-format version this build encodes and accepts.
const VERSION: u8 = 1;

/// Size in bytes of everything before the body: version, kind, id,
/// expires_min, hops and body_len.
pub(crate) const HEADER_LEN: usize = 25;

/// What kind of payload an envelope's body carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Private = 1,
    Public = 2,
    Control = 3,
}

impl TryFrom<u8> for Kind {
    type Error = WireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Kind::Private),
            2 => Ok(Kind::Public),
            3 => Ok(Kind::Control),
            other => Err(WireError::UnknownKind(other)),
        }
    }
}

/// A framed message as it travels over a carrier: sound, sneakernet or
/// anything else. The body is opaque to this type; callers encrypt it
/// before building an envelope and decrypt it after decoding one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub kind: Kind,
    pub id: [u8; 16],
    pub expires_min: u32,
    pub hops: u8,
    pub body: Vec<u8>,
}

/// Why a byte slice could not be decoded into an [`Envelope`].
#[derive(Debug, PartialEq, Eq)]
pub enum WireError {
    /// Fewer bytes were available than the header, or than the header plus
    /// the declared body, requires.
    Truncated,
    /// The first byte was not a version this build understands.
    UnknownVersion(u8),
    /// The kind byte was not 1 (Private), 2 (Public) or 3 (Control).
    UnknownKind(u8),
    /// The declared body length would make the encoded envelope larger
    /// than [`MAX_ENVELOPE`].
    TooLarge,
    /// More bytes were supplied than the header plus the declared body
    /// account for.
    TrailingBytes,
}

impl Envelope {
    /// Encode this envelope to its wire format. Infallible: callers are
    /// responsible for keeping `body` within [`MAX_ENVELOPE`] minus the
    /// header before calling this.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.body.len());
        out.push(VERSION);
        out.push(self.kind as u8);
        out.extend_from_slice(&self.id);
        out.extend_from_slice(&self.expires_min.to_be_bytes());
        out.push(self.hops);
        out.extend_from_slice(&(self.body.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.body);
        out
    }

    /// Decode an envelope from its wire format. Checks, in order: enough
    /// bytes for the header, a known version, a known kind, the declared
    /// body length against [`MAX_ENVELOPE`], enough bytes for the declared
    /// body, and no leftover bytes after it. Never panics on any input.
    pub fn decode(buf: &[u8]) -> Result<Envelope, WireError> {
        if buf.len() < HEADER_LEN {
            return Err(WireError::Truncated);
        }

        let version = buf[0];
        if version != VERSION {
            return Err(WireError::UnknownVersion(version));
        }

        let kind = Kind::try_from(buf[1])?;

        let mut id = [0u8; 16];
        id.copy_from_slice(&buf[2..18]);

        let expires_min = u32::from_be_bytes(buf[18..22].try_into().unwrap());
        let hops = buf[22];
        let body_len = u16::from_be_bytes(buf[23..25].try_into().unwrap()) as usize;

        if body_len + HEADER_LEN > MAX_ENVELOPE {
            return Err(WireError::TooLarge);
        }

        if buf.len() < HEADER_LEN + body_len {
            return Err(WireError::Truncated);
        }

        if buf.len() > HEADER_LEN + body_len {
            return Err(WireError::TrailingBytes);
        }

        let body = buf[HEADER_LEN..HEADER_LEN + body_len].to_vec();

        Ok(Envelope {
            kind,
            id,
            expires_min,
            hops,
            body,
        })
    }
}
