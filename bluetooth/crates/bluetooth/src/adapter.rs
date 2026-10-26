//! The CoreBluetooth radio's logic, with no platform code: what to
//! connect to, when a link is up, what to do when Bluetooth is off or not
//! allowed, flow control, and how `stop` tears everything down. It drives
//! a [`Stack`], the seam over the CoreBluetooth calls, so it runs and is
//! tested on any system; `macos.rs` is the real stack.
//!
//! # The advert and the token
//!
//! On macOS an app advertising with `CBPeripheralManager` may set only the
//! local name and the service UUIDs: service data is refused. With our
//! 128-bit service UUID in the advert, macOS 12 and later keep a local
//! name of at most 8 bytes and drop a longer one entirely. So the advert
//! carries our service and an 8-character tag, the first 6 bytes of the
//! token in URL-safe base64 ([`advert_name`]). The whole 16-byte token
//! goes to each central as the first notification after it subscribes
//! (`TOKEN_MARK` and the token), before anything else on the link, and
//! [`RadioEvent::Up`] waits for it. The tag tells the scanning side when
//! an advert has a new token without connecting.
//!
//! # Address rotation
//!
//! macOS picks the Bluetooth address and rotates it on its own timer; no
//! API lets an app ask for a new one. What an app can do is stop and
//! start advertising with each new token, which is what
//! [`Radio::advertise`] does here. Whether macOS gives the restarted
//! advert a new address, and whether the tag (which lasts a token's life)
//! links addresses when macOS rotates one mid-token, are not known yet:
//! the two-Mac hardware test measures both (see the spec's threat model).
//! An observer can link tokens anyway by timing (one advert with our
//! service stops as another starts), by a contact's connection that
//! lives across the change, and by our service UUID, which every
//! Bombechat device advertises.
//!
//! # Links
//!
//! A peer is CoreBluetooth's identifier for a remote: the peripheral's on
//! the central side, the central's on the peripheral side. Each
//! subscription a remote central makes is one link (token unknown); each
//! peripheral this side connects to, discovers, subscribes to and hears the
//! token from is another (token known).
//!
//! [`Radio::disconnect`] cancels a central-side connection and remembers
//! the peer and the advert's tag: neither that peer showing that tag nor
//! another peer showing it, nor that peer showing no tag, is connected to
//! again, until the tag changes or [`CLOSED_MS`] passes.
//!
//! A peripheral manager cannot drop a central, so a central that connects
//! to this device may keep its connection as long as it likes, across
//! this device's rotations and even past `stop`. What this side controls
//! is the token. Each central (by its CoreBluetooth identifier, which
//! follows its address and so its connection) is given the whole token
//! at most once, and only if it was first heard in the current token's
//! period. Every other subscription still gets a link, but its first
//! notification is `WITHHELD_MARK`, one byte that carries nothing: no
//! token and no tag. A central heard before a rotation learns nothing of
//! the new advert over its old connection, however it subscribes again.
//! What it does learn is that it was heard before: an active tracker that
//! connected once from a fixed address can later recognise this device by
//! being refused the token. That is an accepted linker (see the spec's
//! threat model); a decoy can't hide it, because a newcomer from a fresh
//! address is served the real token, and any decoy that carried the tag
//! would hand the new advert to a held connection. A link that already carries a contact's session is
//! untouched by a rotation and keeps working. `disconnect` on a
//! peripheral-side link stops using it: its writes are ignored and nothing
//! more is sent, until the central subscribes again (and is then given a
//! link without the token).
//!
//! Centrals are remembered while subscribed and for [`CENTRAL_MEMORY_MS`]
//! after their last traffic on a link that was given the token (traffic on
//! a link without it refreshes nothing); one that holds a connection
//! silent and unsubscribed longer than that is taken for a new one. So
//! any central first heard under an earlier token gets no token from this
//! device until its address changes (a new identifier) or it has been
//! silent an hour. Two Macs on the same Apple ID may keep the same
//! identifier for each other across address changes, so each may withhold
//! its token from the other in both directions. Contacts don't need it:
//! each side tells its contacts its token inside their encrypted session,
//! when the session is confirmed and at every rotation, and a contact
//! recognises the advert by its tag and names that token in its hello,
//! whatever the link was given.
//! Nothing here is keyed to an identifier a stranger could copy. The
//! central side treats a peripheral that subscribed it but sent nothing
//! as refusing: it waits for a new tag on the advert, or [`REFUSED_MS`],
//! before asking again.
//!
//! Writes are queued per link and handed to CoreBluetooth only while it
//! has room (`canSendWriteWithoutResponse`, and `updateValue` returning
//! true), resumed when it says it is ready again. Nothing queued is
//! dropped quietly: a link whose queue passes [`QUEUE_BYTES`] is closed
//! with [`RadioEvent::Down`].

use std::collections::{BTreeMap, VecDeque};

use bombechat_session::TOKEN_LEN;

use crate::radio::{Advert, LinkId, Mode, Radio, RadioEvent, TAG_LEN};
use crate::transport::MAX_LINKS;

/// CoreBluetooth's identifier for a remote device (its `NSUUID` bytes).
pub(crate) type PeerId = [u8; 16];

/// The part of the token the advert shows.
pub(crate) type Tag = [u8; TAG_LEN];

/// The first byte of the notification that carries the whole token.
pub(crate) const TOKEN_MARK: u8 = 0x54;
/// The whole first notification to a central that is not given the
/// token: one byte, with no tag and nothing random.
pub(crate) const WITHHELD_MARK: u8 = 0x4e;

/// The sentence shown when Bluetooth is switched off on the Mac.
pub const BLUETOOTH_OFF: &str = "Bluetooth is off in System Settings.";
/// The sentence shown when this app may not use Bluetooth.
pub const BLUETOOTH_DENIED: &str =
    "Bombechat may not use Bluetooth. Allow it in System Settings, Privacy and Security, Bluetooth.";
/// The sentence shown when the Mac's Bluetooth can't advertise or connect
/// as Bombechat needs.
pub const BLUETOOTH_UNSUPPORTED: &str = "This Mac has no Bluetooth that Bombechat can use.";
/// The sentence shown when CoreBluetooth refused our service or advert.
pub const BLUETOOTH_FAILED: &str =
    "Bluetooth would not start. Turn it off and on again in System Settings.";

/// Connecting, discovering, subscribing and hearing the token must take
/// no longer than this, or the attempt is cancelled.
pub const CONNECT_MS: u64 = 10_000;
/// After an attempt fails or a link drops, that peer and advert are left
/// alone this long.
pub const RETRY_MS: u64 = 5_000;
/// A link this side closed is not reopened to the same advert; that is
/// forgotten after this long, past any token's life.
pub const CLOSED_MS: u64 = 20 * 60_000;
/// At most this many closed adverts are remembered.
pub const CLOSED_CAP: usize = 256;
/// A remote central is remembered this long after it was last heard
/// from (it is always remembered while subscribed)...
pub const CENTRAL_MEMORY_MS: u64 = 60 * 60_000;
/// ...and at most this many are remembered.
pub const CENTRALS_CAP: usize = 1024;
/// A peripheral that took our subscription but sent nothing at all is not
/// asked again until its advert shows a new tag, or this long.
pub const REFUSED_MS: u64 = 10 * 60_000;
/// At most this many connections being made at once.
pub const CONNECTING_CAP: usize = 4;
/// A link whose writes waiting for room pass this many bytes is closed.
pub const QUEUE_BYTES: usize = 256 * 1024;
/// The write size assumed for a link the radio doesn't know.
const MIN_WRITE: usize = 20;

/// Which of the two managers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    /// `CBCentralManager`: scans, connects, writes, hears notifications.
    Central,
    /// `CBPeripheralManager`: advertises, is written to, notifies.
    Peripheral,
}

/// A manager's state, as CoreBluetooth reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Power {
    Unknown,
    Resetting,
    Unsupported,
    Unauthorized,
    Off,
    On,
}

/// Whether this app may use Bluetooth, read without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// Not decided: creating a manager asks.
    NotAsked,
    /// Refused by the person, or by a policy.
    Denied,
    Allowed,
}

/// What the stack reports, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CbEvent {
    State(Role, Power),
    /// An advert with our service, and the tag in its local name if it
    /// carried a well-formed one.
    Discovered(PeerId, Option<Tag>),
    /// Our characteristic found on a connected peripheral and notifying.
    Ready(PeerId),
    /// A connection attempt failed, or discovery found no characteristic
    /// of ours, or it went away.
    Failed(PeerId),
    /// A connected peripheral disconnected.
    Disconnected(PeerId),
    /// A notification from a peripheral.
    Notified(PeerId, Vec<u8>),
    /// A peripheral can take writes without response again.
    CanWrite(PeerId),
    /// Our service was added, or refused.
    ServiceAdded(bool),
    /// Advertising began, or was refused.
    Advertising(bool),
    /// A central subscribed to our characteristic.
    Subscribed(PeerId),
    /// A central unsubscribed, or disconnected.
    Unsubscribed(PeerId),
    /// A write without response from a central.
    Written(PeerId, Vec<u8>),
    /// The peripheral manager has room for notifications again.
    CanNotify,
}

/// The CoreBluetooth calls the adapter makes: the seam the tests fake.
pub(crate) trait Stack {
    /// Whether this app may use Bluetooth. Never asks.
    fn access(&self) -> Access;
    /// Create both managers and their delegates (macOS asks for
    /// permission here if no one has decided yet).
    fn open(&mut self);
    /// Drop both managers, their delegates and every object they gave.
    fn close(&mut self);
    fn is_open(&self) -> bool;
    /// Milliseconds on a clock that only goes forward.
    fn now_ms(&self) -> u64;
    /// What happened since the last call.
    fn events(&mut self) -> Vec<CbEvent>;

    fn add_service(&mut self);
    fn remove_services(&mut self);
    fn start_advertising(&mut self, local_name: &str);
    fn stop_advertising(&mut self);
    fn scan(&mut self);
    fn stop_scan(&mut self);
    /// Connect, then discover our service and characteristic and
    /// subscribe ([`CbEvent::Ready`] when done).
    fn connect(&mut self, peer: PeerId);
    fn cancel(&mut self, peer: PeerId);
    /// Write without response, if the peripheral has room now.
    fn write(&mut self, peer: PeerId, bytes: &[u8]) -> bool;
    /// Notify one subscribed central, if there is room now.
    fn notify(&mut self, central: PeerId, bytes: &[u8]) -> bool;
    /// `maximumWriteValueLengthForType(withoutResponse)`.
    fn max_write(&self, peer: PeerId) -> usize;
    /// The central's `maximumUpdateValueLength`.
    fn max_notify(&self, central: PeerId) -> usize;
}

/// The advert's local name for `token`: its first [`TAG_LEN`] bytes in
/// URL-safe base64, 8 characters.
pub(crate) fn advert_name(token: &[u8; TOKEN_LEN]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(8);
    for three in token[..TAG_LEN].chunks(3) {
        let n = (three[0] as u32) << 16 | (three[1] as u32) << 8 | three[2] as u32;
        for shift in [18, 12, 6, 0] {
            out.push(ABC[(n >> shift) as usize & 63] as char);
        }
    }
    out
}

/// The tag in an advert's local name, if it is one of ours.
pub(crate) fn parse_tag(name: &str) -> Option<Tag> {
    let b = name.as_bytes();
    if b.len() != 8 {
        return None;
    }
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32)
    };
    let mut tag = [0u8; TAG_LEN];
    for (i, four) in b.chunks(4).enumerate() {
        let mut n = 0u32;
        for &c in four {
            n = n << 6 | value(c)?;
        }
        tag[i * 3] = (n >> 16) as u8;
        tag[i * 3 + 1] = (n >> 8) as u8;
        tag[i * 3 + 2] = n as u8;
    }
    Some(tag)
}

fn tag_of(token: &[u8; TOKEN_LEN]) -> Tag {
    let mut t = [0u8; TAG_LEN];
    t.copy_from_slice(&token[..TAG_LEN]);
    t
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Central side: connecting, discovering, subscribing, or waiting
    /// for the token.
    Connecting {
        since_ms: u64,
        /// Subscribed to the peripheral's characteristic (no token yet).
        subscribed: bool,
    },
    Up,
}

struct Conn {
    link: LinkId,
    state: State,
    /// The advert's tag, for a central-side connection.
    tag: Option<Tag>,
    queue: VecDeque<Vec<u8>>,
    queued: usize,
    /// A peripheral-side link whose central was not given the token: its
    /// traffic keeps nothing remembered.
    withheld: bool,
}

/// A remembered advert this side won't connect to.
#[derive(Debug, Clone, Copy)]
struct Closed {
    peer: PeerId,
    tag: Option<Tag>,
    until_ms: u64,
    /// Closed on purpose: blocked until the advert shows another tag.
    /// Otherwise (a failure) blocked only until `until_ms`.
    until_new_tag: bool,
}

/// What this side remembers of a remote central, by its identifier.
#[derive(Debug, Clone, Copy)]
struct Heard {
    /// The token period it was first heard in.
    epoch: u64,
    /// It has had the token: never again.
    served: bool,
    subscribed: bool,
    last_ms: u64,
}

/// The CoreBluetooth radio over any [`Stack`].
pub(crate) struct Adapter<S: Stack> {
    stack: S,
    running: bool,
    token: Option<[u8; TOKEN_LEN]>,
    central: Power,
    peripheral: Power,
    service_added: bool,
    adding_service: bool,
    advertising: bool,
    advert_confirmed: bool,
    scanning: bool,
    reported: Option<&'static str>,
    conns: BTreeMap<(Role, PeerId), Conn>,
    ids: BTreeMap<LinkId, (Role, PeerId)>,
    closed: Vec<Closed>,
    /// Which token period this is: one more at every new token.
    epoch: u64,
    /// Remote centrals heard from, kept across `stop` (their connections
    /// may outlive it), forgotten when Bluetooth goes off.
    centrals: BTreeMap<PeerId, Heard>,
    /// What happened that the hardware tests print.
    #[cfg(test)]
    pub notes: Vec<String>,
    next_link: u64,
    events: Vec<RadioEvent>,
}

impl<S: Stack> Adapter<S> {
    pub fn new(stack: S) -> Adapter<S> {
        Adapter {
            stack,
            running: false,
            token: None,
            central: Power::Unknown,
            peripheral: Power::Unknown,
            service_added: false,
            adding_service: false,
            advertising: false,
            advert_confirmed: false,
            scanning: false,
            reported: None,
            conns: BTreeMap::new(),
            ids: BTreeMap::new(),
            closed: Vec::new(),
            epoch: 0,
            centrals: BTreeMap::new(),
            #[cfg(test)]
            notes: Vec::new(),
            next_link: 1,
            events: Vec::new(),
        }
    }

    pub fn stack(&self) -> &S {
        &self.stack
    }

    #[cfg(test)]
    pub fn stack_mut(&mut self) -> &mut S {
        &mut self.stack
    }

    /// Whether CoreBluetooth said advertising began.
    pub fn is_advertising(&self) -> bool {
        self.advert_confirmed
    }

    /// Whether both managers are powered on.
    pub fn is_powered_on(&self) -> bool {
        self.central == Power::On && self.peripheral == Power::On
    }

    /// Links up, and connections being made.
    pub fn connections(&self) -> (usize, usize) {
        let up = self.conns.values().filter(|c| c.state == State::Up).count();
        (up, self.conns.len() - up)
    }

    fn report(&mut self, sentence: &'static str) {
        if self.reported != Some(sentence) {
            self.reported = Some(sentence);
            self.events
                .push(RadioEvent::Unavailable(sentence.to_string()));
        }
    }

    fn fresh_link(&mut self) -> LinkId {
        let id = LinkId(self.next_link);
        self.next_link += 1;
        id
    }

    /// Forget a connection; a link that was up goes Down if `announce`.
    fn drop_conn(&mut self, key: (Role, PeerId), announce: bool) -> Option<Conn> {
        let conn = self.conns.remove(&key)?;
        self.ids.remove(&conn.link);
        if announce && conn.state == State::Up {
            self.events.push(RadioEvent::Down(conn.link));
        }
        Some(conn)
    }

    /// Leave `peer`'s advert with `tag` alone for `hold_ms`, or less if
    /// `until_new_tag` and the advert shows a new tag sooner.
    fn remember_closed(
        &mut self,
        peer: PeerId,
        tag: Option<Tag>,
        hold_ms: u64,
        until_new_tag: bool,
    ) {
        let now = self.stack.now_ms();
        let until_ms = now + hold_ms;
        self.closed.retain(|c| !(c.peer == peer && c.tag == tag));
        if self.closed.len() >= CLOSED_CAP {
            self.closed.remove(0);
        }
        self.closed.push(Closed {
            peer,
            tag,
            until_ms,
            until_new_tag,
        });
    }

    fn blocked(&self, peer: PeerId, tag: Option<Tag>) -> bool {
        let now = self.stack.now_ms();
        self.closed.iter().any(|c| {
            if now >= c.until_ms {
                return false;
            }
            if c.until_new_tag {
                // the same advert, by this peer or another showing its
                // tag; or this peer with no tag, so no sign of a new one
                (c.tag == tag && (c.peer == peer || c.tag.is_some()))
                    || (c.peer == peer && tag.is_none())
            } else {
                c.peer == peer
            }
        })
    }

    // ---- power and permission -----------------------------------------

    fn on_state(&mut self, role: Role, power: Power) {
        match role {
            Role::Central => self.central = power,
            Role::Peripheral => self.peripheral = power,
        }
        if power != Power::On {
            self.lose(role, power);
        }
        let states = [self.central, self.peripheral];
        if states.contains(&Power::Unauthorized) {
            self.report(BLUETOOTH_DENIED);
        } else if states.contains(&Power::Off) {
            self.report(BLUETOOTH_OFF);
        } else if states.contains(&Power::Unsupported) {
            self.report(BLUETOOTH_UNSUPPORTED);
        } else if self.is_powered_on() {
            if self.reported.take().is_some() {
                self.events.push(RadioEvent::Available);
            }
            self.arm();
        }
    }

    /// A manager left the powered-on state: CoreBluetooth has stopped
    /// its scanning or advertising, cleared its services and dropped its
    /// connections, so forget them, and say so for each link.
    fn lose(&mut self, role: Role, power: Power) {
        let keys: Vec<(Role, PeerId)> = self
            .conns
            .keys()
            .filter(|(r, _)| *r == role)
            .copied()
            .collect();
        for key in keys {
            self.drop_conn(key, true);
        }
        match role {
            Role::Central => self.scanning = false,
            Role::Peripheral => {
                self.service_added = false;
                self.adding_service = false;
                self.advertising = false;
                self.advert_confirmed = false;
                // Bluetooth went off or was reset: every connection with
                // it ended. (Not when permission is withdrawn: a remote
                // central's connection may outlive that.)
                if matches!(power, Power::Off | Power::Resetting) {
                    self.centrals.clear();
                }
            }
        }
    }

    /// Both managers are on: publish, scan and advertise as needed.
    fn arm(&mut self) {
        if !self.running || !self.is_powered_on() {
            return;
        }
        if !self.service_added && !self.adding_service {
            self.adding_service = true;
            self.stack.add_service();
        }
        if !self.scanning {
            self.scanning = true;
            self.stack.scan();
        }
        self.advertise_now();
    }

    fn advertise_now(&mut self) {
        if self.advertising || !self.service_added || self.peripheral != Power::On {
            return;
        }
        if let Some(token) = self.token {
            self.advertising = true;
            self.stack.start_advertising(&advert_name(&token));
        }
    }

    // ---- central side -------------------------------------------------

    fn on_discovered(&mut self, peer: PeerId, tag: Option<Tag>) {
        if !self.running || self.central != Power::On {
            return;
        }
        let key = (Role::Central, peer);
        if self.conns.contains_key(&key) || self.blocked(peer, tag) {
            return;
        }
        let central = self.conns.keys().filter(|(r, _)| *r == Role::Central);
        let connecting = self
            .conns
            .values()
            .filter(|c| matches!(c.state, State::Connecting { .. }))
            .count();
        if central.count() >= MAX_LINKS || connecting >= CONNECTING_CAP {
            return;
        }
        let link = self.fresh_link();
        let since_ms = self.stack.now_ms();
        self.conns.insert(
            key,
            Conn {
                link,
                state: State::Connecting {
                    since_ms,
                    subscribed: false,
                },
                tag,
                queue: VecDeque::new(),
                queued: 0,
                withheld: false,
            },
        );
        self.ids.insert(link, key);
        self.stack.connect(peer);
    }

    /// A central-side attempt that failed, or a link that dropped: cancel
    /// it, and leave the advert alone for [`RETRY_MS`].
    fn fail(&mut self, peer: PeerId) {
        if let Some(conn) = self.drop_conn((Role::Central, peer), true) {
            self.stack.cancel(peer);
            self.remember_closed(peer, conn.tag, RETRY_MS, false);
        }
    }

    /// A peripheral took our subscription but sent nothing: it doesn't
    /// serve this device. Asking again before its advert changes would get
    /// the same answer, so wait for a new tag, or [`REFUSED_MS`].
    fn refused(&mut self, peer: PeerId) {
        if let Some(conn) = self.drop_conn((Role::Central, peer), true) {
            self.stack.cancel(peer);
            self.remember_closed(peer, conn.tag, REFUSED_MS, true);
        }
    }

    fn on_notified(&mut self, peer: PeerId, bytes: Vec<u8>) {
        let Some(conn) = self.conns.get(&(Role::Central, peer)) else {
            return;
        };
        if conn.state == State::Up {
            self.events.push(RadioEvent::Bytes(conn.link, bytes));
            return;
        }
        // the first notification must be the token, or the mark that it
        // is withheld, and nothing else
        let advert = if bytes == [WITHHELD_MARK] {
            // the tag is the one the scan saw, never from the link
            Advert::Withheld(conn.tag)
        } else if bytes.len() == 1 + TOKEN_LEN && bytes[0] == TOKEN_MARK {
            let mut token = [0u8; TOKEN_LEN];
            token.copy_from_slice(&bytes[1..]);
            Advert::Token(token)
        } else {
            self.fail(peer);
            return;
        };
        if let Some(conn) = self.conns.get_mut(&(Role::Central, peer)) {
            conn.state = State::Up;
            if let Advert::Token(token) = advert {
                // the link is to the advert with this token now
                conn.tag = Some(tag_of(&token));
            }
            self.events.push(RadioEvent::Up {
                link: conn.link,
                advert,
            });
        }
    }

    // ---- peripheral side ----------------------------------------------

    /// `central`'s record, made now if it is new. When the memory is
    /// full, a record from this token's period goes to make room (that
    /// central could only be served the token it may have had already);
    /// if every record is from an earlier period, none is forgotten and
    /// the newcomer is not given the token (`None`): that side fails
    /// closed, and links this device opens are unaffected.
    fn record(&mut self, central: PeerId) -> Option<&mut Heard> {
        let (now, epoch) = (self.stack.now_ms(), self.epoch);
        if !self.centrals.contains_key(&central) && self.centrals.len() >= CENTRALS_CAP {
            let spare = self
                .centrals
                .iter()
                .filter(|(_, h)| h.epoch == epoch)
                .min_by_key(|(_, h)| (h.subscribed, h.last_ms))
                .map(|(c, _)| *c);
            self.centrals.remove(&spare?);
        }
        Some(self.centrals.entry(central).or_insert(Heard {
            epoch,
            served: false,
            subscribed: false,
            last_ms: now,
        }))
    }

    /// Accepted traffic from `central`: only this keeps its record fresh.
    fn touch(&mut self, central: PeerId) {
        let now = self.stack.now_ms();
        if let Some(h) = self.centrals.get_mut(&central) {
            h.last_ms = now;
        }
    }

    fn on_subscribed(&mut self, central: PeerId) {
        let key = (Role::Peripheral, central);
        if !self.running || self.conns.contains_key(&key) {
            return;
        }
        let epoch = self.epoch;
        // the token at most once per connection, and only to a central
        // first heard in this token's period; every other subscription
        // gets a link without it (a full memory of earlier centrals gives
        // none either)
        let given = match self.record(central) {
            Some(h) => {
                h.subscribed = true;
                !h.served && h.epoch == epoch
            }
            None => false,
        };
        let Some(token) = self.token else { return };
        if self
            .conns
            .keys()
            .filter(|(r, _)| *r == Role::Peripheral)
            .count()
            >= MAX_LINKS
        {
            return;
        }
        let first = if given {
            if let Some(h) = self.centrals.get_mut(&central) {
                h.served = true;
            }
            // only a link given the token keeps its central remembered
            self.touch(central);
            let mut first = Vec::with_capacity(1 + TOKEN_LEN);
            first.push(TOKEN_MARK);
            first.extend_from_slice(&token);
            first
        } else {
            #[cfg(test)]
            self.notes.push(format!(
                "central {:02x}{:02x}{:02x}{:02x} linked without the token",
                central[0], central[1], central[2], central[3]
            ));
            // nothing of the token, nor of the advert: a held connection
            // learns nothing of where this device went
            vec![WITHHELD_MARK]
        };
        let link = self.fresh_link();
        self.conns.insert(
            key,
            Conn {
                link,
                state: State::Up,
                tag: None,
                queue: VecDeque::new(),
                queued: 0,
                withheld: !given,
            },
        );
        self.ids.insert(link, key);
        self.events.push(RadioEvent::Up {
            link,
            advert: Advert::Unseen,
        });
        // the token, or the mark that it is withheld, goes before
        // anything else
        self.enqueue(key, first);
    }

    // ---- writing ------------------------------------------------------

    fn enqueue(&mut self, key: (Role, PeerId), bytes: Vec<u8>) {
        let Some(conn) = self.conns.get_mut(&key) else {
            return;
        };
        conn.queued += bytes.len();
        conn.queue.push_back(bytes);
        if conn.queued > QUEUE_BYTES {
            // never dropped quietly: the link closes, and says so; it is
            // congestion, not a choice, so the advert may be tried again
            self.close(key, true, false);
            return;
        }
        self.pump(key);
    }

    /// Hand queued writes to CoreBluetooth while it has room.
    fn pump(&mut self, key: (Role, PeerId)) {
        let Some(conn) = self.conns.get_mut(&key) else {
            return;
        };
        while let Some(front) = conn.queue.front() {
            let taken = match key.0 {
                Role::Central => self.stack.write(key.1, front),
                Role::Peripheral => self.stack.notify(key.1, front),
            };
            if !taken {
                break;
            }
            conn.queued -= front.len();
            conn.queue.pop_front();
        }
    }

    /// Close a link: cancel a central-side connection and remember its
    /// advert (until its tag changes if `on_purpose`, else for
    /// [`RETRY_MS`]), or stop using a central's subscription.
    fn close(&mut self, key: (Role, PeerId), announce: bool, on_purpose: bool) {
        let Some(conn) = self.drop_conn(key, announce) else {
            return;
        };
        match key.0 {
            Role::Central => {
                self.stack.cancel(key.1);
                if on_purpose {
                    self.remember_closed(key.1, conn.tag, CLOSED_MS, true);
                } else {
                    self.remember_closed(key.1, conn.tag, RETRY_MS, false);
                }
            }
            // it was served already: a new subscription gets a link
            // without the token
            Role::Peripheral => {}
        }
    }

    fn on_event(&mut self, event: CbEvent) {
        match event {
            CbEvent::State(role, power) => self.on_state(role, power),
            CbEvent::Discovered(peer, tag) => self.on_discovered(peer, tag),
            CbEvent::Ready(peer) => {
                if let Some(conn) = self.conns.get_mut(&(Role::Central, peer)) {
                    if let State::Connecting { subscribed, .. } = &mut conn.state {
                        *subscribed = true;
                    }
                }
            }
            CbEvent::Failed(peer) => self.fail(peer),
            CbEvent::Disconnected(peer) => {
                if let Some(conn) = self.drop_conn((Role::Central, peer), true) {
                    self.remember_closed(peer, conn.tag, RETRY_MS, false);
                }
            }
            CbEvent::Notified(peer, bytes) => self.on_notified(peer, bytes),
            CbEvent::CanWrite(peer) => self.pump((Role::Central, peer)),
            CbEvent::ServiceAdded(ok) => {
                self.adding_service = false;
                self.service_added = ok;
                if ok {
                    self.advertise_now();
                } else {
                    self.report(BLUETOOTH_FAILED);
                }
            }
            CbEvent::Advertising(ok) => {
                self.advert_confirmed = ok && self.advertising;
                if !ok {
                    self.advertising = false;
                    self.report(BLUETOOTH_FAILED);
                }
            }
            CbEvent::Subscribed(central) => self.on_subscribed(central),
            CbEvent::Unsubscribed(central) => {
                if let Some(h) = self.centrals.get_mut(&central) {
                    h.subscribed = false;
                }
                if let Some(conn) = self.drop_conn((Role::Peripheral, central), true) {
                    if !conn.withheld {
                        self.touch(central);
                    }
                }
            }
            CbEvent::Written(central, bytes) => {
                // first heard now, if never before
                let _ = self.record(central);
                if let Some(conn) = self.conns.get(&(Role::Peripheral, central)) {
                    let given = !conn.withheld;
                    self.events.push(RadioEvent::Bytes(conn.link, bytes));
                    if given {
                        self.touch(central);
                    }
                }
            }
            CbEvent::CanNotify => {
                let keys: Vec<(Role, PeerId)> = self
                    .conns
                    .keys()
                    .filter(|(r, _)| *r == Role::Peripheral)
                    .copied()
                    .collect();
                for key in keys {
                    self.pump(key);
                }
            }
        }
    }

    /// Attempts that took too long are cancelled; old closed adverts are
    /// forgotten.
    fn upkeep(&mut self) {
        let now = self.stack.now_ms();
        let late: Vec<(PeerId, bool)> = self
            .conns
            .iter()
            .filter_map(|((role, peer), c)| match c.state {
                State::Connecting {
                    since_ms,
                    subscribed,
                } if *role == Role::Central && now.saturating_sub(since_ms) >= CONNECT_MS => {
                    Some((*peer, subscribed))
                }
                _ => None,
            })
            .collect();
        for (peer, subscribed) in late {
            if subscribed {
                self.refused(peer);
            } else {
                self.fail(peer);
            }
        }
        self.closed.retain(|c| now < c.until_ms);
        self.centrals
            .retain(|_, h| h.subscribed || now.saturating_sub(h.last_ms) < CENTRAL_MEMORY_MS);
    }
}

impl<S: Stack + Send> Radio for Adapter<S> {
    fn start(&mut self, _mode: Mode) {
        // The mode is the link layer's business: the radio does the same
        // in Contacts and Open.
        if self.running {
            return;
        }
        if !self.stack.is_open() {
            if self.stack.access() == Access::Denied {
                // asked before and refused: no managers, no prompt
                self.report(BLUETOOTH_DENIED);
                return;
            }
            self.stack.open();
        }
        self.running = true;
        self.reported = None;
    }

    fn stop(&mut self) {
        if self.stack.is_open() {
            if self.advertising && self.peripheral == Power::On {
                self.stack.stop_advertising();
            }
            if self.scanning && self.central == Power::On {
                self.stack.stop_scan();
            }
            let central: Vec<PeerId> = self
                .conns
                .keys()
                .filter(|(r, _)| *r == Role::Central)
                .map(|(_, p)| *p)
                .collect();
            for peer in central {
                self.stack.cancel(peer);
            }
            if (self.service_added || self.adding_service) && self.peripheral == Power::On {
                self.stack.remove_services();
            }
            self.stack.close();
        }
        // Forget everything but the link numbers, which are never reused.
        self.running = false;
        self.token = None;
        self.central = Power::Unknown;
        self.peripheral = Power::Unknown;
        self.service_added = false;
        self.adding_service = false;
        self.advertising = false;
        self.advert_confirmed = false;
        self.scanning = false;
        self.reported = None;
        self.conns.clear();
        self.ids.clear();
        self.closed.clear();
        self.events.clear();
    }

    fn advertise(&mut self, token: [u8; TOKEN_LEN]) {
        if self.token != Some(token) {
            // a new token period: no central heard before gets this one
            self.epoch += 1;
        }
        self.token = Some(token);
        if self.advertising {
            // stop and start again, so macOS may give the new advert a
            // new address along with the new token
            self.stack.stop_advertising();
            self.advertising = false;
            self.advert_confirmed = false;
        }
        if self.running {
            self.advertise_now();
        }
    }

    fn disconnect(&mut self, link: LinkId) {
        if let Some(&key) = self.ids.get(&link) {
            self.close(key, false, true);
        }
    }

    fn write(&mut self, link: LinkId, bytes: &[u8]) {
        let Some(&key) = self.ids.get(&link) else {
            return;
        };
        if self.conns.get(&key).is_some_and(|c| c.state == State::Up) {
            self.enqueue(key, bytes.to_vec());
        }
    }

    fn poll(&mut self) -> Vec<RadioEvent> {
        if self.stack.is_open() {
            for event in self.stack.events() {
                self.on_event(event);
            }
            if self.running {
                self.upkeep();
            }
        }
        std::mem::take(&mut self.events)
    }

    fn max_write(&self, link: LinkId) -> usize {
        match self.ids.get(&link) {
            Some((Role::Central, peer)) => self.stack.max_write(*peer),
            Some((Role::Peripheral, central)) => self.stack.max_notify(*central),
            None => MIN_WRITE,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// CoreBluetooth as the adapter sees it, keeping score of what is
    /// alive: managers, delegates, the advert, the scan, our service and
    /// every connection. Closing drops only the managers and delegates,
    /// so the rest is left for the adapter to have undone first.
    struct Fake {
        access: Access,
        opened: u32,
        managers: u32,
        delegates: u32,
        advert: Option<String>,
        scanning: bool,
        services: u32,
        connections: BTreeSet<PeerId>,
        inbox: Vec<CbEvent>,
        now: u64,
        room: bool,
        notify_room: bool,
        writes: Vec<(PeerId, Vec<u8>)>,
        notes: Vec<(PeerId, Vec<u8>)>,
        log: Vec<String>,
    }

    impl Fake {
        fn new(access: Access) -> Fake {
            Fake {
                access,
                opened: 0,
                managers: 0,
                delegates: 0,
                advert: None,
                scanning: false,
                services: 0,
                connections: BTreeSet::new(),
                inbox: Vec::new(),
                now: 0,
                room: true,
                notify_room: true,
                writes: Vec::new(),
                notes: Vec::new(),
                log: Vec::new(),
            }
        }
    }

    impl Stack for Fake {
        fn access(&self) -> Access {
            self.access
        }
        fn open(&mut self) {
            self.opened += 1;
            self.managers = 2;
            self.delegates = 1;
            self.log.push("open".into());
        }
        fn close(&mut self) {
            self.managers = 0;
            self.delegates = 0;
            self.inbox.clear();
            self.log.push("close".into());
        }
        fn is_open(&self) -> bool {
            self.managers > 0
        }
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn events(&mut self) -> Vec<CbEvent> {
            std::mem::take(&mut self.inbox)
        }
        fn add_service(&mut self) {
            self.services += 1;
            self.log.push("add service".into());
        }
        fn remove_services(&mut self) {
            self.services = 0;
            self.log.push("remove services".into());
        }
        fn start_advertising(&mut self, local_name: &str) {
            self.advert = Some(local_name.to_string());
            self.log.push(format!("advertise {local_name}"));
        }
        fn stop_advertising(&mut self) {
            self.advert = None;
            self.log.push("stop advertising".into());
        }
        fn scan(&mut self) {
            self.scanning = true;
            self.log.push("scan".into());
        }
        fn stop_scan(&mut self) {
            self.scanning = false;
            self.log.push("stop scan".into());
        }
        fn connect(&mut self, peer: PeerId) {
            self.connections.insert(peer);
            self.log.push(format!("connect {}", peer[0]));
        }
        fn cancel(&mut self, peer: PeerId) {
            self.connections.remove(&peer);
            self.log.push(format!("cancel {}", peer[0]));
        }
        fn write(&mut self, peer: PeerId, bytes: &[u8]) -> bool {
            if self.room {
                self.writes.push((peer, bytes.to_vec()));
            }
            self.room
        }
        fn notify(&mut self, central: PeerId, bytes: &[u8]) -> bool {
            if self.notify_room {
                self.notes.push((central, bytes.to_vec()));
            }
            self.notify_room
        }
        fn max_write(&self, _peer: PeerId) -> usize {
            182
        }
        fn max_notify(&self, _central: PeerId) -> usize {
            150
        }
    }

    const TOKEN: [u8; TOKEN_LEN] = [7; TOKEN_LEN];

    fn peer(n: u8) -> PeerId {
        [n; 16]
    }

    fn token(n: u8) -> [u8; TOKEN_LEN] {
        [n; TOKEN_LEN]
    }

    fn tag(n: u8) -> Option<Tag> {
        Some(tag_of(&token(n)))
    }

    fn token_note(t: [u8; TOKEN_LEN]) -> Vec<u8> {
        let mut v = vec![TOKEN_MARK];
        v.extend_from_slice(&t);
        v
    }

    type Radio_ = Adapter<Fake>;

    fn push(a: &mut Radio_, events: impl IntoIterator<Item = CbEvent>) -> Vec<RadioEvent> {
        a.stack_mut().inbox.extend(events);
        a.poll()
    }

    /// Started, powered on, published, scanning and advertising.
    fn running() -> Radio_ {
        let mut a = Adapter::new(Fake::new(Access::Allowed));
        a.start(Mode::Contacts);
        a.advertise(TOKEN);
        let ev = push(
            &mut a,
            [
                CbEvent::State(Role::Central, Power::On),
                CbEvent::State(Role::Peripheral, Power::On),
                CbEvent::ServiceAdded(true),
                CbEvent::Advertising(true),
            ],
        );
        assert!(ev.is_empty(), "{ev:?}");
        assert!(a.is_advertising());
        assert_eq!(a.stack().advert.as_deref(), Some(&*advert_name(&TOKEN)));
        assert!(a.stack().scanning);
        a
    }

    /// A central-side link to peer `n`, advertising token `n`.
    fn link_to(a: &mut Radio_, n: u8) -> LinkId {
        assert!(push(a, [CbEvent::Discovered(peer(n), tag(n))]).is_empty());
        assert!(a.stack().connections.contains(&peer(n)));
        let ev = push(
            a,
            [
                CbEvent::Ready(peer(n)),
                CbEvent::Notified(peer(n), token_note(token(n))),
            ],
        );
        match ev.as_slice() {
            [RadioEvent::Up {
                link,
                advert: Advert::Token(t),
            }] => {
                assert_eq!(*t, token(n));
                *link
            }
            other => panic!("expected one Up, got {other:?}"),
        }
    }

    /// A peripheral-side link from central `n`.
    fn link_from(a: &mut Radio_, n: u8) -> LinkId {
        match push(a, [CbEvent::Subscribed(peer(n))]).as_slice() {
            [RadioEvent::Up {
                link,
                advert: Advert::Unseen,
            }] => *link,
            other => panic!("expected one Up, got {other:?}"),
        }
    }

    /// Review Focus 1: stop leaves nothing of CoreBluetooth alive, from
    /// the busiest state there is.
    #[test]
    fn stop_leaves_nothing_alive() {
        let mut a = running();
        let up = link_to(&mut a, 1);
        push(&mut a, [CbEvent::Discovered(peer(2), tag(2))]); // still connecting
        let theirs = link_from(&mut a, 3);
        a.stack_mut().room = false;
        a.write(up, b"waiting for room");
        assert_eq!(a.connections(), (2, 1));
        assert_eq!(a.stack().connections.len(), 2);
        a.stack_mut().log.clear();
        a.stack_mut().notes.clear();

        a.stop();

        let f = a.stack();
        assert_eq!((f.managers, f.delegates), (0, 0), "both managers dropped");
        assert_eq!(f.advert, None, "not advertising");
        assert!(!f.scanning, "not scanning");
        assert_eq!(f.services, 0, "our service removed");
        assert!(f.connections.is_empty(), "every connection cancelled");
        assert_eq!(
            f.log,
            [
                "stop advertising",
                "stop scan",
                "cancel 1",
                "cancel 2",
                "remove services",
                "close",
            ],
            "everything undone before the managers go"
        );
        assert_eq!(a.connections(), (0, 0));
        assert!(!a.is_advertising() && !a.is_powered_on());
        // nothing comes from the old radio, and its links are gone
        assert!(a.poll().is_empty());
        a.stack_mut().room = true;
        a.write(up, b"x");
        a.write(theirs, b"y");
        assert!(a.stack().writes.is_empty() && a.stack().notes.is_empty());
        assert_eq!(a.max_write(up), MIN_WRITE);
        // late events from CoreBluetooth reach nothing
        a.stack_mut().inbox.push(CbEvent::Subscribed(peer(4)));
        assert!(a.poll().is_empty());
        // stopping again does nothing; starting again opens afresh, and
        // link numbers are never reused
        a.stack_mut().log.clear();
        a.stop();
        assert!(a.stack().log.is_empty());
        let mut b = a;
        b.start(Mode::Open);
        b.advertise(TOKEN);
        push(
            &mut b,
            [
                CbEvent::State(Role::Central, Power::On),
                CbEvent::State(Role::Peripheral, Power::On),
                CbEvent::ServiceAdded(true),
            ],
        );
        assert_eq!(b.stack().opened, 2);
        let again = link_to(&mut b, 1);
        assert!(again.0 > theirs.0 && again.0 > up.0);
    }

    #[test]
    fn nothing_is_created_before_start_or_after_a_stop_when_off() {
        let mut a = Adapter::new(Fake::new(Access::NotAsked));
        a.advertise(TOKEN);
        assert!(a.poll().is_empty());
        a.stop();
        assert_eq!(a.stack().opened, 0);
        assert!(a.stack().log.is_empty());
    }

    #[test]
    fn permission_is_asked_at_the_first_start_and_a_refusal_creates_nothing() {
        // not decided: start creates the managers, which is when macOS asks
        let mut a = Adapter::new(Fake::new(Access::NotAsked));
        a.start(Mode::Contacts);
        assert_eq!(a.stack().opened, 1);
        // refused then: unavailable, nothing created, the person told how
        let mut a = Adapter::new(Fake::new(Access::Denied));
        a.start(Mode::Contacts);
        a.advertise(TOKEN);
        assert_eq!(a.stack().opened, 0, "no managers, so no prompt");
        assert_eq!(
            a.poll(),
            vec![RadioEvent::Unavailable(
                "Bombechat may not use Bluetooth. Allow it in System Settings, Privacy and Security, Bluetooth.".into()
            )]
        );
        a.start(Mode::Open);
        assert!(a.poll().is_empty(), "said once");
        // allowed later in System Settings: the next start works
        a.stop();
        a.stack_mut().access = Access::Allowed;
        a.start(Mode::Contacts);
        assert_eq!(a.stack().opened, 1);
        // refused while running (CoreBluetooth reports unauthorized)
        let mut a = running();
        let ev = push(
            &mut a,
            [CbEvent::State(Role::Peripheral, Power::Unauthorized)],
        );
        assert_eq!(ev, vec![RadioEvent::Unavailable(BLUETOOTH_DENIED.into())]);
    }

    #[test]
    fn bluetooth_off_is_unavailable_and_on_again_recovers_without_a_start() {
        let mut a = running();
        let mine = link_to(&mut a, 1);
        let theirs = link_from(&mut a, 2);
        let ev = push(
            &mut a,
            [
                CbEvent::State(Role::Central, Power::Off),
                CbEvent::State(Role::Peripheral, Power::Off),
            ],
        );
        assert_eq!(
            ev,
            vec![
                RadioEvent::Down(mine),
                RadioEvent::Unavailable("Bluetooth is off in System Settings.".into()),
                RadioEvent::Down(theirs),
            ]
        );
        assert!(!a.is_advertising());
        a.stack_mut().log.clear();
        // the person turns Bluetooth on again: no start from above
        let ev = push(
            &mut a,
            [
                CbEvent::State(Role::Peripheral, Power::On),
                CbEvent::State(Role::Central, Power::On),
                CbEvent::ServiceAdded(true),
                CbEvent::Advertising(true),
            ],
        );
        assert_eq!(ev, vec![RadioEvent::Available], "said to be back, once");
        assert_eq!(
            a.stack().log,
            vec![
                "add service".to_string(),
                "scan".into(),
                format!("advertise {}", advert_name(&TOKEN)),
            ]
        );
        assert!(a.is_advertising());
        link_to(&mut a, 3);
        // off again is said again
        let ev = push(&mut a, [CbEvent::State(Role::Central, Power::Off)]);
        assert!(ev.contains(&RadioEvent::Unavailable(BLUETOOTH_OFF.into())));
    }

    #[test]
    fn a_new_token_restarts_the_advert_with_a_new_tag() {
        let mut a = running();
        a.stack_mut().log.clear();
        a.advertise(token(9));
        assert_eq!(
            a.stack().log,
            vec![
                "stop advertising".to_string(),
                format!("advertise {}", advert_name(&token(9))),
            ]
        );
        assert!(!a.is_advertising(), "until CoreBluetooth confirms");
        push(&mut a, [CbEvent::Advertising(true)]);
        assert!(a.is_advertising());
    }

    #[test]
    fn the_advert_tag_is_eight_characters_and_reads_back() {
        let mut t = [0u8; TOKEN_LEN];
        for seed in 0..200u32 {
            for (i, b) in t.iter_mut().enumerate() {
                *b = (seed.wrapping_mul(2654435761).rotate_left(i as u32) >> 3) as u8;
            }
            let name = advert_name(&t);
            assert_eq!(name.len(), 8);
            assert!(name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
            assert_eq!(parse_tag(&name), Some(tag_of(&t)));
        }
        assert_eq!(advert_name(&[0xff; TOKEN_LEN]), "________");
        for bad in [
            "",
            "AAAAAAA",
            "AAAAAAAAA",
            "AAAA+AAA",
            "MacBook ",
            "ÄAAAAAA",
        ] {
            assert_eq!(parse_tag(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_link_is_up_only_once_the_token_arrives_and_then_carries_bytes() {
        let mut a = running();
        push(&mut a, [CbEvent::Discovered(peer(1), tag(1))]);
        assert!(push(&mut a, [CbEvent::Ready(peer(1))]).is_empty());
        assert_eq!(a.connections(), (0, 1));
        let ev = push(&mut a, [CbEvent::Notified(peer(1), token_note(token(1)))]);
        let [RadioEvent::Up {
            link,
            advert: Advert::Token(t),
        }] = ev.as_slice()
        else {
            panic!("{ev:?}");
        };
        assert_eq!(*t, token(1));
        let ev = push(&mut a, [CbEvent::Notified(peer(1), b"hello".to_vec())]);
        assert_eq!(ev, vec![RadioEvent::Bytes(*link, b"hello".to_vec())]);
        // the peripheral went away
        assert_eq!(
            push(&mut a, [CbEvent::Disconnected(peer(1))]),
            vec![RadioEvent::Down(*link)]
        );
    }

    #[test]
    fn a_first_notification_that_is_not_a_token_ends_the_attempt() {
        let mut a = running();
        push(&mut a, [CbEvent::Discovered(peer(1), tag(1))]);
        let ev = push(
            &mut a,
            [
                CbEvent::Ready(peer(1)),
                CbEvent::Notified(peer(1), b"not a token at all".to_vec()),
            ],
        );
        assert!(ev.is_empty(), "no Up, and no Down for a link never up");
        assert!(a.stack().connections.is_empty(), "cancelled");
        // left alone for a while, then tried again
        push(&mut a, [CbEvent::Discovered(peer(1), tag(1))]);
        assert!(a.stack().connections.is_empty());
        a.stack_mut().now += RETRY_MS;
        push(&mut a, [CbEvent::Discovered(peer(1), tag(1))]);
        assert!(a.stack().connections.contains(&peer(1)));
    }

    /// Ruling 3: never back to the same advert after `disconnect`.
    #[test]
    fn a_disconnected_advert_is_not_connected_again_until_its_token_changes() {
        let mut a = running();
        let link = link_to(&mut a, 1);
        a.disconnect(link);
        assert!(a.stack().connections.is_empty(), "cancelled");
        assert!(a.poll().is_empty(), "no Down for a link this side closed");
        // the same advert, from the same peer or under a new address
        push(&mut a, [CbEvent::Discovered(peer(1), tag(1))]);
        push(&mut a, [CbEvent::Discovered(peer(9), tag(1))]);
        assert!(a.stack().connections.is_empty());
        // much later it is still the same advert: still left alone, until
        // the memory runs out
        a.stack_mut().now += CLOSED_MS - 1;
        push(&mut a, [CbEvent::Discovered(peer(1), tag(1))]);
        assert!(a.stack().connections.is_empty());
        // a new token on that device is a new advert
        push(&mut a, [CbEvent::Discovered(peer(1), tag(2))]);
        assert_eq!(a.stack().connections, BTreeSet::from([peer(1)]));
        // and the old one is forgotten after CLOSED_MS
        let mut b = running();
        let link = link_to(&mut b, 1);
        b.disconnect(link);
        b.stack_mut().now += CLOSED_MS;
        push(&mut b, [CbEvent::Discovered(peer(1), tag(1))]);
        assert!(b.stack().connections.contains(&peer(1)));
    }

    #[test]
    fn an_attempt_that_takes_too_long_is_cancelled() {
        let mut a = running();
        push(&mut a, [CbEvent::Discovered(peer(1), tag(1))]);
        a.stack_mut().now += CONNECT_MS - 1;
        a.poll();
        assert!(a.stack().connections.contains(&peer(1)));
        a.stack_mut().now += 1;
        assert!(a.poll().is_empty());
        assert!(a.stack().connections.is_empty());
        assert_eq!(a.connections(), (0, 0));
    }

    #[test]
    fn at_most_four_connections_are_made_at_once() {
        let mut a = running();
        push(
            &mut a,
            (1..=6).map(|n| CbEvent::Discovered(peer(n), tag(n))),
        );
        assert_eq!(a.stack().connections.len(), CONNECTING_CAP);
    }

    #[test]
    fn writes_wait_for_room_and_go_in_order() {
        let mut a = running();
        let mine = link_to(&mut a, 1);
        a.stack_mut().room = false;
        a.write(mine, b"one");
        a.write(mine, b"two");
        assert!(a.stack().writes.is_empty());
        a.stack_mut().room = true;
        push(&mut a, [CbEvent::CanWrite(peer(1))]);
        assert_eq!(
            a.stack().writes,
            vec![(peer(1), b"one".to_vec()), (peer(1), b"two".to_vec())]
        );
        // the peripheral side: the token first, then writes, as room allows
        a.stack_mut().notify_room = false;
        let theirs = link_from(&mut a, 2);
        a.write(theirs, b"three");
        assert!(a.stack().notes.is_empty());
        a.stack_mut().notify_room = true;
        push(&mut a, [CbEvent::CanNotify]);
        assert_eq!(
            a.stack().notes,
            vec![(peer(2), token_note(TOKEN)), (peer(2), b"three".to_vec())]
        );
    }

    #[test]
    fn a_link_that_never_has_room_is_closed_out_loud() {
        let mut a = running();
        let mine = link_to(&mut a, 1);
        a.stack_mut().room = false;
        for _ in 0..=QUEUE_BYTES / 182 {
            a.write(mine, &[0; 182]);
        }
        assert_eq!(a.poll(), vec![RadioEvent::Down(mine)]);
        assert!(a.stack().connections.is_empty());
    }

    #[test]
    fn a_subscribing_central_is_a_link_that_hears_the_token_first() {
        let mut a = running();
        let link = link_from(&mut a, 2);
        assert_eq!(a.stack().notes, vec![(peer(2), token_note(TOKEN))]);
        let ev = push(&mut a, [CbEvent::Written(peer(2), b"hi".to_vec())]);
        assert_eq!(ev, vec![RadioEvent::Bytes(link, b"hi".to_vec())]);
        assert_eq!(
            push(&mut a, [CbEvent::Unsubscribed(peer(2))]),
            vec![RadioEvent::Down(link)]
        );
        // a write from a central that isn't subscribed goes nowhere
        assert!(push(&mut a, [CbEvent::Written(peer(5), b"x".to_vec())]).is_empty());
    }

    #[test]
    fn a_central_this_side_closed_is_ignored_and_never_served_again() {
        let mut a = running();
        let link = link_from(&mut a, 2);
        a.disconnect(link);
        assert!(a.poll().is_empty(), "no Down for a link this side closed");
        let ev = push(&mut a, [CbEvent::Written(peer(2), b"still here".to_vec())]);
        assert!(ev.is_empty());
        a.write(link, b"nothing goes");
        assert_eq!(a.stack().notes.len(), 1, "only the token, earlier");
        // letting go and subscribing again on the same connection: a new
        // link, but no token, the token was given once already
        let ev = push(
            &mut a,
            [CbEvent::Unsubscribed(peer(2)), CbEvent::Subscribed(peer(2))],
        );
        let again = match ev.as_slice() {
            [RadioEvent::Up {
                link,
                advert: Advert::Unseen,
            }] => *link,
            other => panic!("{other:?}"),
        };
        assert_ne!(again, link);
        assert_eq!(a.stack().notes[0], (peer(2), token_note(TOKEN)));
        assert!(is_withheld(&told(&mut a, 2)[1..]));
    }

    /// Exactly one note, the withheld mark and nothing else.
    fn is_withheld(notes: &[Vec<u8>]) -> bool {
        notes == [vec![WITHHELD_MARK]]
    }

    /// The notifications `a` has sent `central`, since the last look.
    fn told(a: &mut Radio_, central: u8) -> Vec<Vec<u8>> {
        let notes = std::mem::take(&mut a.stack_mut().notes);
        notes
            .into_iter()
            .filter(|(c, _)| *c == peer(central))
            .map(|(_, n)| n)
            .collect()
    }

    /// C1: a central holding its connection across this device's rotation
    /// gets no new token, however it subscribes again. It still gets a
    /// link, with the withheld mark first.
    #[test]
    fn a_central_from_before_a_rotation_never_hears_the_new_token() {
        let mut a = running();
        link_from(&mut a, 2);
        a.stack_mut().notes.clear();
        let ev = push(&mut a, [CbEvent::Unsubscribed(peer(2))]);
        assert_eq!(ev.len(), 1, "Down");
        a.advertise(token(9));
        let ev = push(
            &mut a,
            [CbEvent::Advertising(true), CbEvent::Subscribed(peer(2))],
        );
        assert!(
            matches!(
                ev.as_slice(),
                [RadioEvent::Up {
                    advert: Advert::Unseen,
                    ..
                }]
            ),
            "a link: {ev:?}"
        );
        assert!(is_withheld(&told(&mut a, 2)), "no token");
        // a central heard (only writing) before the rotation is no better
        let mut b = running();
        push(&mut b, [CbEvent::Written(peer(3), b"hello?".to_vec())]);
        b.advertise(token(9));
        push(&mut b, [CbEvent::Subscribed(peer(3))]);
        assert!(is_withheld(&told(&mut b, 3)));
        // forgetting takes being silent and unsubscribed for an hour, and
        // traffic on a link without the token isn't heard as such
        push(
            &mut b,
            [
                CbEvent::Written(peer(3), b"still me".to_vec()),
                CbEvent::Unsubscribed(peer(3)),
            ],
        );
        b.stack_mut().now += CENTRAL_MEMORY_MS - 1;
        push(&mut b, [CbEvent::Subscribed(peer(3))]);
        assert!(is_withheld(&told(&mut b, 3)));
    }

    /// C1: a contact's link across a rotation keeps working both ways.
    #[test]
    fn a_link_carried_across_a_rotation_is_still_served() {
        let mut a = running();
        let link = link_from(&mut a, 2);
        a.stack_mut().notes.clear();
        a.advertise(token(9));
        push(&mut a, [CbEvent::Advertising(true)]);
        let ev = push(
            &mut a,
            [CbEvent::Written(peer(2), b"from a contact".to_vec())],
        );
        assert_eq!(
            ev,
            vec![RadioEvent::Bytes(link, b"from a contact".to_vec())]
        );
        a.write(link, b"to the contact");
        assert_eq!(a.stack().notes, vec![(peer(2), b"to the contact".to_vec())]);
    }

    /// C1: a central first heard after the rotation gets the new token.
    #[test]
    fn a_new_central_after_a_rotation_hears_the_new_token() {
        let mut a = running();
        link_from(&mut a, 2);
        a.advertise(token(9));
        push(&mut a, [CbEvent::Advertising(true)]);
        a.stack_mut().notes.clear();
        link_from(&mut a, 4);
        assert_eq!(a.stack().notes, vec![(peer(4), token_note(token(9)))]);
        // and a central forgotten after an hour's silence counts as new
        let mut b = running();
        push(&mut b, [CbEvent::Written(peer(3), b"x".to_vec())]);
        b.advertise(token(9));
        b.stack_mut().now += CENTRAL_MEMORY_MS;
        b.poll();
        b.stack_mut().notes.clear();
        link_from(&mut b, 3);
        assert_eq!(b.stack().notes, vec![(peer(3), token_note(token(9)))]);
    }

    /// M1: an advert report without a tag doesn't reopen a closed link.
    #[test]
    fn a_report_with_no_tag_does_not_reopen_a_closed_advert() {
        let mut a = running();
        let link = link_to(&mut a, 1);
        a.disconnect(link);
        push(&mut a, [CbEvent::Discovered(peer(1), None)]);
        assert!(a.stack().connections.is_empty());
        // a tagless report from another peer is a different device
        push(&mut a, [CbEvent::Discovered(peer(2), None)]);
        assert!(a.stack().connections.contains(&peer(2)));
    }

    /// M4: closing for congestion only holds the advert off a moment.
    #[test]
    fn a_congestion_close_is_retried_soon() {
        let mut a = running();
        let mine = link_to(&mut a, 1);
        a.stack_mut().room = false;
        for _ in 0..=QUEUE_BYTES / 182 {
            a.write(mine, &[0; 182]);
        }
        assert_eq!(a.poll(), vec![RadioEvent::Down(mine)]);
        a.stack_mut().room = true;
        push(&mut a, [CbEvent::Discovered(peer(1), tag(1))]);
        assert!(a.stack().connections.is_empty(), "not at once");
        a.stack_mut().now += RETRY_MS;
        push(&mut a, [CbEvent::Discovered(peer(1), tag(1))]);
        assert!(
            a.stack().connections.contains(&peer(1)),
            "same advert, soon"
        );
    }

    fn many(n: u16) -> PeerId {
        let mut p = [0xab; 16];
        p[..2].copy_from_slice(&n.to_be_bytes());
        p
    }

    /// I-new-1: a refused central's attempts keep nothing alive: the hour
    /// counts from its last traffic on a link that was given the token.
    #[test]
    fn a_refused_central_retrying_is_forgotten_an_hour_after_it_was_served() {
        let mut a = running();
        link_from(&mut a, 2);
        push(&mut a, [CbEvent::Unsubscribed(peer(2))]); // last accepted, at 0
        a.advertise(token(9));
        push(&mut a, [CbEvent::Advertising(true)]);
        told(&mut a, 2);
        let mut served_at = None;
        while a.stack().now < CENTRAL_MEMORY_MS + 60_000 {
            a.stack_mut().now += 15_000;
            push(
                &mut a,
                [
                    CbEvent::Subscribed(peer(2)),
                    CbEvent::Written(peer(2), b"a hello".to_vec()),
                    CbEvent::Unsubscribed(peer(2)),
                ],
            );
            if told(&mut a, 2) == vec![token_note(token(9))] {
                served_at = Some(a.stack().now);
                break;
            }
        }
        let at = served_at.expect("served again once forgotten");
        assert!(
            (CENTRAL_MEMORY_MS..=CENTRAL_MEMORY_MS + 15_000).contains(&at),
            "{at}"
        );
    }

    /// Breaker ruling: a held connection that subscribes again after this
    /// device's rotation learns nothing of the new advert: the one-byte
    /// mark carries no tag and no token, every time. (It does learn it was
    /// heard before: the accepted "seen before" linker.)
    #[test]
    fn a_withheld_answer_carries_nothing_of_the_advert() {
        let mut a = running();
        link_from(&mut a, 2); // the tracker, served once
        push(&mut a, [CbEvent::Unsubscribed(peer(2))]);
        a.advertise(token(9));
        push(&mut a, [CbEvent::Advertising(true)]);
        told(&mut a, 2);
        let new_tag = &token(9)[..TAG_LEN];
        for _ in 0..3 {
            push(&mut a, [CbEvent::Subscribed(peer(2))]);
            push(&mut a, [CbEvent::Unsubscribed(peer(2))]);
            let notes = told(&mut a, 2);
            assert!(is_withheld(&notes), "{notes:?}");
            for n in &notes {
                assert!(
                    !n.windows(TAG_LEN).any(|w| w == new_tag),
                    "the new tag leaked: {n:?}"
                );
                assert!(!n.windows(TAG_LEN).any(|w| w == &TOKEN[..TAG_LEN]));
            }
        }
        // a newcomer is served the token
        link_from(&mut a, 4);
        assert_eq!(told(&mut a, 4), vec![token_note(token(9))]);
    }

    /// The central side: a peripheral that withholds its token still
    /// gives a link, up at once, with the tag its advert showed when it
    /// was scanned (never anything from the link).
    #[test]
    fn a_withheld_token_brings_the_link_up_with_the_scanned_tag() {
        let mut a = running();
        push(
            &mut a,
            [
                CbEvent::Discovered(peer(1), tag(1)),
                CbEvent::Ready(peer(1)),
            ],
        );
        let ev = push(&mut a, [CbEvent::Notified(peer(1), vec![WITHHELD_MARK])]);
        let [RadioEvent::Up {
            link,
            advert: Advert::Withheld(t),
        }] = ev.as_slice()
        else {
            panic!("{ev:?}");
        };
        assert_eq!(*t, tag(1));
        let ev = push(&mut a, [CbEvent::Notified(peer(1), b"hello".to_vec())]);
        assert_eq!(ev, vec![RadioEvent::Bytes(*link, b"hello".to_vec())]);
        // closed on purpose, it is left alone until the tag changes
        a.disconnect(*link);
        push(&mut a, [CbEvent::Discovered(peer(1), tag(1))]);
        assert!(a.stack().connections.is_empty());
        push(&mut a, [CbEvent::Discovered(peer(1), tag(2))]);
        assert!(a.stack().connections.contains(&peer(1)));
        // an advert that showed no tag comes up withheld with none
        let mut b = running();
        push(
            &mut b,
            [CbEvent::Discovered(peer(1), None), CbEvent::Ready(peer(1))],
        );
        let ev = push(&mut b, [CbEvent::Notified(peer(1), vec![WITHHELD_MARK])]);
        assert!(matches!(
            ev.as_slice(),
            [RadioEvent::Up {
                advert: Advert::Withheld(None),
                ..
            }]
        ));
    }

    /// I-new-1: the central side doesn't ask a refusing peripheral again
    /// every few seconds: it waits for a new tag, or ten minutes.
    #[test]
    fn a_refusal_is_not_retried_until_the_tag_changes_or_ten_minutes_pass() {
        let mut a = running();
        push(
            &mut a,
            [
                CbEvent::Discovered(peer(1), tag(1)),
                CbEvent::Ready(peer(1)),
            ],
        );
        a.stack_mut().now += CONNECT_MS;
        a.poll();
        assert!(a.stack().connections.is_empty(), "given up");
        let connects = |a: &Radio_| a.stack().log.iter().filter(|l| *l == "connect 1").count();
        for wait in [RETRY_MS, 60_000, REFUSED_MS - CONNECT_MS - 1] {
            a.stack_mut().now = CONNECT_MS + wait;
            push(&mut a, [CbEvent::Discovered(peer(1), tag(1))]);
            assert_eq!(connects(&a), 1, "not again after {wait} ms");
        }
        a.stack_mut().now = CONNECT_MS + REFUSED_MS;
        push(&mut a, [CbEvent::Discovered(peer(1), tag(1))]);
        assert_eq!(connects(&a), 2, "once more after ten minutes");
        // a new tag is asked at once
        let mut b = running();
        push(
            &mut b,
            [
                CbEvent::Discovered(peer(1), tag(1)),
                CbEvent::Ready(peer(1)),
            ],
        );
        b.stack_mut().now += CONNECT_MS;
        b.poll();
        push(&mut b, [CbEvent::Discovered(peer(1), tag(2))]);
        assert!(b.stack().connections.contains(&peer(1)));
    }

    /// N1: with the memory full of centrals from earlier tokens, a
    /// newcomer is not given the token rather than one of them forgotten.
    #[test]
    fn a_full_memory_refuses_newcomers_rather_than_forget_earlier_centrals() {
        let mut a = running();
        for n in 0..CENTRALS_CAP as u16 {
            push(&mut a, [CbEvent::Written(many(n), vec![1])]);
        }
        a.advertise(token(9));
        push(&mut a, [CbEvent::Advertising(true)]);
        push(&mut a, [CbEvent::Subscribed(peer(200))]);
        assert!(
            is_withheld(&told(&mut a, 200)),
            "the newcomer gets no token"
        );
        let ev = push(&mut a, [CbEvent::Subscribed(many(0))]);
        assert_eq!(ev.len(), 1, "a link");
        let notes: Vec<Vec<u8>> = std::mem::take(&mut a.stack_mut().notes)
            .into_iter()
            .map(|(c, n)| {
                assert_eq!(c, many(0));
                n
            })
            .collect();
        assert!(is_withheld(&notes), "the earlier one is still remembered");
    }

    /// N1: a flood under this token pushes out only this token's
    /// centrals, never one from earlier.
    #[test]
    fn a_full_memory_makes_room_from_this_tokens_centrals_first() {
        let mut a = running();
        link_from(&mut a, 2);
        push(&mut a, [CbEvent::Unsubscribed(peer(2))]);
        a.advertise(token(9));
        push(&mut a, [CbEvent::Advertising(true)]);
        for n in 0..CENTRALS_CAP as u16 + 50 {
            push(
                &mut a,
                [
                    CbEvent::Written(many(n), vec![1]),
                    CbEvent::Unsubscribed(many(n)),
                ],
            );
        }
        told(&mut a, 2);
        push(&mut a, [CbEvent::Subscribed(peer(2))]);
        assert!(
            is_withheld(&told(&mut a, 2)),
            "the held central from before"
        );
        // and a newcomer under this token is served
        link_from(&mut a, 77);
        assert_eq!(told(&mut a, 77), vec![token_note(token(9))]);
    }

    /// N1: losing permission doesn't forget centrals (their connections
    /// may outlive it); Bluetooth going off does.
    #[test]
    fn centrals_are_remembered_across_a_permission_change_but_not_bluetooth_off() {
        for (power, remembered) in [(Power::Unauthorized, true), (Power::Off, false)] {
            let mut a = running();
            link_from(&mut a, 2);
            push(&mut a, [CbEvent::Unsubscribed(peer(2))]);
            a.advertise(token(9));
            push(
                &mut a,
                [
                    CbEvent::State(Role::Peripheral, power),
                    CbEvent::State(Role::Peripheral, Power::On),
                    CbEvent::ServiceAdded(true),
                    CbEvent::Advertising(true),
                ],
            );
            told(&mut a, 2);
            push(&mut a, [CbEvent::Subscribed(peer(2))]);
            let given = told(&mut a, 2) == vec![token_note(token(9))];
            assert_eq!(given, !remembered, "{power:?}");
        }
    }

    #[test]
    fn the_largest_write_is_the_central_or_peripheral_one() {
        let mut a = running();
        let mine = link_to(&mut a, 1);
        let theirs = link_from(&mut a, 2);
        assert_eq!(a.max_write(mine), 182);
        assert_eq!(a.max_write(theirs), 150);
        assert_eq!(a.max_write(LinkId(999)), MIN_WRITE);
    }
}
