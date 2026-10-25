//! The carrier abstraction the node talks to: something that can put an
//! on-air frame out into the world and report what it heard back.
//!
//! A transport deals only in frames (`bombechat_modem::split` output); it
//! knows nothing of envelopes, keys or contacts. The node reassembles and
//! opens what the frames carry.

use bombechat_store::Channel;

/// What a transport reports from one [`Transport::poll`].
#[derive(Debug, Clone, PartialEq)]
pub enum TransportEvent {
    /// One received frame, exactly as it came off the carrier (not yet
    /// Reed-Solomon corrected).
    Frame(Vec<u8>),
    /// The room's loudness in approximate dB, for the meter and the quiet
    /// gate.
    RoomDb(f32),
    /// The carrier cannot work, with a short human-readable reason such as
    /// "microphone permission denied". Reported once per failure.
    Unavailable(String),
    /// Something the user should know that does not make the carrier
    /// unavailable, e.g. "send queue full, message not sent".
    Notice(String),
    /// The first frame of the envelope under this tag has gone to the
    /// carrier.
    Playing(u16),
    /// Every frame of the envelope under this tag, repeats included, has
    /// finished going out. Reported once, after [`TransportEvent::Playing`].
    Sent(u16),
}

/// A carrier for on-air frames.
pub trait Transport: Send {
    /// Which channel this transport is, recorded on messages it delivers.
    fn name(&self) -> Channel;

    /// Queue one frame for transmission. It goes out when the carrier
    /// allows (for sound, once the room is quiet).
    fn send(&mut self, frame: Vec<u8>);

    /// Do any pending work and report what happened since the last poll.
    /// `now_ms` is a monotonic millisecond clock, the same one passed to
    /// [`crate::Node::tick`].
    fn poll(&mut self, now_ms: u64) -> Vec<TransportEvent>;
}
