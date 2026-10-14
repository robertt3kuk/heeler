//! Where provider API keys live (ADR 0008): the macOS Keychain, behind a trait so that every
//! test but one runs against memory.
//!
//! One generic-password item per provider: service [`KEYCHAIN_SERVICE`], account = the provider
//! id. The database keeps only [`keychain_ref`] (a pointer to it), never the secret.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::secret::Secret;

/// The Keychain service of every provider credential.
pub const KEYCHAIN_SERVICE: &str = "dev.heeler.provider";

/// What the database stores to say where a provider's credential is.
pub fn keychain_ref(provider_id: &str) -> String {
    format!("{KEYCHAIN_SERVICE}/{provider_id}")
}

/// A failure of the credential store. Never contains a secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CredentialError(pub String);

/// Blocking: the Keychain may wait for the user, so call it from a blocking task.
pub trait CredentialStore: Send + Sync + Debug {
    /// The secret of `account`; `None` when there is none.
    fn get(&self, account: &str) -> Result<Option<Secret>, CredentialError>;
    /// Stores or replaces the secret of `account`.
    fn set(&self, account: &str, secret: &Secret) -> Result<(), CredentialError>;
    /// Removes the secret of `account`; it is not an error that there was none.
    fn delete(&self, account: &str) -> Result<(), CredentialError>;
}

/// Secrets in memory only: tests, and platforms without a Keychain.
#[derive(Debug, Default)]
pub struct MemoryStore {
    secrets: Mutex<HashMap<String, Secret>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<Secret>, CredentialError> {
        Ok(self.secrets.lock().get(account).cloned())
    }

    fn set(&self, account: &str, secret: &Secret) -> Result<(), CredentialError> {
        self.secrets
            .lock()
            .insert(account.to_owned(), secret.clone());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), CredentialError> {
        self.secrets.lock().remove(account);
        Ok(())
    }
}

/// The store a daemon uses: the user's Keychain on macOS.
pub fn system_store() -> Arc<dyn CredentialStore> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(KeychainStore::login())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(UnsupportedStore)
    }
}

/// Platforms without a Keychain integration: keys cannot be stored, which the error says.
#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
struct UnsupportedStore;

#[cfg(not(target_os = "macos"))]
impl CredentialStore for UnsupportedStore {
    fn get(&self, _account: &str) -> Result<Option<Secret>, CredentialError> {
        Ok(None)
    }

    fn set(&self, _account: &str, _secret: &Secret) -> Result<(), CredentialError> {
        Err(CredentialError(
            "storing credentials needs the macOS Keychain, which this platform lacks".into(),
        ))
    }

    fn delete(&self, _account: &str) -> Result<(), CredentialError> {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub use keychain::KeychainStore;

#[cfg(target_os = "macos")]
mod keychain {
    use std::path::Path;

    use security_framework::base::Error;
    use security_framework::os::macos::keychain::SecKeychain;

    use super::{CredentialError, CredentialStore, KEYCHAIN_SERVICE, Secret};

    /// `errSecItemNotFound`.
    const NOT_FOUND: i32 = -25300;

    /// Generic-password items in a macOS keychain: the user's default one, or a keychain file
    /// (tests use a throwaway file, never the login keychain).
    #[derive(Default)]
    pub struct KeychainStore {
        /// `None` is the user's default keychain.
        keychain: Option<SecKeychain>,
    }

    impl std::fmt::Debug for KeychainStore {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("KeychainStore")
                .field("file", &self.keychain.is_some())
                .finish()
        }
    }

    fn failure(what: &str, error: &Error) -> CredentialError {
        CredentialError(format!(
            "Keychain: {what} failed: {} (code {})",
            error.message().unwrap_or_else(|| "unknown error".into()),
            error.code()
        ))
    }

    impl KeychainStore {
        /// The user's default (login) keychain.
        pub fn login() -> Self {
            Self { keychain: None }
        }

        /// The keychain file at `path`, unlocked with `password`. The file must exist
        /// (`security create-keychain`).
        pub fn open_file(path: &Path, password: &str) -> Result<Self, CredentialError> {
            let mut keychain =
                SecKeychain::open(path).map_err(|e| failure("opening the keychain file", &e))?;
            keychain
                .unlock(Some(password))
                .map_err(|e| failure("unlocking the keychain file", &e))?;
            Ok(Self {
                keychain: Some(keychain),
            })
        }

        fn keychain(&self) -> Result<SecKeychain, CredentialError> {
            match &self.keychain {
                Some(keychain) => Ok(keychain.clone()),
                None => SecKeychain::default().map_err(|e| failure("finding the keychain", &e)),
            }
        }
    }

    impl CredentialStore for KeychainStore {
        fn get(&self, account: &str) -> Result<Option<Secret>, CredentialError> {
            let keychain = self.keychain()?;
            match keychain.find_generic_password(KEYCHAIN_SERVICE, account) {
                Ok((password, _item)) => {
                    let text = String::from_utf8(password.to_vec()).map_err(|_| {
                        CredentialError("Keychain: the stored credential is not text".into())
                    })?;
                    Ok(Some(Secret::new(text)))
                }
                Err(e) if e.code() == NOT_FOUND => Ok(None),
                Err(e) => Err(failure("reading the credential", &e)),
            }
        }

        fn set(&self, account: &str, secret: &Secret) -> Result<(), CredentialError> {
            self.keychain()?
                .set_generic_password(KEYCHAIN_SERVICE, account, secret.expose().as_bytes())
                .map_err(|e| failure("writing the credential", &e))
        }

        fn delete(&self, account: &str) -> Result<(), CredentialError> {
            match self
                .keychain()?
                .find_generic_password(KEYCHAIN_SERVICE, account)
            {
                Ok((_password, item)) => {
                    item.delete();
                    Ok(())
                }
                Err(e) if e.code() == NOT_FOUND => Ok(()),
                Err(e) => Err(failure("removing the credential", &e)),
            }
        }
    }
}
