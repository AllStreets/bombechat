//! The app's hold on the transport once the node owns it, and the
//! Bluetooth switch.

use std::sync::{Arc, Mutex};

use bombechat_core::PublicIdentity;
use bombechat_session::SessionKeys;

use crate::radio::{LinkId, Mode};

/// The Bluetooth switch on the front of the machine. Off by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BluetoothSwitch {
    /// The radio idle: no advert, no scan, no connection.
    #[default]
    Off,
    /// Sessions with contacts only. Requests to connect are neither sent
    /// nor shown ([`Mode::Contacts`]).
    Contacts,
    /// Contacts, and requests to connect both ways ([`Mode::Open`]).
    Open,
}

impl BluetoothSwitch {
    /// The switch by its position, as the settings page numbers it: 0 Off,
    /// 1 Contacts, 2 Open.
    pub fn from_index(i: usize) -> Option<BluetoothSwitch> {
        match i {
            0 => Some(BluetoothSwitch::Off),
            1 => Some(BluetoothSwitch::Contacts),
            2 => Some(BluetoothSwitch::Open),
            _ => None,
        }
    }

    /// The transport mode it stands for: `None` is Off, the radio stopped.
    pub fn mode(self) -> Option<Mode> {
        match self {
            BluetoothSwitch::Off => None,
            BluetoothSwitch::Contacts => Some(Mode::Contacts),
            BluetoothSwitch::Open => Some(Mode::Open),
        }
    }
}

/// What the app can know about who is near.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Peers {
    /// Contacts with a confirmed session now.
    pub contacts: Vec<PublicIdentity>,
    /// Strangers a request can go to now: live links this device opened
    /// to an advert, carrying no confirmed contact session, in Open mode.
    /// Only links this device opened are counted, so a device that also
    /// opened a link back is listed once.
    pub strangers: Vec<LinkId>,
    /// Strangers seen in the last [`crate::STRANGER_MS`] (fifteen
    /// minutes), by the adverts this device connected to. An upper bound:
    /// a stranger who rotated its token in that time counts twice, as
    /// nothing links its two tokens.
    pub strangers_seen: usize,
}

impl Peers {
    /// How many strangers were seen nearby lately.
    pub fn strangers_in_range(&self) -> usize {
        self.strangers_seen
    }
}

#[derive(Default)]
pub(crate) struct Control {
    pub keys: Option<Option<SessionKeys>>,
    pub contacts: Option<Vec<PublicIdentity>>,
    pub mode: Option<Option<Mode>>,
    /// Requests to send: (link, hello).
    pub requests: Vec<(LinkId, Vec<u8>)>,
    /// Requests received, by link: accepted with our hello, or declined.
    pub answers: Vec<(LinkId, Option<Vec<u8>>)>,
    pub peers: Peers,
}

/// The app's hold on a [`crate::BleTransport`] once the node owns it.
/// Changes take effect at the transport's next poll. The keys and the
/// contacts come from the node, through [`bombechat_node::Transport`]'s
/// setters; requests to connect go through the node too
/// ([`bombechat_node::Node::request`] and the rest).
#[derive(Clone)]
pub struct BleHandle(pub(crate) Arc<Mutex<Control>>);

impl BleHandle {
    /// The transport mode: `None` is Off, the radio stopped.
    pub fn set_mode(&self, mode: Option<Mode>) {
        self.0.lock().unwrap().mode = Some(mode);
    }

    /// The Bluetooth switch.
    pub fn set_switch(&self, switch: BluetoothSwitch) {
        self.set_mode(switch.mode());
    }

    /// Who was near at the last poll.
    pub fn peers(&self) -> Peers {
        self.0.lock().unwrap().peers.clone()
    }
}
