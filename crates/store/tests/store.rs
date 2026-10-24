use bombechat_core::{Identity, PublicIdentity};
use bombechat_store::{Channel, Contact, Store, StoreError, StoredMessage};
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
        })
        .unwrap();

    // Drop the store so every page is flushed before we inspect the files.
    drop(store);

    let mut to_scan = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
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

    assert!(!to_scan.is_empty(), "expected at least the db file and its sidecar to exist");

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
            !contains(bytes, &secret[..]),
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
            })
            .unwrap();
        contact
    };

    let reopened = Store::open(&path, "correct horse").unwrap();

    assert_eq!(reopened.me().public(), me.public());

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
        })
        .unwrap();

    let conversation = store.conversation(contact);
    assert_eq!(conversation.len(), 1);
    assert_eq!(conversation[0].text, "second");
    assert!(conversation[0].heard);
}
