//! RaptorQ fountain framing: an endless stream of frames for one envelope,
//! and a collector that rebuilds envelopes from whichever frames it sees.
//!
//! [`encode`] turns an envelope into an [`Encoder`] whose frames carry the
//! envelope's source symbols first and then fresh repair symbols forever,
//! so a receiver that starts watching late, or misses frames, still
//! completes once it has seen about `K + 2` distinct symbols, where `K` is
//! [`CodeSpec::source_symbols`] for the envelope, in any order.
//!
//! [`Collector`] takes frame bytes as the camera reads them, by tag, and
//! emits [`LightEvent`]s. Like the sound path's reassembler, a partial
//! envelope (and the memory of a completed tag) is dropped ten minutes
//! after its last frame; see [`Collector::expire`].

use std::collections::{HashMap, HashSet};

use raptorq::{
    Decoder, EncodingPacket, ObjectTransmissionInformation, PayloadId, SourceBlockEncoder,
};

use crate::code::{read_frame, write_frame, CodeSpec, MAX_SOURCE_SYMBOLS};

/// A partial or completed envelope is forgotten this many milliseconds
/// after its last frame.
const EXPIRY_MS: u64 = 600_000;

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
/// Panics if `spec` is not valid ([`CodeSpec::is_valid`]), if `envelope`
/// is longer than 65535 bytes, or if it would need more than
/// [`MAX_SOURCE_SYMBOLS`] symbols at this spec's symbol size. Envelopes
/// are capped at 4096 bytes upstream, well inside every preset.
pub fn encode(spec: &CodeSpec, envelope: &[u8], tag: u16) -> Encoder {
    assert!(spec.is_valid(), "invalid light code spec {spec:?}");
    let len = u16::try_from(envelope.len()).expect("envelope longer than 65535 bytes");
    let k = spec.source_symbols(envelope.len());
    assert!(
        k <= MAX_SOURCE_SYMBOLS,
        "envelope of {} bytes needs {k} symbols of {} bytes, more than {MAX_SOURCE_SYMBOLS}",
        envelope.len(),
        spec.symbol_size,
    );

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
    tag: u16,
    len: u16,
    k: u32,
    block: Option<SourceBlockEncoder>,
    source: Vec<Vec<u8>>,
    next_id: u32,
}

impl Encoder {
    /// The next frame's bytes, ready for [`crate::render`]. Source symbols
    /// come first, then repair symbols; symbol ids wrap after 2^24.
    pub fn next_frame(&mut self) -> Vec<u8> {
        let count = self.spec.packets_per_frame() as u32;
        let first = self.next_id;
        let symbols: Vec<Vec<u8>> = (0..count)
            .map(|i| self.symbol((first + i) % SYMBOL_ID_LIMIT))
            .collect();
        self.next_id = (first + count) % SYMBOL_ID_LIMIT;
        write_frame(&self.spec, self.tag, self.len, first, &symbols)
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

/// Rebuilds envelopes from light frames, by tag. Frames may arrive in any
/// order, repeat, or go missing.
pub struct Collector {
    partials: HashMap<u16, Partial>,
    /// Tags that already completed, with when, so the sender's frames that
    /// keep arriving afterwards produce nothing.
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
    /// whatever `frame_bytes` holds.
    ///
    /// Returns nothing for a frame that fails its CRC, does not match what
    /// its tag has carried so far, or belongs to a tag that already
    /// completed. Otherwise returns `FrameOk`, followed by `Complete` if
    /// this frame was enough to recover the envelope.
    pub fn push(&mut self, frame_bytes: &[u8], now_ms: u64) -> Vec<LightEvent> {
        let Some(frame) = read_frame(frame_bytes) else {
            return Vec::new();
        };
        let tag = frame.tag;
        if self.completed.contains_key(&tag) {
            return Vec::new();
        }
        let need = frame.spec.source_symbols(frame.transfer_length as usize);
        if need > MAX_SOURCE_SYMBOLS {
            return Vec::new();
        }

        if frame.transfer_length == 0 {
            self.partials.remove(&tag);
            self.completed.insert(tag, now_ms);
            return vec![
                LightEvent::FrameOk {
                    tag,
                    got: 0,
                    need: 0,
                },
                LightEvent::Complete {
                    tag,
                    bytes: Vec::new(),
                },
            ];
        }

        let partial = self.partials.entry(tag).or_insert_with(|| Partial {
            spec: frame.spec,
            len: frame.transfer_length,
            decoder: Decoder::new(transmission_info(&frame.spec, frame.transfer_length)),
            seen: HashSet::new(),
            last_ms: now_ms,
        });
        if partial.spec != frame.spec || partial.len != frame.transfer_length {
            return Vec::new();
        }
        partial.last_ms = now_ms;

        let mut recovered = None;
        for (i, symbol) in frame.symbols.into_iter().enumerate() {
            let id = (frame.symbol_id as u64 + i as u64) % SYMBOL_ID_LIMIT as u64;
            let id = id as u32;
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
            bytes.truncate(partial.len as usize);
            self.partials.remove(&tag);
            self.completed.insert(tag, now_ms);
            events.push(LightEvent::Complete { tag, bytes });
        }
        events
    }

    /// Drop partial envelopes whose last frame arrived more than ten
    /// minutes before `now_ms`, and forget tags that completed more than
    /// ten minutes before it (so the tag number can be reused).
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
        /// symbol id or symbol bytes, any preset) never panic the collector,
        /// even mixed together under one tag.
        #[test]
        fn push_never_panics_on_crc_valid_nonsense(
            frames in proptest::collection::vec(
                (0usize..3, 0u16..4, any::<u16>(), any::<u32>(), any::<u8>()),
                1..40,
            ),
        ) {
            let presets = [CodeSpec::slow(), CodeSpec::normal(), CodeSpec::fast()];
            let mut c = Collector::new();
            for (i, (p, tag, len, id, fill)) in frames.into_iter().enumerate() {
                let spec = presets[p];
                let symbols = vec![vec![fill; spec.symbol_size as usize]; spec.packets_per_frame()];
                let bytes = write_frame(&spec, tag, len, id, &symbols);
                c.push(&bytes, i as u64);
            }
        }

        /// Arbitrary bytes of exactly a preset's frame length never panic.
        #[test]
        fn push_never_panics_on_frame_sized_noise(
            p in 0usize..3,
            seed in proptest::collection::vec(any::<u8>(), 471),
        ) {
            let spec = [CodeSpec::slow(), CodeSpec::normal(), CodeSpec::fast()][p];
            let mut c = Collector::new();
            c.push(&seed[..spec.frame_bytes()], 0);
        }
    }
}
