//! API tokens live in the OS keychain, never in the database.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::{AppError, AppResult};

pub trait SecretStore: Send + Sync {
    fn get(&self, server_id: &str) -> AppResult<Option<String>>;
    fn set(&self, server_id: &str, token: &str) -> AppResult<()>;
    fn delete(&self, server_id: &str) -> AppResult<()>;

    fn require(&self, server_id: &str) -> AppResult<String> {
        self.get(server_id)?
            .ok_or_else(|| AppError::Secret("no token stored for this server".into()))
    }
}

const SERVICE: &str = "dev.jostweyers.prhelper";

pub struct KeyringStore;

impl KeyringStore {
    fn entry(server_id: &str) -> AppResult<keyring::Entry> {
        keyring::Entry::new(SERVICE, server_id).map_err(|e| AppError::Secret(e.to_string()))
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, server_id: &str) -> AppResult<Option<String>> {
        match Self::entry(server_id)?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AppError::Secret(e.to_string())),
        }
    }

    fn set(&self, server_id: &str, token: &str) -> AppResult<()> {
        Self::entry(server_id)?
            .set_password(token)
            .map_err(|e| AppError::Secret(e.to_string()))
    }

    fn delete(&self, server_id: &str) -> AppResult<()> {
        match Self::entry(server_id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AppError::Secret(e.to_string())),
        }
    }
}

/// Used in tests and as a fallback when no keychain is available.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, server_id: &str) -> AppResult<Option<String>> {
        Ok(self.0.lock().unwrap().get(server_id).cloned())
    }

    fn set(&self, server_id: &str, token: &str) -> AppResult<()> {
        self.0
            .lock()
            .unwrap()
            .insert(server_id.into(), token.into());
        Ok(())
    }

    fn delete(&self, server_id: &str) -> AppResult<()> {
        self.0.lock().unwrap().remove(server_id);
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
