//! The Tauri commands and event payloads, against a fake machine: no
//! audio, no window, no store on disk.

use std::sync::{Arc, Mutex};

use bombechat_app::commands::{self, bridge_event, Lock, Machine, Shared};
use bombechat_core::{ChunkView, PublicIdentity};
use bombechat_node::{NodeEvent, OutboxState, NOT_SENT, NOT_STORED};
use bombechat_store::{Channel, Contact, ContactId, StoredMessage};
use serde_json::json;

/// What the fake was asked to do.
#[derive(Default)]
struct Calls {
    sent: Vec<(Vec<ContactId>, String, Channel)>,
    broadcasts: Vec<String>,
    pairings: Vec<String>,
    confirmed: Vec<(PublicIdentity, String)>,
    settings: Vec<(String, usize)>,
    passphrases: Vec<String>,
}

struct Fake {
    calls: Arc<Mutex<Calls>>,
    lock: Lock,
}

const ID: [u8; 16] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10,
];

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
    fn broadcast(&mut self, text: &str) -> [u8; 16] {
        self.calls.lock().unwrap().broadcasts.push(text.to_string());
        ID
    }
    fn start_pairing(&mut self, my_name: &str) {
        self.calls
            .lock()
            .unwrap()
            .pairings
            .push(my_name.to_string());
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
    fn set_setting(&mut self, id: &str, value_index: usize) {
        self.calls
            .lock()
            .unwrap()
            .settings
            .push((id.to_string(), value_index));
    }
    fn lock(&self) -> Lock {
        self.lock
    }
    fn unlock(&mut self, passphrase: &str) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .passphrases
            .push(passphrase.to_string());
        if passphrase == "correct horse" {
            self.lock = Lock::Unlocked;
            Ok(())
        } else {
            Err("wrong passphrase".to_string())
        }
    }
    fn tick(&mut self, _now_ms: u64) -> Vec<NodeEvent> {
        Vec::new()
    }
}

fn fake(lock: Lock) -> (Shared, Arc<Mutex<Calls>>) {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let machine: Box<dyn Machine> = Box::new(Fake {
        calls: calls.clone(),
        lock,
    });
    (Mutex::new(machine), calls)
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
    let id = commands::broadcast(&m, "Shelter open.".into()).unwrap();
    assert_eq!(id, "0123456789abcdeffedcba9876543210");
    assert_eq!(calls.lock().unwrap().broadcasts, vec!["Shelter open."]);
}

#[test]
fn pair_announces_machine_no_1_and_refuses_while_locked() {
    let (m, calls) = fake(Lock::Unlocked);
    commands::pair(&m).unwrap();
    assert_eq!(calls.lock().unwrap().pairings, vec!["Machine No. 1"]);

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
fn unlock_passes_the_passphrase_and_returns_the_error_the_page_shows() {
    let (m, calls) = fake(Lock::NoStore);
    assert_eq!(
        commands::unlock(&m, "wrong".into()),
        Err("wrong passphrase".to_string())
    );
    assert_eq!(commands::unlock(&m, "correct horse".into()), Ok(()));
    assert_eq!(
        calls.lock().unwrap().passphrases,
        vec!["wrong", "correct horse"]
    );
}

#[test]
fn contacts_are_listed_once_unlocked() {
    let (m, _) = fake(Lock::Unlocked);
    let listed = serde_json::to_value(commands::contacts(&m).unwrap()).unwrap();
    assert_eq!(
        listed,
        json!([{ "id": 1, "name": "Marisol Ortega", "checked": true }])
    );

    let (locked, _) = fake(Lock::Locked);
    assert_eq!(commands::contacts(&locked).unwrap_err(), "locked");
    let (fresh, _) = fake(Lock::NoStore);
    assert_eq!(commands::contacts(&fresh).unwrap_err(), "no store yet");
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
    });
    assert_eq!(
        bridge_event(&message),
        (
            "message",
            json!({ "id": ID.to_vec(), "contact": 1, "outgoing": false, "text": "Bring the radio.",
                    "channel": "sound", "at_min": 29_000_000, "heard": false })
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
            signer: who.clone(),
            text: "Road north is open.".to_string(),
            verified: true
        }),
        (
            "broadcast",
            json!({ "signer": { "sign": sign, "dh": dh }, "text": "Road north is open.", "verified": true })
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
    for (state, word) in [
        (OutboxState::Waiting, "waiting"),
        (OutboxState::Playing, "playing"),
        (OutboxState::Sent, "sent"),
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
