//! bombechat-store: encrypted on-device storage for identity, contacts and
//! messages.
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
    heard INTEGER NOT NULL
);
";

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

/// The encrypted on-device store: identity, contacts and messages.
pub struct Store {
    conn: Mutex<Connection>,
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
        })
    }

    /// Open an existing encrypted store at `path`.
    pub fn open(path: &Path, passphrase: &str) -> Result<Store, StoreError> {
        let salt = read_salt_sidecar(path)?;
        let key = derive_key(passphrase, &salt)?;

        let conn = open_existing(path)?;
        key_connection(&conn, &key)?;
        verify_key(&conn)?;
        verify_identity_present(&conn)?;

        Ok(Store {
            conn: Mutex::new(conn),
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
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO messages (id, contact_id, outgoing, text, channel, at_min, heard)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                 contact_id = excluded.contact_id,
                 outgoing = excluded.outgoing,
                 text = excluded.text,
                 channel = excluded.channel,
                 at_min = excluded.at_min,
                 heard = excluded.heard",
            params![
                m.id.as_slice(),
                m.contact.0,
                m.outgoing as i64,
                m.text,
                m.channel.to_i64(),
                m.at_min,
                m.heard as i64,
            ],
        )
        .map_err(|_| StoreError::Corrupt)?;
        Ok(())
    }

    /// All messages exchanged with `c`, in the order they were first stored.
    pub fn conversation(&self, c: ContactId) -> Vec<StoredMessage> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, contact_id, outgoing, text, channel, at_min, heard
                 FROM messages WHERE contact_id = ?1 ORDER BY rowid",
            )
            .expect("preparing conversation query");
        let rows = stmt
            .query_map(params![c.0], |row| {
                let id: Vec<u8> = row.get(0)?;
                let outgoing: i64 = row.get(2)?;
                let channel: i64 = row.get(4)?;
                let heard: i64 = row.get(6)?;
                Ok((
                    id,
                    row.get::<_, i64>(1)?,
                    outgoing,
                    row.get::<_, String>(3)?,
                    channel,
                    row.get::<_, u32>(5)?,
                    heard,
                ))
            })
            .expect("querying conversation");

        rows.map(|r| r.expect("reading message row"))
            .map(|(id, contact, outgoing, text, channel, at_min, heard)| StoredMessage {
                id: id.try_into().expect("message id is 16 bytes"),
                contact: ContactId(contact),
                outgoing: outgoing != 0,
                text,
                channel: Channel::from_i64(channel).expect("valid channel"),
                at_min,
                heard: heard != 0,
            })
            .collect()
    }

    /// Whether a message with this envelope id is already stored, used to
    /// dedup envelopes that arrive more than once.
    pub fn has_message(&self, id: &[u8; 16]) -> bool {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT 1 FROM messages WHERE id = ?1",
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
        .map_err(|_| StoreError::Io)
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
