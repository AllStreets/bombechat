//! The real machine: a [`Node`] over the sound card, and the store in the
//! app data folder.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use bombechat_core::{Identity, PublicIdentity};
use bombechat_modem::{AudioIo, Band, CpalAudio, ModemConfig, QuietGate};
use bombechat_node::{Node, NodeEvent, SoundSwitch, SoundTransport, Transport};
use bombechat_store::{Channel, Contact, ContactId, Store, StoreError, StoreKey, StoredMessage};

use crate::commands::{is_setting, Lock, Machine, Opened, Unlocking, SETTINGS};
use crate::keychain::{Keeper, Nowhere};

/// The quiet gate's defaults, as the settings page shows them: send under
/// 40 dB, two repeats a minute and a half apart.
const SEND_UNDER_DB: f32 = 40.0;
const REPEATS: u8 = 2;
const REPEAT_GAP_S: u32 = 90;

/// The settings page's Send under values, in dB, by position.
const SEND_UNDER_STEPS: [f32; 5] = [30.0, 35.0, 40.0, 45.0, 50.0];

/// The settings page's Delete messages after values, in days, by
/// position: Never, 1 day, 7 days, 30 days.
const EXPIRE_DAYS: [Option<u32>; 4] = [None, Some(1), Some(7), Some(30)];

/// How often old messages are looked for while the store is open, by the
/// pump's monotonic clock.
const PURGE_EVERY_MS: u64 = 60 * 60 * 1000;

/// The gain Send volume `value_index` (0 to 4, shown 1 to 5) plays at:
/// 0.2 to 1.0 of full scale.
pub fn send_gain(value_index: usize) -> f32 {
    0.2 * (value_index.min(4) + 1) as f32
}

/// Where the machine's running age is kept, beside the store: one number
/// of milliseconds, nothing more. See [`Device::age_ms`].
const AGE_FILE: &str = "age.json";

/// The running age is written down at least this often while it runs (and
/// at each purge and when the machine is let go), so a crash loses at most
/// this much of it: the age then runs behind, never ahead.
const AGE_SAVE_EVERY_MS: u64 = 60 * 1000;

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
/// Returns the store with its key.
pub fn create_store(
    path: &Path,
    passphrase: &str,
    name: &str,
) -> Result<(Store, StoreKey), StoreError> {
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
    open_store(path, passphrase)
}

/// Open the store at `path` with `passphrase`, and the key it derives.
fn open_store(path: &Path, passphrase: &str) -> Result<(Store, StoreKey), StoreError> {
    let key = Store::key_for(path, passphrase)?;
    Store::open_with_key(path, &key).map(|store| (store, key))
}

/// What an unlock hands back to [`Machine::finish_unlock`]: the opened
/// store and its key, kept in memory while the store is open so the lock
/// can be turned off without asking for the passphrase again.
struct Unlocked {
    store: Store,
    key: StoreKey,
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
    /// at load. Each the machine acts on is applied (see
    /// [`Device::apply`]); a setting never set acts at its default.
    settings: HashMap<String, usize>,
    /// Reported on the first tick, e.g. why there is no sound transport.
    pending: Vec<NodeEvent>,
    /// Where the store key is kept while Lock with passphrase is off.
    keeper: Box<dyn Keeper>,
    /// The open store's key, while it is open: needed to turn the lock
    /// off. Never leaves the machine except into the keychain.
    store_key: Option<StoreKey>,
    /// When to look for old messages to delete next, by the pump's
    /// clock: an hour after the unlock, then every hour.
    next_purge_ms: Option<u64>,
    /// The machine's running age, in ms: see [`Device::age_ms`].
    age_ms: u64,
    /// The age last written to [`AGE_FILE`].
    age_saved: u64,
    /// The pump's clock at the last tick, once there has been one.
    now_ms: Option<u64>,
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
        let age_ms = load_age(&age_path(&store_path));
        let mut device = Device {
            node: Node::new(None, transports),
            store_path,
            unlocked: false,
            unlocking: false,
            my_name: None,
            settings,
            pending,
            keeper: Box::new(Nowhere),
            store_key: None,
            next_purge_ms: None,
            age_ms,
            age_saved: age_ms,
            now_ms: None,
        };
        for (id, _, default) in SETTINGS {
            let value = device.setting(id).unwrap_or(default);
            device.apply(id, value);
        }
        device
    }

    /// Keep the store key in `keeper` while Lock with passphrase is off,
    /// and if it is off now and `keeper` has the key, open the store with
    /// it straight away, without asking. A kept key that does not open
    /// the store (it was made again since) is let go, and the passphrase
    /// is asked for as usual.
    pub fn with_keeper(mut self, keeper: Box<dyn Keeper>) -> Device {
        self.keeper = keeper;
        if self.lock_is_off() && self.store_path.exists() {
            if let Some(key) = self.keeper.get() {
                match Store::open_with_key(&self.store_path, &key) {
                    Ok(store) => self.install(store, key),
                    // not this store's key (it was made again since): let it go
                    Err(StoreError::WrongPassphrase) => {
                        eprintln!("bombechat: the key kept in the keychain does not open this store; asking for the passphrase");
                        let _ = self.keeper.remove();
                    }
                    // the store could not be read just now: keep the key for next time
                    Err(e) => eprintln!("bombechat: the store did not open with the kept key ({e}); asking for the passphrase"),
                }
            }
        }
        self
    }

    /// Whether Lock with passphrase is off.
    fn lock_is_off(&self) -> bool {
        self.setting("lock") == Some(0)
    }

    /// The store is open: hand it to the node, keep its key while it is
    /// open, keep it in the keychain if the lock is off (and make sure
    /// none is left there if it is on), and delete what is older than
    /// Delete messages after. If the keychain will not keep the key, the
    /// lock goes back on, so it is never shown off while launch would
    /// still ask.
    fn install(&mut self, store: Store, key: StoreKey) {
        self.my_name = Some(store.my_name());
        store.set_age(self.age_ms);
        if let Err(e) = store.adopt_unaged(self.age_ms) {
            eprintln!("bombechat: messages from before ages were kept have none: {e}");
        }
        self.node.unlock(store);
        self.unlocked = true;
        if self.lock_is_off() {
            if let Err(e) = self.keeper.put(&key) {
                eprintln!("bombechat: {e}; Lock with passphrase is back on");
                self.settings.insert("lock".to_string(), 1);
                if let Err(e) = save_settings(&settings_path(&self.store_path), &self.settings) {
                    eprintln!("bombechat: settings not saved: {e}");
                }
            }
        } else if let Err(e) = self.keeper.remove() {
            eprintln!("bombechat: {e}");
        }
        self.store_key = Some(key);
        self.purge();
        self.next_purge_ms = Some(self.now_ms.unwrap_or(0).saturating_add(PURGE_EVERY_MS));
    }

    /// The machine's running age, in ms: how long it has been running,
    /// all told, counted only by the pump's monotonic clock and kept
    /// beside the store between launches. It never goes back and never
    /// jumps, whatever the wall clock does. Each message is stored with
    /// the age it was stored at, and "Delete messages after" counts in it:
    /// time while the app is closed does not count, so deletion can run
    /// late, never early.
    pub fn age_ms(&self) -> u64 {
        self.age_ms
    }

    /// Move the age on by the pump's clock since the last tick, and write
    /// it down if it has moved far enough since it last was.
    fn grow(&mut self, now_ms: u64) {
        if let Some(last) = self.now_ms {
            self.age_ms = self.age_ms.saturating_add(now_ms.saturating_sub(last));
        }
        self.now_ms = Some(now_ms.max(self.now_ms.unwrap_or(0)));
        self.node.set_age(self.age_ms);
        if self.age_ms.saturating_sub(self.age_saved) >= AGE_SAVE_EVERY_MS {
            self.save_age();
        }
    }

    fn save_age(&mut self) {
        save_age(&age_path(&self.store_path), self.age_ms);
        self.age_saved = self.age_ms;
    }

    /// Delete stored messages kept longer than Delete messages after
    /// allows, in running age ([`Device::age_ms`]); never by the wall
    /// clock. Nothing while locked or at Never. Returns how many went.
    fn purge(&mut self) -> usize {
        let days = self
            .setting("expire")
            .or_else(|| default_of("expire"))
            .and_then(|i| EXPIRE_DAYS.get(i).copied().flatten());
        match days {
            Some(days) if self.unlocked => {
                self.save_age();
                let keep_ms = u64::from(days) * 24 * 60 * 60 * 1000;
                self.node.forget_older_than(self.age_ms, keep_ms)
            }
            _ => 0,
        }
    }

    /// Lock with passphrase: Off keeps the store key in the keychain (only
    /// while the store is open, which is when the key is here), On lets
    /// go of it.
    fn set_lock(&mut self, value_index: usize) -> Result<(), String> {
        if value_index == 0 {
            let key = self
                .store_key
                .as_ref()
                .ok_or("Unlock first, then the lock can come off")?;
            self.keeper.put(key)
        } else {
            self.keeper.remove()
        }
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
            "volume" => self.node.set_volume(send_gain(value_index)),
            "band" => self.node.set_band(if value_index == 0 {
                Band::NearSilent
            } else {
                Band::Audible
            }),
            "repeats" => self
                .node
                .set_repeats(value_index.min(u8::MAX as usize) as u8),
            _ => {}
        }
    }
}

/// Setting `id`'s default, as [`SETTINGS`] has it.
fn default_of(id: &str) -> Option<usize> {
    SETTINGS
        .iter()
        .find(|(name, _, _)| *name == id)
        .map(|(_, _, default)| *default)
}

/// Where the running age is kept, beside the store.
fn age_path(store_path: &Path) -> PathBuf {
    store_path.with_file_name(AGE_FILE)
}

/// The running age kept at `path`, or 0 if there is none (or it cannot be
/// read): starting again from 0 only makes deletion later.
fn load_age(path: &Path) -> u64 {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|v| v.get("age_ms")?.as_u64())
        .unwrap_or(0)
}

/// Keep the running age at `path`, written beside it first and moved into
/// place so a crash never leaves half a file.
fn save_age(path: &Path, age_ms: u64) {
    let staged = with_suffix(path, ".new");
    let json = serde_json::json!({ "age_ms": age_ms }).to_string();
    if let Err(e) = fs::write(&staged, json).and_then(|()| fs::rename(&staged, path)) {
        eprintln!("bombechat: the machine's age was not kept: {e}");
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

    /// Applied and kept, or refused and left as it was. Delete messages
    /// after deletes nothing here: the next purge (at unlock, or within
    /// the hour) does, which leaves time to put a plug back.
    fn set_setting(&mut self, id: &str, value_index: usize) -> Result<(), String> {
        let before = self.setting(id);
        if id == "lock" {
            self.set_lock(value_index)?;
        }
        self.apply(id, value_index);
        if before == Some(value_index) {
            return Ok(());
        }
        self.settings.insert(id.to_string(), value_index);
        if let Err(e) = save_settings(&settings_path(&self.store_path), &self.settings) {
            let old = before.or_else(|| default_of(id)).unwrap_or(0);
            match before {
                Some(b) => self.settings.insert(id.to_string(), b),
                None => self.settings.remove(id),
            };
            self.apply(id, old);
            if id == "lock" {
                let _ = self.set_lock(old);
            }
            return Err(format!("The setting was not saved: {e}"));
        }
        Ok(())
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
            let opened = if !create {
                open_store(&path, passphrase)
            } else if passphrase.is_empty() {
                return Err("choose a passphrase".to_string());
            } else if name.is_empty() {
                return Err("your name is needed".to_string());
            } else {
                create_store(&path, passphrase, name)
            };
            opened
                .map(|(store, key)| Box::new(Unlocked { store, key }) as Opened)
                .map_err(store_error)
        }))
    }

    fn finish_unlock(&mut self, opened: Result<Opened, String>) -> Result<(), String> {
        self.unlocking = false;
        let opened = opened?
            .downcast::<Unlocked>()
            .map_err(|_| "not a store".to_string())?;
        let Unlocked { store, key } = *opened;
        self.install(store, key);
        Ok(())
    }

    fn rates(&self) -> Vec<(Channel, f32)> {
        self.node.rates()
    }

    fn band(&self) -> Option<Band> {
        self.node.band()
    }

    /// Ticks the node, and while the store is open deletes old messages
    /// once an hour.
    fn tick(&mut self, now_ms: u64) -> Vec<NodeEvent> {
        let mut events = std::mem::take(&mut self.pending);
        self.grow(now_ms);
        events.extend(self.node.tick(now_ms));
        if self.unlocked {
            match self.next_purge_ms {
                Some(at) if now_ms < at => {}
                Some(_) => {
                    self.purge();
                    self.next_purge_ms = Some(now_ms + PURGE_EVERY_MS);
                }
                None => self.next_purge_ms = Some(now_ms + PURGE_EVERY_MS),
            }
        }
        events
    }
}

/// The age is written down when the machine is let go (the app quits),
/// so the next launch carries on from it.
impl Drop for Device {
    fn drop(&mut self) {
        if self.age_ms != self.age_saved {
            self.save_age();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombechat_modem::FakeAudio;
    use bombechat_store::ContactId;

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

        device.set_setting("sound", 0).unwrap(); // Off
        assert_eq!(heard.stops(), 1, "closed at once, not on the next tick");
        assert!(!heard.is_listening());

        let events = device.tick(1);
        assert_eq!(heard.starts(), 1, "stayed closed");
        assert!(
            sound_states(&events).contains(&"off".to_string()),
            "the channel reports off, not a stale listening"
        );

        device.set_setting("sound", 2).unwrap(); // Listen and send
        assert!(!heard.is_listening(), "not reopened until the next tick");
        device.tick(2);
        assert_eq!(heard.starts(), 2, "reopened");
        assert!(heard.is_listening());
    }

    /// An unlocked device over `audio` with its store in `dir`.
    fn unlocked(dir: &tempfile::TempDir, audio: FakeAudio) -> Device {
        let mut device = Device::with_audio(dir.path().join("m.db"), Box::new(audio));
        let work = device.begin_unlock().unwrap();
        device.finish_unlock(work("correct horse", "Ana")).unwrap();
        device
    }

    /// Play the pairing hello across a quiet room for `ms`, and what the
    /// speaker was handed.
    fn hello(device: &mut Device, card: &FakeAudio, ms: u64) -> Vec<f32> {
        device.start_pairing("Ana").unwrap();
        let mut now = 0;
        while now <= ms {
            card.feed(&vec![0.0; 9_600]);
            device.tick(now);
            now += 200;
        }
        card.played()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |m, x| m.max(x.abs()))
    }

    #[test]
    fn send_volume_is_the_gain_on_what_plays() {
        assert_eq!(send_gain(0), 0.2);
        assert_eq!(send_gain(4), 1.0);
        let mut peaks = Vec::new();
        for volume in [0, 2, 4] {
            let dir = tempfile::TempDir::new().unwrap();
            let card = FakeAudio::new(48_000);
            let mut device = unlocked(&dir, card.clone());
            device.set_setting("volume", volume).unwrap();
            peaks.push(peak(&hello(&mut device, &card, 2_000)));
        }
        let full = peaks[2];
        assert!(full > 0.1, "something played");
        assert!(
            (peaks[0] - 0.2 * full).abs() < 1e-3,
            "1 is a fifth of 5: {peaks:?}"
        );
        assert!(
            (peaks[1] - 0.6 * full).abs() < 1e-3,
            "3, the default: {peaks:?}"
        );
    }

    #[test]
    fn a_device_never_set_plays_at_the_default_volume() {
        let dir = tempfile::TempDir::new().unwrap();
        let card = FakeAudio::new(48_000);
        let mut device = unlocked(&dir, card.clone());
        let default = peak(&hello(&mut device, &card, 2_000));
        let dir = tempfile::TempDir::new().unwrap();
        let card = FakeAudio::new(48_000);
        let mut loud = unlocked(&dir, card.clone());
        loud.set_setting("volume", 4).unwrap();
        let full = peak(&hello(&mut loud, &card, 2_000));
        assert!((default - 0.6 * full).abs() < 1e-3);
    }

    #[test]
    fn repeats_is_how_often_each_frame_plays_again() {
        let mut lengths = Vec::new();
        for repeats in [0, 1, 3] {
            let dir = tempfile::TempDir::new().unwrap();
            let card = FakeAudio::new(48_000);
            let mut device = unlocked(&dir, card.clone());
            device.set_setting("repeats", repeats).unwrap();
            lengths.push(hello(&mut device, &card, 300_000).len());
        }
        assert!(lengths[0] > 0);
        assert_eq!(lengths[1], 2 * lengths[0], "{lengths:?}");
        assert_eq!(lengths[2], 4 * lengths[0], "{lengths:?}");
    }

    #[test]
    fn the_tone_band_moves_the_sound_card_and_a_slow_card_stays_audible() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut device =
            Device::with_audio(dir.path().join("m.db"), Box::new(FakeAudio::new(48_000)));
        assert_eq!(device.band(), Some(Band::NearSilent));
        device.set_setting("band", 1).unwrap();
        assert_eq!(device.band(), Some(Band::Audible));
        drop(device);
        let again = Device::with_audio(dir.path().join("m.db"), Box::new(FakeAudio::new(48_000)));
        assert_eq!(
            again.band(),
            Some(Band::Audible),
            "kept for the next launch"
        );

        let dir = tempfile::TempDir::new().unwrap();
        let mut slow =
            Device::with_audio(dir.path().join("m.db"), Box::new(FakeAudio::new(32_000)));
        slow.set_setting("band", 0).unwrap();
        assert_eq!(slow.setting("band"), Some(0), "the choice is kept");
        assert_eq!(
            slow.band(),
            Some(Band::Audible),
            "but the card cannot go higher"
        );
    }

    const HOUR: u64 = 60 * 60 * 1000;
    const DAY: u64 = 24 * HOUR;

    /// A store at `dir` holding messages stamped (wall clock) at `at`
    /// minutes, stored before the machine kept ages.
    fn aged_store(dir: &tempfile::TempDir, at: &[u32]) -> (PathBuf, ContactId) {
        let path = dir.path().join("m.db");
        let store = Store::create(&path, "correct horse", &Identity::generate(), "Ana").unwrap();
        let c = store
            .add_contact("Marisol", &Identity::generate().public(), true)
            .unwrap();
        for (i, at_min) in at.iter().enumerate() {
            store
                .put_message(&message(i as u8 + 1, c, *at_min))
                .unwrap();
        }
        (path, c)
    }

    fn message(id: u8, contact: ContactId, at_min: u32) -> StoredMessage {
        StoredMessage {
            id: [id; 16],
            contact,
            outgoing: false,
            text: format!("at {at_min}"),
            channel: Channel::Sound,
            at_min,
            heard: false,
            delivery: bombechat_store::Delivery::Sent,
        }
    }

    /// A device over the store at `path`, Delete messages after at 1 day,
    /// its pump started at `now_ms`, unlocked.
    fn opened(path: &Path, now_ms: u64) -> Device {
        let mut device = Device::new(path.to_path_buf(), Vec::new(), Vec::new());
        device.set_setting("expire", 1).unwrap();
        device.tick(now_ms);
        let work = device.begin_unlock().unwrap();
        device.finish_unlock(work("correct horse", "")).unwrap();
        device
    }

    /// Store a message as the node would, at the device's age now.
    fn arrive(device: &Device, id: u8, at_min: u32) {
        let store =
            Store::open_with_key(&device.store_path, device.store_key.as_ref().unwrap()).unwrap();
        store.set_age(device.age_ms());
        store
            .put_message(&message(id, ContactId(1), at_min))
            .unwrap();
    }

    fn ids(device: &Device) -> Vec<u8> {
        device
            .conversation(ContactId(1))
            .iter()
            .map(|m| m.id[0])
            .collect()
    }

    #[test]
    fn a_message_goes_once_it_has_been_kept_a_day_of_running_time() {
        let dir = tempfile::TempDir::new().unwrap();
        let (path, _) = aged_store(&dir, &[0]);
        let mut device = opened(&path, 0);
        assert_eq!(
            ids(&device),
            vec![1],
            "found at unlock: taken as new, not as old"
        );
        device.tick(2 * HOUR);
        arrive(&device, 2, 0);
        device.tick(DAY - HOUR);
        device.tick(DAY);
        assert_eq!(
            ids(&device),
            vec![1, 2],
            "exactly a day is not more than a day"
        );
        device.tick(DAY + HOUR);
        assert_eq!(
            ids(&device),
            vec![2],
            "the hour after a day of running time"
        );
        device.tick(3 * DAY);
        assert!(ids(&device).is_empty());
    }

    #[test]
    fn a_wall_clock_a_month_ahead_or_set_back_deletes_nothing_early() {
        // The device reads no wall clock at all; the wall clock reaches the
        // store only as each message's at_min. These are the stamps a clock
        // a month ahead at launch and at the first message, one set two days
        // back, and one put right again would leave.
        const NOW: u32 = 29_000_000;
        let month = 30 * 1440;
        let dir = tempfile::TempDir::new().unwrap();
        let (path, _) = aged_store(&dir, &[NOW + month, NOW - 2 * 1440]);
        let mut device = opened(&path, 0);
        arrive(&device, 3, NOW - 2 * 1440 + 5);
        arrive(&device, 4, NOW + 10);
        device.tick(HOUR);
        assert_eq!(
            ids(&device),
            vec![1, 2, 3, 4],
            "nothing early at this launch"
        );
        drop(device);
        let mut device = opened(&path, 0);
        device.tick(HOUR);
        assert_eq!(ids(&device), vec![1, 2, 3, 4], "nor at the next");
        device.tick(DAY + HOUR);
        assert!(
            ids(&device).is_empty(),
            "a day of running time later, all go"
        );
    }

    #[test]
    fn the_running_age_carries_on_across_launches_and_never_goes_back() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("m.db");
        let mut device = Device::new(path.clone(), Vec::new(), Vec::new());
        assert_eq!(device.age_ms(), 0);
        device.tick(5_000);
        assert_eq!(
            device.age_ms(),
            0,
            "the first tick only sets where the pump starts"
        );
        device.tick(65_000);
        assert_eq!(device.age_ms(), 60_000);
        device.tick(40_000); // a pump clock that went back: the age does not
        assert_eq!(device.age_ms(), 60_000);
        device.tick(70_000);
        assert_eq!(device.age_ms(), 65_000);
        drop(device);

        // the next launch's pump starts at 0 again: the age carries on
        let mut device = Device::new(path.clone(), Vec::new(), Vec::new());
        assert_eq!(device.age_ms(), 65_000);
        device.tick(0);
        device.tick(1_000);
        assert_eq!(device.age_ms(), 66_000);
        drop(device);
        assert_eq!(load_age(&age_path(&path)), 66_000);
    }

    #[test]
    fn a_crash_loses_at_most_a_minute_of_age_and_deletes_nothing_early() {
        let dir = tempfile::TempDir::new().unwrap();
        let (path, _) = aged_store(&dir, &[0]);
        let mut device = opened(&path, 0);
        device.tick(10 * HOUR);
        device.tick(10 * HOUR + 30_000);
        let kept = load_age(&age_path(&path));
        assert!(
            kept >= 10 * HOUR && kept <= device.age_ms(),
            "written down, behind if anything"
        );
        std::mem::forget(device); // a crash: no goodbye
        let mut device = opened(&path, 0);
        assert!(device.age_ms() <= 10 * HOUR + 30_000);
        device.tick(13 * HOUR);
        assert_eq!(
            ids(&device),
            vec![1],
            "23 hours kept, so far as anyone can tell"
        );
    }
}
