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

use std::sync::{Mutex, MutexGuard};

use bombechat_core::PublicIdentity;
use bombechat_node::{NodeEvent, NOT_SENT, NOT_STORED};
use bombechat_store::{Channel, Contact, ContactId};
use serde::Serialize;
use serde_json::{json, Value};

/// The name this machine announces when pairing and gives its store.
pub const MACHINE_NAME: &str = "Machine No. 1";

/// The command names, exactly as `bridge.js` invokes them.
pub const NAMES: [&str; 7] = [
    "send",
    "broadcast",
    "pair",
    "confirm_pair",
    "set_setting",
    "unlock",
    "contacts",
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

/// The node as the commands see it. The app's is a [`bombechat_node::Node`]
/// over the sound card and the store in the app data folder; tests use a
/// fake.
pub trait Machine: Send {
    fn send(&mut self, to: &[ContactId], text: &str, channel: Channel) -> [u8; 16];
    fn broadcast(&mut self, text: &str) -> [u8; 16];
    fn start_pairing(&mut self, my_name: &str);
    fn confirm_pairing(&mut self, peer: &PublicIdentity, name: &str) -> ContactId;
    fn contacts(&self) -> Vec<Contact>;
    /// Record a setting the page changed. `id` and `value_index` have been
    /// checked against [`SETTINGS`].
    fn set_setting(&mut self, id: &str, value_index: usize);
    fn lock(&self) -> Lock;
    /// Open the store with `passphrase`, or create it on first run. The
    /// error is shown to the person as it stands.
    fn unlock(&mut self, passphrase: &str) -> Result<(), String>;
    fn tick(&mut self, now_ms: u64) -> Vec<NodeEvent>;
}

/// The machine, shared by the commands and the event pump. Each holds the
/// lock only for one call.
pub type Shared = Mutex<Box<dyn Machine>>;

/// A contact as the page sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContactView {
    pub id: i64,
    pub name: String,
    pub checked: bool,
}

fn machine(m: &Shared) -> MutexGuard<'_, Box<dyn Machine>> {
    m.lock().unwrap_or_else(|p| p.into_inner())
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

/// `send { contactIds, text, channel }`: the message id, as hex.
pub fn send(
    m: &Shared,
    contact_ids: Vec<i64>,
    text: String,
    channel: String,
) -> Result<String, String> {
    let channel = parse_channel(&channel)?;
    let to: Vec<ContactId> = contact_ids.into_iter().map(ContactId).collect();
    sent(machine(m).send(&to, &text, channel))
}

/// `broadcast { text }`: the broadcast's id, as hex.
pub fn broadcast(m: &Shared, text: String) -> Result<String, String> {
    sent(machine(m).broadcast(&text))
}

/// `pair`: announce this machine for pairing.
pub fn pair(m: &Shared) -> Result<(), String> {
    let mut machine = machine(m);
    if machine.lock() != Lock::Unlocked {
        return Err("locked".to_string());
    }
    machine.start_pairing(MACHINE_NAME);
    Ok(())
}

/// `confirm_pair { peerHex, name }`: store the peer (its sign key then its
/// dh key, 128 hex digits) as a checked contact; the new contact's id.
pub fn confirm_pair(m: &Shared, peer_hex: String, name: String) -> Result<i64, String> {
    let peer_hex = peer_hex.to_ascii_lowercase();
    let (Some(sign), Some(dh)) = (
        peer_hex.get(..64).and_then(unhex::<32>),
        peer_hex.get(64..).and_then(unhex::<32>),
    ) else {
        return Err("peer must be 128 hex digits".to_string());
    };
    match machine(m).confirm_pairing(&PublicIdentity { sign, dh }, &name) {
        NOT_STORED => Err("not stored".to_string()),
        ContactId(id) => Ok(id),
    }
}

/// `set_setting { id, valueIndex }`.
pub fn set_setting(m: &Shared, id: String, value_index: usize) -> Result<(), String> {
    match SETTINGS.iter().find(|(name, _)| *name == id) {
        Some((_, count)) if value_index < *count => {
            machine(m).set_setting(&id, value_index);
            Ok(())
        }
        Some(_) => Err(format!("no value {value_index} for {id}")),
        None => Err(format!("no such setting: {id}")),
    }
}

/// `unlock { passphrase }`.
pub fn unlock(m: &Shared, passphrase: String) -> Result<(), String> {
    machine(m).unlock(&passphrase)
}

/// `contacts`: everyone this machine can send to. Fails with "locked" or
/// "no store yet" until the store is open, so the page knows to ask for
/// the passphrase (and, the first time, that it is choosing one).
pub fn contacts(m: &Shared) -> Result<Vec<ContactView>, String> {
    let machine = machine(m);
    match machine.lock() {
        Lock::Unlocked => Ok(machine
            .contacts()
            .into_iter()
            .map(|c| ContactView {
                id: c.id.0,
                name: c.name,
                checked: c.checked,
            })
            .collect()),
        Lock::Locked => Err("locked".to_string()),
        Lock::NoStore => Err("no store yet".to_string()),
    }
}

fn identity(p: &PublicIdentity) -> Value {
    json!({ "sign": p.sign.to_vec(), "dh": p.dh.to_vec() })
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
        NodeEvent::Message(m) => (
            "message",
            json!({ "id": m.id.to_vec(), "contact": m.contact.0, "outgoing": m.outgoing,
                    "text": m.text, "channel": channel_name(m.channel), "at_min": m.at_min,
                    "heard": m.heard }),
        ),
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
