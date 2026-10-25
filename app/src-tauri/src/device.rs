//! The real machine: a [`Node`] over the sound card, and the store in the
//! app data folder.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use bombechat_core::{Identity, PublicIdentity};
use bombechat_modem::{AudioIo, Band, CpalAudio, ModemConfig, QuietGate};
use bombechat_node::{Node, NodeEvent, SoundSwitch, SoundTransport, Transport};
use bombechat_store::{Channel, Contact, ContactId, Store, StoreError, StoredMessage};

use crate::commands::{is_setting, Lock, Machine, Opened, Unlocking};

/// The quiet gate's defaults, as the settings page shows them: send under
/// 40 dB, two repeats a minute and a half apart.
const SEND_UNDER_DB: f32 = 40.0;
const REPEATS: u8 = 2;
const REPEAT_GAP_S: u32 = 90;

/// The settings page's Send under values, in dB, by position.
const SEND_UNDER_STEPS: [f32; 5] = [30.0, 35.0, 40.0, 45.0, 50.0];

/// The settings file's name, beside the store. Settings are not secret
/// (which switch is where, a threshold), so they are kept as plain JSON.
const SETTINGS_FILE: &str = "settings.json";

/// The channel state when the sound card runs too slow for either band.
pub const RATE_TOO_LOW: &str = "unavailable: microphone sample rate too low";

/// The modem configuration for a sound card at `sample_rate`: the
/// near-silent band if the rate can carry it, else the audible band, else
/// none.
pub fn modem_config(sample_rate: u32) -> Option<ModemConfig> {
    let near_silent = ModemConfig {
        sample_rate,
        ..ModemConfig::default()
    };
    let audible = ModemConfig {
        band: Band::Audible,
        ..near_silent
    };
    [near_silent, audible]
        .into_iter()
        .find(|cfg| cfg.validate().is_ok())
}

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
/// the real name). `name` is the person's own name, kept in the store.
pub fn create_store(path: &Path, passphrase: &str, name: &str) -> Result<Store, StoreError> {
    tidy_unfinished(path);
    let staged = staging(path);
    let made = Store::create(&staged, passphrase, &Identity::generate(), name);
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
    /// The person's name from the store, once unlocked.
    my_name: Option<String>,
    /// Settings as the person last set them, kept in [`SETTINGS_FILE`]
    /// beside the store: the only record of them, which the page reads
    /// at load. The Sound switch and Send under are applied to the node;
    /// the rest are only kept in this plan.
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
            Ok(audio) => Device::with_audio(store_path, Box::new(audio)),
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

    /// A machine listening and playing through `audio`: on the
    /// near-silent band if its sample rate can carry it, else on the
    /// audible band; if neither fits it runs with no transport and
    /// reports [`RATE_TOO_LOW`] on its first tick.
    pub fn with_audio(store_path: PathBuf, audio: Box<dyn AudioIo>) -> Device {
        let rate = audio.sample_rate();
        match modem_config(rate) {
            Some(cfg) => {
                if cfg.band != Band::NearSilent {
                    eprintln!("bombechat: the sound card runs at {rate} Hz, too slow for the near-silent band; using the audible band");
                }
                let gate = QuietGate::new(SEND_UNDER_DB, REPEATS, REPEAT_GAP_S);
                let sound = SoundTransport::new(audio, cfg, gate);
                Device::new(store_path, vec![Box::new(sound)], Vec::new())
            }
            None => Device::new(
                store_path,
                Vec::new(),
                vec![NodeEvent::ChannelState {
                    channel: Channel::Sound,
                    state: RATE_TOO_LOW.to_string(),
                }],
            ),
        }
    }

    /// A machine over `transports`, reporting `pending` on its first tick,
    /// with the settings kept beside the store applied.
    pub fn new(
        store_path: PathBuf,
        transports: Vec<Box<dyn Transport>>,
        pending: Vec<NodeEvent>,
    ) -> Device {
        tidy_unfinished(&store_path);
        let settings = load_settings(&settings_path(&store_path));
        let mut device = Device {
            node: Node::new(None, transports),
            store_path,
            unlocked: false,
            unlocking: false,
            my_name: None,
            settings,
            pending,
        };
        let kept: Vec<(String, usize)> = device
            .settings
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        for (id, value) in kept {
            device.apply(&id, value);
        }
        device
    }

    /// A setting's value, as its position on the settings page, if it has
    /// been set.
    pub fn setting(&self, id: &str) -> Option<usize> {
        self.settings.get(id).copied()
    }

    /// Retune the node for a setting it acts on.
    fn apply(&mut self, id: &str, value_index: usize) {
        match id {
            "sound" => {
                if let Some(switch) = SoundSwitch::from_index(value_index) {
                    self.node.set_sound(switch);
                }
            }
            "quiet" => {
                if let Some(db) = SEND_UNDER_STEPS.get(value_index) {
                    self.node.set_send_under(*db);
                }
            }
            _ => {}
        }
    }
}

/// Where the settings are kept, beside the store at `store_path`.
fn settings_path(store_path: &Path) -> PathBuf {
    store_path.with_file_name(SETTINGS_FILE)
}

/// The settings kept at `path`, leaving out any id or value the settings
/// page does not have; none if there are none or the file cannot be read
/// (each setting is then at its default).
fn load_settings(path: &Path) -> HashMap<String, usize> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<HashMap<String, usize>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(id, value)| is_setting(id, *value))
        .collect()
}

/// Keep `settings` at `path`, written beside it first and moved into
/// place so a crash never leaves half a file.
fn save_settings(path: &Path, settings: &HashMap<String, usize>) -> std::io::Result<()> {
    let staged = with_suffix(path, ".new");
    let json = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    fs::write(&staged, json)?;
    fs::rename(&staged, path)
}

impl Machine for Device {
    fn send(&mut self, to: &[ContactId], text: &str, channel: Channel) -> [u8; 16] {
        self.node.send(to, text, channel)
    }

    fn broadcast(&mut self, text: &str) -> [u8; 16] {
        self.node.broadcast(text)
    }

    fn start_pairing(&mut self, my_name: &str) -> Result<(), String> {
        self.node.start_pairing(my_name).map_err(str::to_string)
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

    fn my_name(&self) -> Option<String> {
        self.my_name.clone()
    }

    fn me(&self) -> Option<PublicIdentity> {
        self.node.me()
    }

    /// Plan 1's node does not carry messages for others yet.
    fn held_for_others(&self) -> usize {
        0
    }

    fn set_setting(&mut self, id: &str, value_index: usize) {
        self.apply(id, value_index);
        if self.settings.insert(id.to_string(), value_index) != Some(value_index) {
            if let Err(e) = save_settings(&settings_path(&self.store_path), &self.settings) {
                eprintln!("bombechat: settings not saved: {e}");
            }
        }
    }

    fn settings(&self) -> HashMap<String, usize> {
        self.settings.clone()
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
        Ok(Box::new(move |passphrase: &str, name: &str| {
            let store = if !create {
                Store::open(&path, passphrase)
            } else if passphrase.is_empty() {
                return Err("choose a passphrase".to_string());
            } else if name.is_empty() {
                return Err("your name is needed".to_string());
            } else {
                create_store(&path, passphrase, name)
            };
            store.map(|s| Box::new(s) as Opened).map_err(store_error)
        }))
    }

    fn finish_unlock(&mut self, opened: Result<Opened, String>) -> Result<(), String> {
        self.unlocking = false;
        let store = opened?
            .downcast::<Store>()
            .map_err(|_| "not a store".to_string())?;
        self.my_name = Some(store.my_name());
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

#[cfg(test)]
mod tests {
    use super::*;
    use bombechat_modem::FakeAudio;

    fn sound_states(events: &[NodeEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::ChannelState {
                    channel: Channel::Sound,
                    state,
                } => Some(state.clone()),
                _ => None,
            })
            .collect()
    }

    /// A device with the Sound switch persisted at Off, as it would be
    /// for someone who quit the app with it there: the microphone must
    /// never open on the next launch, not even once.
    #[test]
    fn a_persisted_sound_off_never_opens_the_microphone() {
        let dir = tempfile::TempDir::new().unwrap();
        let store_path = dir.path().join("m.db");
        std::fs::write(settings_path(&store_path), r#"{"sound":0}"#).unwrap();

        let audio = FakeAudio::new(48_000);
        let heard = audio.clone();
        let mut device = Device::with_audio(store_path, Box::new(audio));

        for ms in [0, 1, 1_000] {
            device.tick(ms);
        }
        assert_eq!(heard.starts(), 0, "the microphone was never opened");
    }

    /// Turning the Sound switch to Off through `set_setting` (as the
    /// settings page does) really closes the microphone, at once, not
    /// only once the app happens to quit; turning it back on reopens it.
    #[test]
    fn turning_sound_off_closes_the_microphone_and_on_reopens_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let audio = FakeAudio::new(48_000);
        let heard = audio.clone();
        let mut device = Device::with_audio(dir.path().join("m.db"), Box::new(audio));

        device.tick(0);
        assert_eq!(heard.starts(), 1, "listening by default, mic opened");
        assert!(heard.is_listening());

        device.set_setting("sound", 0); // Off
        assert_eq!(heard.stops(), 1, "closed at once, not on the next tick");
        assert!(!heard.is_listening());

        let events = device.tick(1);
        assert_eq!(heard.starts(), 1, "stayed closed");
        assert!(
            sound_states(&events).contains(&"off".to_string()),
            "the channel reports off, not a stale listening"
        );

        device.set_setting("sound", 2); // Listen and send
        assert!(!heard.is_listening(), "not reopened until the next tick");
        device.tick(2);
        assert_eq!(heard.starts(), 2, "reopened");
        assert!(heard.is_listening());
    }
}
