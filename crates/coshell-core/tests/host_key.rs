use coshell_core::host_key::SqliteHostKeyStore;
use coshell_core::host_key::{HostKeyPrompt, HostKeyStore, HostKeyTrust, TofuVerifier};
use coshell_drivers::ssh::{HostKey, HostKeyDecision, HostKeyVerifier, SshError, SshFuture};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Store(Arc<Mutex<Vec<HostKey>>>);
struct Permit {
    store: Arc<Mutex<Vec<HostKey>>>,
    key: HostKey,
    save: bool,
}
impl coshell_drivers::ssh::HostKeyPermit for Permit {
    fn authenticated(self: Box<Self>) -> Result<(), SshError> {
        if self.save {
            self.store.lock().unwrap().push(self.key.clone());
        }
        Ok(())
    }
}
impl HostKeyStore for Store {
    fn acquire_permit(
        &self,
        key: &HostKey,
        save: bool,
        _require_known: bool,
    ) -> Result<Box<dyn coshell_drivers::ssh::HostKeyPermit>, SshError> {
        Ok(Box::new(Permit {
            store: self.0.clone(),
            key: key.clone(),
            save,
        }))
    }
    fn changed_error(&self, key: &HostKey) -> Result<SshError, SshError> {
        let keys = self.0.lock().unwrap();
        let old = keys
            .iter()
            .find(|k| k.host == key.host && k.port == key.port)
            .ok_or(SshError::Transport)?;
        Ok(SshError::HostKeyChanged {
            old_algorithm: old.algorithm.clone(),
            old_fingerprint: old.fingerprint.clone(),
            new_algorithm: key.algorithm.clone(),
            new_fingerprint: key.fingerprint.clone(),
        })
    }
    fn inspect(&self, key: &HostKey) -> Result<HostKeyTrust, SshError> {
        let keys = self.0.lock().unwrap();
        Ok(if keys.iter().any(|k| k == key) {
            HostKeyTrust::Match
        } else if keys
            .iter()
            .any(|k| k.host == key.host && k.port == key.port)
        {
            HostKeyTrust::Changed
        } else {
            HostKeyTrust::Unknown
        })
    }
    fn insert_if_unknown(&self, key: &HostKey) -> Result<HostKeyTrust, SshError> {
        let mut keys = self.0.lock().unwrap();
        if keys.iter().any(|k| k == key) {
            Ok(HostKeyTrust::Match)
        } else if keys
            .iter()
            .any(|k| k.host == key.host && k.port == key.port)
        {
            Ok(HostKeyTrust::Changed)
        } else {
            keys.push(key.clone());
            Ok(HostKeyTrust::Match)
        }
    }
}
struct Prompt {
    decision: HostKeyDecision,
    calls: Mutex<usize>,
}
impl HostKeyPrompt for Prompt {
    fn ask<'a>(&'a self, _: &'a HostKey) -> SshFuture<'a, HostKeyDecision> {
        Box::pin(async move {
            *self.calls.lock().unwrap() += 1;
            Ok(self.decision)
        })
    }
}
fn key(host: &str, port: u16, algorithm: &str, fingerprint: &str) -> HostKey {
    HostKey {
        host: host.into(),
        port,
        algorithm: algorithm.into(),
        fingerprint: fingerprint.into(),
    }
}

#[tokio::test]
async fn first_accept_once_and_reject_do_not_persist() {
    for decision in [HostKeyDecision::Reject, HostKeyDecision::AcceptOnce] {
        let store = Arc::new(Store::default());
        let prompt = Arc::new(Prompt {
            decision,
            calls: Mutex::new(0),
        });
        let verifier = TofuVerifier::new(store.clone(), prompt.clone());
        assert_eq!(
            verifier
                .verify(&key("host", 22, "ed25519", "a"))
                .await
                .unwrap()
                .decision,
            decision
        );
        assert!(store.0.lock().unwrap().is_empty());
        assert_eq!(*prompt.calls.lock().unwrap(), 1);
    }
}

#[tokio::test]
async fn saved_key_matches_without_prompt_and_changed_key_never_overwrites() {
    let store = Arc::new(Store::default());
    let prompt = Arc::new(Prompt {
        decision: HostKeyDecision::AcceptAndSave,
        calls: Mutex::new(0),
    });
    let verifier = TofuVerifier::new(store.clone(), prompt.clone());
    let original = key("host", 22, "ed25519", "a");
    let verification = verifier.verify(&original).await.unwrap();
    assert_eq!(verification.decision, HostKeyDecision::AcceptAndSave);
    verification.permit.unwrap().authenticated().unwrap();
    assert_eq!(
        verifier.verify(&original).await.unwrap().decision,
        HostKeyDecision::AcceptOnce
    );
    assert!(
        matches!(verifier.verify(&key("host",22,"ed25519","b")).await,
        Err(SshError::HostKeyChanged { old_algorithm, old_fingerprint, new_algorithm, new_fingerprint })
        if old_algorithm=="ed25519" && old_fingerprint=="a" && new_algorithm=="ed25519" && new_fingerprint=="b")
    );
    assert_eq!(store.0.lock().unwrap().as_slice(), &[original]);
    assert_eq!(*prompt.calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn host_port_and_algorithm_are_scoped_independently() {
    let store = Arc::new(Store::default());
    let prompt = Arc::new(Prompt {
        decision: HostKeyDecision::AcceptAndSave,
        calls: Mutex::new(0),
    });
    let verifier = TofuVerifier::new(store.clone(), prompt.clone());
    for k in [
        key("a", 22, "ed25519", "a"),
        key("b", 22, "ed25519", "b"),
        key("a", 23, "ed25519", "b"),
    ] {
        verifier
            .verify(&k)
            .await
            .unwrap()
            .permit
            .unwrap()
            .authenticated()
            .unwrap();
    }
    assert!(matches!(
        verifier.verify(&key("a", 22, "rsa", "b")).await,
        Err(SshError::HostKeyChanged { .. })
    ));
    assert_eq!(store.0.lock().unwrap().len(), 3);
    assert_eq!(*prompt.calls.lock().unwrap(), 3);
}

#[tokio::test]
async fn distinct_sqlite_instances_detect_equivalent_hostname_change_with_structured_warning() {
    let path = std::env::temp_dir().join(format!(
        "coshell-tofu-core-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut first = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut first).unwrap();
    let second = rusqlite::Connection::open(&path).unwrap();
    let prompt = Arc::new(Prompt {
        decision: HostKeyDecision::AcceptAndSave,
        calls: Mutex::new(0),
    });
    let a = TofuVerifier::new(Arc::new(SqliteHostKeyStore::new(first)), prompt.clone());
    let b = TofuVerifier::new(Arc::new(SqliteHostKeyStore::new(second)), prompt.clone());
    a.verify(&key("Example.COM.", 22, "ssh-ed25519", "SHA256:old"))
        .await
        .unwrap()
        .permit
        .unwrap()
        .authenticated()
        .unwrap();
    assert!(
        matches!(b.verify(&key("example.com",22,"ssh-ed25519","SHA256:new")).await,
        Err(SshError::HostKeyChanged {old_fingerprint,new_fingerprint,..}) if old_fingerprint=="SHA256:old" && new_fingerprint=="SHA256:new")
    );
    assert_eq!(*prompt.calls.lock().unwrap(), 1);
    drop(a);
    drop(b);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn accept_once_permit_blocks_concurrent_different_key_until_authentication_ends() {
    let path = std::env::temp_dir().join(format!(
        "coshell-tofu-lease-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let second = rusqlite::Connection::open(&path).unwrap();
    let once = TofuVerifier::new(
        Arc::new(SqliteHostKeyStore::new(db)),
        Arc::new(Prompt {
            decision: HostKeyDecision::AcceptOnce,
            calls: Mutex::new(0),
        }),
    );
    let save = TofuVerifier::new(
        Arc::new(SqliteHostKeyStore::new(second)),
        Arc::new(Prompt {
            decision: HostKeyDecision::AcceptAndSave,
            calls: Mutex::new(0),
        }),
    );
    let first = once
        .verify(&key("EXAMPLE.com.", 22, "ed25519", "a"))
        .await
        .unwrap();
    assert!(matches!(
        save.verify(&key("example.com", 22, "ed25519", "b")).await,
        Err(SshError::HostKeyBusy)
    ));
    drop(first);
    save.verify(&key("example.com", 22, "ed25519", "b"))
        .await
        .unwrap()
        .permit
        .unwrap()
        .authenticated()
        .unwrap();
    drop(once);
    drop(save);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn known_match_also_retains_permit_until_authentication() {
    let path = std::env::temp_dir().join(format!(
        "coshell-tofu-known-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    coshell_storage::host_key::HostKeyRepository::new(&mut db)
        .insert_if_unknown("host", 22, "ed25519", "a")
        .unwrap();
    let verifier = TofuVerifier::new(
        Arc::new(SqliteHostKeyStore::new(db)),
        Arc::new(Prompt {
            decision: HostKeyDecision::AcceptOnce,
            calls: Mutex::new(0),
        }),
    );
    let known = verifier
        .verify(&key("host", 22, "ed25519", "a"))
        .await
        .unwrap();
    assert!(known.permit.is_some());
    let other = rusqlite::Connection::open(&path).unwrap();
    other.busy_timeout(std::time::Duration::ZERO).unwrap();
    assert!(other.execute_batch("BEGIN IMMEDIATE").is_err());
    drop(known);
    other.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").unwrap();
    drop(other);
    drop(verifier);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn locked_database_does_not_block_runtime_during_inspection() {
    let path = std::env::temp_dir().join(format!(
        "coshell-tofu-locked-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.busy_timeout(std::time::Duration::from_millis(500))
        .unwrap();
    let locker = rusqlite::Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let verifier = TofuVerifier::new(
        Arc::new(SqliteHostKeyStore::new(db)),
        Arc::new(Prompt {
            decision: HostKeyDecision::Reject,
            calls: Mutex::new(0),
        }),
    );
    let start = std::time::Instant::now();
    let result = verifier.verify(&key("host", 22, "ed25519", "a")).await;
    let elapsed = start.elapsed();
    locker.execute_batch("ROLLBACK").unwrap();
    assert!(matches!(result, Err(SshError::HostKeyBusy)));
    assert_eq!(
        verifier
            .verify(&key("host", 22, "ed25519", "a"))
            .await
            .unwrap()
            .decision,
        HostKeyDecision::Reject
    );
    drop(locker);
    drop(verifier);
    std::fs::remove_file(path).unwrap();
    assert!(
        elapsed < std::time::Duration::from_millis(250),
        "inspection blocked for {elapsed:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_prompt_leaves_sqlite_store_usable() {
    struct PendingPrompt;
    impl HostKeyPrompt for PendingPrompt {
        fn ask<'a>(&'a self, _: &'a HostKey) -> SshFuture<'a, HostKeyDecision> {
            Box::pin(std::future::pending())
        }
    }
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let store = Arc::new(SqliteHostKeyStore::new(db));
    let verifier = TofuVerifier::new(store.clone(), Arc::new(PendingPrompt));
    let candidate = key("host", 22, "ed25519", "a");
    assert!(tokio::time::timeout(
        std::time::Duration::from_millis(20),
        verifier.verify(&candidate)
    )
    .await
    .is_err());
    assert_eq!(store.inspect(&candidate).unwrap(), HostKeyTrust::Unknown);
}
