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
use std::collections::{BTreeMap, HashMap};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bombechat_core::{safety_number, PublicIdentity};
use bombechat_modem::Band;
use bombechat_node::{NodeEvent, MAX_BROADCAST, NOT_SENT, NOT_STORED};
use bombechat_store::{Channel, Contact, ContactId, StoredBroadcast, StoredMessage};
use serde::Serialize;
use serde_json::{json, Value};

/// The machine's plate: the sheet's number and the name it shows for
/// itself. People are known by their own names (the first run asks), and
/// a store from before that announces this instead.
pub const MACHINE_NAME: &str = "Machine No. 1";

/// A tick that keeps panicking is logged and reported at most this often.
const PANIC_REPORT_GAP: Duration = Duration::from_secs(5);

/// Longest name kept for a person, in characters (hellos cut at 64 bytes).
const MAX_NAME_CHARS: usize = 40;

/// The command names, exactly as `bridge.js` invokes them.
pub const NAMES: [&str; 13] = [
    "send",
    "broadcast",
    "pair",
    "confirm_pair",
    "set_setting",
    "settings",
    "setting_notes",
    "unlock",
    "contacts",
    "channels",
    "conversation",
    "broadcasts",
    "machine",
];

/// Each setting the settings page can change, with how many values it
/// has and the value it has until the person changes it
/// (`app/ui/settings.html`). `set_setting` accepts only these, and
/// `settings` reports each of them.
pub const SETTINGS: [(&str, usize, usize); 15] = [
    ("sound", 3, 2),
    ("volume", 5, 2),
    ("band", 2, 0),
    ("light", 3, 0),
    ("flash", 3, 1),
    ("machine", 2, 1),
    ("machvol", 5, 2),
    ("carry", 2, 1),
    ("hops", 7, 3),
    ("hold", 5, 2),
    ("quiet", 5, 2),
    ("repeats", 4, 2),
    ("lock", 2, 1),
    ("expire", 4, 0),
    ("inperson", 2, 1),
];

/// Settings whose feature this build does not have yet, with their name
/// on the settings page and what they wait for. `set_setting` refuses
/// them and `settings` reports their default: nothing pretends they work.
pub const COMING: [(&str, &str, &str); 5] = [
    ("carry", "Carry for others", "comes with relay"),
    ("hops", "Hop limit", "comes with relay"),
    ("hold", "Hold for", "comes with relay"),
    ("light", "Light switch", "comes with light"),
    ("flash", "Flash speed", "comes with light"),
];

/// New contacts need a key check: on this build every contact is added
/// by checking keys in person (there is no other way to add one), so the
/// setting is always On.
pub const KEY_CHECK_ALWAYS: &str = "always, on this build";

/// Lock with passphrase, off a Mac: Off keeps the store key in the macOS
/// Keychain, which other systems do not have, so it stays On.
pub const KEYCHAIN_ONLY_ON_MACOS: &str = "Keychain only on macOS";

/// Whether the Keychain is there to keep the store key when the lock is
/// off.
const HAS_KEYCHAIN: bool = cfg!(target_os = "macos");

/// A setting the person cannot change on this machine: what it says
/// instead of a working value, and the value it is held at, if it has
/// one (a setting that waits for a feature has none).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingNote {
    pub note: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<usize>,
}

/// Every setting the person cannot change on this machine, by id.
fn held_settings() -> BTreeMap<&'static str, SettingNote> {
    let mut held: BTreeMap<&'static str, SettingNote> = COMING
        .iter()
        .map(|&(id, _, note)| (id, SettingNote { note, value: None }))
        .collect();
    held.insert(
        "inperson",
        SettingNote {
            note: KEY_CHECK_ALWAYS,
            value: Some(1),
        },
    );
    if !HAS_KEYCHAIN {
        held.insert(
            "lock",
            SettingNote {
                note: KEYCHAIN_ONLY_ON_MACOS,
                value: Some(1),
            },
        );
    }
    held
}

/// Why setting `id` cannot be changed on this machine, as a sentence to
/// show, if it cannot.
fn refusal(id: &str) -> Option<String> {
    if let Some((_, name, note)) = COMING.iter().find(|(c, _, _)| *c == id) {
        return Some(format!("{name} {note}"));
    }
    match id {
        "inperson" => Some("On this build every new contact needs a key check".to_string()),
        "lock" if !HAS_KEYCHAIN => {
            Some("The Keychain is only on macOS, so the lock stays on".to_string())
        }
        _ => None,
    }
}

/// Whether `value_index` is one of the values setting `id` has.
pub fn is_setting(id: &str, value_index: usize) -> bool {
    SETTINGS
        .iter()
        .any(|(name, count, _)| *name == id && value_index < *count)
}

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
/// machine: given the passphrase and the person's name (trimmed; empty if
/// none was given, and only used when the store is created), open or
/// create the store.
pub type Unlocking = Box<dyn FnOnce(&str, &str) -> Result<Opened, String> + Send>;

/// The node as the commands see it. The app's is [`crate::Device`], a
/// [`bombechat_node::Node`] over the sound card and the store in the app
/// data folder; tests use a fake.
pub trait Machine: Send {
    fn send(&mut self, to: &[ContactId], text: &str, channel: Channel) -> [u8; 16];
    fn broadcast(&mut self, text: &str) -> [u8; 16];
    /// Play this machine's pairing hello, or say why not (for example,
    /// the Sound switch is at Listen).
    fn start_pairing(&mut self, my_name: &str) -> Result<(), String>;
    fn confirm_pairing(&mut self, peer: &PublicIdentity, name: &str) -> ContactId;
    fn contacts(&self) -> Vec<Contact>;
    /// The stored messages with one contact, oldest first.
    fn conversation(&self, contact: ContactId) -> Vec<StoredMessage>;
    /// The broadcasts kept, sent and heard, oldest first.
    fn broadcasts(&self) -> Vec<StoredBroadcast>;
    /// The person's name, as their pairing hellos carry it, once unlocked.
    fn my_name(&self) -> Option<String>;
    /// This machine's public identity, once unlocked.
    fn me(&self) -> Option<PublicIdentity>;
    /// How many sealed messages it holds for other people.
    fn held_for_others(&self) -> usize;
    /// Record and apply a setting the page changed, or refuse it with a
    /// sentence to show (for example, the lock cannot come off while the
    /// store is locked). `id` and `value_index` have been checked against
    /// [`SETTINGS`], and are never one the machine cannot change
    /// ([`setting_notes`]).
    fn set_setting(&mut self, id: &str, value_index: usize) -> Result<(), String>;
    /// The settings as last kept, by id, whatever the lock. May hold ids
    /// or values [`SETTINGS`] does not know; [`settings`] drops those.
    fn settings(&self) -> HashMap<String, usize>;
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
    /// The band sound is on, if there is a sound channel: not always the
    /// Tone band setting (a sound card too slow for near silent stays
    /// audible).
    fn band(&self) -> Option<Band> {
        None
    }
    fn tick(&mut self, now_ms: u64) -> Vec<NodeEvent>;
}

/// The machine, shared by the commands and the event pump, with the last
/// state each channel reported. Each command holds the machine only for
/// its own call; key derivation holds it not at all.
pub struct Shared {
    machine: Mutex<Box<dyn Machine>>,
    channels: Mutex<Vec<(Channel, String)>>,
    /// When a panicking tick was last logged and reported.
    panic_reported: Mutex<Option<Instant>>,
}

impl Shared {
    pub fn new(machine: Box<dyn Machine>) -> Shared {
        Shared {
            machine: Mutex::new(machine),
            channels: Mutex::new(Vec::new()),
            panic_reported: Mutex::new(None),
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

/// A channel's last reported state, its measured rate in payload bytes a
/// second if it has one, and for sound the band it is on ("near silent"
/// or "audible").
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChannelView {
    pub channel: &'static str,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub band: Option<&'static str>,
}

fn band_name(b: Band) -> &'static str {
    match b {
        Band::NearSilent => "near silent",
        Band::Audible => "audible",
    }
}

/// This machine as the page sees it: its plate name, its public identity
/// (sign key then dh key, as hex), the person's own name, how many
/// messages it holds for others, the build, and the longest broadcast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MachineView {
    pub name: &'static str,
    pub key: String,
    pub you: String,
    pub held: usize,
    pub version: &'static str,
    /// The longest broadcast, in UTF-8 bytes of text.
    pub broadcast_max: usize,
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

/// `broadcast { text }`: the broadcast's id, as hex. Refused with a
/// sentence to show when there is nothing to say, or more than one
/// envelope holds ([`MAX_BROADCAST`] bytes, which `machine` reports).
/// The Sound switch's refusal comes as for `send`: the channel state,
/// then the outbox's `failed`.
pub fn broadcast(s: &Shared, text: String) -> Result<String, String> {
    if text.trim().is_empty() {
        return Err("Write the notice first".to_string());
    }
    if text.len() > MAX_BROADCAST {
        return Err(format!(
            "Too long to broadcast: {} bytes, at most {MAX_BROADCAST}",
            text.len()
        ));
    }
    sent(s.machine().broadcast(&text))
}

/// `pair`: announce this machine for pairing, under the person's name.
/// Refused with the reason when the Sound switch does not allow sending.
pub fn pair(s: &Shared) -> Result<(), String> {
    let mut machine = s.machine();
    if machine.lock() != Lock::Unlocked {
        return Err("locked".to_string());
    }
    let name = machine
        .my_name()
        .unwrap_or_else(|| MACHINE_NAME.to_string());
    machine.start_pairing(&name)
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

/// `set_setting { id, valueIndex }`. Refused, with a sentence to show,
/// for a setting this machine cannot change ([`setting_notes`]) and when
/// the machine refuses it.
pub fn set_setting(s: &Shared, id: String, value_index: usize) -> Result<(), String> {
    match SETTINGS.iter().find(|(name, _, _)| *name == id) {
        Some((_, count, _)) if value_index < *count => match refusal(&id) {
            Some(why) => Err(why),
            None => s.machine().set_setting(&id, value_index),
        },
        Some(_) => Err(format!("no value {value_index} for {id}")),
        None => Err(format!("no such setting: {id}")),
    }
}

/// `setting_notes`: the settings this machine cannot change, by id, each
/// with what the settings page says in its place ("comes with relay",
/// "always, on this build") and the value it is held at, if any.
pub fn setting_notes(_s: &Shared) -> BTreeMap<&'static str, SettingNote> {
    held_settings()
}

/// `settings`: every setting's value, by id, as the machine keeps it, or
/// its default if it has none (or none it knows). The page renders from
/// this and never sends a setting back unless the person changed it.
/// Settings are not secret, so this answers locked or not.
///
/// A setting the machine cannot change reports the value it is held at,
/// or its default if it waits for a feature, whatever an older build
/// kept for it.
pub fn settings(s: &Shared) -> BTreeMap<&'static str, usize> {
    let kept = s.machine().settings();
    let held = held_settings();
    SETTINGS
        .iter()
        .map(|&(id, count, default)| {
            let value = match held.get(id) {
                Some(h) => h.value,
                None => kept.get(id).copied().filter(|v| *v < count),
            };
            (id, value.unwrap_or(default))
        })
        .collect()
}

/// `unlock { passphrase, name? }`. The key is derived (Argon2id, most of a
/// second) with the machine released, so the pump keeps ticking. `name`
/// is the person's own name, needed the first time (when the store is
/// created) and ignored after; it is trimmed and cut to 40 characters.
pub fn unlock(s: &Shared, passphrase: String, name: Option<String>) -> Result<(), String> {
    let name: String = name
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(MAX_NAME_CHARS)
        .collect();
    let work = s.machine().begin_unlock()?;
    let opened = work(&passphrase, &name);
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

/// `broadcasts`: the public broadcasts kept, this machine's own and the
/// ones it heard, oldest first, each shaped as the `broadcast` event
/// (with `outgoing`, and `state` for how far one of its own got).
pub fn broadcasts(s: &Shared) -> Result<Vec<Value>, String> {
    let machine = s.machine();
    unlocked(machine.as_ref())?;
    Ok(machine.broadcasts().iter().map(broadcast_json).collect())
}

fn broadcast_json(b: &StoredBroadcast) -> Value {
    json!({ "id": b.id.to_vec(), "outgoing": b.outgoing, "signer": { "sign": b.signer.to_vec() },
            "text": b.text, "verified": b.verified, "hops": b.hops, "at_min": b.at_min,
            "state": b.delivery.as_str() })
}

/// `machine`: this machine's name, public identity and what it holds.
pub fn machine(s: &Shared) -> Result<MachineView, String> {
    let machine = s.machine();
    unlocked(machine.as_ref())?;
    let me = machine.me().ok_or("locked")?;
    Ok(MachineView {
        name: MACHINE_NAME,
        key: hex(&me.sign) + &hex(&me.dh),
        you: machine
            .my_name()
            .unwrap_or_else(|| MACHINE_NAME.to_string()),
        held: machine.held_for_others(),
        version: env!("CARGO_PKG_VERSION"),
        broadcast_max: MAX_BROADCAST,
    })
}

/// `channels`: the last state each channel reported, so a page that
/// opens (or reloads) after the event knows it: "unavailable: ...",
/// "receiving" mid-message, and so on, with each channel's measured rate
/// where it has one. A channel with a rate that has reported nothing yet
/// is listening.
pub fn channels(s: &Shared) -> Vec<ChannelView> {
    let rates = s.machine().rates();
    let band = s.machine().band().map(band_name);
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
            band: band.filter(|_| c == Channel::Sound),
        })
        .collect()
}

/// One tick of the machine, for the pump. Records each channel's state
/// for [`channels`]. A tick that panics is logged and reported as the
/// sound channel failing, at most once in [`PANIC_REPORT_GAP`] so a tick
/// that panics every time does not flood the log or the page; the next
/// tick carries on.
pub fn tick(s: &Shared, now_ms: u64) -> Vec<NodeEvent> {
    let events = match catch_unwind(AssertUnwindSafe(|| s.machine().tick(now_ms))) {
        Ok(events) => events,
        Err(panic) => {
            let mut last = s.panic_reported.lock().unwrap_or_else(|p| p.into_inner());
            if last.is_some_and(|at| at.elapsed() < PANIC_REPORT_GAP) {
                return Vec::new();
            }
            *last = Some(Instant::now());
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
            "heard": m.heard, "state": m.delivery.as_str() })
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
            id,
            signer,
            text,
            verified,
            hops,
            at_min,
        } => (
            "broadcast",
            json!({ "id": id.to_vec(), "outgoing": false, "signer": identity(signer), "text": text,
                    "verified": verified, "hops": hops, "at_min": at_min }),
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
