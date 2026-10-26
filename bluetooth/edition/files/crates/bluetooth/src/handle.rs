//! The app's hold on the transport once the node owns it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bombechat_core::PublicIdentity;
use bombechat_session::SessionKeys;

use crate::radio::{LinkId, Mode};

/// At most this many Bluetooth events wait for the app.
pub const EVENTS_CAP: usize = 64;

/// What the app can know about who is near.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Peers {
    /// Contacts with a confirmed session now.
    pub contacts: Vec<PublicIdentity>,
    /// Links seen in the last two minutes that carry no confirmed contact
    /// session. Only links this device opened to an advert are counted, so
    /// a device that also opened a link back is counted once.
    pub strangers: Vec<LinkId>,
}

impl Peers {
    /// How many strangers are in range.
    pub fn strangers_in_range(&self) -> usize {
        self.strangers.len()
    }
}

/// Bluetooth news for the app, beyond what the node's transport events
/// carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BleEvent {
    /// In Open mode, a stranger's request to connect arrived on `link`:
    /// `from` is proved by the handshake; `hello` is the envelope the
    /// requester sent, checked to decode and fit. Nothing was sent back but
    /// handshake, acknowledgement and keepalive bytes, which carry nothing
    /// of ours. The request is forgotten after three minutes unless
    /// accepted ([`BleHandle::accept_request`]).
    Request {
        link: LinkId,
        from: PublicIdentity,
        hello: Vec<u8>,
    },
    /// The request sent on `link` was received.
    RequestSent { link: LinkId },
    /// The request for `link` could not be sent: the link is gone, the
    /// mode is not Open, Bluetooth was turned off, or it went unanswered.
    RequestFailed { link: LinkId },
}

#[derive(Default)]
pub(crate) struct Control {
    pub keys: Option<Option<SessionKeys>>,
    pub contacts: Option<Vec<PublicIdentity>>,
    pub mode: Option<Option<Mode>>,
    pub requests: Vec<(LinkId, Vec<u8>)>,
    /// Requests received, by link: accepted (true) or declined (false).
    pub answers: Vec<(LinkId, bool)>,
    pub events: VecDeque<BleEvent>,
    pub peers: Peers,
}

impl Control {
    pub fn notify(&mut self, event: BleEvent) {
        if self.events.len() >= EVENTS_CAP {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
}

/// The app's hold on a [`crate::BleTransport`] once the node owns it.
/// Changes take effect at the transport's next poll.
#[derive(Clone)]
pub struct BleHandle(pub(crate) Arc<Mutex<Control>>);

impl BleHandle {
    /// This device's session keys, or `None` while locked. Without keys
    /// the radio stays off.
    pub fn set_identity(&self, keys: Option<SessionKeys>) {
        self.0.lock().unwrap().keys = Some(keys);
    }

    /// The people a contact session may be with.
    pub fn set_contacts(&self, contacts: Vec<PublicIdentity>) {
        self.0.lock().unwrap().contacts = Some(contacts);
    }

    /// The Bluetooth switch: `None` is Off, the radio stopped.
    pub fn set_mode(&self, mode: Option<Mode>) {
        self.0.lock().unwrap().mode = Some(mode);
    }

    /// Send a request to connect over `link` (one of
    /// [`Peers::strangers`]), carrying `hello`, an envelope. Only in Open
    /// mode, and only from an explicit press.
    pub fn request(&self, link: LinkId, hello: Vec<u8>) {
        self.0.lock().unwrap().requests.push((link, hello));
    }

    /// Keep the request received on `link` past its three minutes: the
    /// person accepted it.
    pub fn accept_request(&self, link: LinkId) {
        self.0.lock().unwrap().answers.push((link, true));
    }

    /// Forget the request received on `link` now.
    pub fn decline_request(&self, link: LinkId) {
        self.0.lock().unwrap().answers.push((link, false));
    }

    /// Who was near at the last poll.
    pub fn peers(&self) -> Peers {
        self.0.lock().unwrap().peers.clone()
    }

    /// Bluetooth news since the last call.
    pub fn take_events(&self) -> Vec<BleEvent> {
        self.0.lock().unwrap().events.drain(..).collect()
    }
}
