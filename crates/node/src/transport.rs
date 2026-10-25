//! The carrier abstraction the node talks to: something that can put an
//! on-air frame out into the world and report what it heard back.
//!
//! A transport deals only in frames (`bombechat_modem::split` output); it
//! knows nothing of envelopes, keys or contacts. The node reassembles and
//! opens what the frames carry.

use bombechat_modem::Band;
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
    /// The envelope under this tag will not go out: the carrier refused
    /// it (a full queue) or could not play it. Reported once, instead of
    /// [`TransportEvent::Sent`].
    Dropped(u16),
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

    /// Payload bytes a second this carrier moves, overhead counted, as
    /// the interface shows it; `None` if it has no measured rate.
    fn rate(&self) -> Option<f32> {
        None
    }

    /// Whether to listen at all. Off, what the carrier picks up is let go
    /// unheard (not kept for later). On by default; a carrier that cannot
    /// stop listening ignores this, and the node drops what it hears.
    fn set_listening(&mut self, _on: bool) {}

    /// Whether frames may go out. Off, queued frames wait (they are not
    /// dropped) until it is on again. On by default.
    fn set_sending(&mut self, _on: bool) {}

    /// For sound: play only while the room is quieter than `db`.
    fn set_send_under(&mut self, _db: f32) {}

    /// For sound: how loud to play, as a gain on the played samples from
    /// 0 (silent) to 1 (full scale).
    fn set_volume(&mut self, _gain: f32) {}

    /// For sound: how many times each frame plays again after its first
    /// play.
    fn set_repeats(&mut self, _repeats: u8) {}

    /// For sound: move to `band`, or as near it as the carrier can go.
    /// What was part way through arriving is lost: the receiver starts
    /// afresh on the new band.
    fn set_band(&mut self, _band: Band) {}

    /// For sound: the band it is on, which may not be the band asked for
    /// (a sound card too slow for near silent stays audible).
    fn band(&self) -> Option<Band> {
        None
    }
}
