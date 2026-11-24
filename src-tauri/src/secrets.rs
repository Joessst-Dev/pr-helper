//! API tokens live in the OS keychain, never in the database.

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;

use crate::error::{AppError, AppResult};

/// Stores API tokens by account: a server id, or `ticket-source:<id>` for a ticket source.
pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> AppResult<Option<String>>;
    fn set(&self, account: &str, token: &str) -> AppResult<()>;
    fn delete(&self, account: &str) -> AppResult<()>;

    fn require(&self, account: &str) -> AppResult<String> {
        self.get(account)?
            .ok_or_else(|| AppError::Secret("no token stored for this connection".into()))
    }
}

const SERVICE: &str = "dev.jostweyers.prhelper";

pub struct KeyringStore;

impl KeyringStore {
    fn entry(account: &str) -> AppResult<keyring::Entry> {
        Ok(keyring::Entry::new(SERVICE, account)?)
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, account: &str) -> AppResult<Option<String>> {
        match Self::entry(account)?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, account: &str, token: &str) -> AppResult<()> {
        Ok(Self::entry(account)?.set_password(token)?)
    }

    fn delete(&self, account: &str) -> AppResult<()> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// In-memory store for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

#[cfg(test)]
impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> AppResult<Option<String>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }

    fn set(&self, account: &str, token: &str) -> AppResult<()> {
        self.0.lock().unwrap().insert(account.into(), token.into());
        Ok(())
    }

    fn delete(&self, account: &str) -> AppResult<()> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Talks to the real OS keychain (macOS asks for permission the first time).
    /// `cargo test keychain_roundtrip -- --ignored`
    #[test]
    #[ignore = "talks to the real OS keychain"]
    fn keychain_roundtrip() {
        let store = KeyringStore;
        let id = format!("test-{}", uuid::Uuid::new_v4());
        store.set(&id, "secret-value").unwrap();
        assert_eq!(store.get(&id).unwrap().as_deref(), Some("secret-value"));
        store.delete(&id).unwrap();
        assert_eq!(store.get(&id).unwrap(), None);
        store.delete(&id).unwrap();
    }
}
