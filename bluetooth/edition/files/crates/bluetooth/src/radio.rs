//! What the link layer needs from a Bluetooth radio, and nothing more.

use bombechat_session::TOKEN_LEN;

/// One connection to one nearby device, as the radio numbers it. A pair
/// of devices may share two links, one each way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LinkId(pub u64);

/// How open the Bluetooth switch is. Off is no mode at all: the radio is
/// stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Sessions with contacts only. Requests to connect are neither sent
    /// nor answered.
    Contacts,
    /// Contacts, and requests to connect both ways.
    Open,
}

/// What a radio reports from one [`Radio::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadioEvent {
    /// A link came up. `token` is the advertising token the other device
    /// was showing when this radio connected to it, or `None` when the
    /// other device connected to this one (the radio then never saw its
    /// advert). Only a link with a token can begin a contact handshake,
    /// which must name the token it is for.
    Up {
        link: LinkId,
        token: Option<[u8; TOKEN_LEN]>,
    },
    /// The link is gone. Nothing more arrives on it.
    Down(LinkId),
    /// One write from the other end, exactly as it was written.
    Bytes(LinkId, Vec<u8>),
    /// The radio can't work, in a sentence the person can act on.
    Unavailable(String),
}

/// A Bluetooth LE radio: advertise this device's token, find others
/// advertising ours, and move writes over links.
///
/// Writes are write-without-response: one may be lost, and nothing says
/// so. The link layer above copes with loss, duplication and reordering.
pub trait Radio: Send {
    /// Begin advertising, scanning and connecting. Called again with
    /// another mode, the radio carries on in that mode.
    fn start(&mut self, mode: Mode);

    /// Stop everything: no advertising, no scanning, no links. Every link
    /// is gone without a [`RadioEvent::Down`].
    fn stop(&mut self);

    /// Advertise `token` from now on, in place of any earlier one. The
    /// advert carries our service and this token, nothing stable.
    fn advertise(&mut self, token: [u8; TOKEN_LEN]);

    /// Write `bytes` to `link`, at most [`Radio::max_write`] of them.
    fn write(&mut self, link: LinkId, bytes: &[u8]);

    /// What happened since the last poll.
    fn poll(&mut self) -> Vec<RadioEvent>;

    /// The largest write `link` takes.
    fn max_write(&self, link: LinkId) -> usize;
}
