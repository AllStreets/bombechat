//! RaptorQ fountain framing: an endless stream of frames for one envelope,
//! and a collector that rebuilds envelopes from whichever frames it sees.
//!
//! [`encode`] turns an envelope into an [`Encoder`] whose frames carry
//! RaptorQ symbols, source symbols first and then fresh repair symbols
//! forever, so a receiver that starts watching late, or misses frames,
//! still completes once it has seen about `K + 2` distinct symbols, where
//! `K` is [`CodeSpec::source_symbols`] for the envelope, in any order. A
//! symbol whose frame cannot be drawn within the photosensitivity bound
//! under any mask is skipped (see [`crate::code`]); the fountain makes
//! that free.
//!
//! [`Collector`] takes frame bytes as the camera reads them, by tag, and
//! emits [`LightEvent`]s. Like the sound path's reassembler, a partial
//! envelope (and the memory of a completed tag) is dropped ten minutes
//! after its last frame. It holds at most [`MAX_PARTIALS`] envelopes in
//! progress, dropping the stalest when a new tag arrives.

use std::collections::{HashMap, HashSet};

use bombechat_core::MAX_ENVELOPE;
use raptorq::{
    Decoder, EncodingPacket, ObjectTransmissionInformation, PayloadId, SourceBlockEncoder,
};

use crate::code::{code_region, finish_frame, read_frame, CodeSpec, Layout, MASKS};

/// A partial or completed envelope is forgotten this many milliseconds
/// after its last frame.
const EXPIRY_MS: u64 = 600_000;

/// Most envelopes a collector holds in progress at once.
pub const MAX_PARTIALS: usize = 16;

/// RaptorQ encoding symbol ids are 24 bits.
const SYMBOL_ID_LIMIT: u32 = 1 << 24;

/// The RaptorQ parameters for one envelope: one source block, no
/// sub-blocks, byte alignment.
fn transmission_info(spec: &CodeSpec, len: u16) -> ObjectTransmissionInformation {
    ObjectTransmissionInformation::new(len as u64, spec.symbol_size, 1, 1, 1)
}

/// Start an endless frame stream for `envelope`, tagged `tag`.
///
/// # Panics
///
/// Panics if `spec` is not a preset, or if `envelope` is longer than
/// `bombechat_core::MAX_ENVELOPE`.
pub fn encode(spec: &CodeSpec, envelope: &[u8], tag: u16) -> Encoder {
    assert!(spec.is_valid(), "{spec:?} is not a light code preset");
    assert!(
        envelope.len() <= MAX_ENVELOPE,
        "envelope of {} bytes is over the {MAX_ENVELOPE} byte cap",
        envelope.len()
    );
    let len = envelope.len() as u16;
    let k = spec.source_symbols(envelope.len());

    let (block, source) = if envelope.is_empty() {
        (None, Vec::new())
    } else {
        let info = transmission_info(spec, len);
        let mut padded = envelope.to_vec();
        padded.resize(k as usize * spec.symbol_size as usize, 0);
        let block = SourceBlockEncoder::new(0, &info, &padded);
        let source = block
            .source_packets()
            .into_iter()
            .map(|p| p.data().to_vec())
            .collect();
        (Some(block), source)
    };

    Encoder {
        spec: *spec,
        layout: Layout::new(spec),
        tag,
        len,
        k,
        block,
        source,
        next_id: 0,
    }
}

/// An endless stream of frames for one envelope; see [`encode`].
pub struct Encoder {
    spec: CodeSpec,
    layout: Layout,
    tag: u16,
    len: u16,
    k: u32,
    block: Option<SourceBlockEncoder>,
    source: Vec<Vec<u8>>,
    next_id: u32,
}

impl Encoder {
    /// The next frame's bytes, ready for [`crate::render`]. Every frame
    /// returned keeps each 8 by 8 block of the drawn code between 40 and
    /// 60 percent dark. Symbol ids wrap after 2^24.
    pub fn next_frame(&mut self) -> Vec<u8> {
        let count = self.spec.packets_per_frame() as u32;
        loop {
            let first = self.next_id;
            self.next_id = (first + count) % SYMBOL_ID_LIMIT;
            let symbols: Vec<Vec<u8>> = (0..count)
                .map(|i| self.symbol((first + i) % SYMBOL_ID_LIMIT))
                .collect();
            let region = code_region(&self.spec, self.tag, self.len, first, &symbols);
            for mask in 0..MASKS {
                let frame = finish_frame(&self.spec, &region, mask);
                if self.layout.is_balanced(&frame) {
                    return frame;
                }
            }
        }
    }

    /// Number of distinct symbols a receiver needs at minimum.
    pub fn source_symbols(&self) -> u32 {
        self.k
    }

    fn symbol(&self, id: u32) -> Vec<u8> {
        match &self.block {
            None => vec![0; self.spec.symbol_size as usize],
            Some(_) if id < self.k => self.source[id as usize].clone(),
            Some(block) => block
                .repair_packets(id - self.k, 1)
                .pop()
                .map(|p| p.data().to_vec())
                .unwrap_or_default(),
        }
    }
}

/// What [`Collector::push`] saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LightEvent {
    /// A frame passed its CRC. `got` distinct symbols of `tag` are in
    /// hand; `need` is the minimum that can complete it (a few more may be
    /// needed).
    FrameOk { tag: u16, got: u32, need: u32 },
    /// Every byte of `tag` is recovered.
    Complete { tag: u16, bytes: Vec<u8> },
}

struct Partial {
    spec: CodeSpec,
    len: u16,
    decoder: Decoder,
    seen: HashSet<u32>,
    last_ms: u64,
}

impl Partial {
    fn new(spec: CodeSpec, len: u16, now_ms: u64) -> Self {
        Partial {
            spec,
            len,
            decoder: Decoder::new(transmission_info(&spec, len)),
            seen: HashSet::new(),
            last_ms: now_ms,
        }
    }
}

/// Rebuilds envelopes from light frames, by tag. Frames may arrive in any
/// order, repeat, or go missing.
pub struct Collector {
    partials: HashMap<u16, Partial>,
    /// Tags that already completed, with when one of their frames was last
    /// seen, so the sender's frames that keep arriving afterwards produce
    /// nothing.
    completed: HashMap<u16, u64>,
}

impl Collector {
    pub fn new() -> Self {
        Collector {
            partials: HashMap::new(),
            completed: HashMap::new(),
        }
    }

    /// Feed one frame's bytes in, as read from the camera. Never panics,
    /// whatever `frame_bytes` holds. Expires stale state first.
    ///
    /// Returns nothing for a frame that cannot be repaired or fails its
    /// CRC, claims an envelope over `MAX_ENVELOPE`, or belongs to a tag
    /// that already completed. Otherwise returns `FrameOk`, followed by
    /// `Complete` if this frame was enough to recover the envelope. If a
    /// frame's preset or length disagrees with what its tag carried so
    /// far, the newest wins: the partial starts again from this frame.
    pub fn push(&mut self, frame_bytes: &[u8], now_ms: u64) -> Vec<LightEvent> {
        self.expire(now_ms);
        let Some(frame) = read_frame(frame_bytes) else {
            return Vec::new();
        };
        let tag = frame.tag;
        if let Some(seen) = self.completed.get_mut(&tag) {
            *seen = now_ms;
            return Vec::new();
        }
        let len = frame.transfer_length;
        if len as usize > MAX_ENVELOPE {
            return Vec::new();
        }
        let need = frame.spec.source_symbols(len as usize);

        if len == 0 {
            self.partials.remove(&tag);
            self.completed.insert(tag, now_ms);
            return vec![
                LightEvent::FrameOk { tag, got: 0, need },
                LightEvent::Complete {
                    tag,
                    bytes: Vec::new(),
                },
            ];
        }

        let stale = self
            .partials
            .get(&tag)
            .is_some_and(|p| p.spec != frame.spec || p.len != len);
        if stale {
            self.partials.remove(&tag);
        }
        if !self.partials.contains_key(&tag) && self.partials.len() >= MAX_PARTIALS {
            if let Some(oldest) = self
                .partials
                .iter()
                .min_by_key(|(_, p)| p.last_ms)
                .map(|(t, _)| *t)
            {
                self.partials.remove(&oldest);
            }
        }
        let partial = self
            .partials
            .entry(tag)
            .or_insert_with(|| Partial::new(frame.spec, len, now_ms));
        partial.last_ms = now_ms;

        let mut recovered = None;
        for (i, symbol) in frame.symbols.into_iter().enumerate() {
            let id = ((frame.symbol_id as u64 + i as u64) % SYMBOL_ID_LIMIT as u64) as u32;
            if !partial.seen.insert(id) {
                continue;
            }
            let packet = EncodingPacket::new(PayloadId::new(0, id), symbol);
            if let Some(bytes) = partial.decoder.decode(packet) {
                recovered = Some(bytes);
                break;
            }
        }

        let got = partial.seen.len() as u32;
        let mut events = vec![LightEvent::FrameOk { tag, got, need }];
        if let Some(mut bytes) = recovered {
            bytes.truncate(len as usize);
            self.partials.remove(&tag);
            self.completed.insert(tag, now_ms);
            events.push(LightEvent::Complete { tag, bytes });
        }
        events
    }

    /// Forget that `tag` completed, and any partial for it, so its frames
    /// are collected afresh: for an envelope that arrived but would not
    /// open.
    pub fn reject(&mut self, tag: u16) {
        self.completed.remove(&tag);
        self.partials.remove(&tag);
    }

    /// Drop partial envelopes whose last frame arrived more than ten
    /// minutes before `now_ms`, and forget completed tags none of whose
    /// frames arrived in the last ten minutes (so the tag number can be
    /// reused).
    pub fn expire(&mut self, now_ms: u64) {
        self.partials
            .retain(|_, p| now_ms.saturating_sub(p.last_ms) <= EXPIRY_MS);
        self.completed
            .retain(|_, &mut at| now_ms.saturating_sub(at) <= EXPIRY_MS);
    }
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Frames that pass the CRC but carry nonsense (any tag, length,
        /// symbol id or symbol bytes, any preset or mask) never panic the
        /// collector, even mixed together under one tag.
        #[test]
        fn push_never_panics_on_crc_valid_nonsense(
            frames in proptest::collection::vec(
                (0u8..3, 0u16..4, any::<u16>(), 0u32..(1 << 24), any::<u8>(), 0u8..MASKS),
                1..40,
            ),
        ) {
            let mut c = Collector::new();
            for (i, (p, tag, len, id, fill, mask)) in frames.into_iter().enumerate() {
                let spec = CodeSpec::from_preset(p).unwrap();
                let symbols = vec![vec![fill; spec.symbol_size as usize]; spec.packets_per_frame()];
                let region = code_region(&spec, tag, len, id, &symbols);
                c.push(&finish_frame(&spec, &region, mask), i as u64);
            }
        }

        /// Arbitrary bytes of exactly a preset's frame length never panic.
        #[test]
        fn push_never_panics_on_frame_sized_noise(
            p in 0u8..3,
            noise in proptest::collection::vec(any::<u8>(), 512),
        ) {
            let spec = CodeSpec::from_preset(p).unwrap();
            let mut c = Collector::new();
            c.push(&noise[..spec.frame_bytes()], 0);
        }
    }

    #[test]
    fn lengths_over_the_envelope_cap_are_refused() {
        let spec = CodeSpec::normal();
        let symbols = vec![vec![0; spec.symbol_size as usize]];
        let region = code_region(&spec, 1, MAX_ENVELOPE as u16 + 1, 0, &symbols);
        assert!(Collector::new()
            .push(&finish_frame(&spec, &region, 0), 0)
            .is_empty());
    }

    #[test]
    fn at_most_sixteen_partials_are_held_dropping_the_stalest() {
        let spec = CodeSpec::slow();
        let mut c = Collector::new();
        let env = vec![7u8; 1000];
        for tag in 0..20u16 {
            let mut enc = encode(&spec, &env, tag);
            c.push(&enc.next_frame(), tag as u64);
        }
        assert_eq!(c.partials.len(), MAX_PARTIALS);
        assert!(!c.partials.contains_key(&0));
        assert!(c.partials.contains_key(&19));
    }
}
