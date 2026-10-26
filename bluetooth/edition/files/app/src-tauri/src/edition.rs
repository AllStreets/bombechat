//! What the standard build fits to the machine beyond sound and light:
//! Bluetooth. This file is laid over the root's `app/src-tauri/src/edition.rs`
//! when `bluetooth/` is synced; every other file of the app is shared.
//!
//! The Bluetooth switch is a setting, `bluetooth` (0 Off, 1 Contacts,
//! 2 Open), Off until the person turns it. Its channel says what the
//! switch has it doing, in the machine's words: [`BLUETOOTH_OFF`],
//! [`CONTACTS_ONLY`] or [`OPEN_TO_REQUESTS`]; "receiving" while a message
//! arrives; "unavailable: " and a sentence the person can act on while
//! the radio can't work, until it works again; and at Contacts or Open
//! before the store is opened, [`WAITING_FOR_PASSPHRASE`]: the radio has
//! no keys yet, so it is idle.
//!
//! Requests to connect make a second way to add a contact, so New
//! contacts need a key check is the person's choice here, On until they
//! change it.

use bombechat_bluetooth::{BleHandle, BleTransport, BluetoothSwitch, CoreBluetoothRadio};
use bombechat_node::{NodeEvent, Transport};
use bombechat_store::Channel;

/// The Bluetooth switch: 3 positions, Off until the person turns it.
pub const SETTINGS: &[(&str, usize, usize)] = &[("bluetooth", 3, 0)];

/// A contact can also be added by accepting a request to connect, so New
/// contacts need a key check is a real choice.
pub const ONLY_IN_PERSON: bool = false;

/// The Bluetooth channel at Off: the radio idle.
pub const BLUETOOTH_OFF: &str = "off";
/// At Contacts: sessions with contacts only.
pub const CONTACTS_ONLY: &str = "contacts only";
/// At Open: contacts, and requests to connect both ways.
pub const OPEN_TO_REQUESTS: &str = "open to requests";

/// At Contacts or Open before the store is opened: the radio waits for
/// the keys it opens with.
pub const WAITING_FOR_PASSPHRASE: &str = "waiting for the passphrase";

/// The unavailable states, as the node words them.
const UNAVAILABLE: &str = "unavailable: ";

/// What the switch has the channel doing.
fn word(switch: BluetoothSwitch) -> &'static str {
    match switch {
        BluetoothSwitch::Off => BLUETOOTH_OFF,
        BluetoothSwitch::Contacts => CONTACTS_ONLY,
        BluetoothSwitch::Open => OPEN_TO_REQUESTS,
    }
}

/// The parts this build fits: the Bluetooth transport's handle, and the
/// switch as last set.
pub struct Fittings {
    bluetooth: Option<BleHandle>,
    switch: BluetoothSwitch,
    /// Why the radio can't work, as last reported, while it can't.
    unavailable: Option<String>,
    /// The store is not open yet: the radio has no keys, so it is idle.
    locked: bool,
    /// The switch was turned (or set at start) since the last tick.
    turned: bool,
}

impl Fittings {
    /// No Bluetooth: for a machine over the carriers it is given.
    pub fn none() -> Fittings {
        Fittings {
            bluetooth: None,
            switch: BluetoothSwitch::Off,
            unavailable: None,
            locked: true,
            turned: false,
        }
    }

    /// Bluetooth through `handle`, the hold on a
    /// [`BleTransport`] that is among the node's carriers.
    pub fn bluetooth(handle: BleHandle) -> Fittings {
        Fittings {
            bluetooth: Some(handle),
            turned: true,
            ..Fittings::none()
        }
    }

    /// Bluetooth on this computer: a [`BleTransport`] over the
    /// CoreBluetooth radio on macOS (elsewhere a radio that says Bluetooth
    /// isn't supported there yet). Nothing is opened here: the radio's
    /// managers are made, and macOS asks for permission, only when the
    /// switch is first turned on.
    pub fn hardware() -> (Fittings, Vec<Box<dyn Transport>>, Vec<NodeEvent>) {
        // contact handshakes read the system clock each time one is built:
        // the pump's clock stops while the Mac sleeps
        let ble = BleTransport::new(CoreBluetoothRadio::new());
        let fittings = Fittings::bluetooth(ble.handle());
        (fittings, vec![Box::new(ble)], Vec::new())
    }

    /// The store was opened: the radio starts as the switch has it, and
    /// the channel says so at the next tick.
    pub fn unlocked(&mut self) {
        self.locked = false;
        self.turned = true;
    }

    /// The Bluetooth switch was set: the radio follows at the transport's
    /// next poll, and the channel says so at the next tick.
    pub fn apply(&mut self, id: &str, value_index: usize) {
        if id != "bluetooth" {
            return;
        }
        let Some(switch) = BluetoothSwitch::from_index(value_index) else {
            return;
        };
        if let Some(h) = &self.bluetooth {
            h.set_switch(switch);
        }
        if switch == BluetoothSwitch::Off {
            // stopped: a radio turned on again says afresh what is wrong
            self.unavailable = None;
        }
        self.switch = switch;
        self.turned = true;
    }

    /// The Bluetooth channel's state as the switch says it; while the
    /// store is locked, that the radio waits for it; or why the radio
    /// can't work while it can't.
    fn state(&self) -> String {
        if self.switch == BluetoothSwitch::Off {
            return BLUETOOTH_OFF.to_string();
        }
        if self.locked {
            return WAITING_FOR_PASSPHRASE.to_string();
        }
        match &self.unavailable {
            Some(why) => why.clone(),
            None => word(self.switch).to_string(),
        }
    }

    /// The Bluetooth channel's state before it has reported anything.
    pub fn states(&self) -> Vec<(Channel, String)> {
        match self.bluetooth {
            Some(_) => vec![(Channel::Bluetooth, self.state())],
            None => Vec::new(),
        }
    }

    /// Put the node's Bluetooth states in the switch's words: at Off
    /// everything but a refusal is [`BLUETOOTH_OFF`]; otherwise the
    /// node's "listening" (which follows the radio working again) is what
    /// the switch has it doing. A switch turned since the last tick is
    /// said first.
    pub fn tick(&mut self, events: &mut Vec<NodeEvent>) {
        if self.bluetooth.is_none() {
            return;
        }
        for e in events.iter_mut() {
            let NodeEvent::ChannelState {
                channel: Channel::Bluetooth,
                state,
            } = e
            else {
                continue;
            };
            let off = self.switch == BluetoothSwitch::Off;
            if state.starts_with(UNAVAILABLE) {
                if off {
                    *state = BLUETOOTH_OFF.to_string();
                } else {
                    self.unavailable = Some(state.clone());
                }
            } else if state == "listening" || (off && state == "receiving") {
                self.unavailable = None;
                *state = self.state();
            }
        }
        if std::mem::take(&mut self.turned) {
            events.insert(
                0,
                NodeEvent::ChannelState {
                    channel: Channel::Bluetooth,
                    state: self.state(),
                },
            );
        }
    }
}
