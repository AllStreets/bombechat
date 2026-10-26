//! The Tauri commands and event payloads, against a fake machine: no
//! audio, no window, no store on disk.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use bombechat_app::commands::{self, bridge_event, Lock, Machine, Opened, Shared, Unlocking};
use bombechat_app::edition;
use bombechat_core::{safety_number, ChunkView, PublicIdentity};
use bombechat_node::{
    max_private_text, NodeEvent, OutboxState, RequestState, WaitingRequest, MAX_BROADCAST,
    MEET_FIRST, NOT_IN_RANGE, NOT_SENT, NOT_STORED, REQUEST_GONE,
};
use bombechat_store::{Channel, Contact, ContactId, Delivery, StoredBroadcast, StoredMessage};
use serde_json::json;

/// What the fake was asked to do.
#[derive(Default)]
struct Calls {
    sent: Vec<(Vec<ContactId>, String, Channel)>,
    broadcasts: Vec<String>,
    /// The channel each broadcast went on.
    broadcast_on: Vec<Channel>,
    /// Each pairing started: the name it announces, and the channel.
    pairings: Vec<(String, Channel)>,
    /// How many times pairing was stopped.
    stopped: usize,
    /// How many times pairing was finished (their hello in).
    finished: usize,
    /// Each self-test started, by channel.
    self_tests: Vec<Channel>,
    /// How many times a self-test was stopped.
    self_tests_stopped: usize,
    /// What the page asked of the eyepiece, in turn.
    eyepiece: Vec<bool>,
    confirmed: Vec<(PublicIdentity, String)>,
    settings: Vec<(String, usize)>,
    /// What the fake's settings file holds, as `settings` reads it back.
    saved: HashMap<String, usize>,
    /// When set, every setting is refused with this.
    refuse: Option<String>,
    passphrases: Arc<Mutex<Vec<String>>>,
    /// The name each unlock was given.
    names: Arc<Mutex<Vec<String>>>,
    /// Requests to connect waiting; each accepted (with the name) or
    /// declined, and each stranger asked.
    waiting: Vec<WaitingRequest>,
    accepted: Vec<(u64, String)>,
    declined: Vec<u64>,
    asked: Vec<String>,
    /// New contacts need a key check: accepting and asking are refused.
    in_person: bool,
    /// What each channel's switch alone says.
    steady: Vec<(Channel, String)>,
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
    fn start_pairing(&mut self, my_name: &str, channel: Channel) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .pairings
            .push((my_name.to_string(), channel));
        Ok(())
    }
    fn stop_pairing(&mut self) {
        self.calls.lock().unwrap().stopped += 1;
    }
    fn finish_pairing(&mut self) {
        self.calls.lock().unwrap().finished += 1;
    }
    fn start_self_test(&mut self, channel: Channel) -> Result<(), String> {
        self.calls.lock().unwrap().self_tests.push(channel);
        if channel == Channel::Light {
            Err("not sent: light is off".to_string())
        } else {
            Ok(())
        }
    }
    fn stop_self_test(&mut self) {
        self.calls.lock().unwrap().self_tests_stopped += 1;
    }
    fn set_eyepiece(&mut self, on: bool) {
        self.calls.lock().unwrap().eyepiece.push(on);
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
                channel: Channel::Sound,
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
                channel: Channel::Light,
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
    fn steady_states(&self) -> Vec<(Channel, String)> {
        self.calls.lock().unwrap().steady.clone()
    }
    fn requests(&self) -> Vec<WaitingRequest> {
        self.calls.lock().unwrap().waiting.clone()
    }
    fn accept_request(&mut self, id: u64, name: &str) -> Result<ContactId, String> {
        let mut c = self.calls.lock().unwrap();
        if c.in_person {
            return Err(MEET_FIRST.to_string());
        }
        let at = c
            .waiting
            .iter()
            .position(|w| w.id == id)
            .ok_or(REQUEST_GONE)?;
        c.waiting.remove(at);
        c.accepted.push((id, name.to_string()));
        Ok(ContactId(5))
    }
    fn decline_request(&mut self, id: u64) {
        let mut c = self.calls.lock().unwrap();
        c.waiting.retain(|w| w.id != id);
        c.declined.push(id);
    }
    fn send_request(&mut self, label: &str) -> Result<(), String> {
        let mut c = self.calls.lock().unwrap();
        if c.in_person {
            return Err(MEET_FIRST.to_string());
        }
        if label != "K7MX" {
            return Err(NOT_IN_RANGE.to_string());
        }
        c.asked.push(label.to_string());
        Ok(())
    }
    fn strangers(&self) -> Vec<String> {
        vec!["K7MX".to_string()]
    }
    fn peers(&self) -> (Vec<ContactId>, usize) {
        (vec![ContactId(1)], 2)
    }
    fn unconfirmed(&self) -> Vec<ContactId> {
        vec![ContactId(5)]
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
fn stop_and_finish_pairing_reach_the_machine() {
    let (m, calls) = fake(Lock::Unlocked);
    commands::pair(&m, "light".into()).unwrap();
    commands::stop_pairing(&m);
    assert_eq!(calls.lock().unwrap().stopped, 1);
    commands::pair(&m, "light".into()).unwrap();
    commands::finish_pairing(&m);
    let calls = calls.lock().unwrap();
    assert_eq!((calls.stopped, calls.finished), (1, 1));
}

#[test]
fn a_self_test_reaches_the_machine_and_is_refused_as_pairing_is() {
    let (m, calls) = fake(Lock::Unlocked);
    commands::self_test(&m, "sound".into()).unwrap();
    assert_eq!(
        commands::self_test(&m, "light".into()),
        Err("not sent: light is off".to_string()),
        "the machine's refusal, as it gives it"
    );
    assert_eq!(
        commands::self_test(&m, "bluetooth".into()),
        Err(bombechat_node::NOT_IN_THIS_PLAN.to_string())
    );
    assert_eq!(
        commands::self_test(&m, "radio".into()),
        Err("no such channel: radio".to_string())
    );
    commands::stop_self_test(&m);
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.self_tests, vec![Channel::Sound, Channel::Light]);
        assert_eq!(calls.self_tests_stopped, 1);
    }

    let (locked, calls) = fake(Lock::Locked);
    assert_eq!(
        commands::self_test(&locked, "sound".into()),
        Err("locked".to_string())
    );
    assert!(calls.lock().unwrap().self_tests.is_empty());
}

#[test]
fn a_self_test_reaches_the_page_with_its_rate_and_band() {
    use bombechat_modem::Band;
    use bombechat_node::SelfTestState;
    let event = |state| NodeEvent::SelfTest {
        tag: 0x0abc,
        channel: Channel::Sound,
        state,
    };
    for (state, word) in [
        (SelfTestState::Waiting, "waiting"),
        (SelfTestState::Playing, "playing"),
        (SelfTestState::NotHeard, "unheard"),
        (SelfTestState::NotPlayed, "unplayed"),
        (SelfTestState::Stopped, "stopped"),
        (SelfTestState::RoomNeverQuiet, "noquiet"),
    ] {
        assert_eq!(
            bridge_event(&event(state)),
            (
                "selfTest",
                json!({ "tag": 0x0abc, "channel": "sound", "state": word })
            )
        );
    }
    assert_eq!(
        bridge_event(&event(SelfTestState::Heard {
            text: "The machine hears itself.".into(),
            bytes: 379,
            ms: Some(23_500),
            band: Some(Band::NearSilent),
        })),
        (
            "selfTest",
            json!({ "tag": 0x0abc, "channel": "sound", "state": "heard",
                    "text": "The machine hears itself.", "bytes": 379, "ms": 23_500,
                    "rate": 16.1, "band": "near silent" })
        )
    );
    // by light there is no band; a time not measured (or measured as
    // nothing) makes no rate at all, rather than an enormous one
    for ms in [None, Some(0)] {
        let (_, light) = bridge_event(&NodeEvent::SelfTest {
            tag: 1,
            channel: Channel::Light,
            state: SelfTestState::Heard {
                text: "x".into(),
                bytes: 300,
                ms,
                band: None,
            },
        });
        assert_eq!(
            light,
            json!({ "tag": 1, "channel": "light", "state": "heard", "text": "x", "bytes": 300 })
        );
    }
}

#[test]
fn the_page_and_the_simulator_give_a_self_test_the_nodes_time_and_text() {
    let sim = include_str!("../../ui/simulator.js");
    assert!(
        sim.contains(&format!(
            "const SELF_TEST_MS = {};",
            bombechat_node::SELF_TEST_MS
        )),
        "simulator.js's SELF_TEST_MS is not the node's"
    );
    assert!(
        sim.contains(&format!(
            "const SELF_TEST_TEXT = '{}';",
            bombechat_node::SELF_TEST_TEXT
        )),
        "simulator.js's SELF_TEST_TEXT is not the node's"
    );
    let page = include_str!("../../ui/bombechat.html");
    assert!(
        page.contains(&format!(
            "const SELF_TEST_TEXT='{}';",
            bombechat_node::SELF_TEST_TEXT
        )),
        "bombechat.html's SELF_TEST_TEXT is not the node's"
    );
}

#[test]
fn the_eyepiece_is_asked_for_and_let_go_by_the_page() {
    let (m, calls) = fake(Lock::Unlocked);
    commands::eyepiece(&m, true);
    commands::eyepiece(&m, false);
    assert_eq!(calls.lock().unwrap().eyepiece, vec![true, false]);
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
fn pair_announces_your_name_on_the_channel_asked_and_refuses_while_locked() {
    let (m, calls) = fake(Lock::Unlocked);
    commands::pair(&m, "sound".into()).unwrap();
    commands::pair(&m, "light".into()).unwrap();
    assert_eq!(
        calls.lock().unwrap().pairings,
        vec![
            ("Ana Lima".to_string(), Channel::Sound),
            ("Ana Lima".to_string(), Channel::Light)
        ]
    );
    assert_eq!(
        commands::pair(&m, "radio".into()),
        Err("no such channel: radio".to_string())
    );
    assert_eq!(
        commands::pair(&m, "bluetooth".into()),
        Err(bombechat_node::NOT_IN_THIS_PLAN.to_string())
    );
    assert_eq!(calls.lock().unwrap().pairings.len(), 2);

    let (locked, calls) = fake(Lock::Locked);
    assert_eq!(
        commands::pair(&locked, "light".into()),
        Err("locked".to_string())
    );
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
    // the page's own rows, and the ones this build adds (edition.js)
    let page = concat!(
        include_str!("../../ui/settings.html"),
        include_str!("../../ui/edition.js")
    );
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
    // the simulator takes the rows this build adds (edition.js) as the
    // settings page has them, which the test above checks
    let added = |f: fn(&(&str, usize, usize)) -> String| -> Vec<String> {
        edition::SETTINGS.iter().map(f).collect()
    };
    let with = |mut v: Vec<String>, more: Vec<String>| {
        v.extend(more);
        v.sort();
        v
    };
    assert_eq!(
        with(
            table("DEFAULTS"),
            added(|(id, _, default)| format!("{id}:{default}"))
        ),
        want(|(id, _, default)| format!("{id}:{default}"))
    );
    assert_eq!(
        with(
            table("COUNTS"),
            added(|(id, count, _)| format!("{id}:{count}"))
        ),
        want(|(id, count, _)| format!("{id}:{count}"))
    );
    let rows = include_str!("../../ui/edition.js");
    assert!(
        rows.contains("export const SETTING_ROWS = [")
            && sim.contains("for (const r of SETTING_ROWS)"),
        "the simulator takes edition.js's rows"
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
                "you": "Ana Lima", "held": 3, "version": "0.5.0", "commit": env!("BOMBECHAT_COMMIT"),
                "broadcast_max": MAX_BROADCAST, "reply_max": max_private_text(1) })
    );
    // the commit names this build: a short hash, marked + when built from uncommitted changes
    let commit = env!("BOMBECHAT_COMMIT");
    let hash = commit.strip_suffix('+').unwrap_or(commit);
    assert!(
        commit == "unknown" || (hash.len() >= 7 && hash.bytes().all(|b| b.is_ascii_hexdigit())),
        "{commit}"
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
    assert_eq!(
        commands::tick(&r.shared, 20).len(),
        1,
        "the first is reported"
    );
    assert!(commands::tick(&r.shared, 40).is_empty(), "the next is not");
    assert!(commands::tick(&r.shared, 60).is_empty());
    assert_eq!(
        commands::tick(&r.shared, 80).len(),
        1,
        "and a good tick still is"
    );
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
        channel: Channel::Light,
    };
    assert_eq!(
        bridge_event(&chunk),
        (
            "chunk",
            json!({ "tag": 0x5a1e, "index": 1, "of": 3, "channel": "light",
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
            safety: "12345".to_string(),
            channel: Channel::Light
        }),
        (
            "pairOffer",
            json!({ "peer": { "sign": sign, "dh": dh }, "name": "Marisol", "safety": "12345", "channel": "light" })
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
            hello: true,
            test: false,
        }),
        (
            "display",
            json!({ "tag": 0x0d0e, "bits": [0xa5, 0x0f], "spec": 2, "index": 7, "hello": true, "test": false })
        )
    );
    assert_eq!(
        bridge_event(&NodeEvent::DisplayStopped { tag: 0x0d0e }),
        ("displayStopped", json!({ "tag": 0x0d0e }))
    );
    assert_eq!(
        bridge_event(&NodeEvent::Eyepiece {
            width: 2,
            height: 1,
            pixels: vec![12, 240],
        }),
        (
            "eyepiece",
            json!({ "width": 2, "height": 1, "pixels": [12, 240] })
        )
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
        "eyepiece",
        "selfTest",
        "request",
        "requestGone",
        "requested",
        "peers",
        "contactConfirmed",
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
    if edition::ONLY_IN_PERSON {
        assert_eq!(all["inperson"], 1, "always on");
    } else {
        assert_eq!(all["inperson"], 0, "the person's own choice");
    }
}

/// New contacts need a key check: held On where checking keys in person
/// is the only way to add a contact; elsewhere the person's choice, On
/// until they change it.
#[test]
fn new_contacts_need_a_key_check_always_where_there_is_no_other_way() {
    let (m, calls) = fake(Lock::Unlocked);
    assert_eq!(commands::settings(&m)["inperson"], 1, "On by default");
    if edition::ONLY_IN_PERSON {
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
    } else {
        commands::set_setting(&m, "inperson".into(), 0).unwrap();
        assert_eq!(
            calls.lock().unwrap().settings,
            vec![("inperson".to_string(), 0)]
        );
        assert!(!commands::setting_notes(&m).contains_key("inperson"));
        assert_eq!(commands::settings(&m)["inperson"], 0);
    }
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
        // the simulator lets go of the key check's note where the person
        // may choose, as setting_notes does
        .filter(|e| edition::ONLY_IN_PERSON || !e.starts_with("inperson:"))
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

#[test]
fn the_page_and_the_simulator_know_the_cameras_hint_as_the_node_says_it() {
    let tilt = format!("'{}'", bombechat_node::TILT_OR_DIM);
    let sim = include_str!("../../ui/simulator.js");
    assert!(
        sim.lines()
            .any(|l| l.starts_with("const LIGHT_TILT = ") && l.contains(&tilt)),
        "simulator.js LIGHT_TILT"
    );
    let page = include_str!("../../ui/bombechat.html");
    assert!(
        page.lines()
            .any(|l| l.starts_with("const TILT_OR_DIM=") && l.contains(&tilt)),
        "bombechat.html TILT_OR_DIM"
    );
}

#[test]
fn the_simulator_knows_the_longest_reply_as_the_node_does() {
    let sim = include_str!("../../ui/simulator.js");
    assert!(
        sim.lines()
            .any(|l| l == format!("const REPLY_MAX = {};", max_private_text(1))),
        "simulator.js REPLY_MAX"
    );
    let check = include_str!("../../../tools/check.cjs");
    assert!(check.contains(&format!("reply_max: {}", max_private_text(1))));
}

#[test]
fn the_page_and_the_simulator_end_pairing_when_the_node_does() {
    let cap = bombechat_node::PAIRING_CAP_MS.to_string();
    let page = include_str!("../../ui/bombechat.html");
    assert!(
        page.contains(&format!(
            "const PAIR_CAP=Q.has('paircap')?+Q.get('paircap'):{cap};"
        )),
        "bombechat.html's PAIR_CAP is not the node's PAIRING_CAP_MS ({cap})"
    );
    let sim = include_str!("../../ui/simulator.js");
    assert!(
        sim.contains(&format!("const PAIRING_CAP_MS = {cap};")),
        "simulator.js's PAIRING_CAP_MS is not the node's ({cap})"
    );
}

#[test]
fn requests_to_connect_reach_the_machine_and_come_back_as_the_page_reads_them() {
    // nothing of requests or who is near until the store is open
    let (m, calls) = fake(Lock::Locked);
    let locked = Some("locked".to_string());
    assert_eq!(commands::requests(&m).err(), locked);
    assert_eq!(commands::unconfirmed(&m).err(), locked);
    assert_eq!(commands::strangers(&m).err(), locked);
    assert_eq!(commands::peers(&m).err(), locked);
    assert_eq!(commands::accept_request(&m, 3, "Ines".into()).err(), locked);
    assert_eq!(commands::decline_request(&m, 3).err(), locked);
    assert_eq!(commands::send_request(&m, "K7MX".into()).err(), locked);
    {
        let c = calls.lock().unwrap();
        assert!(c.accepted.is_empty() && c.declined.is_empty() && c.asked.is_empty());
    }
    let (m, calls) = fake(Lock::Unlocked);
    calls.lock().unwrap().waiting.push(WaitingRequest {
        id: 3,
        name: "Ines".into(),
        fingerprint: [0xc4, 0x1d, 0, 1, 2, 3, 4, 5],
        safety: "12345 67890".into(),
    });
    assert_eq!(
        serde_json::to_value(commands::requests(&m).unwrap()).unwrap(),
        json!([{ "id": 3, "name": "Ines", "fingerprint": [0xc4, 0x1d, 0, 1, 2, 3, 4, 5],
                 "safety": "12345 67890" }]),
        "shaped as the request event"
    );
    // the name as the person corrected it, trimmed and cut as a pairing's
    assert_eq!(
        commands::accept_request(&m, 3, "   ".into()),
        Err("Write their name first".to_string())
    );
    let long = format!("  Ines {}  ", "x".repeat(60));
    assert_eq!(commands::accept_request(&m, 3, long), Ok(5));
    let (id, name) = calls.lock().unwrap().accepted[0].clone();
    assert_eq!((id, name.chars().count()), (3, 40));
    assert!(name.starts_with("Ines x"));
    assert_eq!(
        commands::accept_request(&m, 3, "Ines".into()),
        Err(REQUEST_GONE.to_string())
    );
    commands::decline_request(&m, 9).unwrap();
    assert_eq!(calls.lock().unwrap().declined, vec![9]);

    assert_eq!(commands::strangers(&m).unwrap(), vec!["K7MX".to_string()]);
    assert_eq!(commands::send_request(&m, "K7MX".into()), Ok(()));
    assert_eq!(
        commands::send_request(&m, "AAAA".into()),
        Err(NOT_IN_RANGE.to_string())
    );
    assert_eq!(calls.lock().unwrap().asked, vec!["K7MX".to_string()]);
    assert_eq!(
        serde_json::to_value(commands::peers(&m).unwrap()).unwrap(),
        json!({ "contacts": [1], "strangers": 2 })
    );
    assert_eq!(commands::unconfirmed(&m), Ok(vec![5]));

    // with key checks required, both ways refused in so many words
    calls.lock().unwrap().in_person = true;
    calls.lock().unwrap().waiting.push(WaitingRequest {
        id: 4,
        name: "Jo".into(),
        fingerprint: [0; 8],
        safety: String::new(),
    });
    assert_eq!(
        commands::accept_request(&m, 4, "Jo".into()),
        Err(MEET_FIRST.to_string())
    );
    assert_eq!(
        commands::send_request(&m, "K7MX".into()),
        Err(MEET_FIRST.to_string())
    );
    assert_eq!(commands::requests(&m).unwrap().len(), 1, "it stays");
}

#[test]
fn a_channel_its_switch_alone_describes_is_in_channels_until_it_reports() {
    let r = rig(Lock::Unlocked, None);
    r.calls
        .lock()
        .unwrap()
        .steady
        .push((Channel::Bluetooth, "off".into()));
    assert_eq!(
        serde_json::to_value(commands::channels(&r.shared)).unwrap(),
        json!([{ "channel": "bluetooth", "state": "off" }])
    );
    r.script
        .lock()
        .unwrap()
        .push_back(Some(vec![NodeEvent::ChannelState {
            channel: Channel::Bluetooth,
            state: "unavailable: Bluetooth is off in System Settings.".into(),
        }]));
    commands::tick(&r.shared, 20);
    assert_eq!(
        commands::channels(&r.shared)[0].state,
        "unavailable: Bluetooth is off in System Settings.",
        "what it reported, over what its switch says"
    );
}

#[test]
fn request_events_become_the_bridge_events_bridge_js_reads() {
    let cases = [
        (
            NodeEvent::Request {
                id: 3,
                name: "Ines".into(),
                fingerprint: [1, 2, 3, 4, 5, 6, 7, 8],
                safety: "12345".into(),
            },
            "request",
            json!({ "id": 3, "name": "Ines", "fingerprint": [1, 2, 3, 4, 5, 6, 7, 8], "safety": "12345" }),
        ),
        (
            NodeEvent::RequestGone { id: 3 },
            "requestGone",
            json!({ "id": 3 }),
        ),
        (
            NodeEvent::Requested {
                label: "K7MX".into(),
                state: RequestState::Accepted(ContactId(6)),
            },
            "requested",
            json!({ "label": "K7MX", "state": "accepted", "contact": 6 }),
        ),
        (
            NodeEvent::Requested {
                label: "K7MX".into(),
                state: RequestState::Delivered,
            },
            "requested",
            json!({ "label": "K7MX", "state": "delivered", "contact": null }),
        ),
        (
            NodeEvent::Peers {
                contacts_in_range: vec![ContactId(1)],
                strangers_in_range: 2,
            },
            "peers",
            json!({ "contacts": [1], "strangers": 2 }),
        ),
        (
            NodeEvent::ContactConfirmed {
                contact: ContactId(6),
            },
            "contactConfirmed",
            json!({ "contact": 6 }),
        ),
    ];
    for (event, name, payload) in cases {
        assert_eq!(bridge_event(&event), (name, payload));
    }
}
