//! What the page can ask of the machine, and what the machine tells the
//! page, as `app/ui/bridge.js` names them.
//!
//! Each command here is a plain function over [`Shared`], so it can be
//! tested with a fake [`Machine`]; `lib.rs` wraps each in a
//! `#[tauri::command]` of the same name. Ids leave as lowercase hex;
//! event payloads carry bytes as arrays of numbers, the way
//! `bridge.js`'s `normalise` expects them.
//!
//! Nothing secret crosses here: the page sees message text, contact names
//! and public keys, never a private or session key.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, MutexGuard};

use bombechat_core::{safety_number, PublicIdentity};
use bombechat_node::{NodeEvent, NOT_SENT, NOT_STORED};
use bombechat_store::{Channel, Contact, ContactId, StoredMessage};
use serde::Serialize;
use serde_json::{json, Value};

/// The name this machine announces when pairing and gives its store.
pub const MACHINE_NAME: &str = "Machine No. 1";

/// The command names, exactly as `bridge.js` invokes them.
pub const NAMES: [&str; 10] = [
    "send",
    "broadcast",
    "pair",
    "confirm_pair",
    "set_setting",
    "unlock",
    "contacts",
    "channels",
    "conversation",
    "machine",
];

/// Each setting the settings page can change, with how many values it
/// has (`app/ui/settings.html`). `set_setting` accepts only these.
pub const SETTINGS: [(&str, usize); 15] = [
    ("sound", 3),
    ("volume", 5),
    ("band", 2),
    ("light", 3),
    ("flash", 3),
    ("machine", 2),
    ("machvol", 5),
    ("carry", 2),
    ("hops", 7),
    ("hold", 5),
    ("quiet", 5),
    ("repeats", 4),
    ("lock", 2),
    ("expire", 4),
    ("inperson", 2),
];

/// Whether the store is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lock {
    Unlocked,
    /// A store exists; its passphrase has not been given yet.
    Locked,
    /// First run: the first passphrase given creates the store.
    NoStore,
}

/// An opened store, passed from [`Unlocking`] back to
/// [`Machine::finish_unlock`]. Opaque here, so a fake can pass anything.
pub type Opened = Box<dyn Any + Send>;

/// The slow part of an unlock (key derivation), run without holding the
/// machine: given the passphrase, open or create the store.
pub type Unlocking = Box<dyn FnOnce(&str) -> Result<Opened, String> + Send>;

/// The node as the commands see it. The app's is [`crate::Device`], a
/// [`bombechat_node::Node`] over the sound card and the store in the app
/// data folder; tests use a fake.
pub trait Machine: Send {
    fn send(&mut self, to: &[ContactId], text: &str, channel: Channel) -> [u8; 16];
    fn broadcast(&mut self, text: &str) -> [u8; 16];
    fn start_pairing(&mut self, my_name: &str);
    fn confirm_pairing(&mut self, peer: &PublicIdentity, name: &str) -> ContactId;
    fn contacts(&self) -> Vec<Contact>;
    /// The stored messages with one contact, oldest first.
    fn conversation(&self, contact: ContactId) -> Vec<StoredMessage>;
    /// This machine's public identity, once unlocked.
    fn me(&self) -> Option<PublicIdentity>;
    /// How many sealed messages it holds for other people.
    fn held_for_others(&self) -> usize;
    /// Record a setting the page changed. `id` and `value_index` have been
    /// checked against [`SETTINGS`].
    fn set_setting(&mut self, id: &str, value_index: usize);
    fn lock(&self) -> Lock;
    /// Begin an unlock: refuse if already unlocked or unlocking, otherwise
    /// mark it unlocking and hand back the work to do off the lock.
    fn begin_unlock(&mut self) -> Result<Unlocking, String>;
    /// End an unlock begun with [`Machine::begin_unlock`], whatever came
    /// of it: install the store, or pass the error on. The error is shown
    /// to the person as it stands.
    fn finish_unlock(&mut self, opened: Result<Opened, String>) -> Result<(), String>;
    /// Each channel's measured rate, in payload bytes a second, for the
    /// channels that have one: what the page's bytes-a-second readout shows.
    fn rates(&self) -> Vec<(Channel, f32)>;
    fn tick(&mut self, now_ms: u64) -> Vec<NodeEvent>;
}

/// The machine, shared by the commands and the event pump, with the last
/// state each channel reported. Each command holds the machine only for
/// its own call; key derivation holds it not at all.
pub struct Shared {
    machine: Mutex<Box<dyn Machine>>,
    channels: Mutex<Vec<(Channel, String)>>,
}

impl Shared {
    pub fn new(machine: Box<dyn Machine>) -> Shared {
        Shared {
            machine: Mutex::new(machine),
            channels: Mutex::new(Vec::new()),
        }
    }

    fn machine(&self) -> MutexGuard<'_, Box<dyn Machine>> {
        self.machine.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A contact as the page sees it. `safety` is the 60-digit safety number
/// shared with them (12 groups of five), computed from public keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContactView {
    pub id: i64,
    pub name: String,
    pub checked: bool,
    pub safety: String,
}

/// A channel's last reported state, and its measured rate in payload
/// bytes a second if it has one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChannelView {
    pub channel: &'static str,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f32>,
}

/// This machine as the page sees it: its name, its public identity (sign
/// key then dh key, as hex), how many messages it holds for others, and
/// the build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MachineView {
    pub name: &'static str,
    pub key: String,
    pub held: usize,
    pub version: &'static str,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn parse_channel(name: &str) -> Result<Channel, String> {
    match name.to_ascii_lowercase().as_str() {
        "sound" => Ok(Channel::Sound),
        "light" => Ok(Channel::Light),
        "bluetooth" => Ok(Channel::Bluetooth),
        _ => Err(format!("no such channel: {name}")),
    }
}

fn channel_name(c: Channel) -> &'static str {
    match c {
        Channel::Sound => "sound",
        Channel::Light => "light",
        Channel::Bluetooth => "bluetooth",
    }
}

fn sent(id: [u8; 16]) -> Result<String, String> {
    if id == NOT_SENT {
        Err("not sent".to_string())
    } else {
        Ok(hex(&id))
    }
}

fn unlocked(machine: &dyn Machine) -> Result<(), String> {
    match machine.lock() {
        Lock::Unlocked => Ok(()),
        Lock::Locked => Err("locked".to_string()),
        Lock::NoStore => Err("no store yet".to_string()),
    }
}

/// `send { contactIds, text, channel }`: the message id, as hex.
pub fn send(
    s: &Shared,
    contact_ids: Vec<i64>,
    text: String,
    channel: String,
) -> Result<String, String> {
    let channel = parse_channel(&channel)?;
    let to: Vec<ContactId> = contact_ids.into_iter().map(ContactId).collect();
    sent(s.machine().send(&to, &text, channel))
}

/// `broadcast { text }`: the broadcast's id, as hex.
pub fn broadcast(s: &Shared, text: String) -> Result<String, String> {
    sent(s.machine().broadcast(&text))
}

/// `pair`: announce this machine for pairing.
pub fn pair(s: &Shared) -> Result<(), String> {
    let mut machine = s.machine();
    if machine.lock() != Lock::Unlocked {
        return Err("locked".to_string());
    }
    machine.start_pairing(MACHINE_NAME);
    Ok(())
}

/// `confirm_pair { peerHex, name }`: store the peer (its sign key then its
/// dh key, 128 hex digits) as a checked contact; the new contact's id.
pub fn confirm_pair(s: &Shared, peer_hex: String, name: String) -> Result<i64, String> {
    let peer_hex = peer_hex.to_ascii_lowercase();
    let (Some(sign), Some(dh)) = (
        peer_hex.get(..64).and_then(unhex::<32>),
        peer_hex.get(64..).and_then(unhex::<32>),
    ) else {
        return Err("peer must be 128 hex digits".to_string());
    };
    match s
        .machine()
        .confirm_pairing(&PublicIdentity { sign, dh }, &name)
    {
        NOT_STORED => Err("not stored".to_string()),
        ContactId(id) => Ok(id),
    }
}

/// `set_setting { id, valueIndex }`.
pub fn set_setting(s: &Shared, id: String, value_index: usize) -> Result<(), String> {
    match SETTINGS.iter().find(|(name, _)| *name == id) {
        Some((_, count)) if value_index < *count => {
            s.machine().set_setting(&id, value_index);
            Ok(())
        }
        Some(_) => Err(format!("no value {value_index} for {id}")),
        None => Err(format!("no such setting: {id}")),
    }
}

/// `unlock { passphrase }`. The key is derived (Argon2id, most of a
/// second) with the machine released, so the pump keeps ticking.
pub fn unlock(s: &Shared, passphrase: String) -> Result<(), String> {
    let work = s.machine().begin_unlock()?;
    let opened = work(&passphrase);
    s.machine().finish_unlock(opened)
}

/// `contacts`: everyone this machine can send to. Fails with "locked" or
/// "no store yet" until the store is open, so the page knows to ask for
/// the passphrase (and, the first time, that it is choosing one).
pub fn contacts(s: &Shared) -> Result<Vec<ContactView>, String> {
    let machine = s.machine();
    unlocked(machine.as_ref())?;
    let me = machine.me().ok_or("locked")?;
    Ok(machine
        .contacts()
        .into_iter()
        .map(|c| ContactView {
            id: c.id.0,
            safety: safety_number(&me, &c.who),
            name: c.name,
            checked: c.checked,
        })
        .collect())
}

/// `conversation { contactId }`: the stored messages with one contact,
/// oldest first, each shaped as the `message` event.
pub fn conversation(s: &Shared, contact_id: i64) -> Result<Vec<Value>, String> {
    let machine = s.machine();
    unlocked(machine.as_ref())?;
    Ok(machine
        .conversation(ContactId(contact_id))
        .iter()
        .map(message_json)
        .collect())
}

/// `machine`: this machine's name, public identity and what it holds.
pub fn machine(s: &Shared) -> Result<MachineView, String> {
    let machine = s.machine();
    unlocked(machine.as_ref())?;
    let me = machine.me().ok_or("locked")?;
    Ok(MachineView {
        name: MACHINE_NAME,
        key: hex(&me.sign) + &hex(&me.dh),
        held: machine.held_for_others(),
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// `channels`: the last state each channel reported, so a page that
/// opens (or reloads) after the event knows it: "unavailable: ...",
/// "receiving" mid-message, and so on, with each channel's measured rate
/// where it has one. A channel with a rate that has reported nothing yet
/// is listening.
pub fn channels(s: &Shared) -> Vec<ChannelView> {
    let rates = s.machine().rates();
    let mut seen: Vec<(Channel, String)> =
        s.channels.lock().unwrap_or_else(|p| p.into_inner()).clone();
    for (c, _) in &rates {
        if !seen.iter().any(|(k, _)| k == c) {
            seen.push((*c, "listening".to_string()));
        }
    }
    seen.into_iter()
        .map(|(c, state)| ChannelView {
            channel: channel_name(c),
            state,
            rate: rates.iter().find(|(k, _)| *k == c).map(|(_, r)| *r),
        })
        .collect()
}

/// One tick of the machine, for the pump. Records each channel's state
/// for [`channels`]. A tick that panics is logged and reported as the
/// sound channel failing; the next tick carries on.
pub fn tick(s: &Shared, now_ms: u64) -> Vec<NodeEvent> {
    let events = match catch_unwind(AssertUnwindSafe(|| s.machine().tick(now_ms))) {
        Ok(events) => events,
        Err(panic) => {
            let why = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            eprintln!("bombechat: the node's tick panicked: {why}");
            vec![NodeEvent::ChannelState {
                channel: Channel::Sound,
                state: "failed".to_string(),
            }]
        }
    };
    let mut channels = s.channels.lock().unwrap_or_else(|p| p.into_inner());
    for e in &events {
        if let NodeEvent::ChannelState { channel, state } = e {
            match channels.iter_mut().find(|(c, _)| c == channel) {
                Some(slot) => slot.1 = state.clone(),
                None => channels.push((*channel, state.clone())),
            }
        }
    }
    events
}

fn identity(p: &PublicIdentity) -> Value {
    json!({ "sign": p.sign.to_vec(), "dh": p.dh.to_vec() })
}

fn message_json(m: &StoredMessage) -> Value {
    json!({ "id": m.id.to_vec(), "contact": m.contact.0, "outgoing": m.outgoing,
            "text": m.text, "channel": channel_name(m.channel), "at_min": m.at_min,
            "heard": m.heard })
}

/// A node event as the bridge event it becomes: its name and payload.
pub fn bridge_event(e: &NodeEvent) -> (&'static str, Value) {
    match e {
        NodeEvent::ChunkLanded {
            tag,
            index,
            of,
            view,
        } => (
            "chunk",
            json!({ "tag": tag, "index": index, "of": of,
                    "view": { "plain": view.plain, "heard": view.heard, "key": view.key } }),
        ),
        NodeEvent::Message(m) => ("message", message_json(m)),
        NodeEvent::Heard { id, by } => ("heard", json!({ "id": id.to_vec(), "by": by.0 })),
        NodeEvent::Broadcast {
            signer,
            text,
            verified,
        } => (
            "broadcast",
            json!({ "signer": identity(signer), "text": text, "verified": verified }),
        ),
        NodeEvent::PairOffer { peer, name, safety } => (
            "pairOffer",
            json!({ "peer": identity(peer), "name": name, "safety": safety }),
        ),
        NodeEvent::RoomDb(db) => ("roomDb", json!(db)),
        NodeEvent::ChannelState { channel, state } => (
            "channel",
            json!({ "channel": channel_name(*channel), "state": state }),
        ),
        NodeEvent::Outbox { id, state } => (
            "outbox",
            json!({ "id": id.to_vec(), "state": state.as_str() }),
        ),
    }
}
