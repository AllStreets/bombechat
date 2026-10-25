use bombechat_core::{Identity, PublicIdentity};
use bombechat_store::{
    Channel, Contact, Delivery, Store, StoreError, StoreKey, StoredBroadcast, StoredMessage,
};
use tempfile::tempdir;

fn marisol() -> PublicIdentity {
    Identity::from_secret_bytes(&[0x42; 64]).public()
}

#[test]
fn wrong_passphrase_refused() {
    let d = tempdir().unwrap();
    Store::create(
        &d.path().join("b.db"),
        "correct horse",
        &Identity::generate(),
        "Kitchen laptop",
    )
    .unwrap();
    assert!(matches!(
        Store::open(&d.path().join("b.db"), "wrong"),
        Err(StoreError::WrongPassphrase)
    ));
}

#[test]
fn nothing_readable_on_disk() {
    let d = tempdir().unwrap();
    let path = d.path().join("b.db");
    let me = Identity::generate();
    let secret = me.secret_bytes();

    let store = Store::create(&path, "correct horse", &me, "Kitchen laptop").unwrap();
    let contact = store
        .add_contact("Marisol Ortega", &marisol(), false)
        .unwrap();
    store
        .put_message(&StoredMessage {
            id: [9u8; 16],
            contact,
            outgoing: true,
            text: "Water is at the church".to_string(),
            channel: Channel::Sound,
            at_min: 1,
            heard: false,
            delivery: Delivery::Sent,
        })
        .unwrap();

    // Scan once while the store is still open: rusqlite commits each
    // statement immediately (autocommit), so the plaintext must already be
    // unreadable on disk, not just once the store is closed.
    assert_nothing_readable(&path, &secret[..]);

    // And again after drop/close, in case closing writes anything further.
    drop(store);
    assert_nothing_readable(&path, &secret[..]);
}

/// Scans the main db file, its `-wal`/`-shm` files (if present) and its
/// `.salt` sidecar for plaintext and asserts none of it shows up. Fails
/// loudly (rather than vacuously passing) if the main db file can't be
/// read or is implausibly small, since either would mean the scan never
/// actually looked at the data we just wrote.
fn assert_nothing_readable(path: &std::path::Path, secret: &[u8]) {
    let main_db = std::fs::read(path).expect("main db file must exist and be readable");
    assert!(
        main_db.len() >= 512,
        "main db file is implausibly small ({} bytes) for a store with a \
         contact and a message in it; the plaintext scan would be vacuous",
        main_db.len()
    );

    let mut to_scan = vec![main_db];
    for suffix in ["-wal", "-shm"] {
        let mut candidate = path.as_os_str().to_owned();
        candidate.push(suffix);
        let candidate = std::path::PathBuf::from(candidate);
        if candidate.exists() {
            to_scan.push(std::fs::read(&candidate).unwrap());
        }
    }
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".salt");
    to_scan.push(std::fs::read(std::path::PathBuf::from(sidecar)).unwrap());

    for bytes in &to_scan {
        assert!(
            !contains(bytes, b"Marisol Ortega"),
            "contact name found in plaintext on disk"
        );
        assert!(
            !contains(bytes, b"Water is at the church"),
            "message text found in plaintext on disk"
        );
        assert!(
            !contains(bytes, secret),
            "identity secret bytes found in plaintext on disk"
        );
    }
}

/// Naive substring search over raw bytes.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn reopen_keeps_contacts_and_messages() {
    let d = tempdir().unwrap();
    let path = d.path().join("b.db");
    let me = Identity::generate();

    let contact_id = {
        let store = Store::create(&path, "correct horse", &me, "Kitchen laptop").unwrap();
        let contact = store
            .add_contact("Marisol Ortega", &marisol(), true)
            .unwrap();
        store
            .put_message(&StoredMessage {
                id: [9u8; 16],
                contact,
                outgoing: true,
                text: "Water is at the church".to_string(),
                channel: Channel::Sound,
                at_min: 1,
                heard: false,
                delivery: Delivery::Sent,
            })
            .unwrap();
        store
            .put_message(&StoredMessage {
                id: [10u8; 16],
                contact,
                outgoing: false,
                text: "On my way".to_string(),
                channel: Channel::Light,
                at_min: 2,
                heard: true,
                delivery: Delivery::Sent,
            })
            .unwrap();
        contact
    };

    let reopened = Store::open(&path, "correct horse").unwrap();

    assert_eq!(reopened.me().public(), me.public());
    assert_eq!(reopened.my_name(), "Kitchen laptop");

    let contacts: Vec<Contact> = reopened.contacts();
    assert_eq!(contacts.len(), 1);
    assert_eq!(contacts[0].id, contact_id);
    assert_eq!(contacts[0].name, "Marisol Ortega");
    assert_eq!(contacts[0].who, marisol());
    assert!(contacts[0].checked);

    let conversation = reopened.conversation(contact_id);
    assert_eq!(conversation.len(), 2);
    assert_eq!(conversation[0].id, [9u8; 16]);
    assert_eq!(conversation[0].text, "Water is at the church");
    assert!(conversation[0].outgoing);
    assert!(!conversation[0].heard);
    assert_eq!(conversation[1].id, [10u8; 16]);
    assert_eq!(conversation[1].text, "On my way");
    assert!(!conversation[1].outgoing);
    assert!(conversation[1].heard);
}

#[test]
fn has_message_and_mark_heard() {
    let d = tempdir().unwrap();
    let path = d.path().join("b.db");
    let me = Identity::generate();
    let store = Store::create(&path, "correct horse", &me, "Kitchen laptop").unwrap();
    let contact = store.add_contact("Marisol Ortega", &marisol(), false).unwrap();

    let id = [5u8; 16];
    assert!(!store.has_message(&id));

    store
        .put_message(&StoredMessage {
            id,
            contact,
            outgoing: false,
            text: "hi".to_string(),
            channel: Channel::Bluetooth,
            at_min: 3,
            heard: false,
            delivery: Delivery::Sent,
        })
        .unwrap();
    assert!(store.has_message(&id));

    store.mark_heard(&id).unwrap();
    let conversation = store.conversation(contact);
    assert!(conversation[0].heard);
}

#[test]
fn put_message_with_existing_id_upserts() {
    let d = tempdir().unwrap();
    let path = d.path().join("b.db");
    let me = Identity::generate();
    let store = Store::create(&path, "correct horse", &me, "Kitchen laptop").unwrap();
    let contact = store.add_contact("Marisol Ortega", &marisol(), false).unwrap();

    let id = [7u8; 16];
    store
        .put_message(&StoredMessage {
            id,
            contact,
            outgoing: false,
            text: "first".to_string(),
            channel: Channel::Sound,
            at_min: 1,
            heard: false,
            delivery: Delivery::Sent,
        })
        .unwrap();
    store
        .put_message(&StoredMessage {
            id,
            contact,
            outgoing: false,
            text: "second".to_string(),
            channel: Channel::Sound,
            at_min: 1,
            heard: true,
            delivery: Delivery::Sent,
        })
        .unwrap();

    let conversation = store.conversation(contact);
    assert_eq!(conversation.len(), 1);
    assert_eq!(conversation[0].text, "second");
    assert!(conversation[0].heard);
}

#[test]
fn create_on_an_existing_path_is_refused_and_preserves_the_original() {
    let d = tempdir().unwrap();
    let path = d.path().join("b.db");
    let me = Identity::generate();
    Store::create(&path, "correct horse", &me, "Kitchen laptop").unwrap();

    let second = Store::create(
        &path,
        "a different passphrase",
        &Identity::generate(),
        "Someone else's laptop",
    );
    assert!(matches!(second, Err(StoreError::AlreadyExists)));

    // The original store must still open with its original passphrase:
    // the failed second `create` must not have touched the salt or db.
    let reopened = Store::open(&path, "correct horse").unwrap();
    assert_eq!(reopened.me().public(), me.public());
}

#[test]
fn open_on_a_nonexistent_path_is_io_error() {
    let d = tempdir().unwrap();
    let path = d.path().join("missing.db");
    assert!(matches!(
        Store::open(&path, "whatever"),
        Err(StoreError::Io)
    ));
}

#[test]
fn open_with_a_missing_sidecar_is_io_error() {
    let d = tempdir().unwrap();
    let path = d.path().join("b.db");
    Store::create(&path, "correct horse", &Identity::generate(), "Kitchen laptop").unwrap();

    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".salt");
    std::fs::remove_file(std::path::PathBuf::from(sidecar)).unwrap();

    assert!(matches!(
        Store::open(&path, "correct horse"),
        Err(StoreError::Io)
    ));
}

#[test]
fn open_with_a_short_sidecar_is_corrupt() {
    let d = tempdir().unwrap();
    let path = d.path().join("b.db");
    Store::create(&path, "correct horse", &Identity::generate(), "Kitchen laptop").unwrap();

    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".salt");
    std::fs::write(std::path::PathBuf::from(sidecar), [1u8, 2, 3]).unwrap();

    assert!(matches!(
        Store::open(&path, "correct horse"),
        Err(StoreError::Corrupt)
    ));
}

#[test]
fn open_on_an_empty_db_file_is_corrupt_not_a_panic() {
    let d = tempdir().unwrap();
    let path = d.path().join("b.db");
    // A zero-byte db file keys fine (there is nothing to decrypt yet) but
    // has no schema at all, so the identity row open() checks for is
    // missing. This must not make `me()` panic; `open` itself must refuse.
    std::fs::write(&path, []).unwrap();
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".salt");
    std::fs::write(std::path::PathBuf::from(sidecar), [0u8; 16]).unwrap();

    assert!(matches!(
        Store::open(&path, "anything"),
        Err(StoreError::Corrupt)
    ));
}

#[test]
fn files_are_created_private_on_unix() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let d = tempdir().unwrap();
        let path = d.path().join("b.db");
        Store::create(&path, "correct horse", &Identity::generate(), "Kitchen laptop").unwrap();

        let db_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(db_mode, 0o600, "db file mode was {:o}", db_mode);

        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(".salt");
        let sidecar_mode = std::fs::metadata(std::path::PathBuf::from(sidecar))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(sidecar_mode, 0o600, "sidecar mode was {:o}", sidecar_mode);
    }
}

#[test]
fn set_checked_updates_the_contact() {
    let d = tempdir().unwrap();
    let store =
        Store::create(&d.path().join("b.db"), "pw", &Identity::generate(), "me").unwrap();
    let c = store.add_contact("Marisol", &marisol(), false).unwrap();
    assert!(!store.contacts()[0].checked);
    store.set_checked(c, true).unwrap();
    assert!(store.contacts()[0].checked);
    store.set_checked(c, false).unwrap();
    assert!(!store.contacts()[0].checked);
}

#[test]
fn message_by_id() {
    let d = tempdir().unwrap();
    let store =
        Store::create(&d.path().join("b.db"), "pw", &Identity::generate(), "me").unwrap();
    let c = store.add_contact("Marisol", &marisol(), true).unwrap();
    let m = StoredMessage {
        id: [3u8; 16],
        contact: c,
        outgoing: false,
        text: "Water is at the church on Fifth.".into(),
        channel: Channel::Sound,
        at_min: 7,
        heard: false,
        delivery: Delivery::Sent,
    };
    store.put_message(&m).unwrap();
    assert_eq!(store.message(&[3u8; 16]), Some(m));
    assert_eq!(store.message(&[4u8; 16]), None);
}

#[test]
fn an_outgoing_message_keeps_where_its_delivery_got_to() {
    let d = tempdir().unwrap();
    let path = d.path().join("b.db");
    let store = Store::create(&path, "pw", &Identity::generate(), "me").unwrap();
    let c = store.add_contact("Marisol", &marisol(), true).unwrap();
    let m = StoredMessage {
        id: [5u8; 16],
        contact: c,
        outgoing: true,
        text: "Bring water.".into(),
        channel: Channel::Sound,
        at_min: 3,
        heard: false,
        delivery: Delivery::Waiting,
    };
    store.put_outgoing(&m, b"sealed envelope").unwrap();
    assert_eq!(store.waiting(), vec![(m.clone(), b"sealed envelope".to_vec())]);
    drop(store);

    let store = Store::open(&path, "pw").unwrap();
    assert_eq!(store.waiting().len(), 1, "still waiting after a restart");
    assert_eq!(store.conversation(c)[0].delivery, Delivery::Waiting);
    store.set_delivery(&[5u8; 16], Delivery::Sent).unwrap();
    assert!(store.waiting().is_empty());
    assert_eq!(store.message(&[5u8; 16]).unwrap().delivery, Delivery::Sent);
    store.set_delivery(&[5u8; 16], Delivery::Failed).unwrap();
    assert_eq!(store.message(&[5u8; 16]).unwrap().delivery, Delivery::Failed);
    assert_eq!(Delivery::Failed.as_str(), "failed");
}

fn message(
    id: u8,
    contact: bombechat_store::ContactId,
    at_min: u32,
    outgoing: bool,
    delivery: Delivery,
) -> StoredMessage {
    StoredMessage {
        id: [id; 16],
        contact,
        outgoing,
        text: format!("message {id}"),
        channel: Channel::Sound,
        at_min,
        heard: false,
        delivery,
    }
}

#[test]
fn the_key_a_passphrase_derives_opens_the_store_without_the_passphrase() {
    let d = tempdir().unwrap();
    let path = d.path().join("b.db");
    let me = Identity::generate();
    drop(Store::create(&path, "correct horse", &me, "Kitchen laptop").unwrap());

    let key = Store::key_for(&path, "correct horse").unwrap();
    let kept = StoreKey::from_bytes(key.as_bytes()).unwrap();
    let store = Store::open_with_key(&path, &kept).unwrap();
    assert_eq!(store.me().public(), me.public());
    drop(store);

    let wrong = Store::key_for(&path, "wrong").unwrap();
    assert!(matches!(
        Store::open_with_key(&path, &wrong),
        Err(StoreError::WrongPassphrase)
    ));
    assert!(StoreKey::from_bytes(&[1; 31]).is_none());
    assert_eq!(format!("{key:?}"), "StoreKey(..)", "never printed");
}

#[test]
fn forgetting_erases_only_what_has_been_kept_longer_than_the_running_time_allowed() {
    let d = tempdir().unwrap();
    let store = Store::create(&d.path().join("b.db"), "pw", &Identity::generate(), "me").unwrap();
    let c = store.add_contact("Marisol", &marisol(), true).unwrap();
    // at_min (the wall clock's minute) plays no part: these are all over the place
    store.set_age(1_000);
    store.put_message(&message(1, c, 9_999_999, false, Delivery::Sent)).unwrap();
    store.set_age(5_000);
    store.put_message(&message(2, c, 1, true, Delivery::Sent)).unwrap();
    store.put_outgoing(&message(3, c, 1, true, Delivery::Waiting), b"sealed").unwrap();
    store.set_age(9_000);
    store.put_message(&message(4, c, 500, false, Delivery::Sent)).unwrap();

    assert_eq!(store.forget_older_than(9_000, 10_000).unwrap(), 0, "younger than the age itself");
    assert_eq!(store.forget_older_than(9_000, 4_000).unwrap(), 1, "kept 8000 of 4000");
    let left: Vec<u8> = store.conversation(c).iter().map(|m| m.id[0]).collect();
    assert_eq!(left, vec![2, 3, 4], "kept exactly 4000 is not longer than 4000");
    assert_eq!(store.forget_older_than(9_001, 4_000).unwrap(), 1, "now it is");
    assert_eq!(store.waiting().len(), 1, "still waiting to go out: kept");
    store.set_delivery(&[3; 16], Delivery::Sent).unwrap();
    assert_eq!(store.forget_older_than(9_001, 4_000).unwrap(), 1, "gone, so it can go");

    // updating a stored message keeps the age it was first stored at
    store.set_age(20_000);
    store.put_message(&message(4, c, 500, false, Delivery::Sent)).unwrap();
    assert_eq!(store.forget_older_than(13_001, 4_000).unwrap(), 1);
}

#[test]
fn messages_stored_before_ages_were_kept_take_the_age_they_are_found_at() {
    let d = tempdir().unwrap();
    let store = Store::create(&d.path().join("b.db"), "pw", &Identity::generate(), "me").unwrap();
    let c = store.add_contact("Marisol", &marisol(), true).unwrap();
    store.put_message(&message(1, c, 1, false, Delivery::Sent)).unwrap(); // no age set yet
    assert_eq!(store.forget_older_than(u64::MAX, 0).unwrap(), 0, "no age: never erased");
    assert_eq!(store.adopt_unaged(50_000).unwrap(), 1);
    assert_eq!(store.adopt_unaged(90_000).unwrap(), 0, "adopted once");
    assert_eq!(store.forget_older_than(60_000, 10_000).unwrap(), 0);
    assert_eq!(store.forget_older_than(60_001, 10_000).unwrap(), 1);
}

fn notice(id: u8, outgoing: bool, delivery: Delivery) -> StoredBroadcast {
    StoredBroadcast {
        id: [id; 16],
        outgoing,
        signer: [id; 32],
        verified: outgoing,
        hops: if outgoing { 0 } else { 2 },
        text: format!("notice {id}"),
        at_min: 100 + id as u32,
        delivery,
    }
}

#[test]
fn broadcasts_sent_and_heard_are_kept_across_a_reopen_in_order() {
    let d = tempdir().unwrap();
    let path = d.path().join("b.db");
    let me = Identity::generate();
    let (sent, heard) = (notice(1, true, Delivery::Waiting), notice(2, false, Delivery::Sent));
    {
        let store = Store::create(&path, "pw", &me, "me").unwrap();
        store.put_broadcast(&sent, Some(b"sealed")).unwrap();
        store.put_broadcast(&heard, None).unwrap();
        assert!(store.has_message(&[1; 16]) && store.has_message(&[2; 16]));
        assert!(!store.has_message(&[3; 16]));
    }
    let store = Store::open(&path, "pw").unwrap();
    assert_eq!(store.broadcasts(), vec![sent.clone(), heard]);
    assert!(store.waiting().is_empty(), "not a message to anyone");
    assert_eq!(store.waiting_broadcasts(), vec![(sent, b"sealed".to_vec())]);

    store.set_delivery(&[1; 16], Delivery::Sent).unwrap();
    assert!(store.waiting_broadcasts().is_empty(), "its envelope let go once sent");
    assert_eq!(store.broadcasts()[0].delivery, Delivery::Sent);
    store.set_delivery(&[1; 16], Delivery::Failed).unwrap();
    assert_eq!(store.broadcasts()[0].delivery, Delivery::Failed);
}

#[test]
fn broadcasts_are_forgotten_as_messages_are_but_never_while_waiting() {
    let d = tempdir().unwrap();
    let store = Store::create(&d.path().join("b.db"), "pw", &Identity::generate(), "me").unwrap();
    store.put_broadcast(&notice(9, false, Delivery::Sent), None).unwrap(); // no age yet
    assert_eq!(store.adopt_unaged(500).unwrap(), 1);
    store.set_age(1_000);
    store.put_broadcast(&notice(1, false, Delivery::Sent), None).unwrap();
    store.put_broadcast(&notice(2, true, Delivery::Waiting), Some(b"sealed")).unwrap();
    store.set_age(9_000);
    store.put_broadcast(&notice(3, true, Delivery::Sent), None).unwrap();
    // heard again: still the age it was first kept at
    store.put_broadcast(&notice(1, false, Delivery::Sent), None).unwrap();

    assert_eq!(store.forget_older_than(9_000, 4_000).unwrap(), 2, "9 and 1, not the waiting 2");
    let left: Vec<u8> = store.broadcasts().iter().map(|b| b.id[0]).collect();
    assert_eq!(left, vec![2, 3]);
    store.set_delivery(&[2; 16], Delivery::Sent).unwrap();
    assert_eq!(store.forget_older_than(9_000, 4_000).unwrap(), 1);
}
