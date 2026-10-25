//! The real machine over a real store in a temporary folder, with no
//! sound card.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use bombechat_app::commands::{self, Lock, Machine, Shared};
use bombechat_app::keychain::Keeper;
use bombechat_app::Device;
use bombechat_core::Identity;
use bombechat_node::{NodeEvent, Transport, TransportEvent};
use bombechat_store::{Channel, ContactId, Delivery, Store, StoreKey, StoredMessage};
use tempfile::TempDir;

fn device(path: &Path) -> Device {
    Device::new(path.to_path_buf(), Vec::new(), Vec::new())
}

fn files(dir: &TempDir) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_first_passphrase_creates_the_store_and_later_ones_open_it() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");

    let first = Shared::new(Box::new(device(&path)));
    assert_eq!(commands::contacts(&first).unwrap_err(), "no store yet");
    assert_eq!(
        commands::unlock(&first, String::new(), None),
        Err("choose a passphrase".to_string())
    );
    assert!(
        files(&dir).is_empty(),
        "nothing made for an empty passphrase"
    );
    assert_eq!(
        commands::unlock(&first, "correct horse".into(), Some("  ".into())),
        Err("your name is needed".to_string())
    );
    assert!(files(&dir).is_empty(), "nothing made without a name");
    assert_eq!(
        commands::unlock(&first, "correct horse".into(), Some(" Ana Lima ".into())),
        Ok(())
    );
    assert_eq!(
        commands::machine(&first).unwrap().you,
        "Ana Lima",
        "the name hellos carry"
    );
    assert_eq!(commands::contacts(&first), Ok(vec![]));
    assert_eq!(commands::machine(&first).unwrap().name, "Machine No. 1");
    assert_eq!(files(&dir), vec!["bombechat.db", "bombechat.db.salt"]);
    let key = commands::machine(&first).unwrap().key;
    drop(first);

    let later = Shared::new(Box::new(device(&path)));
    assert_eq!(commands::contacts(&later).unwrap_err(), "locked");
    assert_eq!(
        commands::unlock(&later, "wrong".into(), None),
        Err("wrong passphrase".to_string())
    );
    assert_eq!(
        commands::unlock(&later, "correct horse".into(), None),
        Ok(())
    );
    assert_eq!(commands::machine(&later).unwrap().key, key, "same identity");
    assert_eq!(
        commands::machine(&later).unwrap().you,
        "Ana Lima",
        "the name is kept in the store"
    );
    assert_eq!(
        commands::unlock(&later, "correct horse".into(), None),
        Err("already unlocked".to_string())
    );
}

#[test]
fn a_second_unlock_is_refused_while_the_first_derives_its_key() {
    let dir = TempDir::new().unwrap();
    let mut d = device(&dir.path().join("bombechat.db"));
    let work = d.begin_unlock().unwrap();
    assert_eq!(
        d.begin_unlock().err(),
        Some("already unlocking".to_string())
    );
    assert_eq!(
        d.finish_unlock(work("", "Ana")),
        Err("choose a passphrase".to_string())
    );
    assert_eq!(d.lock(), Lock::NoStore);
    assert!(d.begin_unlock().is_ok(), "a failed unlock frees the way");
}

#[test]
fn a_first_run_that_never_finished_leaves_nothing_to_ask_a_passphrase_for() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    // What a crash part way through making the store could leave.
    for leftover in [
        "bombechat.db.salt",
        "bombechat.db.creating",
        "bombechat.db.creating.salt",
    ] {
        fs::write(dir.path().join(leftover), b"half made").unwrap();
    }
    let s = Shared::new(Box::new(device(&path)));
    assert!(files(&dir).is_empty());
    assert_eq!(commands::contacts(&s).unwrap_err(), "no store yet");
    assert_eq!(
        commands::unlock(&s, "correct horse".into(), Some("Ana".into())),
        Ok(())
    );
}

#[test]
fn why_there_is_no_sound_is_kept_for_a_page_that_opens_later() {
    let dir = TempDir::new().unwrap();
    let d = Device::new(
        dir.path().join("bombechat.db"),
        Vec::new(),
        vec![NodeEvent::ChannelState {
            channel: Channel::Sound,
            state: "unavailable: no audio device available".to_string(),
        }],
    );
    let s = Shared::new(Box::new(d));
    assert_eq!(
        commands::tick(&s, 20).len(),
        1,
        "reported on the first tick"
    );
    for now in [40, 60, 80] {
        commands::tick(&s, now);
    }
    let kept = commands::channels(&s);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (kept[0].channel, kept[0].state.as_str()),
        ("sound", "unavailable: no audio device available")
    );
}

/// A carrier that only reports a rate.
struct Measured;

impl Transport for Measured {
    fn name(&self) -> Channel {
        Channel::Sound
    }
    fn send(&mut self, _frame: Vec<u8>) {}
    fn poll(&mut self, _now_ms: u64) -> Vec<TransportEvent> {
        Vec::new()
    }
    fn rate(&self) -> Option<f32> {
        Some(16.1)
    }
}

#[test]
fn the_page_learns_the_rate_the_sound_transport_measures() {
    let dir = TempDir::new().unwrap();
    let d = Device::new(
        dir.path().join("bombechat.db"),
        vec![Box::new(Measured)],
        Vec::new(),
    );
    assert_eq!(d.rates(), vec![(Channel::Sound, 16.1)]);
    let s = Shared::new(Box::new(d));
    let kept = commands::channels(&s);
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (kept[0].channel, kept[0].state.as_str(), kept[0].rate),
        ("sound", "listening", Some(16.1))
    );
}

#[test]
fn every_setting_is_kept_and_read_back_after_a_restart() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    let first = Shared::new(Box::new(device(&path)));
    assert_eq!(commands::settings(&first)["sound"], 2, "nothing saved yet");
    let held = commands::setting_notes(&first);
    let changeable = || {
        commands::SETTINGS
            .into_iter()
            .filter(|(id, _, _)| !held.contains_key(id))
    };
    for (id, count, _) in changeable() {
        commands::set_setting(&first, id.into(), count - 1).unwrap();
    }
    drop(first);

    let later = Shared::new(Box::new(device(&path)));
    let all = commands::settings(&later);
    for (id, count, _) in changeable() {
        assert_eq!(all[id], count - 1, "{id} kept");
    }
    for (id, _, default) in commands::SETTINGS {
        if let Some(h) = held.get(id) {
            assert_eq!(all[id], h.value.unwrap_or(default), "{id} held");
        }
    }
}

#[test]
fn a_saved_listen_is_what_the_next_launch_reports() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    let first = Shared::new(Box::new(device(&path)));
    commands::set_setting(&first, "sound".into(), 1).unwrap();
    drop(first);
    let later = device(&path);
    assert_eq!(later.setting("sound"), Some(1));
    assert_eq!(
        commands::settings(&Shared::new(Box::new(later)))["sound"],
        1
    );
}

#[test]
fn a_corrupt_settings_file_falls_back_to_the_defaults() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    fs::write(dir.path().join("settings.json"), b"{\"sound\": 1, \"quie").unwrap();
    let s = Shared::new(Box::new(device(&path)));
    let all = commands::settings(&s);
    assert_eq!(all["sound"], 2);
    assert_eq!(all["quiet"], 2);
}

#[test]
fn unknown_ids_and_values_out_of_range_in_the_file_are_ignored() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    fs::write(
        dir.path().join("settings.json"),
        br#"{"warp": 3, "sound": 9, "quiet": 4}"#,
    )
    .unwrap();
    let d = device(&path);
    assert_eq!(d.setting("warp"), None);
    assert_eq!(d.setting("sound"), None, "out of range is not kept");
    let all = commands::settings(&Shared::new(Box::new(d)));
    assert!(!all.contains_key("warp"));
    assert_eq!(all["sound"], 2);
    assert_eq!(all["quiet"], 4);
}

/// A keychain in memory, shared with the test.
#[derive(Clone, Default)]
struct Pocket(std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>);

impl Keeper for Pocket {
    fn get(&self) -> Option<StoreKey> {
        StoreKey::from_bytes(self.0.lock().unwrap().as_deref()?)
    }
    fn put(&self, key: &StoreKey) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(key.as_bytes().to_vec());
        Ok(())
    }
    fn remove(&self) -> Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

impl Pocket {
    fn holds_a_key(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }
}

fn kept_device(path: &Path, pocket: &Pocket) -> Shared {
    Shared::new(Box::new(device(path).with_keeper(Box::new(pocket.clone()))))
}

#[test]
fn with_the_lock_off_the_next_launch_opens_without_asking() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    let pocket = Pocket::default();

    let first = kept_device(&path, &pocket);
    assert_eq!(
        commands::set_setting(&first, "lock".into(), 0),
        Err("Unlock first, then the lock can come off".to_string()),
        "no store open, so no key to keep"
    );
    assert_eq!(commands::settings(&first)["lock"], 1, "still on");
    commands::unlock(&first, "correct horse".into(), Some("Ana".into())).unwrap();
    assert!(!pocket.holds_a_key(), "the lock is on: nothing kept");
    commands::set_setting(&first, "lock".into(), 0).unwrap();
    assert!(pocket.holds_a_key(), "off: the key is in the keychain");
    let key = commands::machine(&first).unwrap().key;
    drop(first);

    let later = kept_device(&path, &pocket);
    assert_eq!(
        commands::contacts(&later),
        Ok(vec![]),
        "open without a passphrase"
    );
    assert_eq!(commands::machine(&later).unwrap().key, key);
    assert_eq!(commands::settings(&later)["lock"], 0);

    commands::set_setting(&later, "lock".into(), 1).unwrap();
    assert!(!pocket.holds_a_key(), "on: the key is let go");
    drop(later);
    let asks = kept_device(&path, &pocket);
    assert_eq!(commands::contacts(&asks).unwrap_err(), "locked");
}

#[test]
fn without_the_keeper_the_lock_off_still_asks() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    let pocket = Pocket::default();
    let first = kept_device(&path, &pocket);
    commands::unlock(&first, "correct horse".into(), Some("Ana".into())).unwrap();
    commands::set_setting(&first, "lock".into(), 0).unwrap();
    drop(first);
    // the person deleted the Keychain item
    pocket.remove().unwrap();
    let later = kept_device(&path, &pocket);
    assert_eq!(commands::contacts(&later).unwrap_err(), "locked");
    commands::unlock(&later, "correct horse".into(), None).unwrap();
    assert!(pocket.holds_a_key(), "kept again, since the lock is off");
}

#[test]
fn a_kept_key_that_no_longer_opens_the_store_is_let_go_and_the_passphrase_asked() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    let pocket = Pocket::default();
    let first = kept_device(&path, &pocket);
    commands::unlock(&first, "correct horse".into(), Some("Ana".into())).unwrap();
    commands::set_setting(&first, "lock".into(), 0).unwrap();
    drop(first);
    pocket
        .put(&StoreKey::from_bytes(&[9; 32]).unwrap())
        .unwrap();

    let later = kept_device(&path, &pocket);
    assert_eq!(commands::contacts(&later).unwrap_err(), "locked");
    assert!(!pocket.holds_a_key(), "the stale key is let go");
    commands::unlock(&later, "correct horse".into(), None).unwrap();
    assert!(pocket.holds_a_key());
}

/// A store with messages from Marisol stamped `ages` minutes before now.
fn store_with_ages(path: &Path, ages: &[u32]) -> ContactId {
    let store = Store::create(path, "correct horse", &Identity::generate(), "Ana").unwrap();
    let marisol = store
        .add_contact("Marisol", &Identity::generate().public(), true)
        .unwrap();
    let now = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 60) as u32;
    for (i, age) in ages.iter().enumerate() {
        store
            .put_message(&StoredMessage {
                id: [i as u8 + 1; 16],
                contact: marisol,
                outgoing: false,
                text: format!("{age} minutes old"),
                channel: Channel::Sound,
                at_min: now - age,
                heard: false,
                delivery: Delivery::Sent,
            })
            .unwrap();
    }
    marisol
}

#[test]
fn messages_older_than_delete_messages_after_go_at_unlock() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    let day = 24 * 60;
    let marisol = store_with_ages(&path, &[2 * day, day + 5, day - 5, 10]);
    let s = Shared::new(Box::new(device(&path)));
    commands::set_setting(&s, "expire".into(), 1).unwrap(); // 1 day, while locked: nothing yet
    commands::unlock(&s, "correct horse".into(), None).unwrap();
    let left: Vec<String> = commands::conversation(&s, marisol.0)
        .unwrap()
        .iter()
        .map(|m| m["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(left, vec!["1435 minutes old", "10 minutes old"]);

    commands::set_setting(&s, "expire".into(), 0).unwrap(); // Never
    assert_eq!(commands::conversation(&s, marisol.0).unwrap().len(), 2);
}

#[test]
fn at_its_default_delete_messages_after_keeps_a_week() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    let day = 24 * 60;
    let marisol = store_with_ages(&path, &[8 * day, 6 * day]);
    let s = Shared::new(Box::new(device(&path)));
    assert_eq!(commands::settings(&s)["expire"], 2);
    commands::unlock(&s, "correct horse".into(), None).unwrap();
    assert_eq!(commands::conversation(&s, marisol.0).unwrap().len(), 1);
}

#[test]
fn choosing_a_shorter_time_deletes_at_once() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    let day = 24 * 60;
    let marisol = store_with_ages(&path, &[3 * day, 60]);
    let s = Shared::new(Box::new(device(&path)));
    commands::unlock(&s, "correct horse".into(), None).unwrap();
    assert_eq!(commands::conversation(&s, marisol.0).unwrap().len(), 2);
    commands::set_setting(&s, "expire".into(), 1).unwrap();
    assert_eq!(commands::conversation(&s, marisol.0).unwrap().len(), 1);
}

#[test]
fn machine_sound_and_its_volume_are_kept_for_the_page() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bombechat.db");
    let s = Shared::new(Box::new(device(&path)));
    let all = commands::settings(&s);
    assert_eq!((all["machine"], all["machvol"]), (1, 2), "on, at 3");
    commands::set_setting(&s, "machine".into(), 0).unwrap();
    commands::set_setting(&s, "machvol".into(), 4).unwrap();
    drop(s);
    let all = commands::settings(&Shared::new(Box::new(device(&path))));
    assert_eq!((all["machine"], all["machvol"]), (0, 4));
}
