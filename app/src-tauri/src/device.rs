//! The real machine: a [`Node`] over the sound card, and the store in the
//! app data folder.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use bombechat_core::{Identity, PublicIdentity};
use bombechat_modem::{AudioIo, CpalAudio, ModemConfig, QuietGate};
use bombechat_node::{Node, NodeEvent, SoundTransport, Transport};
use bombechat_store::{Channel, Contact, ContactId, Store, StoreError, StoredMessage};

use crate::commands::{Lock, Machine, Opened, Unlocking, MACHINE_NAME};

/// The quiet gate's defaults, as the settings page shows them: send under
/// 40 dB, two repeats a minute and a half apart.
const SEND_UNDER_DB: f32 = 40.0;
const REPEATS: u8 = 2;
const REPEAT_GAP_S: u32 = 90;

/// `path` with `suffix` added to its file name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut p: OsString = path.as_os_str().to_owned();
    p.push(suffix);
    PathBuf::from(p)
}

/// The store's salt sidecar, as `bombechat-store` names it.
fn salt(path: &Path) -> PathBuf {
    with_suffix(path, ".salt")
}

/// Where a first-run store is built before it is moved into place.
fn staging(path: &Path) -> PathBuf {
    with_suffix(path, ".creating")
}

/// Clear what an unfinished first run left behind (the app quit or
/// crashed while the store was being made), so the next launch offers a
/// fresh start instead of asking for a passphrase to a store that never
/// finished. The database is moved into place last, so a salt with no
/// database beside it is always such a leftover.
pub fn tidy_unfinished(path: &Path) {
    let _ = fs::remove_file(staging(path));
    let _ = fs::remove_file(salt(&staging(path)));
    if !path.exists() {
        let _ = fs::remove_file(salt(path));
    }
}

/// Make a new store at `path` without ever leaving a half-made one there:
/// build it under a staging name, move the salt and then the database
/// into place, and open it again from there (so SQLite's own files follow
/// the real name).
pub fn create_store(path: &Path, passphrase: &str) -> Result<Store, StoreError> {
    tidy_unfinished(path);
    let staged = staging(path);
    let made = Store::create(&staged, passphrase, &Identity::generate(), MACHINE_NAME);
    if made.is_err() {
        tidy_unfinished(path);
    }
    drop(made?);
    let moved = fs::rename(salt(&staged), salt(path)).and_then(|()| fs::rename(&staged, path));
    if moved.is_err() {
        tidy_unfinished(path);
        return Err(StoreError::Io);
    }
    Store::open(path, passphrase)
}

fn store_error(e: StoreError) -> String {
    match e {
        StoreError::WrongPassphrase => "wrong passphrase".to_string(),
        other => other.to_string(),
    }
}

/// The real machine.
pub struct Device {
    node: Node,
    store_path: PathBuf,
    unlocked: bool,
    /// An unlock is deriving its key right now.
    unlocking: bool,
    /// Settings as the page last set them. Plan 1's node has nothing to
    /// retune yet (band, gate, volume), so they are only kept.
    settings: HashMap<String, usize>,
    /// Reported on the first tick, e.g. why there is no sound transport.
    pending: Vec<NodeEvent>,
}

impl Device {
    /// A machine listening through the default sound card, with its store
    /// at `store_path`. If the sound card cannot be opened, it runs with
    /// no transport and says why on its first tick.
    pub fn with_sound_card(store_path: PathBuf) -> Device {
        match CpalAudio::new() {
            Ok(audio) => {
                let cfg = ModemConfig {
                    sample_rate: audio.sample_rate(),
                    ..ModemConfig::default()
                };
                let gate = QuietGate::new(SEND_UNDER_DB, REPEATS, REPEAT_GAP_S);
                let sound = SoundTransport::new(Box::new(audio), cfg, gate);
                Device::new(store_path, vec![Box::new(sound)], Vec::new())
            }
            Err(e) => Device::new(
                store_path,
                Vec::new(),
                vec![NodeEvent::ChannelState {
                    channel: Channel::Sound,
                    state: format!("unavailable: {e}"),
                }],
            ),
        }
    }

    /// A machine over `transports`, reporting `pending` on its first tick.
    pub fn new(
        store_path: PathBuf,
        transports: Vec<Box<dyn Transport>>,
        pending: Vec<NodeEvent>,
    ) -> Device {
        tidy_unfinished(&store_path);
        Device {
            node: Node::new(None, transports),
            store_path,
            unlocked: false,
            unlocking: false,
            settings: HashMap::new(),
            pending,
        }
    }
}

impl Machine for Device {
    fn send(&mut self, to: &[ContactId], text: &str, channel: Channel) -> [u8; 16] {
        self.node.send(to, text, channel)
    }

    fn broadcast(&mut self, text: &str) -> [u8; 16] {
        self.node.broadcast(text)
    }

    fn start_pairing(&mut self, my_name: &str) {
        self.node.start_pairing(my_name)
    }

    fn confirm_pairing(&mut self, peer: &PublicIdentity, name: &str) -> ContactId {
        self.node.confirm_pairing(peer, name)
    }

    fn contacts(&self) -> Vec<Contact> {
        self.node.contacts()
    }

    fn conversation(&self, contact: ContactId) -> Vec<StoredMessage> {
        self.node.conversation(contact)
    }

    fn me(&self) -> Option<PublicIdentity> {
        self.node.me()
    }

    /// Plan 1's node does not carry messages for others yet.
    fn held_for_others(&self) -> usize {
        0
    }

    fn set_setting(&mut self, id: &str, value_index: usize) {
        self.settings.insert(id.to_string(), value_index);
    }

    fn lock(&self) -> Lock {
        if self.unlocked {
            Lock::Unlocked
        } else if self.store_path.exists() {
            Lock::Locked
        } else {
            Lock::NoStore
        }
    }

    fn begin_unlock(&mut self) -> Result<Unlocking, String> {
        if self.unlocking {
            return Err("already unlocking".to_string());
        }
        let create = match self.lock() {
            Lock::Unlocked => return Err("already unlocked".to_string()),
            Lock::Locked => false,
            Lock::NoStore => true,
        };
        self.unlocking = true;
        let path = self.store_path.clone();
        Ok(Box::new(move |passphrase: &str| {
            let store = if !create {
                Store::open(&path, passphrase)
            } else if passphrase.is_empty() {
                return Err("choose a passphrase".to_string());
            } else {
                create_store(&path, passphrase)
            };
            store.map(|s| Box::new(s) as Opened).map_err(store_error)
        }))
    }

    fn finish_unlock(&mut self, opened: Result<Opened, String>) -> Result<(), String> {
        self.unlocking = false;
        let store = opened?
            .downcast::<Store>()
            .map_err(|_| "not a store".to_string())?;
        self.node.unlock(*store);
        self.unlocked = true;
        Ok(())
    }

    fn rates(&self) -> Vec<(Channel, f32)> {
        self.node.rates()
    }

    fn tick(&mut self, now_ms: u64) -> Vec<NodeEvent> {
        let mut events = std::mem::take(&mut self.pending);
        events.extend(self.node.tick(now_ms));
        events
    }
}
