//! bombechat-store: encrypted on-device storage for identity, contacts,
//! messages and public broadcasts.
//!
//! Everything lives in one SQLCipher-encrypted SQLite database. The
//! encryption key is derived from a user passphrase with Argon2id
//! (`m = 64 MiB`, `t = 3`, `p = 1`); the 16-byte salt that derivation needs
//! is not secret and is kept next to the database in a `<db path>.salt`
//! sidecar file. Nothing readable, neither key material nor message text,
//! is ever written to disk: SQLCipher encrypts every page, including the
//! rollback journal and any WAL/SHM files it creates.

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use argon2::{Algorithm, Argon2, Params, Version};
use bombechat_core::{Identity, PublicIdentity};
use rand_core::{OsRng, RngCore};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use zeroize::{Zeroize, Zeroizing};

/// Argon2id parameters for deriving the database key from the user's
/// passphrase. These are production parameters; see `[profile.dev.package.argon2]`
/// in the workspace `Cargo.toml` for why debug builds aren't slow despite them.
const ARGON2_MEMORY_KIB: u32 = 64 * 1024;
const ARGON2_ITERATIONS: u32 = 3;
const ARGON2_PARALLELISM: u32 = 1;
const ARGON2_KEY_LEN: usize = 32;

/// Size of the random salt kept in the `<db path>.salt` sidecar file.
const SALT_LEN: usize = 16;

const SCHEMA: &str = "
PRAGMA auto_vacuum = INCREMENTAL;
CREATE TABLE identity (
    id INTEGER PRIMARY KEY CHECK (id = 0),
    secret BLOB NOT NULL,
    name TEXT NOT NULL
);
CREATE TABLE contacts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    sign BLOB NOT NULL,
    dh BLOB NOT NULL,
    checked INTEGER NOT NULL
);
CREATE TABLE messages (
    id BLOB PRIMARY KEY,
    contact_id INTEGER NOT NULL REFERENCES contacts(id),
    outgoing INTEGER NOT NULL,
    text TEXT NOT NULL,
    channel INTEGER NOT NULL,
    at_min INTEGER NOT NULL,
    heard INTEGER NOT NULL,
    delivery INTEGER NOT NULL DEFAULT 1,
    envelope BLOB,
    age_at INTEGER
);
CREATE TABLE broadcasts (
    id BLOB PRIMARY KEY,
    outgoing INTEGER NOT NULL,
    signer BLOB NOT NULL,
    verified INTEGER NOT NULL,
    hops INTEGER NOT NULL,
    text TEXT NOT NULL,
    at_min INTEGER NOT NULL,
    delivery INTEGER NOT NULL,
    envelope BLOB,
    age_at INTEGER
);
PRAGMA user_version = 4;
";

/// The schema version [`SCHEMA`] makes, kept in `PRAGMA user_version`.
/// Version 0 is the first schema, before messages kept their delivery;
/// version 1 is before messages kept the machine's age; version 2 is
/// before public broadcasts were kept; version 3 is before the store gave
/// back the pages erased rows leave ([`MIGRATE_TO_4`]).
const SCHEMA_VERSION: i64 = 4;

/// Brings a version 0 store up to version 1: outgoing messages gain a
/// delivery state and, while waiting, their sealed envelope. Rows from
/// before are taken as sent, as the store showed them then.
const MIGRATE_TO_1: &str = "
BEGIN;
ALTER TABLE messages ADD COLUMN delivery INTEGER NOT NULL DEFAULT 1;
ALTER TABLE messages ADD COLUMN envelope BLOB;
PRAGMA user_version = 1;
COMMIT;
";

/// Brings a version 1 store up to version 2: messages gain the machine's
/// age when they were stored ([`Store::set_age`]). Rows from before have
/// none until [`Store::adopt_unaged`] gives them the age at that moment,
/// so none counts as older than it is.
const MIGRATE_TO_2: &str = "
BEGIN;
ALTER TABLE messages ADD COLUMN age_at INTEGER;
PRAGMA user_version = 2;
COMMIT;
";

/// Brings a version 2 store up to version 3: public broadcasts, heard and
/// sent, are kept in a table of their own (none were kept before).
const MIGRATE_TO_3: &str = "
BEGIN;
CREATE TABLE broadcasts (
    id BLOB PRIMARY KEY,
    outgoing INTEGER NOT NULL,
    signer BLOB NOT NULL,
    verified INTEGER NOT NULL,
    hops INTEGER NOT NULL,
    text TEXT NOT NULL,
    at_min INTEGER NOT NULL,
    delivery INTEGER NOT NULL,
    envelope BLOB,
    age_at INTEGER
);
PRAGMA user_version = 3;
COMMIT;
";

/// Brings a version 3 store up to version 4: from now on the pages that
/// erased rows leave are given back to the file system at each purge
/// (`auto_vacuum = INCREMENTAL`, then `PRAGMA incremental_vacuum`). An
/// existing store only switches with a full `VACUUM`, which rewrites it
/// once, leaving no free page behind. `VACUUM` cannot run inside a
/// transaction; it is atomic on its own, and if it is cut short the
/// version stays at 3 and the next open does it again.
const MIGRATE_TO_4: &str = "
PRAGMA auto_vacuum = INCREMENTAL;
VACUUM;
PRAGMA user_version = 4;
";

/// How many heard broadcasts (not this device's own) are kept: the newest
/// this many. Anyone in range can broadcast, so without a cap strangers
/// could grow the store without bound.
pub const HEARD_BROADCASTS_KEPT: usize = 500;

/// The transport a message traveled over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Sound,
    Light,
    Bluetooth,
}

impl Channel {
    fn to_i64(self) -> i64 {
        match self {
            Channel::Sound => 0,
            Channel::Light => 1,
            Channel::Bluetooth => 2,
        }
    }

    fn from_i64(v: i64) -> Result<Channel, StoreError> {
        match v {
            0 => Ok(Channel::Sound),
            1 => Ok(Channel::Light),
            2 => Ok(Channel::Bluetooth),
            _ => Err(StoreError::Corrupt),
        }
    }
}

/// How far an outgoing message got. Messages received are stored as
/// [`Delivery::Sent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Queued to go out, not yet finished playing.
    Waiting,
    /// Every frame went out.
    Sent,
    /// It will not go out.
    Failed,
}

impl Delivery {
    fn to_i64(self) -> i64 {
        match self {
            Delivery::Waiting => 0,
            Delivery::Sent => 1,
            Delivery::Failed => 2,
        }
    }

    fn from_i64(v: i64) -> Delivery {
        match v {
            0 => Delivery::Waiting,
            2 => Delivery::Failed,
            _ => Delivery::Sent,
        }
    }

    /// As the interface names it: "waiting", "sent" or "failed".
    pub fn as_str(self) -> &'static str {
        match self {
            Delivery::Waiting => "waiting",
            Delivery::Sent => "sent",
            Delivery::Failed => "failed",
        }
    }
}

/// A contact's local row identifier. Stable for the lifetime of the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContactId(pub i64);

/// A known contact: a display name, their public identity, and whether the
/// user has verified their safety number out of band.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contact {
    pub id: ContactId,
    pub name: String,
    pub who: PublicIdentity,
    pub checked: bool,
}

/// A message stored in the conversation with one contact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: [u8; 16],
    pub contact: ContactId,
    pub outgoing: bool,
    pub text: String,
    pub channel: Channel,
    pub at_min: u32,
    pub heard: bool,
    pub delivery: Delivery,
}

/// A public broadcast: one this device signed and sent (`outgoing`), or
/// one it heard. Not tied to a contact; anyone in range can read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredBroadcast {
    pub id: [u8; 16],
    pub outgoing: bool,
    /// The key that signed it: this device's own for one it sent.
    pub signer: [u8; 32],
    /// Signed by a contact whose key was checked in person (always true
    /// for this device's own).
    pub verified: bool,
    /// How many carriers it came through to reach this device.
    pub hops: u8,
    pub text: String,
    pub at_min: u32,
    pub delivery: Delivery,
}

/// Errors returned by [`Store`] operations.
#[derive(Debug)]
pub enum StoreError {
    /// `create` was asked to make a store at a path (or sidecar) that
    /// already has one. Nothing was touched; the existing store is
    /// untouched and can still be opened with its original passphrase.
    AlreadyExists,
    /// The passphrase did not decrypt the database: SQLCipher reported the
    /// first page it read back as not belonging to a database once we
    /// tried to read it with the derived key. A file that isn't a
    /// SQLite/SQLCipher database at all (garbage, truncated, or otherwise
    /// unrelated) is reported identically by SQLCipher, so it also comes
    /// back as `WrongPassphrase` here rather than `Corrupt`.
    WrongPassphrase,
    /// The database (or its sidecar, or the derived key material) is
    /// present but malformed, e.g. a missing identity row or a short salt.
    Corrupt,
    /// A filesystem operation failed, or the store (or its sidecar) does
    /// not exist where `open` was asked to look.
    Io,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::AlreadyExists => write!(f, "a store already exists at this path"),
            StoreError::WrongPassphrase => write!(f, "wrong passphrase"),
            StoreError::Corrupt => write!(f, "store is corrupt"),
            StoreError::Io => write!(f, "store i/o error"),
        }
    }
}

impl std::error::Error for StoreError {}

/// The 32-byte SQLCipher key a passphrase derives for one store. Wiped
/// from memory when dropped. Kept outside the store only when the person
/// turns the passphrase lock off, and then only in the system keychain.
pub struct StoreKey(Zeroizing<[u8; ARGON2_KEY_LEN]>);

impl StoreKey {
    /// The key's bytes.
    pub fn as_bytes(&self) -> &[u8; ARGON2_KEY_LEN] {
        &self.0
    }

    /// A key from its bytes, as kept in the keychain; none if they are
    /// not 32 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<StoreKey> {
        let mut key = Zeroizing::new([0u8; ARGON2_KEY_LEN]);
        if bytes.len() != ARGON2_KEY_LEN {
            return None;
        }
        key.copy_from_slice(bytes);
        Some(StoreKey(key))
    }
}

impl std::fmt::Debug for StoreKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreKey(..)")
    }
}

/// The encrypted on-device store: identity, contacts and messages.
pub struct Store {
    conn: Mutex<Connection>,
    /// The machine's age, in ms of running time, that messages stored now
    /// are stamped with; none until [`Store::set_age`].
    age_ms: Mutex<Option<u64>>,
}

impl Store {
    /// Create a new encrypted store at `path`. `me` is this device's
    /// identity, kept (encrypted) inside the database; `my_name` is this
    /// device's own display label.
    ///
    /// Refuses with [`StoreError::AlreadyExists`] if either `path` or its
    /// `.salt` sidecar already exists, without touching either file. If
    /// any later step fails, the files this call itself created are
    /// removed before returning, so a failed `create` never leaves a
    /// half-written store (or clobbers an existing one's salt) behind.
    pub fn create(
        path: &Path,
        passphrase: &str,
        me: &Identity,
        my_name: &str,
    ) -> Result<Store, StoreError> {
        if path.exists() || salt_path(path).exists() {
            return Err(StoreError::AlreadyExists);
        }

        let guard = CreateGuard::new(path);

        let salt = random_salt();
        write_salt_sidecar(path, &salt)?;
        create_private_db_file(path)?;

        let key = derive_key(passphrase, &salt)?;
        let conn = open_existing(path)?;
        key_connection(&conn, &key)?;

        conn.execute_batch(SCHEMA).map_err(|_| StoreError::Corrupt)?;
        store_identity(&conn, me, my_name)?;

        guard.disarm();
        Ok(Store {
            conn: Mutex::new(conn),
            age_ms: Mutex::new(None),
        })
    }

    /// Open an existing encrypted store at `path`.
    pub fn open(path: &Path, passphrase: &str) -> Result<Store, StoreError> {
        Store::open_with_key(path, &Store::key_for(path, passphrase)?)
    }

    /// The key `passphrase` derives for the store at `path` (Argon2id
    /// with the store's salt: most of a second). Whether it is the right
    /// key is only known by opening the store with it.
    pub fn key_for(path: &Path, passphrase: &str) -> Result<StoreKey, StoreError> {
        let salt = read_salt_sidecar(path)?;
        derive_key(passphrase, &salt).map(StoreKey)
    }

    /// Open an existing encrypted store at `path` with its key, as
    /// [`Store::key_for`] derived it: no key derivation, so it is quick.
    /// A key that is not this store's is [`StoreError::WrongPassphrase`].
    pub fn open_with_key(path: &Path, key: &StoreKey) -> Result<Store, StoreError> {
        let conn = open_existing(path)?;
        key_connection(&conn, &key.0)?;
        verify_key(&conn)?;
        verify_identity_present(&conn)?;
        migrate(&conn)?;

        Ok(Store {
            conn: Mutex::new(conn),
            age_ms: Mutex::new(None),
        })
    }

    /// This device's own identity.
    pub fn me(&self) -> Identity {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut bytes: Vec<u8> = conn
            .query_row("SELECT secret FROM identity WHERE id = 0", [], |row| {
                row.get(0)
            })
            .expect("identity row present");

        let mut secret = Zeroizing::new([0u8; 64]);
        secret.copy_from_slice(&bytes);
        bytes.zeroize();

        Identity::from_secret_bytes(&secret)
    }

    /// This device's own display name, as given to [`Store::create`]: the
    /// name its pairing hellos carry.
    pub fn my_name(&self) -> String {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row("SELECT name FROM identity WHERE id = 0", [], |row| {
            row.get(0)
        })
        .expect("identity row present")
    }

    /// Add a contact, returning its newly assigned id.
    pub fn add_contact(
        &self,
        name: &str,
        who: &PublicIdentity,
        checked: bool,
    ) -> Result<ContactId, StoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO contacts (name, sign, dh, checked) VALUES (?1, ?2, ?3, ?4)",
            params![name, who.sign.as_slice(), who.dh.as_slice(), checked as i64],
        )
        .map_err(|_| StoreError::Corrupt)?;
        Ok(ContactId(conn.last_insert_rowid()))
    }

    /// All known contacts, ordered by id.
    pub fn contacts(&self) -> Vec<Contact> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare("SELECT id, name, sign, dh, checked FROM contacts ORDER BY id")
            .expect("preparing contacts query");
        let rows = stmt
            .query_map([], |row| {
                let sign: Vec<u8> = row.get(2)?;
                let dh: Vec<u8> = row.get(3)?;
                let checked: i64 = row.get(4)?;
                Ok(Contact {
                    id: ContactId(row.get(0)?),
                    name: row.get(1)?,
                    who: PublicIdentity {
                        sign: sign.try_into().expect("sign is 32 bytes"),
                        dh: dh.try_into().expect("dh is 32 bytes"),
                    },
                    checked: checked != 0,
                })
            })
            .expect("querying contacts");
        rows.map(|r| r.expect("reading contact row")).collect()
    }

    /// Insert or update (by id) a message.
    pub fn put_message(&self, m: &StoredMessage) -> Result<(), StoreError> {
        self.put(m, None)
    }

    /// Insert or update (by id) an outgoing message with the sealed
    /// envelope that carries it, kept while it is [`Delivery::Waiting`] so
    /// it can be queued again after a restart ([`Store::waiting`]).
    pub fn put_outgoing(&self, m: &StoredMessage, envelope: &[u8]) -> Result<(), StoreError> {
        self.put(m, Some(envelope))
    }

    fn put(&self, m: &StoredMessage, envelope: Option<&[u8]>) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let envelope = envelope.filter(|_| m.delivery == Delivery::Waiting);
        conn.execute(
            "INSERT INTO messages
                 (id, contact_id, outgoing, text, channel, at_min, heard, delivery, envelope, age_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                 contact_id = excluded.contact_id,
                 outgoing = excluded.outgoing,
                 text = excluded.text,
                 channel = excluded.channel,
                 at_min = excluded.at_min,
                 heard = excluded.heard,
                 delivery = excluded.delivery,
                 envelope = excluded.envelope",
            params![
                m.id.as_slice(),
                m.contact.0,
                m.outgoing as i64,
                m.text,
                m.channel.to_i64(),
                m.at_min,
                m.heard as i64,
                m.delivery.to_i64(),
                envelope,
                self.age(),
            ],
        )
        .map_err(|_| StoreError::Corrupt)?;
        Ok(())
    }

    /// Record how far a message's (or a broadcast's) delivery got. Once it is no longer
    /// waiting its envelope is let go. Unknown ids are ignored.
    pub fn set_delivery(&self, id: &[u8; 16], delivery: Delivery) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        for table in ["messages", "broadcasts"] {
            conn.execute(
                &format!(
                    "UPDATE {table} SET delivery = ?1,
                         envelope = CASE WHEN ?1 = 0 THEN envelope ELSE NULL END
                     WHERE id = ?2"
                ),
                params![delivery.to_i64(), id.as_slice()],
            )
            .map_err(|_| StoreError::Corrupt)?;
        }
        Ok(())
    }

    /// Outgoing messages still waiting to go out, oldest first, each with
    /// the sealed envelope that carries it.
    pub fn waiting(&self) -> Vec<(StoredMessage, Vec<u8>)> {
        let ids: Vec<(Vec<u8>, Vec<u8>)> = {
            let conn = self.conn.lock().expect("store mutex poisoned");
            let mut stmt = conn
                .prepare(
                    "SELECT id, envelope FROM messages
                     WHERE outgoing = 1 AND delivery = 0 AND envelope IS NOT NULL
                     ORDER BY rowid",
                )
                .expect("preparing waiting query");
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("querying waiting");
            rows.map(|r| r.expect("reading waiting row")).collect()
        };
        ids.into_iter()
            .filter_map(|(id, envelope)| {
                let id: [u8; 16] = id.try_into().ok()?;
                Some((self.message(&id)?, envelope))
            })
            .collect()
    }

    /// All messages exchanged with `c`, in the order they were first stored.
    pub fn conversation(&self, c: ContactId) -> Vec<StoredMessage> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, contact_id, outgoing, text, channel, at_min, heard, delivery
                 FROM messages WHERE contact_id = ?1 ORDER BY rowid",
            )
            .expect("preparing conversation query");
        let rows = stmt
            .query_map(params![c.0], |row| {
                let id: Vec<u8> = row.get(0)?;
                let outgoing: i64 = row.get(2)?;
                let channel: i64 = row.get(4)?;
                let heard: i64 = row.get(6)?;
                let delivery: i64 = row.get(7)?;
                Ok((
                    id,
                    row.get::<_, i64>(1)?,
                    outgoing,
                    row.get::<_, String>(3)?,
                    channel,
                    row.get::<_, u32>(5)?,
                    heard,
                    delivery,
                ))
            })
            .expect("querying conversation");

        rows.map(|r| r.expect("reading message row"))
            .map(|(id, contact, outgoing, text, channel, at_min, heard, delivery)| StoredMessage {
                id: id.try_into().expect("message id is 16 bytes"),
                contact: ContactId(contact),
                outgoing: outgoing != 0,
                text,
                channel: Channel::from_i64(channel).expect("valid channel"),
                at_min,
                heard: heard != 0,
                delivery: Delivery::from_i64(delivery),
            })
            .collect()
    }

    /// Whether a message or broadcast with this envelope id is already
    /// stored, used to dedup envelopes that arrive more than once.
    pub fn has_message(&self, id: &[u8; 16]) -> bool {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT 1 FROM messages WHERE id = ?1
             UNION ALL SELECT 1 FROM broadcasts WHERE id = ?1",
            params![id.as_slice()],
            |_| Ok(()),
        )
        .optional()
        .expect("querying has_message")
        .is_some()
    }

    /// Mark a stored message as heard (delivery receipt received).
    pub fn mark_heard(&self, id: &[u8; 16]) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "UPDATE messages SET heard = 1 WHERE id = ?1",
            params![id.as_slice()],
        )
        .map_err(|_| StoreError::Corrupt)?;
        Ok(())
    }

    /// The stored message with this envelope id, if any.
    pub fn message(&self, id: &[u8; 16]) -> Option<StoredMessage> {
        let contact: i64 = {
            let conn = self.conn.lock().expect("store mutex poisoned");
            conn.query_row(
                "SELECT contact_id FROM messages WHERE id = ?1",
                params![id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .expect("querying message")?
        };
        self.conversation(ContactId(contact))
            .into_iter()
            .find(|m| &m.id == id)
    }

    /// The machine's age, in ms of the time it has been running, to stamp
    /// on messages stored from now on (as `age_at`; never shown). The
    /// age only ever grows, and never follows the wall clock, so "Delete
    /// messages after" can count from it without trusting the clock.
    pub fn set_age(&self, age_ms: u64) {
        *self.age_ms.lock().expect("store age mutex poisoned") = Some(age_ms);
    }

    fn age(&self) -> Option<i64> {
        let age = *self.age_ms.lock().expect("store age mutex poisoned");
        age.map(|a| i64::try_from(a).unwrap_or(i64::MAX))
    }

    /// Give every message stored without an age (from before ages were
    /// kept, or before [`Store::set_age`]) the age `age_ms`: the youngest
    /// it can be, so none is erased earlier than it should be. Returns how
    /// many.
    pub fn adopt_unaged(&self, age_ms: u64) -> Result<usize, StoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let age = i64::try_from(age_ms).unwrap_or(i64::MAX);
        let mut n = 0;
        for table in ["messages", "broadcasts"] {
            n += conn
                .execute(
                    &format!("UPDATE {table} SET age_at = ?1 WHERE age_at IS NULL"),
                    params![age],
                )
                .map_err(|_| StoreError::Corrupt)?;
        }
        Ok(n)
    }

    /// Erase every message and broadcast stored more than `keep_ms` of running time
    /// before the machine's age `age_ms`, except outgoing ones still
    /// waiting to go out (they are erased once they have gone, at the
    /// next purge) and ones with no age yet. Returns how many went.
    pub fn forget_older_than(&self, age_ms: u64, keep_ms: u64) -> Result<usize, StoreError> {
        let Some(before) = age_ms.checked_sub(keep_ms) else {
            return Ok(0);
        };
        let conn = self.conn.lock().expect("store mutex poisoned");
        let before = i64::try_from(before).unwrap_or(i64::MAX);
        let mut n = 0;
        for table in ["messages", "broadcasts"] {
            n += conn
                .execute(
                    &format!(
                        "DELETE FROM {table}
                         WHERE age_at IS NOT NULL AND age_at < ?1
                           AND NOT (outgoing = 1 AND delivery = 0)"
                    ),
                    params![before],
                )
                .map_err(|_| StoreError::Corrupt)?;
        }
        if n > 0 {
            give_back_free_pages(&conn);
        }
        Ok(n)
    }

    /// Keep a broadcast: one this device sent, with the sealed envelope
    /// that carries it while it is [`Delivery::Waiting`] (so it can be
    /// queued again after a restart, [`Store::waiting_broadcasts`]), or
    /// one it heard (no envelope). Stored again under the same id, it
    /// updates the row but keeps the age it was first stored at. Only the
    /// newest [`HEARD_BROADCASTS_KEPT`] heard ones are kept; this device's
    /// own are never let go here.
    pub fn put_broadcast(
        &self,
        b: &StoredBroadcast,
        envelope: Option<&[u8]>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let envelope = envelope.filter(|_| b.delivery == Delivery::Waiting);
        conn.execute(
            "INSERT INTO broadcasts
                 (id, outgoing, signer, verified, hops, text, at_min, delivery, envelope, age_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                 outgoing = excluded.outgoing,
                 signer = excluded.signer,
                 verified = excluded.verified,
                 hops = excluded.hops,
                 text = excluded.text,
                 at_min = excluded.at_min,
                 delivery = excluded.delivery,
                 envelope = excluded.envelope",
            params![
                b.id.as_slice(),
                b.outgoing as i64,
                b.signer.as_slice(),
                b.verified as i64,
                b.hops as i64,
                b.text,
                b.at_min,
                b.delivery.to_i64(),
                envelope,
                self.age(),
            ],
        )
        .map_err(|_| StoreError::Corrupt)?;
        if !b.outgoing {
            conn.execute(
                "DELETE FROM broadcasts WHERE outgoing = 0 AND rowid NOT IN
                     (SELECT rowid FROM broadcasts WHERE outgoing = 0 ORDER BY rowid DESC LIMIT ?1)",
                params![HEARD_BROADCASTS_KEPT as i64],
            )
            .map_err(|_| StoreError::Corrupt)?;
        }
        Ok(())
    }

    /// Every broadcast kept, sent and heard, in the order first stored.
    pub fn broadcasts(&self) -> Vec<StoredBroadcast> {
        self.broadcasts_where("1")
            .into_iter()
            .map(|(b, _)| b)
            .collect()
    }

    /// This device's broadcasts still waiting to go out, oldest first,
    /// each with the sealed envelope that carries it.
    pub fn waiting_broadcasts(&self) -> Vec<(StoredBroadcast, Vec<u8>)> {
        self.broadcasts_where("outgoing = 1 AND delivery = 0 AND envelope IS NOT NULL")
            .into_iter()
            .filter_map(|(b, e)| Some((b, e?)))
            .collect()
    }

    fn broadcasts_where(&self, filter: &str) -> Vec<(StoredBroadcast, Option<Vec<u8>>)> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT id, outgoing, signer, verified, hops, text, at_min, delivery, envelope
                 FROM broadcasts WHERE {filter} ORDER BY rowid"
            ))
            .expect("preparing broadcasts query");
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, u32>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<Vec<u8>>>(8)?,
                ))
            })
            .expect("querying broadcasts");
        rows.map(|r| r.expect("reading broadcast row"))
            .filter_map(|(id, outgoing, signer, verified, hops, text, at_min, delivery, envelope)| {
                Some((
                    StoredBroadcast {
                        id: id.try_into().ok()?,
                        outgoing: outgoing != 0,
                        signer: signer.try_into().ok()?,
                        verified: verified != 0,
                        hops: u8::try_from(hops).unwrap_or(u8::MAX),
                        text,
                        at_min,
                        delivery: Delivery::from_i64(delivery),
                    },
                    envelope,
                ))
            })
            .collect()
    }

    /// Set whether a contact's safety number has been checked. Unknown
    /// ids are ignored.
    pub fn set_checked(&self, c: ContactId, checked: bool) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "UPDATE contacts SET checked = ?1 WHERE id = ?2",
            params![checked as i64, c.0],
        )
        .map_err(|_| StoreError::Corrupt)?;
        Ok(())
    }
}

/// Derive the 32-byte SQLCipher key from `passphrase` and `salt` with
/// Argon2id (`m = 64 MiB`, `t = 3`, `p = 1`).
fn derive_key(passphrase: &str, salt: &[u8; SALT_LEN]) -> Result<Zeroizing<[u8; ARGON2_KEY_LEN]>, StoreError> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        Some(ARGON2_KEY_LEN),
    )
    .map_err(|_| StoreError::Corrupt)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = Zeroizing::new([0u8; ARGON2_KEY_LEN]);
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|_| StoreError::Corrupt)?;
    Ok(key)
}

/// Key `conn` for SQLCipher via `PRAGMA key`, passing the key as a raw hex
/// literal (`x'...'`) so SQLCipher does not re-derive a key from it. The hex
/// string and the pragma statement that embeds it both live in `Zeroizing`
/// buffers.
fn key_connection(conn: &Connection, key: &Zeroizing<[u8; ARGON2_KEY_LEN]>) -> Result<(), StoreError> {
    let mut hex = Zeroizing::new(String::with_capacity(ARGON2_KEY_LEN * 2));
    for byte in key.iter() {
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
    }

    let pragma = Zeroizing::new(format!("PRAGMA key = \"x'{}'\";", hex.as_str()));
    conn.execute_batch(pragma.as_str())
        .map_err(|_| StoreError::Io)?;
    erase_on_delete(conn)
}

/// Have SQLite overwrite what it deletes with zeros (`secure_delete`):
/// the cells of erased rows and the pages they leave free. Without it an
/// erased message stays in the file's free space, readable by anyone who
/// can open the store. The bundled build is not compiled with it on, so
/// every connection turns it on itself.
fn erase_on_delete(conn: &Connection) -> Result<(), StoreError> {
    let on: i64 = conn
        .query_row("PRAGMA secure_delete = ON", [], |row| row.get(0))
        .map_err(|_| StoreError::Io)?;
    if on == 1 {
        Ok(())
    } else {
        Err(StoreError::Io)
    }
}

/// Give the pages erased rows left free back to the file system
/// (`incremental_vacuum`), so the file shrinks to what it holds. Their
/// contents were already overwritten ([`erase_on_delete`]); if this fails
/// the pages only stay in the file, zeroed, to be used again. The pragma
/// gives back one page per step, so it is stepped to the end.
fn give_back_free_pages(conn: &Connection) {
    let Ok(mut stmt) = conn.prepare("PRAGMA incremental_vacuum") else {
        return;
    };
    let Ok(mut rows) = stmt.query([]) else {
        return;
    };
    while let Ok(Some(_)) = rows.next() {}
}

/// After keying a freshly opened connection, force SQLCipher to actually
/// read a page. With the wrong key, SQLCipher cannot tell until this point;
/// it then reports the page as not belonging to a valid database at all
/// (`SQLITE_NOTADB`), which is exactly the same code a file that was never
/// a SQLite/SQLCipher database in the first place (garbage, truncated)
/// would produce. We can't tell those two apart, so both come back as
/// `WrongPassphrase`.
fn verify_key(conn: &Connection) -> Result<(), StoreError> {
    match conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::NotADatabase => {
            Err(StoreError::WrongPassphrase)
        }
        Err(_) => Err(StoreError::Corrupt),
    }
}

/// Once the key has been verified, confirm the one identity row this
/// store's schema requires is actually there. A freshly keyed but empty
/// file (no `CREATE TABLE`s run against it yet) keys and reads back fine
/// but has no `identity` table at all; without this check, `Store::me`
/// would panic on it instead of `open` reporting `Corrupt`.
fn verify_identity_present(conn: &Connection) -> Result<(), StoreError> {
    let row: Option<i64> = conn
        .query_row("SELECT 1 FROM identity WHERE id = 0", [], |row| row.get(0))
        .optional()
        .map_err(|_| StoreError::Corrupt)?;
    row.map(|_| ()).ok_or(StoreError::Corrupt)
}

/// Bring a store made by an earlier version of this crate up to
/// [`SCHEMA_VERSION`], in one transaction.
fn migrate(conn: &Connection) -> Result<(), StoreError> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| StoreError::Corrupt)?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::Corrupt);
    }
    for (to, step) in [
        (1, MIGRATE_TO_1),
        (2, MIGRATE_TO_2),
        (3, MIGRATE_TO_3),
        (4, MIGRATE_TO_4),
    ] {
        if version < to && conn.execute_batch(step).is_err() {
            let _ = conn.execute_batch("ROLLBACK;");
            return Err(StoreError::Corrupt);
        }
    }
    Ok(())
}

/// Open an already-existing database file for read/write without SQLite's
/// implicit `CREATE`, so a missing file reliably surfaces as
/// [`StoreError::Io`] instead of silently starting a new, empty, unkeyed
/// database at that path.
fn open_existing(path: &Path) -> Result<Connection, StoreError> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|_| StoreError::Io)
}

/// Store this device's identity secret and display name inside the
/// (already encrypted) database.
fn store_identity(conn: &Connection, me: &Identity, name: &str) -> Result<(), StoreError> {
    let secret = me.secret_bytes();
    conn.execute(
        "INSERT INTO identity (id, secret, name) VALUES (0, ?1, ?2)",
        params![&secret[..], name],
    )
    .map_err(|_| StoreError::Corrupt)?;
    Ok(())
}

fn random_salt() -> [u8; SALT_LEN] {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    salt
}

/// The sidecar path for a database at `db_path`: `<db_path>.salt`. The salt
/// is not secret, so it is written and read as plain bytes.
fn salt_path(db_path: &Path) -> PathBuf {
    let mut p = db_path.as_os_str().to_owned();
    p.push(".salt");
    PathBuf::from(p)
}

/// Write the salt sidecar, failing if it already exists (`create_new`),
/// rather than truncating whatever salt a previous store at this path had.
/// Not secret, so plain bytes, but still mode 0600 on Unix: there's no
/// reason to let other local accounts read it either.
fn write_salt_sidecar(db_path: &Path, salt: &[u8; SALT_LEN]) -> Result<(), StoreError> {
    let path = salt_path(db_path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|_| StoreError::Io)?;
    file.write_all(salt).map_err(|_| StoreError::Io)?;
    drop(file);
    set_private_permissions(&path)
}

fn read_salt_sidecar(db_path: &Path) -> Result<[u8; SALT_LEN], StoreError> {
    let bytes = std::fs::read(salt_path(db_path)).map_err(|_| StoreError::Io)?;
    bytes.try_into().map_err(|_| StoreError::Corrupt)
}

/// Create the (empty) database file itself, failing if it already exists,
/// so `Store::create` never opens and re-keys someone else's database.
/// Mode 0600 on Unix, set before any page is ever written to it.
fn create_private_db_file(path: &Path) -> Result<(), StoreError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| StoreError::Io)?;
    set_private_permissions(path)
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| StoreError::Io)
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

/// Removes the db file and its sidecar if `Store::create` fails after
/// creating one or both of them, so a failed `create` never leaves a
/// half-written store sitting where a later `open` could be pointed at
/// it. Disarmed only once `create` has fully succeeded.
struct CreateGuard {
    db_path: PathBuf,
    salt_path: PathBuf,
    armed: bool,
}

impl CreateGuard {
    fn new(db_path: &Path) -> CreateGuard {
        CreateGuard {
            db_path: db_path.to_path_buf(),
            salt_path: salt_path(db_path),
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for CreateGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.db_path);
            let _ = std::fs::remove_file(&self.salt_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first schema, as stores made before delivery states have it.
    const SCHEMA_0: &str = "
    CREATE TABLE identity (id INTEGER PRIMARY KEY CHECK (id = 0), secret BLOB NOT NULL, name TEXT NOT NULL);
    CREATE TABLE contacts (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, sign BLOB NOT NULL,
        dh BLOB NOT NULL, checked INTEGER NOT NULL);
    CREATE TABLE messages (id BLOB PRIMARY KEY, contact_id INTEGER NOT NULL REFERENCES contacts(id),
        outgoing INTEGER NOT NULL, text TEXT NOT NULL, channel INTEGER NOT NULL, at_min INTEGER NOT NULL,
        heard INTEGER NOT NULL);
    ";

    #[test]
    fn a_store_from_before_delivery_states_opens_and_takes_its_messages_as_sent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        let me = Identity::generate();
        let them = Identity::generate().public();
        {
            let salt = random_salt();
            write_salt_sidecar(&path, &salt).unwrap();
            create_private_db_file(&path).unwrap();
            let conn = open_existing(&path).unwrap();
            key_connection(&conn, &derive_key("pw", &salt).unwrap()).unwrap();
            conn.execute_batch(SCHEMA_0).unwrap();
            store_identity(&conn, &me, "me").unwrap();
            conn.execute(
                "INSERT INTO contacts (name, sign, dh, checked) VALUES ('Marisol', ?1, ?2, 1)",
                params![them.sign.as_slice(), them.dh.as_slice()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages VALUES (?1, 1, 1, 'Bring water.', 0, 5, 0)",
                params![[7u8; 16].as_slice()],
            )
            .unwrap();
        }

        let store = Store::open(&path, "pw").unwrap();
        let old = store.message(&[7u8; 16]).unwrap();
        assert_eq!((old.text.as_str(), old.delivery), ("Bring water.", Delivery::Sent));
        assert!(store.waiting().is_empty());
        let new = StoredMessage {
            id: [8u8; 16],
            delivery: Delivery::Waiting,
            ..old
        };
        store.put_outgoing(&new, b"sealed").unwrap();
        assert_eq!(store.waiting(), vec![(new, b"sealed".to_vec())]);
        drop(store);

        // Opening it again does not migrate twice.
        let store = Store::open(&path, "pw").unwrap();
        assert_eq!(store.waiting().len(), 1);
    }

    /// The version 1 schema, as stores made before messages kept the
    /// machine's age have it.
    const SCHEMA_1: &str = "
    CREATE TABLE identity (id INTEGER PRIMARY KEY CHECK (id = 0), secret BLOB NOT NULL, name TEXT NOT NULL);
    CREATE TABLE contacts (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, sign BLOB NOT NULL,
        dh BLOB NOT NULL, checked INTEGER NOT NULL);
    CREATE TABLE messages (id BLOB PRIMARY KEY, contact_id INTEGER NOT NULL REFERENCES contacts(id),
        outgoing INTEGER NOT NULL, text TEXT NOT NULL, channel INTEGER NOT NULL, at_min INTEGER NOT NULL,
        heard INTEGER NOT NULL, delivery INTEGER NOT NULL DEFAULT 1, envelope BLOB);
    PRAGMA user_version = 1;
    ";

    #[test]
    fn a_store_from_before_ages_opens_and_erases_nothing_until_its_messages_have_an_age() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.db");
        let them = Identity::generate().public();
        {
            let salt = random_salt();
            write_salt_sidecar(&path, &salt).unwrap();
            create_private_db_file(&path).unwrap();
            let conn = open_existing(&path).unwrap();
            key_connection(&conn, &derive_key("pw", &salt).unwrap()).unwrap();
            conn.execute_batch(SCHEMA_1).unwrap();
            store_identity(&conn, &Identity::generate(), "me").unwrap();
            conn.execute(
                "INSERT INTO contacts (name, sign, dh, checked) VALUES ('Marisol', ?1, ?2, 1)",
                params![them.sign.as_slice(), them.dh.as_slice()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages VALUES (?1, 1, 0, 'From long ago.', 0, 5, 0, 1, NULL)",
                params![[7u8; 16].as_slice()],
            )
            .unwrap();
        }

        let store = Store::open(&path, "pw").unwrap();
        assert_eq!(store.forget_older_than(u64::MAX, 0).unwrap(), 0);
        assert_eq!(store.adopt_unaged(3_600_000).unwrap(), 1);
        assert_eq!(store.forget_older_than(3_600_000 + 86_400_000, 86_400_000).unwrap(), 0);
        assert!(store.message(&[7u8; 16]).is_some(), "a day of running time, not more");
        drop(store);
        // opening it again does not migrate twice
        let store = Store::open(&path, "pw").unwrap();
        assert_eq!(store.adopt_unaged(1).unwrap(), 0);
    }

    /// The version 2 schema, as stores made before broadcasts were kept
    /// have it.
    const SCHEMA_2: &str = "
    CREATE TABLE identity (id INTEGER PRIMARY KEY CHECK (id = 0), secret BLOB NOT NULL, name TEXT NOT NULL);
    CREATE TABLE contacts (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, sign BLOB NOT NULL,
        dh BLOB NOT NULL, checked INTEGER NOT NULL);
    CREATE TABLE messages (id BLOB PRIMARY KEY, contact_id INTEGER NOT NULL REFERENCES contacts(id),
        outgoing INTEGER NOT NULL, text TEXT NOT NULL, channel INTEGER NOT NULL, at_min INTEGER NOT NULL,
        heard INTEGER NOT NULL, delivery INTEGER NOT NULL DEFAULT 1, envelope BLOB, age_at INTEGER);
    PRAGMA user_version = 2;
    ";

    #[test]
    fn a_store_from_before_broadcasts_were_kept_opens_with_none_and_keeps_them_from_then() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2.db");
        let them = Identity::generate().public();
        {
            let salt = random_salt();
            write_salt_sidecar(&path, &salt).unwrap();
            create_private_db_file(&path).unwrap();
            let conn = open_existing(&path).unwrap();
            key_connection(&conn, &derive_key("pw", &salt).unwrap()).unwrap();
            conn.execute_batch(SCHEMA_2).unwrap();
            store_identity(&conn, &Identity::generate(), "me").unwrap();
            conn.execute(
                "INSERT INTO contacts (name, sign, dh, checked) VALUES ('Marisol', ?1, ?2, 1)",
                params![them.sign.as_slice(), them.dh.as_slice()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages VALUES (?1, 1, 0, 'Kept.', 0, 5, 0, 1, NULL, 9)",
                params![[7u8; 16].as_slice()],
            )
            .unwrap();
        }

        let store = Store::open(&path, "pw").unwrap();
        assert!(store.broadcasts().is_empty());
        assert_eq!(store.message(&[7u8; 16]).unwrap().text, "Kept.");
        let b = StoredBroadcast {
            id: [3u8; 16],
            outgoing: false,
            signer: them.sign,
            verified: true,
            hops: 1,
            text: "Road north is open.".into(),
            at_min: 6,
            delivery: Delivery::Sent,
        };
        store.put_broadcast(&b, None).unwrap();
        drop(store);
        // opening it again does not migrate twice, and keeps what it holds
        let store = Store::open(&path, "pw").unwrap();
        assert_eq!(store.broadcasts(), vec![b]);
    }
    fn message_to(contact: ContactId, id: u8, text: &str) -> StoredMessage {
        StoredMessage {
            id: [id; 16],
            contact,
            outgoing: false,
            text: text.into(),
            channel: Channel::Sound,
            at_min: 5,
            heard: false,
            delivery: Delivery::Sent,
        }
    }

    fn pragma(store: &Store, name: &str) -> i64 {
        let conn = store.conn.lock().unwrap();
        conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn a_store_made_or_opened_overwrites_what_it_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        let store = Store::create(&path, "pw", &Identity::generate(), "me").unwrap();
        assert_eq!(pragma(&store, "secure_delete"), 1, "made");
        assert_eq!(pragma(&store, "auto_vacuum"), 2, "incremental, from the start");
        drop(store);
        let store = Store::open(&path, "pw").unwrap();
        assert_eq!(pragma(&store, "secure_delete"), 1, "opened");
    }

    /// A store set up exactly as [`Store::create`] sets one up, minus
    /// SQLCipher's page encryption, so a test can read the file's bytes as
    /// SQLite wrote them: whatever an erased row leaves there is what
    /// anyone holding the key could read back from the real store.
    fn unencrypted_store(path: &Path) -> Store {
        create_private_db_file(path).unwrap();
        let conn = open_existing(path).unwrap();
        erase_on_delete(&conn).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        store_identity(&conn, &Identity::generate(), "me").unwrap();
        Store {
            conn: Mutex::new(conn),
            age_ms: Mutex::new(None),
        }
    }

    fn holds(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn an_erased_message_leaves_nothing_readable_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.db");
        let store = unencrypted_store(&path);
        let c = store
            .add_contact("Marisol", &Identity::generate().public(), true)
            .unwrap();
        let short = "Meet at the quarry gate, Quokka-7731.";
        // long enough to spill onto overflow pages, which a purge frees whole
        let long = "Overflow-Pangolin-4410 ".repeat(400);
        store.set_age(0);
        store.put_message(&message_to(c, 1, short)).unwrap();
        store.put_message(&message_to(c, 2, &long)).unwrap();
        store.set_age(1_000);
        store.put_message(&message_to(c, 3, "Still here.")).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(holds(&before, b"Quokka-7731") && holds(&before, b"Overflow-Pangolin-4410"));

        assert_eq!(store.forget_older_than(1_000, 500).unwrap(), 2);
        assert_eq!(pragma(&store, "freelist_count"), 0, "the freed pages were given back");
        drop(store);

        let after = std::fs::read(&path).unwrap();
        assert!(!holds(&after, b"Quokka-7731"), "the short message is gone from the file");
        assert!(!holds(&after, b"Overflow-Pangolin-4410"), "and so is the long one");
        assert!(holds(&after, b"Still here."), "the one kept is still there");
        assert!(after.len() < before.len(), "the file shrank");
    }

    #[test]
    fn a_store_from_before_free_pages_were_given_back_is_rewritten_once_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v3.db");
        {
            let salt = random_salt();
            write_salt_sidecar(&path, &salt).unwrap();
            create_private_db_file(&path).unwrap();
            let conn = open_existing(&path).unwrap();
            key_connection(&conn, &derive_key("pw", &salt).unwrap()).unwrap();
            // the version 3 schema: today's, without auto_vacuum
            conn.execute_batch(&SCHEMA.replace("PRAGMA auto_vacuum = INCREMENTAL;", "")
                .replace("user_version = 4", "user_version = 3"))
                .unwrap();
            store_identity(&conn, &Identity::generate(), "me").unwrap();
            let v: i64 = conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0)).unwrap();
            assert_eq!(v, 0);
        }
        let store = Store::open(&path, "pw").unwrap();
        assert_eq!(pragma(&store, "auto_vacuum"), 2);
        assert_eq!(pragma(&store, "user_version"), 4);
        assert_eq!(store.my_name(), "me");
    }

    fn heard(n: u16) -> StoredBroadcast {
        let mut id = [0u8; 16];
        id[..2].copy_from_slice(&n.to_be_bytes());
        StoredBroadcast {
            id,
            outgoing: false,
            signer: [9; 32],
            verified: false,
            hops: 1,
            text: format!("heard {n}"),
            at_min: 1,
            delivery: Delivery::Sent,
        }
    }

    #[test]
    fn only_the_newest_heard_broadcasts_are_kept_and_never_at_the_cost_of_our_own() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.db");
        let store = Store::create(&path, "pw", &Identity::generate(), "me").unwrap();
        let own = StoredBroadcast {
            id: [0xff; 16],
            outgoing: true,
            text: "Ours, the oldest.".into(),
            verified: true,
            hops: 0,
            ..heard(0)
        };
        store.put_broadcast(&own, None).unwrap();
        let extra = 3;
        for n in 0..(HEARD_BROADCASTS_KEPT as u16 + extra) {
            store.put_broadcast(&heard(n), None).unwrap();
        }
        let kept = store.broadcasts();
        assert_eq!(kept.len(), HEARD_BROADCASTS_KEPT + 1);
        assert_eq!(kept[0], own, "our own stays");
        assert_eq!(kept[1].text, format!("heard {extra}"), "the oldest heard went");
        assert_eq!(kept.last().unwrap().text, format!("heard {}", HEARD_BROADCASTS_KEPT as u16 + extra - 1));
    }
}
