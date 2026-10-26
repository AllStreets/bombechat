//! The real machine over the fake Bluetooth radio, as the page drives it
//! through the commands: what the Bluetooth switch does and how its
//! channel says it, a radio that can't work and works again, the key
//! check setting, and requests to connect between two machines, with
//! the events the page is handed.
//!
//! Identities and the transports' randomness are seeded, and no test
//! reads a clock (the node seals hellos by the wall clock; the
//! transports' fixed base, a moment in the past, keeps those current).

use std::path::{Path, PathBuf};

use bombechat_app::commands::{self, bridge_event, Shared};
use bombechat_app::edition::{
    Fittings, BLUETOOTH_OFF, CONTACTS_ONLY, OPEN_TO_REQUESTS, WAITING_FOR_PASSPHRASE,
};
use bombechat_app::Device;
use bombechat_bluetooth::{
    BleTransport, FakeRadio, Mode, Room, SeededRng, BLUETOOTH_OFF as SYSTEM_OFF,
    BLUETOOTH_SEND_OFF, REQUESTS_NEED_OPEN,
};
use bombechat_core::Identity;
use bombechat_node::{MEET_FIRST, NOT_IN_RANGE};
use bombechat_store::Store;
use serde_json::{json, Value};
use tempfile::TempDir;

/// The transports' clock reads this Unix time at zero.
const BASE: u64 = 1_700_000_000_000;
const STEP: u64 = 50;
const PASS: &str = "correct horse";

/// A machine with Bluetooth over a fake radio in `room`, its store at
/// `path` (made with a seeded identity the first time), ticked by hand.
struct Bench {
    s: Shared,
    radio: FakeRadio,
    now: u64,
    /// Every event the page was handed, as (name, payload).
    seen: Vec<(&'static str, Value)>,
}

impl Bench {
    fn new(room: &Room, path: &Path, seed: u8, name: &str) -> Bench {
        if !path.exists() {
            let id = Identity::from_secret_bytes(&[seed; 64]);
            drop(Store::create(path, PASS, &id, name).unwrap());
        }
        let radio = room.radio();
        let watch = radio.watch();
        let ble = BleTransport::with_rng(radio, BASE, Box::new(SeededRng::new(u64::from(seed))));
        let fittings = Fittings::bluetooth(ble.handle());
        let device = Device::fitted(
            path.to_path_buf(),
            vec![Box::new(ble)],
            Vec::new(),
            fittings,
        );
        Bench {
            s: Shared::new(Box::new(device)),
            radio: watch,
            now: 0,
            seen: Vec::new(),
        }
    }

    fn unlock(&self) {
        commands::unlock(&self.s, PASS.into(), None).unwrap();
    }

    fn step(&mut self) {
        let events = commands::tick(&self.s, self.now);
        self.seen.extend(events.iter().map(bridge_event));
        self.now += STEP;
    }

    fn set(&self, id: &str, value: usize) {
        commands::set_setting(&self.s, id.into(), value).unwrap();
    }

    /// The Bluetooth channel as `channels` has it.
    fn bluetooth(&self) -> Value {
        let all = serde_json::to_value(commands::channels(&self.s)).unwrap();
        all.as_array()
            .unwrap()
            .iter()
            .find(|c| c["channel"] == "bluetooth")
            .cloned()
            .expect("a Bluetooth channel")
    }

    /// The Bluetooth states the page was handed, in turn, and forget them.
    fn said(&mut self) -> Vec<String> {
        let said = self
            .seen
            .iter()
            .filter(|(n, p)| *n == "channel" && p["channel"] == "bluetooth")
            .map(|(_, p)| p["state"].as_str().unwrap().to_string())
            .collect();
        self.seen.retain(|(n, _)| *n != "channel");
        said
    }

    /// The payloads of the events named `name` the page was handed.
    fn events(&self, name: &str) -> Vec<Value> {
        self.seen
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

/// Tick every bench together for `ms`.
fn run(benches: &mut [&mut Bench], ms: u64) {
    for _ in 0..ms / STEP {
        for b in benches.iter_mut() {
            b.step();
        }
    }
}

fn store(dir: &TempDir, name: &str) -> PathBuf {
    dir.path().join(name)
}

#[test]
fn bluetooth_is_off_until_turned_and_a_persisted_off_never_starts_the_radio() {
    let dir = TempDir::new().unwrap();
    let room = Room::new(1);
    let mut b = Bench::new(&room, &store(&dir, "a.db"), 1, "Ana");
    assert_eq!(commands::settings(&b.s)["bluetooth"], 0, "Off by default");
    assert_eq!(
        b.bluetooth(),
        json!({ "channel": "bluetooth", "state": BLUETOOTH_OFF }),
        "said before any tick, with no rate: none is measured"
    );
    b.unlock();
    run(&mut [&mut b], 2_000);
    assert_eq!(b.radio.starts_and_stops(), (0, 0), "never started");
    assert!(!b.radio.is_on());
    assert_eq!(b.said(), vec![BLUETOOTH_OFF.to_string()]);

    // kept at Off across a launch: still never started
    b.set("bluetooth", 2);
    b.set("bluetooth", 0);
    drop(b);
    let mut b = Bench::new(&room, &store(&dir, "a.db"), 1, "Ana");
    b.unlock();
    run(&mut [&mut b], 1_000);
    assert_eq!(b.radio.starts_and_stops(), (0, 0));
}

#[test]
fn the_switch_moves_the_radio_says_so_and_is_kept() {
    let dir = TempDir::new().unwrap();
    let room = Room::new(2);
    let mut b = Bench::new(&room, &store(&dir, "a.db"), 1, "Ana");
    b.unlock();
    b.step();
    b.said();

    b.set("bluetooth", 1);
    b.step();
    assert_eq!(b.radio.mode(), Some(Mode::Contacts));
    assert_eq!(b.said(), vec![CONTACTS_ONLY.to_string()]);
    assert_eq!(b.bluetooth()["state"], CONTACTS_ONLY);

    b.set("bluetooth", 2);
    b.step();
    assert_eq!(b.radio.mode(), Some(Mode::Open));
    assert_eq!(b.said(), vec![OPEN_TO_REQUESTS.to_string()]);

    b.set("bluetooth", 0);
    b.step();
    assert!(!b.radio.is_on(), "stopped at once");
    assert_eq!(b.said(), vec![BLUETOOTH_OFF.to_string()]);
    assert_eq!(
        commands::set_setting(&b.s, "bluetooth".into(), 3),
        Err("no value 3 for bluetooth".to_string())
    );

    // kept for the next launch, and the radio follows it there
    b.set("bluetooth", 2);
    drop(b);
    let mut b = Bench::new(&room, &store(&dir, "a.db"), 1, "Ana");
    assert_eq!(commands::settings(&b.s)["bluetooth"], 2);
    assert!(!b.radio.is_on(), "not before the store is open: no keys");
    b.unlock();
    b.step();
    assert_eq!(b.radio.mode(), Some(Mode::Open));
}

/// While the store is locked the radio has no keys and stays idle, so the
/// channel says it waits for the passphrase, not the switch's word: in
/// `channels` and in the event, before and after the unlock.
#[test]
fn while_locked_the_channel_says_it_waits_for_the_passphrase() {
    let dir = TempDir::new().unwrap();
    let room = Room::new(10);
    let b = Bench::new(&room, &store(&dir, "a.db"), 1, "Ana");
    b.set("bluetooth", 2);
    drop(b);
    let mut b = Bench::new(&room, &store(&dir, "a.db"), 1, "Ana");
    assert_eq!(
        b.bluetooth(),
        json!({ "channel": "bluetooth", "state": WAITING_FOR_PASSPHRASE }),
        "before any tick"
    );
    run(&mut [&mut b], 1_000);
    assert!(!b.radio.is_on());
    let said = b.said();
    assert!(!said.is_empty(), "said on the first tick");
    assert!(
        said.iter().all(|s| s == WAITING_FOR_PASSPHRASE),
        "never the switch's word while locked: {said:?}"
    );
    assert_eq!(b.bluetooth()["state"], WAITING_FOR_PASSPHRASE);
    // turned while locked: still waiting; Off is Off, locked or not
    b.set("bluetooth", 1);
    b.step();
    assert_eq!(b.said(), vec![WAITING_FOR_PASSPHRASE.to_string()]);
    b.set("bluetooth", 0);
    b.step();
    assert_eq!(b.said(), vec![BLUETOOTH_OFF.to_string()]);
    assert_eq!(b.bluetooth()["state"], BLUETOOTH_OFF);
    b.set("bluetooth", 2);
    b.step();
    assert_eq!(b.said(), vec![WAITING_FOR_PASSPHRASE.to_string()]);

    b.unlock();
    run(&mut [&mut b], 200);
    assert_eq!(b.radio.mode(), Some(Mode::Open), "started once unlocked");
    assert_eq!(b.said(), vec![OPEN_TO_REQUESTS.to_string()]);
    assert_eq!(b.bluetooth()["state"], OPEN_TO_REQUESTS);
}

#[test]
fn a_radio_that_cannot_work_says_why_and_says_when_it_works_again() {
    let dir = TempDir::new().unwrap();
    let room = Room::new(3);
    let mut b = Bench::new(&room, &store(&dir, "a.db"), 1, "Ana");
    b.unlock();
    b.set("bluetooth", 1);
    run(&mut [&mut b], 200);
    b.said();
    let why = format!("unavailable: {SYSTEM_OFF}");

    b.radio.say_unavailable(Some(SYSTEM_OFF));
    run(&mut [&mut b], 200);
    assert_eq!(b.said(), vec![why.clone()]);
    assert_eq!(b.bluetooth()["state"], why.as_str());
    // turned to Open meanwhile: still unavailable, not open
    b.set("bluetooth", 2);
    run(&mut [&mut b], 200);
    assert_eq!(b.said(), vec![why.clone()]);

    b.radio.say_unavailable(None);
    run(&mut [&mut b], 200);
    assert_eq!(
        b.said(),
        vec![OPEN_TO_REQUESTS.to_string()],
        "available again"
    );
    assert_eq!(b.bluetooth()["state"], OPEN_TO_REQUESTS);

    // unavailable, then turned Off: off, whatever the radio said
    b.radio.say_unavailable(Some(SYSTEM_OFF));
    run(&mut [&mut b], 200);
    b.set("bluetooth", 0);
    run(&mut [&mut b], 200);
    assert_eq!(b.said(), vec![why, BLUETOOTH_OFF.to_string()]);
    assert_eq!(b.bluetooth()["state"], BLUETOOTH_OFF);
}

#[test]
fn a_send_by_bluetooth_at_off_is_refused_in_so_many_words() {
    let dir = TempDir::new().unwrap();
    let room = Room::new(4);
    let mut b = Bench::new(&room, &store(&dir, "a.db"), 1, "Ana");
    b.unlock();
    b.step();
    b.said();
    let ben = Identity::from_secret_bytes(&[2; 64]).public();
    let key: String = ben
        .sign
        .iter()
        .chain(ben.dh.iter())
        .map(|x| format!("{x:02x}"))
        .collect();
    let id = commands::confirm_pair(&b.s, key, "Ben".into()).unwrap();
    assert_eq!(
        commands::send(&b.s, vec![id], "hi".into(), "bluetooth".into()).map(|_| ()),
        Ok(()),
        "taken, then refused by the switch"
    );
    run(&mut [&mut b], 200);
    assert_eq!(
        b.said(),
        vec![BLUETOOTH_SEND_OFF.to_string(), BLUETOOTH_OFF.to_string()],
        "why, then the state it is in"
    );
    assert!(b.events("outbox").iter().any(|o| o["state"] == "failed"));
}

#[test]
fn new_contacts_need_a_key_check_is_the_persons_choice_and_on_until_changed() {
    let dir = TempDir::new().unwrap();
    let room = Room::new(5);
    let b = Bench::new(&room, &store(&dir, "a.db"), 1, "Ana");
    assert_eq!(commands::settings(&b.s)["inperson"], 1, "On by default");
    assert!(!commands::setting_notes(&b.s).contains_key("inperson"));
    b.set("inperson", 0);
    drop(b);
    let b = Bench::new(&room, &store(&dir, "a.db"), 1, "Ana");
    assert_eq!(commands::settings(&b.s)["inperson"], 0, "kept");
}

/// Two machines in range, both at Open, each unlocked, Ana with key
/// checks off unless `ana_checks`.
fn pair(room: &Room, dir: &TempDir, ana_checks: bool) -> (Bench, Bench) {
    let mut a = Bench::new(room, &store(dir, "a.db"), 1, "Ana");
    let mut b = Bench::new(room, &store(dir, "b.db"), 2, "Ben");
    for (x, checks) in [(&a, ana_checks), (&b, false)] {
        x.unlock();
        x.set("bluetooth", 2);
        x.set("inperson", usize::from(checks));
    }
    run(&mut [&mut a, &mut b], 1_000);
    (a, b)
}

#[test]
fn a_request_goes_by_label_and_an_accept_makes_both_contacts() {
    let dir = TempDir::new().unwrap();
    let room = Room::new(6);
    let (mut a, mut b) = pair(&room, &dir, false);
    let labels = commands::strangers(&a.s).unwrap();
    assert_eq!(labels.len(), 1, "Ben, by a label");
    assert_eq!(
        a.events("peers").last(),
        Some(&json!({ "contacts": [], "strangers": 1 }))
    );
    assert_eq!(
        commands::send_request(&a.s, "AAAA".into()),
        Err(NOT_IN_RANGE.to_string())
    );
    commands::send_request(&a.s, labels[0].clone()).unwrap();
    run(&mut [&mut a, &mut b], 2_000);
    assert_eq!(
        a.events("requested"),
        vec![json!({ "label": labels[0], "state": "delivered", "contact": null })]
    );
    let shown = b.events("request");
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0]["name"], "Ana");
    assert_eq!(shown[0]["fingerprint"].as_array().unwrap().len(), 8);
    assert_eq!(
        serde_json::to_value(commands::requests(&b.s).unwrap()).unwrap(),
        json!(shown),
        "listed for a page that opens late, as the event said"
    );
    assert!(commands::contacts(&b.s).unwrap().is_empty(), "nothing kept");

    let id = shown[0]["id"].as_u64().unwrap();
    let ana = commands::accept_request(&b.s, id, " Ana from the bus ".into()).unwrap();
    assert_eq!(commands::unconfirmed(&b.s), Ok(vec![ana]));
    assert!(commands::requests(&b.s).unwrap().is_empty());
    run(&mut [&mut a, &mut b], 10_000);

    let bens = commands::contacts(&a.s).unwrap();
    assert_eq!((bens[0].name.as_str(), bens[0].checked), ("Ben", false));
    assert_eq!(
        a.events("requested").last(),
        Some(&json!({ "label": labels[0], "state": "accepted", "contact": bens[0].id }))
    );
    let anns = commands::contacts(&b.s).unwrap();
    assert_eq!(
        (anns[0].id, anns[0].name.as_str(), anns[0].checked),
        (ana, "Ana from the bus", false)
    );
    assert_eq!(
        b.events("contactConfirmed"),
        vec![json!({ "contact": ana })],
        "confirmed once Ana's own answer shows she took Ben"
    );
    assert_eq!(commands::unconfirmed(&b.s), Ok(vec![]));
    // Ana in range; her advert still counts among strangers seen lately
    // (an upper bound, for fifteen minutes)
    assert_eq!(
        serde_json::to_value(commands::peers(&b.s).unwrap()).unwrap()["contacts"],
        json!([ana])
    );
    assert_eq!(
        serde_json::to_value(commands::peers(&a.s).unwrap()).unwrap()["contacts"],
        json!([bens[0].id])
    );
    assert!(
        commands::strangers(&a.s).unwrap().is_empty(),
        "Ben is no stranger now"
    );

    // and they talk by Bluetooth
    commands::send(
        &a.s,
        vec![bens[0].id],
        "Road north is open.".into(),
        "bluetooth".into(),
    )
    .unwrap();
    run(&mut [&mut a, &mut b], 3_000);
    assert!(b
        .events("message")
        .iter()
        .any(|m| m["text"] == "Road north is open."
            && m["contact"] == ana
            && m["channel"] == "bluetooth"));
    assert!(a.events("heard").iter().any(|h| h["by"] == bens[0].id));
}

#[test]
fn with_key_checks_on_a_request_is_shown_but_accepting_and_asking_are_refused() {
    let dir = TempDir::new().unwrap();
    let room = Room::new(7);
    let (mut a, mut b) = pair(&room, &dir, false);
    b.set("inperson", 1);
    let label = commands::strangers(&a.s).unwrap()[0].clone();
    commands::send_request(&a.s, label).unwrap();
    run(&mut [&mut a, &mut b], 2_000);
    let id = b.events("request")[0]["id"].as_u64().unwrap();
    assert_eq!(
        commands::accept_request(&b.s, id, "Ana".into()),
        Err(MEET_FIRST.to_string())
    );
    assert_eq!(
        MEET_FIRST,
        "Meet them and compare keys first: this machine only adds people checked in person."
    );
    assert_eq!(commands::requests(&b.s).unwrap().len(), 1, "it stays");
    let ana_label = commands::strangers(&b.s).unwrap()[0].clone();
    assert_eq!(
        commands::send_request(&b.s, ana_label),
        Err(MEET_FIRST.to_string())
    );
    run(&mut [&mut a, &mut b], 5_000);
    assert!(commands::contacts(&a.s).unwrap().is_empty());
    assert!(commands::contacts(&b.s).unwrap().is_empty());
}

#[test]
fn a_decline_keeps_nothing_and_says_the_request_is_gone() {
    let dir = TempDir::new().unwrap();
    let room = Room::new(8);
    let (mut a, mut b) = pair(&room, &dir, false);
    let label = commands::strangers(&a.s).unwrap()[0].clone();
    commands::send_request(&a.s, label).unwrap();
    run(&mut [&mut a, &mut b], 2_000);
    let id = b.events("request")[0]["id"].as_u64().unwrap();
    commands::decline_request(&b.s, id).unwrap();
    run(&mut [&mut a, &mut b], 2_000);
    assert!(commands::requests(&b.s).unwrap().is_empty());
    assert!(commands::contacts(&b.s).unwrap().is_empty());
    assert!(commands::contacts(&a.s).unwrap().is_empty());
    // the requester is never told: still delivered
    assert_eq!(a.events("requested").len(), 1);
    assert_eq!(a.events("requested")[0]["state"], "delivered");
}

#[test]
fn at_contacts_no_request_goes_and_the_switch_says_why() {
    let dir = TempDir::new().unwrap();
    let room = Room::new(9);
    let (mut a, mut b) = pair(&room, &dir, false);
    let label = commands::strangers(&a.s).unwrap()[0].clone();
    a.set("bluetooth", 1);
    run(&mut [&mut a, &mut b], 200);
    assert!(
        commands::strangers(&a.s).unwrap().is_empty(),
        "no stranger offered"
    );
    assert_eq!(
        commands::send_request(&a.s, label),
        Err(NOT_IN_RANGE.to_string())
    );
    // asked before the poll applies the switch: the switch's own sentence
    a.set("bluetooth", 2);
    run(&mut [&mut a, &mut b], 1_000);
    let label = commands::strangers(&a.s).unwrap()[0].clone();
    a.set("bluetooth", 1);
    assert_eq!(
        commands::send_request(&a.s, label),
        Err(REQUESTS_NEED_OPEN.to_string())
    );
}
