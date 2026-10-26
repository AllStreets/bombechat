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

/// The part of an advertising token an advert shows: its first bytes.
pub const TAG_LEN: usize = 6;

/// What this side knows of the advert behind a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advert {
    /// The other device connected to this one: this side never saw its
    /// advert.
    Unseen,
    /// This side connected to the other device's advert and was given a
    /// token for it. Its first [`TAG_LEN`] bytes are the advert's tag, but
    /// the rest may be a decoy: a device that heard this one under an
    /// earlier token gives a decoy, which no one can tell from its token
    /// without its key. A contact recognises the advert by the tag, from
    /// the tokens it was told inside a session.
    Token([u8; TOKEN_LEN]),
}

/// What a radio reports from one [`Radio::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadioEvent {
    /// A link came up, and what this side knows of the advert behind it.
    /// Only a link this device opened can begin a contact handshake,
    /// which must name the token of the device it is for: the one heard
    /// here, or one the contact gave inside an earlier session.
    Up { link: LinkId, advert: Advert },
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
    /// advert carries our service and this token, nothing stable. A device
    /// that connects is given the token only if this radio hasn't heard it
    /// under an earlier one; otherwise it is given a decoy that looks the
    /// same (the token's tag, then random bytes drawn afresh).
    fn advertise(&mut self, token: [u8; TOKEN_LEN]);

    /// Close `link`. The radio sends no [`RadioEvent::Down`] for it, and
    /// does not connect again to the same advert on its own: a new link to
    /// that device waits until it shows a token other than the one this
    /// link was made to.
    fn disconnect(&mut self, link: LinkId);

    /// Write `bytes` to `link`, at most [`Radio::max_write`] of them.
    fn write(&mut self, link: LinkId, bytes: &[u8]);

    /// What happened since the last poll.
    fn poll(&mut self) -> Vec<RadioEvent>;

    /// The largest write `link` takes.
    fn max_write(&self, link: LinkId) -> usize;
}
