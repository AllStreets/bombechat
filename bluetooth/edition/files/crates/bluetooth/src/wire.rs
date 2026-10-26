//! Link messages and their fragments.
//!
//! A link message is `kind u8 | flags u8 | len u16 BE | body[len]`. It is
//! cut into fragments no longer than the link's largest write, each
//! `message u16 BE | index u16 BE | count u16 BE | data`, and put back
//! together by [`Reassembly`] with hard caps on how many messages may be
//! part way at once, how big each may grow and how long each may take.

use std::collections::{BTreeMap, HashMap, VecDeque};

/// What a link message is. Every device sends every kind, so the kind
/// says what a message does, never who sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Message 1 of a contact handshake (`Noise_IK`).
    Contact = 1,
    /// Message 1 of a request to connect (`Noise_XN`).
    Request = 2,
    /// Any later handshake message, of either pattern.
    Handshake = 3,
    /// A transport message inside a session: data, acknowledgement or
    /// keepalive, which only the session's two ends can tell apart.
    Sealed = 4,
}

impl Kind {
    fn from_byte(b: u8) -> Option<Kind> {
        Some(match b {
            1 => Kind::Contact,
            2 => Kind::Request,
            3 => Kind::Handshake,
            4 => Kind::Sealed,
            _ => return None,
        })
    }
}

/// Flag: the writer began the exchange this message belongs to. Message
/// 1 always carries it.
pub const FROM_INITIATOR: u8 = 0x01;

/// Bytes before a link message's body.
pub const HEADER_LEN: usize = 4;

/// The largest link message, header included: 64 KiB.
pub const MAX_MESSAGE: usize = 64 * 1024;

/// The largest body a link message may carry.
pub const MAX_BODY: usize = MAX_MESSAGE - HEADER_LEN;

/// Bytes before a fragment's data.
pub const FRAGMENT_HEADER: usize = 6;

/// The smallest write a link is assumed to take: Bluetooth LE's default
/// 23-byte ATT MTU less its 3-byte header. A radio reporting less is
/// treated as this.
pub const MIN_WRITE: usize = 20;

/// The most fragments one message can need, at the smallest write.
pub const MAX_FRAGMENTS: usize = MAX_MESSAGE.div_ceil(MIN_WRITE - FRAGMENT_HEADER);

/// At most this many messages may be part way on one link; one more
/// drops the oldest.
pub const MAX_ASSEMBLIES: usize = 32;

/// A message not complete this long after its first fragment is dropped.
pub const ASSEMBLY_MS: u64 = 30_000;

/// How many just-completed message numbers a link remembers, so a late
/// duplicate fragment of one does not start it again.
const REMEMBERED: usize = 32;

/// One link message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkMessage {
    pub kind: Kind,
    pub flags: u8,
    pub body: Vec<u8>,
}

impl LinkMessage {
    /// The wire bytes. `body` must be at most [`MAX_BODY`] bytes; every
    /// message this crate builds is far smaller.
    pub fn encode(&self) -> Vec<u8> {
        debug_assert!(self.body.len() <= MAX_BODY);
        let mut out = Vec::with_capacity(HEADER_LEN + self.body.len());
        out.push(self.kind as u8);
        out.push(self.flags);
        out.extend_from_slice(&(self.body.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.body);
        out
    }

    /// Read a message back. `None` for an unknown kind, an unknown flag, or
    /// a length that disagrees with the bytes. Never panics.
    pub fn decode(bytes: &[u8]) -> Option<LinkMessage> {
        if bytes.len() < HEADER_LEN {
            return None;
        }
        let kind = Kind::from_byte(bytes[0])?;
        let flags = bytes[1];
        if flags & !FROM_INITIATOR != 0 {
            return None;
        }
        let len = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        if bytes.len() != HEADER_LEN + len {
            return None;
        }
        Some(LinkMessage {
            kind,
            flags,
            body: bytes[HEADER_LEN..].to_vec(),
        })
    }
}

/// Cut `message` into fragments numbered `id`, each at most `max_write`
/// bytes (at least [`MIN_WRITE`]).
pub fn fragment(message: &[u8], id: u16, max_write: usize) -> Vec<Vec<u8>> {
    let room = max_write.max(MIN_WRITE) - FRAGMENT_HEADER;
    let chunks: Vec<&[u8]> = if message.is_empty() {
        vec![&[][..]]
    } else {
        message.chunks(room).collect()
    };
    let count = chunks.len() as u16;
    chunks
        .iter()
        .enumerate()
        .map(|(i, data)| {
            let mut f = Vec::with_capacity(FRAGMENT_HEADER + data.len());
            f.extend_from_slice(&id.to_be_bytes());
            f.extend_from_slice(&(i as u16).to_be_bytes());
            f.extend_from_slice(&count.to_be_bytes());
            f.extend_from_slice(data);
            f
        })
        .collect()
}

struct Assembly {
    count: u16,
    pieces: BTreeMap<u16, Vec<u8>>,
    bytes: usize,
    started_ms: u64,
}

/// Puts one link's fragments back into messages. Fragments may come in
/// any order and more than once. Bounded: at most [`MAX_ASSEMBLIES`]
/// messages part way, each at most [`MAX_MESSAGE`] bytes and
/// [`MAX_FRAGMENTS`] fragments, each given [`ASSEMBLY_MS`] to finish.
#[derive(Default)]
pub struct Reassembly {
    parts: HashMap<u16, Assembly>,
    done: VecDeque<u16>,
}

impl Reassembly {
    pub fn new() -> Reassembly {
        Reassembly::default()
    }

    /// Take one fragment; the whole message once its last piece is in.
    /// Never panics.
    pub fn push(&mut self, fragment: &[u8], now_ms: u64) -> Option<Vec<u8>> {
        self.expire(now_ms);
        if fragment.len() <= FRAGMENT_HEADER {
            return None;
        }
        let id = u16::from_be_bytes([fragment[0], fragment[1]]);
        let index = u16::from_be_bytes([fragment[2], fragment[3]]);
        let count = u16::from_be_bytes([fragment[4], fragment[5]]);
        let data = &fragment[FRAGMENT_HEADER..];
        if count == 0 || index >= count || count as usize > MAX_FRAGMENTS {
            return None;
        }
        // every fragment but the last fills at least the smallest write, so
        // a message can't be held in thousands of tiny pieces
        if index + 1 < count && fragment.len() < MIN_WRITE {
            return None;
        }
        if self.done.contains(&id) {
            return None;
        }
        if count == 1 {
            self.parts.remove(&id);
            self.remember(id);
            return Some(data.to_vec());
        }
        if !self.parts.contains_key(&id) && self.parts.len() >= MAX_ASSEMBLIES {
            let oldest = self
                .parts
                .iter()
                .min_by_key(|(_, a)| a.started_ms)
                .map(|(id, _)| *id);
            if let Some(oldest) = oldest {
                self.parts.remove(&oldest);
            }
        }
        let a = self.parts.entry(id).or_insert_with(|| Assembly {
            count,
            pieces: BTreeMap::new(),
            bytes: 0,
            started_ms: now_ms,
        });
        if a.count != count {
            self.parts.remove(&id);
            return None;
        }
        if a.pieces.contains_key(&index) {
            return None;
        }
        a.bytes += data.len();
        if a.bytes > MAX_MESSAGE {
            self.parts.remove(&id);
            return None;
        }
        a.pieces.insert(index, data.to_vec());
        if a.pieces.len() < a.count as usize {
            return None;
        }
        let a = self.parts.remove(&id)?;
        self.remember(id);
        let mut out = Vec::with_capacity(a.bytes);
        for piece in a.pieces.into_values() {
            out.extend_from_slice(&piece);
        }
        Some(out)
    }

    /// Drop every message part way for longer than [`ASSEMBLY_MS`].
    pub fn expire(&mut self, now_ms: u64) {
        self.parts
            .retain(|_, a| now_ms.saturating_sub(a.started_ms) < ASSEMBLY_MS);
    }

    /// Messages part way.
    pub fn in_progress(&self) -> usize {
        self.parts.len()
    }

    /// Bytes held for messages part way.
    pub fn held_bytes(&self) -> usize {
        self.parts.values().map(|a| a.bytes).sum()
    }

    fn remember(&mut self, id: u16) {
        self.done.push_back(id);
        if self.done.len() > REMEMBERED {
            self.done.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_survives_fragmenting_at_every_size() {
        let message: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        for max_write in [1, 20, 21, 64, 182, 512, 6000] {
            let mut r = Reassembly::new();
            let frags = fragment(&message, 7, max_write);
            assert!(frags.iter().all(|f| f.len() <= max_write.max(MIN_WRITE)));
            let mut out = None;
            for f in frags.iter().rev() {
                out = r.push(f, 0).or(out);
            }
            assert_eq!(out.as_deref(), Some(&message[..]), "max_write {max_write}");
            assert_eq!(r.in_progress(), 0);
        }
    }

    #[test]
    fn link_messages_round_trip_and_refuse_what_is_not_one() {
        let m = LinkMessage {
            kind: Kind::Sealed,
            flags: FROM_INITIATOR,
            body: vec![1, 2, 3],
        };
        assert_eq!(LinkMessage::decode(&m.encode()), Some(m.clone()));
        let mut bad = m.encode();
        bad[0] = 9;
        assert_eq!(LinkMessage::decode(&bad), None, "unknown kind");
        let mut bad = m.encode();
        bad[1] = 0x02;
        assert_eq!(LinkMessage::decode(&bad), None, "unknown flag");
        let mut bad = m.encode();
        bad.push(0);
        assert_eq!(LinkMessage::decode(&bad), None, "trailing byte");
        assert_eq!(LinkMessage::decode(&m.encode()[..5]), None, "short");
        assert_eq!(LinkMessage::decode(&[]), None);
    }

    #[test]
    fn the_fragment_that_crosses_64_kib_drops_the_message() {
        let mut r = Reassembly::new();
        // 400 full fragments of 170 bytes would be 68,000 bytes
        let piece = |index: u16| {
            let mut f = 1u16.to_be_bytes().to_vec();
            f.extend_from_slice(&index.to_be_bytes());
            f.extend_from_slice(&400u16.to_be_bytes());
            f.extend_from_slice(&[9u8; 170]);
            f
        };
        let fits = MAX_MESSAGE / 170;
        for i in 0..fits as u16 {
            assert_eq!(r.push(&piece(i), 0), None);
        }
        assert_eq!(r.held_bytes(), fits * 170);
        assert_eq!(r.push(&piece(fits as u16), 0), None);
        assert_eq!(r.in_progress(), 0, "the crossing fragment dropped it");
        assert_eq!(r.held_bytes(), 0);
    }

    #[test]
    fn at_most_32_messages_are_part_way_and_each_lives_30_seconds() {
        let mut r = Reassembly::new();
        for id in 0..100u16 {
            let mut f = id.to_be_bytes().to_vec();
            f.extend_from_slice(&0u16.to_be_bytes());
            f.extend_from_slice(&3u16.to_be_bytes());
            f.extend_from_slice(&[1u8; 30]);
            r.push(&f, u64::from(id));
        }
        assert_eq!(r.in_progress(), MAX_ASSEMBLIES);
        r.expire(99 + ASSEMBLY_MS - 1);
        assert!(r.in_progress() > 0);
        r.expire(99 + ASSEMBLY_MS);
        assert_eq!(r.in_progress(), 0);
    }

    #[test]
    fn a_tiny_fragment_that_is_not_the_last_is_refused() {
        let mut r = Reassembly::new();
        let tiny = [0, 1, 0, 0, 0, 2, 7];
        assert_eq!(r.push(&tiny, 0), None);
        assert_eq!(r.in_progress(), 0);
        // the last may be short
        let mut first = vec![0, 1, 0, 0, 0, 2];
        first.extend_from_slice(&[5u8; 14]);
        r.push(&first, 0);
        assert_eq!(r.push(&[0, 1, 0, 1, 0, 2, 6], 0).map(|m| m.len()), Some(15));
    }

    #[test]
    fn a_fragment_that_disagrees_on_its_count_drops_the_message() {
        let mut r = Reassembly::new();
        let frags = fragment(&[5u8; 100], 1, 20);
        assert_eq!(r.push(&frags[0], 0), None);
        let mut liar = frags[1].clone();
        liar[5] = liar[5].wrapping_add(1);
        assert_eq!(r.push(&liar, 0), None);
        assert_eq!(r.in_progress(), 0);
    }
}
