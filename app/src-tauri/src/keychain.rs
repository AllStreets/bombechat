//! Where the store key is kept while Lock with passphrase is off: the
//! macOS Keychain, so the app can open the store at launch without
//! asking. Nowhere else, and never on disk beside the store.
//!
//! The Keychain is reached through Security.framework (the `keyring`
//! crate's `apple-native` store), which makes no network connection.

use std::path::Path;

use bombechat_store::StoreKey;

/// Somewhere to keep the store key between launches.
pub trait Keeper: Send {
    /// The key kept, if there is one (and it is a key).
    fn get(&self) -> Option<StoreKey>;
    /// Keep `key`, replacing any kept before, or say why not.
    fn put(&self, key: &StoreKey) -> Result<(), String>;
    /// Let go of the kept key; nothing kept is not an error.
    fn remove(&self) -> Result<(), String>;
}

/// No keychain: keeps nothing, so the passphrase is always asked for.
pub struct Nowhere;

impl Keeper for Nowhere {
    fn get(&self) -> Option<StoreKey> {
        None
    }
    fn put(&self, _key: &StoreKey) -> Result<(), String> {
        Err("There is no keychain here to keep the key in".to_string())
    }
    fn remove(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The keychain for the store at `store_path`: the macOS Keychain on a
/// Mac, [`Nowhere`] elsewhere.
pub fn system(store_path: &Path) -> Box<dyn Keeper> {
    #[cfg(target_os = "macos")]
    {
        match mac::Keychain::for_store(store_path) {
            Some(k) => Box::new(k),
            None => Box::new(Nowhere),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = store_path;
        Box::new(Nowhere)
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::path::Path;

    use bombechat_store::StoreKey;
    use keyring::{Entry, Error};
    use zeroize::Zeroize;

    use super::Keeper;

    /// The Keychain item's service name. Its account is the store's path,
    /// so a second store (a test, another build) never shares a key.
    const SERVICE: &str = "Bombechat store key";

    /// The store key as a generic password in the login Keychain.
    pub struct Keychain {
        entry: Entry,
    }

    impl Keychain {
        pub fn for_store(store_path: &Path) -> Option<Keychain> {
            Entry::new(SERVICE, &store_path.to_string_lossy())
                .ok()
                .map(|entry| Keychain { entry })
        }
    }

    impl Keeper for Keychain {
        fn get(&self) -> Option<StoreKey> {
            let mut bytes = self.entry.get_secret().ok()?;
            let key = StoreKey::from_bytes(&bytes);
            bytes.zeroize();
            key
        }

        fn put(&self, key: &StoreKey) -> Result<(), String> {
            self.entry
                .set_secret(key.as_bytes())
                .map_err(|e| format!("The Keychain did not keep the key: {e}"))
        }

        fn remove(&self) -> Result<(), String> {
            match self.entry.delete_credential() {
                Ok(()) | Err(Error::NoEntry) => Ok(()),
                Err(e) => Err(format!("The Keychain did not let go of the key: {e}")),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Against the real login Keychain, so it is run by hand:
        /// `cargo test -p bombechat-app keychain -- --ignored`.
        #[test]
        #[ignore]
        fn the_keychain_keeps_a_key_and_lets_it_go() {
            let k = Keychain::for_store(Path::new("/bombechat/keychain-test.db")).unwrap();
            let key = StoreKey::from_bytes(&[7; 32]).unwrap();
            k.put(&key).unwrap();
            assert_eq!(k.get().unwrap().as_bytes(), key.as_bytes());
            k.remove().unwrap();
            assert!(k.get().is_none());
            k.remove().unwrap();
        }
    }
}
