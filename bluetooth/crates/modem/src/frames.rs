//! On-air framing, Reed-Solomon error correction and reassembly by tag.
//!
//! Wire layout of one frame (before Reed-Solomon parity is appended):
//! `tag u16 BE | seq u8 | total u8 | len u8 | data[len]`.
//!
//! [`split`] cuts an envelope into frames of at most [`FRAME_DATA`] data
//! bytes each and appends [`PARITY`] bytes of Reed-Solomon parity computed
//! over the whole header-plus-data block, so a corrupted header is
//! recoverable too. [`Reassembler`] takes frames back in off the air, in
//! any order, with any amount of repetition, and emits [`Event`]s as they
//! resolve.
//!
//! The on-air frame length depends on `len` (the last frame of a message
//! is usually shorter), so [`Reassembler::push`] takes exactly one frame's
//! bytes already demarcated by the layer below this one; it does not split
//! a byte stream into frames itself.

use std::collections::HashMap;

use reed_solomon::Decoder;
use reed_solomon::Encoder;

/// Maximum number of envelope data bytes carried by one frame.
pub const FRAME_DATA: usize = 48;

/// Number of Reed-Solomon parity bytes appended to every frame.
pub const PARITY: usize = 16;

/// Size in bytes of the frame header: `tag u16 | seq u8 | total u8 | len u8`.
pub(crate) const HEADER_LEN: usize = 5;

/// A partial or complete message resolves after this many milliseconds
/// without a new frame.
const EXPIRY_MS: u64 = 600_000;

/// Split an envelope into on-air frames tagged with `tag`.
///
/// Each returned frame is `tag u16 BE | seq u8 | total u8 | len u8 |
/// data[len]` followed by [`PARITY`] bytes of Reed-Solomon parity computed
/// over that header and data. `len` is at most [`FRAME_DATA`]; only the
/// last frame may be shorter. An empty envelope still produces one frame
/// (with `len == 0`), so a receiver always gets at least one frame to
/// resolve.
///
/// # Panics
///
/// Panics if `envelope` would need more than 255 frames (more than
/// `255 * FRAME_DATA` = 12240 bytes). Envelopes are capped at 4096 bytes
/// upstream (`bombechat_core::MAX_ENVELOPE`), so this only fires if `split`
/// is called directly on something larger than that cap.
pub fn split(envelope: &[u8], tag: u16) -> Vec<Vec<u8>> {
    let total_frames = if envelope.is_empty() {
        1
    } else {
        envelope.len().div_ceil(FRAME_DATA)
    };

    assert!(
        total_frames <= u8::MAX as usize,
        "envelope of {} bytes needs {} frames of {} bytes each, more than the 255 frames a u8 \
         total can represent (max splittable size is {} bytes)",
        envelope.len(),
        total_frames,
        FRAME_DATA,
        u8::MAX as usize * FRAME_DATA,
    );

    let total = total_frames as u8;
    let encoder = Encoder::new(PARITY);

    (0..total_frames)
        .map(|seq| {
            let start = seq * FRAME_DATA;
            let end = (start + FRAME_DATA).min(envelope.len());
            let data = &envelope[start..end];

            let mut plain = Vec::with_capacity(HEADER_LEN + data.len());
            plain.extend_from_slice(&tag.to_be_bytes());
            plain.push(seq as u8);
            plain.push(total);
            plain.push(data.len() as u8);
            plain.extend_from_slice(data);

            encoder.encode(&plain).to_vec()
        })
        .collect()
}

/// An event produced by [`Reassembler::push`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A frame passed Reed-Solomon correction and was accepted.
    FrameOk { tag: u16, seq: u8, total: u8 },
    /// A frame was too corrupted to recover, or too malformed to be a
    /// frame at all.
    FrameBad,
    /// All frames of `tag` have arrived; `bytes` is the reassembled
    /// envelope.
    Complete { tag: u16, bytes: Vec<u8> },
}

/// One message in progress: the frames received so far, and the
/// contiguous prefix built from them.
struct Partial {
    total: u8,
    chunks: Vec<Option<Vec<u8>>>,
    /// Bytes of `chunks[0..next_seq]`, concatenated in order.
    prefix: Vec<u8>,
    next_seq: u8,
    last_ms: u64,
}

impl Partial {
    fn new(total: u8, now_ms: u64) -> Self {
        Partial {
            total,
            chunks: vec![None; total as usize],
            prefix: Vec::new(),
            next_seq: 0,
            last_ms: now_ms,
        }
    }

    fn is_complete(&self) -> bool {
        self.next_seq as usize == self.chunks.len()
    }

    /// Store `data` at `seq` if it has not been seen yet, then extend the
    /// contiguous prefix as far as the chunks received allow.
    fn store(&mut self, seq: u8, data: Vec<u8>) {
        let slot = &mut self.chunks[seq as usize];
        if slot.is_some() {
            return;
        }
        *slot = Some(data);

        while (self.next_seq as usize) < self.chunks.len() {
            match &self.chunks[self.next_seq as usize] {
                Some(chunk) => {
                    self.prefix.extend_from_slice(chunk);
                    self.next_seq += 1;
                }
                None => break,
            }
        }
    }
}

/// Reassembles frames from [`split`] back into envelopes, by tag.
///
/// Frames may arrive out of order and may repeat; duplicates are ignored.
/// A partial message (and, separately, the memory of a just-completed
/// tag) is dropped once ten minutes pass without a new frame for it; see
/// [`Reassembler::expire`].
pub struct Reassembler {
    partials: HashMap<u16, Partial>,
    /// Tags that already produced a `Complete`, with the time they did, so
    /// a late duplicate of the last frame cannot produce a second one.
    completed: HashMap<u16, u64>,
}

impl Reassembler {
    pub fn new() -> Self {
        Reassembler {
            partials: HashMap::new(),
            completed: HashMap::new(),
        }
    }

    /// Feed one received frame in. Never panics, regardless of what
    /// `frame` contains.
    ///
    /// Returns the frame's outcome (`FrameOk` or `FrameBad`), followed by
    /// `Complete` if this frame was the message's last missing piece.
    pub fn push(&mut self, frame: &[u8], now_ms: u64) -> Vec<Event> {
        // Reject anything that cannot possibly be a frame before touching
        // the Reed-Solomon decoder: it assumes a block no larger than the
        // 256-byte Galois field it works over, and the largest real frame
        // is HEADER_LEN + FRAME_DATA + PARITY = 69 bytes.
        let min_len = HEADER_LEN + PARITY;
        let max_len = HEADER_LEN + FRAME_DATA + PARITY;
        if frame.len() < min_len || frame.len() > max_len {
            return vec![Event::FrameBad];
        }

        let corrected = match Decoder::new(PARITY).correct(frame, None) {
            Ok(buf) => buf,
            Err(_) => return vec![Event::FrameBad],
        };
        let plain = corrected.data();

        let tag = u16::from_be_bytes([plain[0], plain[1]]);
        let seq = plain[2];
        let total = plain[3];
        let len = plain[4] as usize;

        if total == 0 || seq >= total || len > FRAME_DATA || plain.len() != HEADER_LEN + len {
            return vec![Event::FrameBad];
        }

        let mut events = vec![Event::FrameOk { tag, seq, total }];

        // A duplicate of a frame belonging to an already-completed message:
        // acknowledge it, but never resurrect the message.
        if self.completed.contains_key(&tag) {
            return events;
        }

        let partial = self
            .partials
            .entry(tag)
            .or_insert_with(|| Partial::new(total, now_ms));

        if partial.total != total {
            // Inconsistent metadata for a tag already in progress: the
            // frame is corrupt in a way Reed-Solomon could not catch.
            events[0] = Event::FrameBad;
            return events;
        }

        partial.last_ms = now_ms;
        let data = plain[HEADER_LEN..].to_vec();
        partial.store(seq, data);

        if partial.is_complete() {
            let partial = self.partials.remove(&tag).expect("just matched above");
            self.completed.insert(tag, now_ms);
            events.push(Event::Complete {
                tag,
                bytes: partial.prefix,
            });
        }

        events
    }

    /// Drop partial messages whose last frame arrived more than ten
    /// minutes before `now_ms`, and forget tags that completed more than
    /// ten minutes before `now_ms` (letting that tag number be reused).
    pub fn expire(&mut self, now_ms: u64) {
        self.partials
            .retain(|_, p| now_ms.saturating_sub(p.last_ms) <= EXPIRY_MS);
        self.completed
            .retain(|_, &mut completed_ms| now_ms.saturating_sub(completed_ms) <= EXPIRY_MS);
    }

    /// Forget `tag` entirely, completed or part way: its frames are
    /// collected afresh, for a message that arrived but would not open
    /// and whose carrier will hand it over again.
    pub fn forget(&mut self, tag: u16) {
        self.partials.remove(&tag);
        self.completed.remove(&tag);
    }

    /// The contiguous bytes received so far for `tag`, from frame 0
    /// onward, so a caller can decrypt chunks as they land rather than
    /// waiting for the whole message. `None` if `tag` has no message in
    /// progress (never started, already complete, or expired).
    pub fn prefix(&self, tag: u16) -> Option<&[u8]> {
        self.partials.get(&tag).map(|p| p.prefix.as_slice())
    }
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}
