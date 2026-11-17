#[cfg(feature = "test-support")]
use coshell_platform::secret_store::{FakeSecretStore, SecretError};
#[cfg(any(feature = "test-support", target_os = "windows", target_os = "macos"))]
use coshell_platform::secret_store::{Secret, SecretStore};

#[cfg(feature = "test-support")]
#[test]
fn fake_is_isolated_and_supports_binary_secrets_and_idempotent_delete() {
    let store = FakeSecretStore::default();
    let other = FakeSecretStore::default();
    let secret = Secret::new(vec![0, 255, 42]);
    store.set("connection-1", &secret).unwrap();
    assert_eq!(
        store.get("connection-1").unwrap().unwrap().expose(),
        &[0, 255, 42]
    );
    assert!(other.get("connection-1").unwrap().is_none());
    store.delete("connection-1").unwrap();
    store.delete("connection-1").unwrap();
    assert!(store.get("connection-1").unwrap().is_none());
}

#[cfg(feature = "test-support")]
#[test]
fn unavailable_store_fails_closed_and_never_exposes_secrets_in_debug() {
    let store = FakeSecretStore::unavailable();
    let secret = Secret::new(b"sensitive-value".to_vec());
    assert!(!format!("{secret:?}").contains("sensitive-value"));
    assert_eq!(store.set("id", &secret), Err(SecretError::Unavailable));
    assert_eq!(store.get("id"), Err(SecretError::Unavailable));
    assert_eq!(store.delete("id"), Err(SecretError::Unavailable));
    assert_eq!(SecretError::Unavailable.code(), "SECRET_STORE_UNAVAILABLE");
}

#[cfg(feature = "test-support")]
#[test]
fn empty_identifiers_are_rejected_consistently() {
    let store = FakeSecretStore::default();
    assert_eq!(
        store.set("", &Secret::new(vec![])),
        Err(SecretError::InvalidIdentifier)
    );
    assert_eq!(store.get(""), Err(SecretError::InvalidIdentifier));
    assert_eq!(store.delete(""), Err(SecretError::InvalidIdentifier));
}

// Explicit opt-in: this test creates and removes a uniquely named temporary
// credential in the real OS store. Run on both Windows and macOS release hosts.
#[cfg(any(target_os = "windows", target_os = "macos"))]
#[test]
#[ignore = "requires an interactive OS credential store"]
fn real_platform_store_round_trip() {
    use coshell_platform::secret_store::PlatformSecretStore;
    use std::time::{SystemTime, UNIX_EPOCH};

    let id = format!(
        "coshell-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let store = PlatformSecretStore::for_data_dir(&std::env::temp_dir()).unwrap();
    struct Cleanup<'a>(&'a PlatformSecretStore, &'a str);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.delete(self.1);
        }
    }
    let _cleanup = Cleanup(&store, &id);
    let secret = Secret::new(vec![0, 255, 42]);
    store.set(&id, &secret).unwrap();
    assert_eq!(store.get(&id).unwrap().unwrap().expose(), secret.expose());
    store.delete(&id).unwrap();
    store.delete(&id).unwrap();
    assert!(store.get(&id).unwrap().is_none());
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[test]
#[ignore = "requires an interactive OS credential store"]
fn distinct_data_directories_isolate_the_same_secret_identifier() {
    use coshell_platform::secret_store::PlatformSecretStore;
    let root = std::env::temp_dir().join(format!("coshell-namespace-{}", std::process::id()));
    let a = root.join("a");
    let b = root.join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let one = PlatformSecretStore::for_data_dir(&a).unwrap();
    let two = PlatformSecretStore::for_data_dir(&b).unwrap();
    one.set("same", &Secret::new(b"one".to_vec())).unwrap();
    two.set("same", &Secret::new(b"two".to_vec())).unwrap();
    one.delete("same").unwrap();
    assert_eq!(two.get("same").unwrap().unwrap().expose(), b"two");
    two.delete("same").unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
