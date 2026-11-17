//! Platform credential storage. Never falls back to plain files or memory.

use std::fmt;
#[cfg(any(test, feature = "test-support"))]
use std::{collections::HashMap, sync::Mutex};
use zeroize::Zeroize;

/// An owned secret whose debug output is always redacted and whose buffer is
/// cleared on drop. Callers should keep the lifetime of exposed bytes short.
#[derive(PartialEq, Eq)]
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(value: Vec<u8>) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

/// Stable errors deliberately omit the identifier, underlying error and secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretError {
    InvalidIdentifier,
    SecretTooLong,
    Unavailable,
    AccessDenied,
    StorageFailure,
    NamespaceUnavailable,
}

impl SecretError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidIdentifier => "SECRET_STORE_INVALID_IDENTIFIER",
            Self::SecretTooLong => "SECRET_STORE_SECRET_TOO_LONG",
            Self::Unavailable => "SECRET_STORE_UNAVAILABLE",
            Self::AccessDenied => "SECRET_STORE_ACCESS_DENIED",
            Self::StorageFailure => "SECRET_STORE_FAILURE",
            Self::NamespaceUnavailable => "SECRET_STORE_NAMESPACE_UNAVAILABLE",
        }
    }
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for SecretError {}

pub trait SecretStore: Send + Sync {
    /// Custom implementations are responsible for their own isolation. Native
    /// stores override this to bind the coordinator to the same directory.
    fn accepts_data_dir(&self, _data_dir: &std::path::Path) -> bool {
        true
    }
    fn set(&self, id: &str, secret: &Secret) -> Result<(), SecretError>;
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError>;
    /// Deleting an absent identifier is successful.
    fn delete(&self, id: &str) -> Result<(), SecretError>;
}

fn validate_id(id: &str) -> Result<(), SecretError> {
    if id.is_empty() || id.contains('\0') {
        Err(SecretError::InvalidIdentifier)
    } else {
        Ok(())
    }
}

/// Windows Credential Manager / macOS Keychain. Other platforms fail closed.
#[derive(Clone)]
pub struct PlatformSecretStore {
    service: String,
}

impl fmt::Debug for PlatformSecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlatformSecretStore([NAMESPACE REDACTED])")
    }
}

impl PlatformSecretStore {
    /// Identity follows the canonical data-directory path. Copying a directory
    /// produces a separate namespace; moving it requires explicit credential
    /// import or reauthentication. No legacy/global namespace is consulted.
    pub fn for_data_dir(path: &std::path::Path) -> Result<Self, SecretError> {
        use sha2::{Digest, Sha256};
        if !path.is_absolute() {
            return Err(SecretError::NamespaceUnavailable);
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| SecretError::NamespaceUnavailable)?;
        if !canonical.is_dir() {
            return Err(SecretError::NamespaceUnavailable);
        }
        #[cfg(windows)]
        let bytes = {
            use std::os::windows::ffi::OsStrExt;
            canonical
                .as_os_str()
                .encode_wide()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        };
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::ffi::OsStrExt;
            canonical.as_os_str().as_bytes().to_vec()
        };
        #[cfg(not(any(windows, unix)))]
        let bytes = canonical.to_string_lossy().as_bytes().to_vec();
        let digest = Sha256::digest(&bytes);
        Ok(Self {
            service: format!(
                "com.coshell.app.{}",
                digest
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ),
        })
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl PlatformSecretStore {
    fn entry(&self, id: &str) -> Result<keyring::Entry, SecretError> {
        validate_id(id)?;
        #[cfg(target_os = "windows")]
        let credential = keyring::windows::WinCredential::new_with_target(None, &self.service, id);
        #[cfg(target_os = "macos")]
        let credential = keyring::macos::MacCredential::new_with_target(None, &self.service, id);
        Ok(keyring::Entry::new_with_credential(Box::new(
            credential.map_err(map_error)?,
        )))
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn map_error(error: keyring::Error) -> SecretError {
    match error {
        keyring::Error::NoStorageAccess(_) => SecretError::AccessDenied,
        keyring::Error::TooLong(attribute, _)
            if attribute == "secret" || attribute.starts_with("password") =>
        {
            SecretError::SecretTooLong
        }
        keyring::Error::Invalid(_, _) | keyring::Error::TooLong(_, _) => {
            SecretError::InvalidIdentifier
        }
        _ => SecretError::StorageFailure,
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl SecretStore for PlatformSecretStore {
    fn accepts_data_dir(&self, data_dir: &std::path::Path) -> bool {
        Self::for_data_dir(data_dir).is_ok_and(|store| store.service == self.service)
    }
    fn set(&self, id: &str, secret: &Secret) -> Result<(), SecretError> {
        self.entry(id)?
            .set_secret(secret.expose())
            .map_err(map_error)
    }

    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        match self.entry(id)?.get_secret() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(map_error(error)),
        }
    }

    fn delete(&self, id: &str) -> Result<(), SecretError> {
        match self.entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(map_error(error)),
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
impl SecretStore for PlatformSecretStore {
    fn accepts_data_dir(&self, data_dir: &std::path::Path) -> bool {
        Self::for_data_dir(data_dir).is_ok_and(|store| store.service == self.service)
    }
    fn set(&self, id: &str, _secret: &Secret) -> Result<(), SecretError> {
        validate_id(id)?;
        Err(SecretError::Unavailable)
    }
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        validate_id(id)?;
        Err(SecretError::Unavailable)
    }
    fn delete(&self, id: &str) -> Result<(), SecretError> {
        validate_id(id)?;
        Err(SecretError::Unavailable)
    }
}

/// Isolated in-memory test double. Never used as a platform fallback.
#[doc(hidden)]
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct FakeSecretStore {
    values: Mutex<HashMap<String, Secret>>,
    unavailable: bool,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeSecretStore {
    pub fn unavailable() -> Self {
        Self {
            unavailable: true,
            ..Self::default()
        }
    }

    fn check(&self, id: &str) -> Result<(), SecretError> {
        validate_id(id)?;
        if self.unavailable {
            Err(SecretError::Unavailable)
        } else {
            Ok(())
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl SecretStore for FakeSecretStore {
    fn set(&self, id: &str, secret: &Secret) -> Result<(), SecretError> {
        self.check(id)?;
        self.values
            .lock()
            .map_err(|_| SecretError::StorageFailure)?
            .insert(id.to_owned(), Secret::new(secret.expose().to_vec()));
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        self.check(id)?;
        Ok(self
            .values
            .lock()
            .map_err(|_| SecretError::StorageFailure)?
            .get(id)
            .map(|secret| Secret::new(secret.expose().to_vec())))
    }

    fn delete(&self, id: &str) -> Result<(), SecretError> {
        self.check(id)?;
        self.values
            .lock()
            .map_err(|_| SecretError::StorageFailure)?
            .remove(id);
        Ok(())
    }
}

#[cfg(all(test, any(target_os = "windows", target_os = "macos")))]
mod platform_error_tests {
    use super::*;

    #[test]
    fn classifies_secret_and_identifier_length_errors_separately() {
        assert_eq!(
            map_error(keyring::Error::TooLong("secret".into(), 2560)),
            SecretError::SecretTooLong
        );
        assert_eq!(
            map_error(keyring::Error::TooLong("user".into(), 512)),
            SecretError::InvalidIdentifier
        );
        assert_eq!(
            SecretError::SecretTooLong.code(),
            "SECRET_STORE_SECRET_TOO_LONG"
        );
    }

    #[test]
    fn platform_entry_is_the_native_credential_type() {
        let store = PlatformSecretStore::for_data_dir(&std::env::temp_dir()).unwrap();
        let entry = store.entry("isolated-builder-test").unwrap();
        #[cfg(target_os = "windows")]
        assert!(entry
            .get_credential()
            .is::<keyring::windows::WinCredential>());
        #[cfg(target_os = "macos")]
        assert!(entry.get_credential().is::<keyring::macos::MacCredential>());
    }
}
