//! The carrier abstraction the node talks to: something that can put an
//! on-air frame out into the world and report what it heard back.
//!
//! A transport deals only in frames (`bombechat_modem::split` output); it
//! knows nothing of envelopes, keys or contacts. The node reassembles and
//! opens what the frames carry.

use bombechat_light::CodeSpec;
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
    /// The carrier works again after [`TransportEvent::Unavailable`],
    /// without being turned off and on (a camera another app let go of).
    Recovered,
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
    /// The envelope under this tag stopped going out part way, and will
    /// carry on when the carrier may send again (for light, the screen
    /// was cleared). [`TransportEvent::Playing`] follows when it does.
    Waiting(u16),
    /// Part of the envelope under this tag has arrived, but none of its
    /// frames can be handed over yet (light collects whole envelopes).
    /// Reported once, until it completes (as [`TransportEvent::Frame`]s)
    /// or is given up ([`TransportEvent::Abandoned`]).
    Arriving(u16),
    /// The envelope under this tag, reported arriving, stopped arriving
    /// and was given up.
    Abandoned(u16),
    /// Light: put this code frame on screen now, in place of the last.
    /// `bits` is the whole grid of preset `spec` (0 slow, 1 normal, 2
    /// fast), packed as `bombechat_light::grid_bits` packs it; `index`
    /// counts the frames of envelope `tag` shown so far.
    Display {
        tag: u16,
        index: u32,
        spec: u8,
        bits: Vec<u8>,
    },
    /// Light: clear the screen; nothing is being shown. `tag` is the
    /// envelope whose frame was on it.
    DisplayStopped { tag: u16 },
    /// Light: a code was in view this poll but could not be read (too
    /// small, blurred, glare). At most once a poll.
    CodeUnreadable,
    /// Light: a code was read this poll. At most once a poll.
    CodeRead,
    /// Light: what the camera sees, as a small grey picture `width` by
    /// `height`, one byte a pixel, row-major from the top left. Only
    /// while [`Transport::set_eyepiece`] is on and the camera reads.
    Eyepiece {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
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

    /// Everyone the envelope under `tag` was for reports having it: stop
    /// sending it. A carrier that keeps sending until told (light) reports
    /// it [`TransportEvent::Sent`]; others ignore this.
    fn delivered(&mut self, _tag: u16) {}

    /// The envelope this transport handed over under `tag` would not
    /// open (not for this machine, damaged, or tampered with). A carrier
    /// that remembers what it collected (light) forgets it, so the same
    /// envelope shown again is collected afresh, and returns true: the
    /// node then forgets the tag's frames too. Others return false.
    fn rejected(&mut self, _tag: u16) -> bool {
        false
    }

    /// For light: show what goes on screen from now on in this code
    /// preset (the flash speed). What is on screen finishes as it started.
    fn set_preset(&mut self, _spec: CodeSpec) {}

    /// For light: whether to report what the camera sees
    /// ([`TransportEvent::Eyepiece`]). Off by default.
    fn set_eyepiece(&mut self, _on: bool) {}

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
