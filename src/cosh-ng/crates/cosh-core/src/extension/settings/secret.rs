//! Operating-system secret persistence boundary.

use super::*;

/// Secret persistence boundary used by production and deterministic tests.
pub trait SecretBackend: Send + Sync {
    /// Stores a sensitive setting.
    fn set(&self, extension: &str, key: &str, value: &str) -> Result<(), SettingsError>;
    /// Reads a sensitive setting without exposing it to registry views.
    fn get(&self, extension: &str, key: &str) -> Result<Option<String>, SettingsError>;
    /// Removes a sensitive setting.
    fn delete(&self, extension: &str, key: &str) -> Result<(), SettingsError>;
}

/// OS keyring-backed production secret store.
#[derive(Debug, Default)]
pub struct KeyringSecretBackend;

impl KeyringSecretBackend {
    fn entry(extension: &str, key: &str) -> Result<keyring::Entry, SettingsError> {
        keyring::Entry::new(KEYRING_SERVICE, &format!("{extension}/{key}")).map_err(|error| {
            SettingsError::new(
                "extension_secret_backend_unavailable",
                format!("failed to open the operating-system secret store: {error}"),
            )
        })
    }
}

impl SecretBackend for KeyringSecretBackend {
    fn set(&self, extension: &str, key: &str, value: &str) -> Result<(), SettingsError> {
        Self::entry(extension, key)?
            .set_password(value)
            .map_err(|error| {
                SettingsError::new(
                    "extension_secret_backend_unavailable",
                    format!("failed to write the operating-system secret store: {error}"),
                )
            })
    }

    fn get(&self, extension: &str, key: &str) -> Result<Option<String>, SettingsError> {
        match Self::entry(extension, key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SettingsError::new(
                "extension_secret_backend_unavailable",
                format!("failed to read the operating-system secret store: {error}"),
            )),
        }
    }

    fn delete(&self, extension: &str, key: &str) -> Result<(), SettingsError> {
        match Self::entry(extension, key)?.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(SettingsError::new(
                "extension_secret_backend_unavailable",
                format!("failed to update the operating-system secret store: {error}"),
            )),
        }
    }
}
