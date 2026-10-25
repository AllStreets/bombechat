//! The Tauri commands and event payloads, against a fake machine: no
//! audio, no window, no store on disk.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use bombechat_app::commands::{self, bridge_event, Lock, Machine, Opened, Shared, Unlocking};
use bombechat_core::{safety_number, ChunkView, PublicIdentity};
use bombechat_node::{NodeEvent, OutboxState, MAX_BROADCAST, NOT_SENT, NOT_STORED};
use bombechat_store::{Channel, Contact, ContactId, Delivery, StoredBroadcast, StoredMessage};
use serde_json::json;

/// What the fake was asked to do.
#[derive(Default)]
struct Calls {
    sent: Vec<(Vec<ContactId>, String, Channel)>,
    broadcasts: Vec<String>,
    /// The channel each broadcast went on.
    broadcast_on: Vec<Channel>,
    pairings: Vec<String>,
    /// How many times pairing was stopped.
    stopped: usize,
    confirmed: Vec<(PublicIdentity, String)>,
    settings: Vec<(String, usize)>,
    /// What the fake's settings file holds, as `settings` reads it back.
    saved: HashMap<String, usize>,
    /// When set, every setting is refused with this.
    refuse: Option<String>,
    passphrases: Arc<Mutex<Vec<String>>>,
    /// The name each unlock was given.
    names: Arc<Mutex<Vec<String>>>,
}

/// What each tick returns, in turn: events, or `None` to panic.
type Script = Arc<Mutex<VecDeque<Option<Vec<NodeEvent>>>>>;

type Rates = Arc<Mutex<Vec<(Channel, f32)>>>;

struct Fake {
    calls: Arc<Mutex<Calls>>,
    lock: Lock,
    unlocking: bool,
    script: Script,
    /// The rate each channel reports, bytes a second.
    rates: Rates,
    /// When set, the next unlock's key derivation says it has started,
    /// then waits to be let go.
    slow: Option<(Sender<()>, Receiver<()>)>,
}

const ID: [u8; 16] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10,
];

fn me() -> PublicIdentity {
    PublicIdentity {
        sign: [1; 32],
        dh: [2; 32],
    }
}

fn marisol() -> Contact {
    Contact {
        id: ContactId(1),
        name: "Marisol Ortega".to_string(),
        who: PublicIdentity {
            sign: [7; 32],
            dh: [9; 32],
        },
        checked: true,
    }
}

fn stored(text: &str) -> StoredMessage {
    StoredMessage {
        id: ID,
        contact: ContactId(1),
        outgoing: false,
        text: text.to_string(),
        channel: Channel::Sound,
        at_min: 29_000_000,
        heard: false,
        delivery: Delivery::Sent,
    }
}

impl Machine for Fake {
    fn send(&mut self, to: &[ContactId], text: &str, channel: Channel) -> [u8; 16] {
        self.calls
            .lock()
            .unwrap()
            .sent
            .push((to.to_vec(), text.to_string(), channel));
        if channel == Channel::Sound && to == [ContactId(1)] {
            ID
        } else {
            NOT_SENT
        }
    }
    fn broadcast(&mut self, text: &str, channel: Channel) -> [u8; 16] {
        let mut calls = self.calls.lock().unwrap();
        calls.broadcasts.push(text.to_string());
        calls.broadcast_on.push(channel);
        ID
    }
    fn start_pairing(&mut self, my_name: &str) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .pairings
            .push(my_name.to_string());
        Ok(())
    }
    fn stop_pairing(&mut self) {
        self.calls.lock().unwrap().stopped += 1;
    }
    fn confirm_pairing(&mut self, peer: &PublicIdentity, name: &str) -> ContactId {
        self.calls
            .lock()
            .unwrap()
            .confirmed
            .push((peer.clone(), name.to_string()));
        if peer.sign == [0; 32] {
            NOT_STORED
        } else {
            ContactId(2)
        }
    }
    fn contacts(&self) -> Vec<Contact> {
        vec![marisol()]
    }
    fn conversation(&self, contact: ContactId) -> Vec<StoredMessage> {
        if contact == ContactId(1) {
            vec![stored("Bring the radio.")]
        } else {
            Vec::new()
        }
    }
    fn broadcasts(&self) -> Vec<StoredBroadcast> {
        vec![
            StoredBroadcast {
                id: ID,
                outgoing: true,
                signer: [1; 32],
                verified: true,
                hops: 0,
                text: "Water at the church.".into(),
                at_min: 29_000_002,
                delivery: Delivery::Waiting,
            },
            StoredBroadcast {
                id: [4; 16],
                outgoing: false,
                signer: [7; 32],
                verified: false,
                hops: 2,
                text: "Road north is open.".into(),
                at_min: 29_000_003,
                delivery: Delivery::Sent,
            },
        ]
    }
    fn my_name(&self) -> Option<String> {
        (self.lock == Lock::Unlocked).then(|| "Ana Lima".to_string())
    }
    fn me(&self) -> Option<PublicIdentity> {
        (self.lock == Lock::Unlocked).then(me)
    }
    fn held_for_others(&self) -> usize {
        3
    }
    fn set_setting(&mut self, id: &str, value_index: usize) -> Result<(), String> {
        if self.calls.lock().unwrap().refuse.is_some() {
            return Err(self.calls.lock().unwrap().refuse.clone().unwrap());
        }
        self.calls
            .lock()
            .unwrap()
            .settings
            .push((id.to_string(), value_index));
        self.calls
            .lock()
            .unwrap()
            .saved
            .insert(id.to_string(), value_index);
        Ok(())
    }
    fn settings(&self) -> HashMap<String, usize> {
        self.calls.lock().unwrap().saved.clone()
    }
    fn lock(&self) -> Lock {
        self.lock
    }
    fn begin_unlock(&mut self) -> Result<Unlocking, String> {
        if self.unlocking {
            return Err("already unlocking".to_string());
        }
        self.unlocking = true;
        let slow = self.slow.take();
        let passphrases = self.calls.lock().unwrap().passphrases.clone();
        let names = self.calls.lock().unwrap().names.clone();
        Ok(Box::new(move |passphrase: &str, name: &str| {
            names.lock().unwrap().push(name.to_string());
            if let Some((started, go)) = slow {
                started.send(()).unwrap();
                go.recv().unwrap();
            }
            passphrases.lock().unwrap().push(passphrase.to_string());
            if passphrase == "correct horse" {
                Ok(Box::new(()) as Opened)
            } else {
                Err("wrong passphrase".to_string())
            }
        }))
    }
    fn finish_unlock(&mut self, opened: Result<Opened, String>) -> Result<(), String> {
        self.unlocking = false;
        opened?;
        self.lock = Lock::Unlocked;
        Ok(())
    }
    fn rates(&self) -> Vec<(Channel, f32)> {
        self.rates.lock().unwrap().clone()
    }
    fn tick(&mut self, _now_ms: u64) -> Vec<NodeEvent> {
        // Taken out first, so a scripted panic does not poison the script.
        let next = self.script.lock().unwrap().pop_front();
        match next {
            Some(Some(events)) => events,
            Some(None) => panic!("scripted tick panic"),
            None => Vec::new(),
        }
    }
}

struct Rig {
    shared: Arc<Shared>,
    calls: Arc<Mutex<Calls>>,
    script: Script,
    rates: Rates,
}

fn rig(lock: Lock, slow: Option<(Sender<()>, Receiver<()>)>) -> Rig {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let script = Script::default();
    let rates = Rates::default();
    let machine: Box<dyn Machine> = Box::new(Fake {
        calls: calls.clone(),
        lock,
        unlocking: false,
        script: script.clone(),
        rates: rates.clone(),
        slow,
    });
    Rig {
        shared: Arc::new(Shared::new(machine)),
        calls,
        script,
        rates,
    }
}

fn fake(lock: Lock) -> (Arc<Shared>, Arc<Mutex<Calls>>) {
    let r = rig(lock, None);
    (r.shared, r.calls)
}

#[test]
fn send_returns_the_id_as_32_hex_and_the_node_gets_the_text() {
    let (m, calls) = fake(Lock::Unlocked);
    let id = commands::send(&m, vec![1], "Water at the church.".into(), "sound".into()).unwrap();
    assert_eq!(id, "0123456789abcdeffedcba9876543210");
    assert_eq!(id.len(), 32);
    assert_eq!(
        calls.lock().unwrap().sent,
        vec![(
            vec![ContactId(1)],
            "Water at the church.".to_string(),
            Channel::Sound
        )]
    );
}

#[test]
fn send_passes_the_channel_and_reports_nothing_sent_as_an_error() {
    let (m, calls) = fake(Lock::Unlocked);
    assert_eq!(
        commands::send(&m, vec![1], "hi".into(), "Light".into()),
        Err("not sent".to_string())
    );
    assert_eq!(calls.lock().unwrap().sent[0].2, Channel::Light);
    assert!(commands::send(&m, vec![1], "hi".into(), "carrier pigeon".into()).is_err());
    assert_eq!(
        calls.lock().unwrap().sent.len(),
        1,
        "unknown channel never reaches the node"
    );
}

#[test]
fn broadcast_returns_the_id_as_hex() {
    let (m, calls) = fake(Lock::Unlocked);
    let id = commands::broadcast(&m, "Shelter open.".into(), "sound".into()).unwrap();
    assert_eq!(id, "0123456789abcdeffedcba9876543210");
    assert_eq!(calls.lock().unwrap().broadcasts, vec!["Shelter open."]);
}

#[test]
fn broadcast_passes_the_channel_and_refuses_one_there_is_none_of() {
    let (m, calls) = fake(Lock::Unlocked);
    commands::broadcast(&m, "By light.".into(), "Light".into()).unwrap();
    commands::broadcast(&m, "By sound.".into(), "sound".into()).unwrap();
    assert_eq!(
        commands::broadcast(&m, "By pigeon.".into(), "pigeon".into()),
        Err("no such channel: pigeon".to_string())
    );
    assert_eq!(
        calls.lock().unwrap().broadcast_on,
        vec![Channel::Light, Channel::Sound]
    );
}

#[test]
fn stop_pairing_ends_the_pairing_in_progress() {
    let (m, calls) = fake(Lock::Unlocked);
    commands::pair(&m).unwrap();
    commands::stop_pairing(&m);
    assert_eq!(calls.lock().unwrap().stopped, 1);
}

#[test]
fn an_empty_broadcast_is_refused_with_a_sentence() {
    let (m, calls) = fake(Lock::Unlocked);
    for text in ["", "   ", "\n\t"] {
        assert_eq!(
            commands::broadcast(&m, text.into(), "sound".into()),
            Err("Write the notice first".to_string())
        );
    }
    assert!(calls.lock().unwrap().broadcasts.is_empty());
}

#[test]
fn a_broadcast_longer_than_an_envelope_holds_is_refused_with_its_length() {
    let (m, calls) = fake(Lock::Unlocked);
    let longest = "x".repeat(MAX_BROADCAST);
    assert!(commands::broadcast(&m, longest.clone(), "sound".into()).is_ok());
    assert_eq!(
        commands::broadcast(&m, "x".repeat(MAX_BROADCAST + 1), "sound".into()),
        Err(format!(
            "Too long to broadcast: {} bytes, at most {MAX_BROADCAST}",
            MAX_BROADCAST + 1
        ))
    );
    // counted in bytes, as the envelope holds it: each of these is two
    let accents = "é".repeat(MAX_BROADCAST / 2 + 1);
    assert!(commands::broadcast(&m, accents, "sound".into()).is_err());
    assert_eq!(calls.lock().unwrap().broadcasts, vec![longest]);
}

#[test]
fn the_simulator_broadcasts_as_much_as_the_node() {
    let sim = include_str!("../../ui/simulator.js");
    let line = sim
        .lines()
        .find(|l| l.starts_with("const MAX_BROADCAST = "))
        .expect("simulator.js has MAX_BROADCAST");
    let n: usize = line
        .trim_start_matches("const MAX_BROADCAST = ")
        .trim_end_matches(';')
        .parse()
        .expect("a number");
    assert_eq!(n, MAX_BROADCAST);
}

#[test]
fn pair_announces_your_name_and_refuses_while_locked() {
    let (m, calls) = fake(Lock::Unlocked);
    commands::pair(&m).unwrap();
    assert_eq!(calls.lock().unwrap().pairings, vec!["Ana Lima"]);

    let (locked, calls) = fake(Lock::Locked);
    assert_eq!(commands::pair(&locked), Err("locked".to_string()));
    assert!(calls.lock().unwrap().pairings.is_empty());
}

#[test]
fn confirm_pair_reads_the_peer_hex_and_returns_the_contact_id() {
    let (m, calls) = fake(Lock::Unlocked);
    let peer = "07".repeat(32) + &"09".repeat(32);
    assert_eq!(
        commands::confirm_pair(&m, peer.clone(), "Teodor".into()),
        Ok(2)
    );
    let (who, name) = calls.lock().unwrap().confirmed[0].clone();
    assert_eq!(who, marisol().who);
    assert_eq!(name, "Teodor");

    assert!(commands::confirm_pair(&m, "07".repeat(32), "short".into()).is_err());
    assert!(commands::confirm_pair(&m, "zz".repeat(64), "not hex".into()).is_err());
    assert_eq!(
        commands::confirm_pair(&m, "00".repeat(64), "refused".into()),
        Err("not stored".to_string())
    );
}

#[test]
fn set_setting_takes_the_ids_and_values_the_settings_page_has() {
    let (m, calls) = fake(Lock::Unlocked);
    commands::set_setting(&m, "sound".into(), 2).unwrap();
    commands::set_setting(&m, "quiet".into(), 4).unwrap();
    assert!(commands::set_setting(&m, "sound".into(), 3).is_err());
    assert!(commands::set_setting(&m, "warp".into(), 0).is_err());
    assert_eq!(
        calls.lock().unwrap().settings,
        vec![("sound".to_string(), 2), ("quiet".to_string(), 4)]
    );
}

#[test]
fn settings_are_the_saved_values_with_defaults_for_the_rest() {
    let (m, calls) = fake(Lock::Locked);
    let all = commands::settings(&m);
    assert_eq!(
        all.len(),
        commands::SETTINGS.len(),
        "every setting, saved or not"
    );
    assert_eq!(
        all["sound"], 2,
        "Listen and send until the person says otherwise"
    );
    assert_eq!(all["quiet"], 2, "40 dB");
    assert_eq!(all["light"], 0);
    assert_eq!(all["hops"], 3);

    commands::set_setting(&m, "sound".into(), 1).unwrap();
    commands::set_setting(&m, "expire".into(), 0).unwrap();
    let all = commands::settings(&m);
    assert_eq!(
        all["sound"], 1,
        "a saved Listen is read back, locked or not"
    );
    assert_eq!(all["expire"], 0);
    assert_eq!(
        calls.lock().unwrap().settings.len(),
        2,
        "reading sets nothing"
    );
}

#[test]
fn settings_drop_unknown_ids_and_values_out_of_range() {
    let (m, calls) = fake(Lock::Unlocked);
    {
        let mut c = calls.lock().unwrap();
        c.saved.insert("warp".into(), 1);
        c.saved.insert("sound".into(), 7);
        c.saved.insert("repeats".into(), 0);
    }
    let all = commands::settings(&m);
    assert!(!all.contains_key("warp"));
    assert_eq!(all["sound"], 2, "out of range: the default");
    assert_eq!(all["repeats"], 0);
}

#[test]
fn every_setting_has_a_default_within_its_values() {
    for (id, count, default) in commands::SETTINGS {
        assert!(default < count, "{id}: default {default} of {count}");
    }
}

#[test]
fn every_setting_is_one_the_settings_page_has_with_its_default() {
    let page = include_str!("../../ui/settings.html");
    for (id, count, default) in commands::SETTINGS {
        let at = page
            .find(&format!("{{id:'{id}',"))
            .unwrap_or_else(|| panic!("settings.html has no {id}"));
        let line = &page[at..at + page[at..].find('\n').unwrap()];
        let values = line
            .split("values:[")
            .nth(1)
            .and_then(|v| v.split(']').next())
            .unwrap();
        assert_eq!(values.split(',').count(), count, "{id}: how many values");
        assert!(
            line.contains(&format!(",v:{default},")),
            "{id}: the page's default is not {default}"
        );
    }
    assert_eq!(
        page.matches("{id:'").count(),
        commands::SETTINGS.len(),
        "settings.html has a setting the device does not keep"
    );
}

#[test]
fn the_simulator_keeps_the_same_settings_with_the_same_defaults() {
    let sim = include_str!("../../ui/simulator.js");
    let table = |name: &str| -> Vec<String> {
        let start = format!("const {name} = {{");
        let line = sim
            .lines()
            .find(|l| l.starts_with(&start))
            .unwrap_or_else(|| panic!("simulator.js has no {name}"));
        let mut entries: Vec<String> = line
            .trim_start_matches(start.as_str())
            .trim_end_matches("};")
            .split(',')
            .map(|kv| kv.replace(' ', ""))
            .collect();
        entries.sort();
        entries
    };
    let want = |f: fn(&(&str, usize, usize)) -> String| -> Vec<String> {
        let mut v: Vec<String> = commands::SETTINGS.iter().map(f).collect();
        v.sort();
        v
    };
    assert_eq!(
        table("DEFAULTS"),
        want(|(id, _, default)| format!("{id}:{default}"))
    );
    assert_eq!(
        table("COUNTS"),
        want(|(id, count, _)| format!("{id}:{count}"))
    );
}

#[test]
fn unlock_passes_the_passphrase_and_returns_the_error_the_page_shows() {
    let (m, calls) = fake(Lock::NoStore);
    assert_eq!(
        commands::unlock(&m, "wrong".into(), None),
        Err("wrong passphrase".to_string())
    );
    assert_eq!(
        commands::unlock(&m, "correct horse".into(), Some("Ana Lima".into())),
        Ok(())
    );
    let passphrases = calls.lock().unwrap().passphrases.lock().unwrap().clone();
    assert_eq!(passphrases, vec!["wrong", "correct horse"]);
    let names = calls.lock().unwrap().names.lock().unwrap().clone();
    assert_eq!(names, vec!["", "Ana Lima"], "no name given is an empty one");
}

#[test]
fn unlock_derives_the_key_without_holding_the_machine() {
    let (started_tx, started) = channel();
    let (go, go_rx) = channel();
    let r = rig(Lock::NoStore, Some((started_tx, go_rx)));
    let shared = r.shared.clone();
    let unlocking = thread::spawn(move || commands::unlock(&shared, "correct horse".into(), None));
    started.recv().unwrap();

    // Mid-derivation, the pump still ticks and commands still answer.
    r.script
        .lock()
        .unwrap()
        .push_back(Some(vec![NodeEvent::RoomDb(41.0)]));
    let (done_tx, done) = channel();
    let shared = r.shared.clone();
    thread::spawn(move || {
        let ticked = commands::tick(&shared, 20).len();
        let second = commands::unlock(&shared, "correct horse".into(), None);
        done_tx.send((ticked, second)).unwrap();
    });
    let (ticked, second) = done
        .recv_timeout(Duration::from_secs(5))
        .expect("the machine was held during key derivation");
    assert_eq!(ticked, 1);
    assert_eq!(second, Err("already unlocking".to_string()));

    go.send(()).unwrap();
    assert_eq!(unlocking.join().unwrap(), Ok(()));
    assert!(commands::contacts(&r.shared).is_ok());
}

#[test]
fn contacts_are_listed_once_unlocked_with_their_safety_numbers() {
    let (m, _) = fake(Lock::Unlocked);
    let listed = serde_json::to_value(commands::contacts(&m).unwrap()).unwrap();
    let safety = safety_number(&me(), &marisol().who);
    assert_eq!(safety.len(), 60 + 11);
    assert_eq!(
        listed,
        json!([{ "id": 1, "name": "Marisol Ortega", "checked": true, "safety": safety }])
    );

    let (locked, _) = fake(Lock::Locked);
    assert_eq!(commands::contacts(&locked).unwrap_err(), "locked");
    let (fresh, _) = fake(Lock::NoStore);
    assert_eq!(commands::contacts(&fresh).unwrap_err(), "no store yet");
}

#[test]
fn conversation_is_the_stored_messages_shaped_as_message_events() {
    let (m, _) = fake(Lock::Unlocked);
    let (_, event) = bridge_event(&NodeEvent::Message(stored("Bring the radio.")));
    assert_eq!(commands::conversation(&m, 1), Ok(vec![event]));
    assert_eq!(commands::conversation(&m, 5), Ok(vec![]));
    let (locked, _) = fake(Lock::Locked);
    assert_eq!(
        commands::conversation(&locked, 1),
        Err("locked".to_string())
    );
}

#[test]
fn broadcasts_are_the_kept_ones_shaped_as_broadcast_events_with_their_state() {
    let (m, _) = fake(Lock::Unlocked);
    assert_eq!(
        commands::broadcasts(&m),
        Ok(vec![
            json!({ "id": ID.to_vec(), "outgoing": true, "signer": { "sign": vec![1u8; 32] },
                    "text": "Water at the church.", "verified": true, "hops": 0,
                    "at_min": 29_000_002, "state": "waiting" }),
            json!({ "id": vec![4u8; 16], "outgoing": false, "signer": { "sign": vec![7u8; 32] },
                    "text": "Road north is open.", "verified": false, "hops": 2,
                    "at_min": 29_000_003, "state": "sent" }),
        ])
    );
    let (locked, _) = fake(Lock::Locked);
    assert_eq!(commands::broadcasts(&locked), Err("locked".to_string()));
}

#[test]
fn machine_is_named_machine_no_1_with_its_public_key() {
    let (m, _) = fake(Lock::Unlocked);
    let view = serde_json::to_value(commands::machine(&m).unwrap()).unwrap();
    assert_eq!(
        view,
        json!({ "name": "Machine No. 1", "key": "01".repeat(32) + &"02".repeat(32),
                "you": "Ana Lima", "held": 3, "version": env!("CARGO_PKG_VERSION"),
                "broadcast_max": MAX_BROADCAST })
    );
    let (fresh, _) = fake(Lock::NoStore);
    assert!(commands::machine(&fresh).is_err());
}

#[test]
fn a_channel_state_reported_before_the_page_listens_is_kept_for_it() {
    let r = rig(Lock::NoStore, None);
    assert!(commands::channels(&r.shared).is_empty());
    r.script.lock().unwrap().extend([
        Some(vec![NodeEvent::ChannelState {
            channel: Channel::Sound,
            state: "unavailable: no audio device available".to_string(),
        }]),
        Some(vec![NodeEvent::RoomDb(40.0)]),
        Some(vec![NodeEvent::ChannelState {
            channel: Channel::Light,
            state: "unavailable: not in this plan".to_string(),
        }]),
    ]);
    for now in [20, 40, 60] {
        commands::tick(&r.shared, now);
    }
    let kept = serde_json::to_value(commands::channels(&r.shared)).unwrap();
    assert_eq!(
        kept,
        json!([
            { "channel": "sound", "state": "unavailable: no audio device available" },
            { "channel": "light", "state": "unavailable: not in this plan" }
        ])
    );

    // A later state replaces the earlier one.
    r.script
        .lock()
        .unwrap()
        .push_back(Some(vec![NodeEvent::ChannelState {
            channel: Channel::Sound,
            state: "receiving".to_string(),
        }]));
    commands::tick(&r.shared, 80);
    assert_eq!(commands::channels(&r.shared)[0].state, "receiving");
}

#[test]
fn channels_carry_the_rate_each_measures_listening_until_it_reports_otherwise() {
    let r = rig(Lock::Unlocked, None);
    r.rates.lock().unwrap().push((Channel::Sound, 16.1));
    assert_eq!(
        serde_json::to_value(commands::channels(&r.shared)).unwrap(),
        json!([{ "channel": "sound", "state": "listening", "rate": 16.1f32 }])
    );
    r.script.lock().unwrap().push_back(Some(vec![
        NodeEvent::ChannelState {
            channel: Channel::Light,
            state: "unavailable: not in this plan".to_string(),
        },
        NodeEvent::ChannelState {
            channel: Channel::Sound,
            state: "receiving".to_string(),
        },
    ]));
    commands::tick(&r.shared, 20);
    assert_eq!(
        serde_json::to_value(commands::channels(&r.shared)).unwrap(),
        json!([
            { "channel": "light", "state": "unavailable: not in this plan" },
            { "channel": "sound", "state": "receiving", "rate": 16.1f32 }
        ])
    );
}

#[test]
fn channels_carry_the_light_rate_with_no_band() {
    let r = rig(Lock::Unlocked, None);
    r.rates
        .lock()
        .unwrap()
        .extend([(Channel::Sound, 16.1), (Channel::Light, 2290.0)]);
    r.script
        .lock()
        .unwrap()
        .push_back(Some(vec![NodeEvent::ChannelState {
            channel: Channel::Light,
            state: "unavailable: camera permission denied".to_string(),
        }]));
    commands::tick(&r.shared, 20);
    assert_eq!(
        serde_json::to_value(commands::channels(&r.shared)).unwrap(),
        json!([
            { "channel": "light", "state": "unavailable: camera permission denied", "rate": 2290.0f32 },
            { "channel": "sound", "state": "listening", "rate": 16.1f32 }
        ])
    );
}

#[test]
fn a_tick_that_panics_reports_sound_failed_and_the_pump_carries_on() {
    let r = rig(Lock::Unlocked, None);
    r.script
        .lock()
        .unwrap()
        .extend([None, Some(vec![NodeEvent::RoomDb(39.0)])]);
    let events = commands::tick(&r.shared, 20);
    assert_eq!(
        events.iter().map(bridge_event).collect::<Vec<_>>(),
        vec![("channel", json!({ "channel": "sound", "state": "failed" }))]
    );
    assert_eq!(commands::channels(&r.shared)[0].state, "failed");
    assert_eq!(commands::tick(&r.shared, 40).len(), 1);
    assert!(
        commands::contacts(&r.shared).is_ok(),
        "the machine is still usable"
    );
}

#[test]
fn a_tick_that_keeps_panicking_is_reported_once_in_a_while() {
    let r = rig(Lock::Unlocked, None);
    r.script
        .lock()
        .unwrap()
        .extend([None, None, None, Some(vec![NodeEvent::RoomDb(39.0)])]);
    assert_eq!(commands::tick(&r.shared, 20).len(), 1, "the first is reported");
    assert!(commands::tick(&r.shared, 40).is_empty(), "the next is not");
    assert!(commands::tick(&r.shared, 60).is_empty());
    assert_eq!(commands::tick(&r.shared, 80).len(), 1, "and a good tick still is");
}

#[test]
fn node_events_become_the_bridge_events_with_the_payloads_bridge_js_expects() {
    let chunk = NodeEvent::ChunkLanded {
        tag: 0x5a1e,
        index: 1,
        of: 3,
        view: ChunkView {
            plain: vec![72, 105],
            heard: vec![1, 2],
            key: vec![73, 107],
        },
    };
    assert_eq!(
        bridge_event(&chunk),
        (
            "chunk",
            json!({ "tag": 0x5a1e, "index": 1, "of": 3,
                    "view": { "plain": [72, 105], "heard": [1, 2], "key": [73, 107] } })
        )
    );

    let message = NodeEvent::Message(StoredMessage {
        id: ID,
        contact: ContactId(1),
        outgoing: false,
        text: "Bring the radio.".to_string(),
        channel: Channel::Sound,
        at_min: 29_000_000,
        heard: false,
        delivery: Delivery::Sent,
    });
    assert_eq!(
        bridge_event(&message),
        (
            "message",
            json!({ "id": ID.to_vec(), "contact": 1, "outgoing": false, "text": "Bring the radio.",
                    "channel": "sound", "at_min": 29_000_000, "heard": false,
                    "state": "sent" })
        )
    );

    assert_eq!(
        bridge_event(&NodeEvent::Heard {
            id: ID,
            by: ContactId(1)
        }),
        ("heard", json!({ "id": ID.to_vec(), "by": 1 }))
    );

    let who = marisol().who;
    let (sign, dh) = (vec![7u8; 32], vec![9u8; 32]);
    assert_eq!(
        bridge_event(&NodeEvent::Broadcast {
            id: ID,
            signer: who.clone(),
            text: "Road north is open.".to_string(),
            verified: true,
            hops: 1,
            at_min: 29_000_004,
        }),
        (
            "broadcast",
            json!({ "id": ID.to_vec(), "outgoing": false, "signer": { "sign": sign, "dh": dh },
                    "text": "Road north is open.", "verified": true, "hops": 1, "at_min": 29_000_004 })
        )
    );
    assert_eq!(
        bridge_event(&NodeEvent::PairOffer {
            peer: who,
            name: "Marisol".to_string(),
            safety: "12345".to_string()
        }),
        (
            "pairOffer",
            json!({ "peer": { "sign": sign, "dh": dh }, "name": "Marisol", "safety": "12345" })
        )
    );

    let (name, db) = bridge_event(&NodeEvent::RoomDb(36.5));
    assert_eq!((name, db.as_f64()), ("roomDb", Some(36.5)));

    assert_eq!(
        bridge_event(&NodeEvent::ChannelState {
            channel: Channel::Sound,
            state: "receiving".to_string()
        }),
        (
            "channel",
            json!({ "channel": "sound", "state": "receiving" })
        )
    );
    assert_eq!(
        bridge_event(&NodeEvent::Display {
            tag: 0x0d0e,
            bits: vec![0xa5, 0x0f],
            spec_id: 2,
            index: 7,
        }),
        (
            "display",
            json!({ "tag": 0x0d0e, "bits": [0xa5, 0x0f], "spec": 2, "index": 7 })
        )
    );
    assert_eq!(
        bridge_event(&NodeEvent::DisplayStopped { tag: 0x0d0e }),
        ("displayStopped", json!({ "tag": 0x0d0e }))
    );
    assert_eq!(
        bridge_event(&NodeEvent::ChannelState {
            channel: Channel::Light,
            state: bombechat_node::TILT_OR_DIM.to_string()
        }),
        (
            "channel",
            json!({ "channel": "light", "state": "seeing a code: tilt or dim the screen" })
        )
    );
    for (state, word) in [
        (OutboxState::Waiting, "waiting"),
        (OutboxState::Playing, "playing"),
        (OutboxState::Sent, "sent"),
        (OutboxState::Failed, "failed"),
    ] {
        assert_eq!(
            bridge_event(&NodeEvent::Outbox { id: ID, state }),
            ("outbox", json!({ "id": ID.to_vec(), "state": word }))
        );
    }
}

#[test]
fn every_event_name_is_one_bridge_js_listens_for() {
    let bridge = include_str!("../../ui/bridge.js");
    let names = [
        "chunk",
        "message",
        "heard",
        "broadcast",
        "pairOffer",
        "roomDb",
        "channel",
        "outbox",
        "display",
        "displayStopped",
    ];
    let list = bridge
        .lines()
        .find(|l| l.starts_with("const EVENTS"))
        .expect("bridge.js lists its events");
    for name in names {
        assert!(
            list.contains(&format!("'{name}'")),
            "{name} missing from bridge.js"
        );
    }
}

#[test]
fn every_command_name_is_one_bridge_js_invokes() {
    let bridge = include_str!("../../ui/bridge.js");
    for name in commands::NAMES {
        assert!(
            bridge.contains(&format!("invoke('{name}'")),
            "bridge.js does not invoke {name}"
        );
    }
}

#[test]
fn every_command_name_is_registered_with_tauri() {
    let lib = include_str!("../src/lib.rs");
    let start = lib
        .find("generate_handler![")
        .expect("lib.rs registers its commands")
        + "generate_handler![".len();
    let end = start + lib[start..].find(']').expect("the list ends");
    let mut registered: Vec<&str> = lib[start..end]
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect();
    let mut names = commands::NAMES.to_vec();
    registered.sort_unstable();
    names.sort_unstable();
    assert_eq!(registered, names);
}

#[test]
fn settings_that_wait_for_relay_are_refused_and_say_what_they_wait_for() {
    let (m, calls) = fake(Lock::Unlocked);
    for (id, name, note) in commands::COMING {
        let why = commands::set_setting(&m, id.into(), 0).unwrap_err();
        assert_eq!(why, format!("{name} {note}"));
        assert!(why.starts_with(char::is_uppercase), "{why}: a sentence");
        assert_eq!(note, "comes with relay");
    }
    assert!(
        calls.lock().unwrap().settings.is_empty(),
        "none reached the machine"
    );

    let notes = serde_json::to_value(commands::setting_notes(&m)).unwrap();
    for id in ["carry", "hops", "hold"] {
        assert_eq!(notes[id], json!({ "note": "comes with relay" }), "{id}");
    }
}

#[test]
fn the_light_switch_and_flash_speed_are_live_settings() {
    let (m, calls) = fake(Lock::Locked);
    let notes = serde_json::to_value(commands::setting_notes(&m)).unwrap();
    assert!(notes.get("light").is_none() && notes.get("flash").is_none());
    assert_eq!(commands::settings(&m)["light"], 0, "Off until turned");
    assert_eq!(commands::settings(&m)["flash"], 1, "Normal");
    commands::set_setting(&m, "light".into(), 2).unwrap();
    commands::set_setting(&m, "flash".into(), 0).unwrap();
    assert!(commands::set_setting(&m, "light".into(), 3).is_err());
    assert!(commands::set_setting(&m, "flash".into(), 3).is_err());
    assert_eq!(
        calls.lock().unwrap().settings,
        vec![("light".to_string(), 2), ("flash".to_string(), 0)]
    );
    let all = commands::settings(&m);
    assert_eq!((all["light"], all["flash"]), (2, 0), "read back as kept");
}

#[test]
fn a_value_an_older_build_kept_for_a_waiting_setting_is_not_reported_as_working() {
    let (m, calls) = fake(Lock::Unlocked);
    {
        let mut c = calls.lock().unwrap();
        c.saved.insert("carry".into(), 0);
        c.saved.insert("hops".into(), 6);
        c.saved.insert("inperson".into(), 0);
    }
    let all = commands::settings(&m);
    assert_eq!((all["carry"], all["hops"]), (1, 3), "their defaults");
    assert_eq!(all["inperson"], 1, "always on");
}

#[test]
fn new_contacts_always_need_a_key_check_on_this_build() {
    let (m, calls) = fake(Lock::Unlocked);
    assert_eq!(
        commands::set_setting(&m, "inperson".into(), 0),
        Err("On this build every new contact needs a key check".to_string())
    );
    assert_eq!(
        commands::set_setting(&m, "inperson".into(), 1),
        Err("On this build every new contact needs a key check".to_string())
    );
    assert!(calls.lock().unwrap().settings.is_empty());
    assert_eq!(
        serde_json::to_value(commands::setting_notes(&m)).unwrap()["inperson"],
        json!({ "note": "always, on this build", "value": 1 })
    );
}

#[test]
fn the_lock_can_come_off_only_where_there_is_a_keychain() {
    let (m, calls) = fake(Lock::Unlocked);
    let notes = serde_json::to_value(commands::setting_notes(&m)).unwrap();
    if cfg!(target_os = "macos") {
        assert_eq!(commands::set_setting(&m, "lock".into(), 0), Ok(()));
        assert_eq!(
            calls.lock().unwrap().settings,
            vec![("lock".to_string(), 0)]
        );
        assert!(notes.get("lock").is_none());
        assert_eq!(commands::settings(&m)["lock"], 0);
    } else {
        assert_eq!(
            commands::set_setting(&m, "lock".into(), 0),
            Err("The Keychain is only on macOS, so the lock stays on".to_string())
        );
        assert_eq!(
            notes["lock"],
            json!({ "note": "Keychain only on macOS", "value": 1 })
        );
        assert_eq!(commands::settings(&m)["lock"], 1);
    }
}

#[test]
fn a_setting_the_machine_refuses_comes_back_as_the_reason() {
    let (m, calls) = fake(Lock::Locked);
    calls.lock().unwrap().refuse = Some("Unlock first, then the lock can come off".into());
    assert_eq!(
        commands::set_setting(&m, "volume".into(), 1),
        Err("Unlock first, then the lock can come off".to_string())
    );
}

#[test]
fn the_simulator_holds_the_same_settings_with_the_same_notes() {
    let sim = include_str!("../../ui/simulator.js");
    let line = sim
        .lines()
        .find(|l| l.starts_with("const NOTES = {"))
        .expect("simulator.js has NOTES");
    let (m, _) = fake(Lock::Unlocked);
    let mut want: Vec<String> = commands::setting_notes(&m)
        .iter()
        .filter(|(id, _)| **id != "lock")
        .map(|(id, n)| match n.value {
            Some(v) => format!("{id}:{{note:'{}',value:{v}}}", n.note),
            None => format!("{id}:{{note:'{}'}}", n.note),
        })
        .collect();
    want.sort();
    let body = line
        .trim_start_matches("const NOTES = {")
        .trim_end_matches("};")
        .replace(' ', "");
    let mut have: Vec<String> = body
        .split("},")
        .map(|e| {
            let e = e.trim_end_matches('}');
            format!("{e}}}")
        })
        .collect();
    have.sort();
    let want: Vec<String> = want.iter().map(|w| w.replace(' ', "")).collect();
    assert_eq!(have, want);
}

/// A `const NAME = [a, b, c];` line of simulator.js, as numbers.
fn simulator_list(name: &str) -> Vec<f64> {
    let sim = include_str!("../../ui/simulator.js");
    let start = format!("const {name} = [");
    sim.lines()
        .find(|l| l.starts_with(&start))
        .unwrap_or_else(|| panic!("simulator.js has no {name}"))
        .trim_start_matches(start.as_str())
        .trim_end_matches("];")
        .split(',')
        .map(|n| n.trim().parse().expect("a number"))
        .collect()
}

#[test]
fn the_simulator_flashes_at_the_presets_rates_and_grid_sizes() {
    use bombechat_light::CodeSpec;
    let presets = CodeSpec::presets();
    let rates: Vec<f64> = presets
        .iter()
        .map(|p| {
            (p.packets_per_frame() * p.symbol_size as usize) as f64
                * f64::from(p.frames_per_second())
        })
        .collect();
    assert_eq!(simulator_list("LIGHT_RATES"), rates);
    let cells: Vec<f64> = presets.iter().map(|p| f64::from(p.cells)).collect();
    assert_eq!(simulator_list("LIGHT_CELLS"), cells);
    let fps: Vec<f64> = presets
        .iter()
        .map(|p| f64::from(p.frames_per_second()))
        .collect();
    assert_eq!(simulator_list("LIGHT_FPS"), fps);
}

#[test]
fn the_simulator_refuses_light_as_the_node_does() {
    let sim = include_str!("../../ui/simulator.js");
    let line = sim
        .lines()
        .find(|l| l.starts_with("const LIGHT_SAYS = ["))
        .expect("simulator.js has LIGHT_SAYS");
    for say in [bombechat_node::LIGHT_OFF, bombechat_node::READ_ONLY] {
        assert!(line.contains(&format!("'{say}'")), "{say}");
    }
}
