//! The real machine over a real store in a temporary folder, with no
//! sound card.

use std::fs;
use std::path::Path;

use bombechat_app::commands::{self, Lock, Machine, Shared};
use bombechat_app::Device;
use bombechat_node::{NodeEvent, Transport, TransportEvent};
use bombechat_store::Channel;
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
        commands::unlock(&first, String::new()),
        Err("choose a passphrase".to_string())
    );
    assert!(
        files(&dir).is_empty(),
        "nothing made for an empty passphrase"
    );
    assert_eq!(commands::unlock(&first, "correct horse".into()), Ok(()));
    assert_eq!(commands::contacts(&first), Ok(vec![]));
    assert_eq!(commands::machine(&first).unwrap().name, "Machine No. 1");
    assert_eq!(files(&dir), vec!["bombechat.db", "bombechat.db.salt"]);
    let key = commands::machine(&first).unwrap().key;
    drop(first);

    let later = Shared::new(Box::new(device(&path)));
    assert_eq!(commands::contacts(&later).unwrap_err(), "locked");
    assert_eq!(
        commands::unlock(&later, "wrong".into()),
        Err("wrong passphrase".to_string())
    );
    assert_eq!(commands::unlock(&later, "correct horse".into()), Ok(()));
    assert_eq!(commands::machine(&later).unwrap().key, key, "same identity");
    assert_eq!(
        commands::unlock(&later, "correct horse".into()),
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
        d.finish_unlock(work("")),
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
    assert_eq!(commands::unlock(&s, "correct horse".into()), Ok(()));
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
