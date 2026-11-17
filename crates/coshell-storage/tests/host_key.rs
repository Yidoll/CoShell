use coshell_storage::{
    host_key::{HostKeyRepository, HostKeyStatus},
    migrate,
};
use rusqlite::Connection;
use std::sync::{Arc, Barrier};

#[test]
fn persisted_host_port_scope_rejects_changed_algorithm_and_fingerprint() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    let mut repo = HostKeyRepository::new(&mut db);
    assert_eq!(
        repo.inspect("host", 22, "ed25519", "a").unwrap(),
        HostKeyStatus::Unknown
    );
    assert_eq!(
        repo.insert_if_unknown("host", 22, "ed25519", "a").unwrap(),
        HostKeyStatus::Match
    );
    assert_eq!(
        repo.inspect("host", 22, "ed25519", "a").unwrap(),
        HostKeyStatus::Match
    );
    assert_eq!(
        repo.insert_if_unknown("host", 22, "ed25519", "b").unwrap(),
        HostKeyStatus::Changed
    );
    assert_eq!(
        repo.insert_if_unknown("host", 22, "rsa", "b").unwrap(),
        HostKeyStatus::Changed
    );
    assert_eq!(
        repo.inspect("host", 23, "rsa", "b").unwrap(),
        HostKeyStatus::Unknown
    );
}

#[test]
fn concurrent_first_trust_cannot_silently_replace_winner() {
    let path = std::env::temp_dir().join(format!(
        "coshell-tofu-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut db = Connection::open(&path).unwrap();
    migrate(&mut db).unwrap();
    drop(db);
    let barrier = Arc::new(Barrier::new(3));
    let mut tasks = Vec::new();
    for fingerprint in ["a", "b"] {
        let p = path.clone();
        let barrier = barrier.clone();
        tasks.push(std::thread::spawn(move || {
            let mut db = Connection::open(p).unwrap();
            db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
            barrier.wait();
            HostKeyRepository::new(&mut db)
                .insert_if_unknown("host", 22, "ed25519", fingerprint)
                .unwrap()
        }));
    }
    barrier.wait();
    let results: Vec<_> = tasks.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(
        results
            .iter()
            .filter(|s| **s == HostKeyStatus::Match)
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|s| **s == HostKeyStatus::Changed)
            .count(),
        1
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn equivalent_host_spellings_cannot_bypass_existing_trust() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    let mut repo = HostKeyRepository::new(&mut db);
    repo.insert_if_unknown("Example.COM.", 22, "ed25519", "a")
        .unwrap();
    assert_eq!(
        repo.inspect("example.com", 22, "ed25519", "b").unwrap(),
        HostKeyStatus::Changed
    );
    repo.insert_if_unknown("2001:db8::1", 22, "ed25519", "a")
        .unwrap();
    assert_eq!(
        repo.inspect("[2001:0db8:0:0:0:0:0:1]", 22, "ed25519", "b")
            .unwrap(),
        HostKeyStatus::Changed
    );
}

#[test]
fn temporary_permit_blocks_other_writer_and_rolls_back_on_drop() {
    use coshell_storage::host_key::HostKeyPermit;
    let path = std::env::temp_dir().join(format!(
        "coshell-tofu-permit-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut db = Connection::open(&path).unwrap();
    migrate(&mut db).unwrap();
    drop(db);
    let permit = HostKeyPermit::acquire(&path, "Example.COM.", 22, "ed25519", "a", false).unwrap();
    let mut other = Connection::open(&path).unwrap();
    other.busy_timeout(std::time::Duration::ZERO).unwrap();
    assert!(HostKeyRepository::new(&mut other)
        .insert_if_unknown("example.com", 22, "ed25519", "b")
        .is_err());
    drop(permit);
    assert_eq!(
        HostKeyRepository::new(&mut other)
            .insert_if_unknown("example.com", 22, "ed25519", "b")
            .unwrap(),
        HostKeyStatus::Match
    );
    drop(other);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn save_permit_commits_only_after_authentication() {
    use coshell_storage::host_key::HostKeyPermit;
    let path = std::env::temp_dir().join(format!(
        "coshell-tofu-commit-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut db = Connection::open(&path).unwrap();
    migrate(&mut db).unwrap();
    drop(db);
    drop(HostKeyPermit::acquire(&path, "host", 22, "ed25519", "a", true).unwrap());
    let mut db = Connection::open(&path).unwrap();
    assert_eq!(
        HostKeyRepository::new(&mut db)
            .inspect("host", 22, "ed25519", "a")
            .unwrap(),
        HostKeyStatus::Unknown
    );
    drop(db);
    HostKeyPermit::acquire(&path, "host", 22, "ed25519", "a", true)
        .unwrap()
        .authenticated()
        .unwrap();
    let mut db = Connection::open(&path).unwrap();
    assert_eq!(
        HostKeyRepository::new(&mut db)
            .inspect("host", 22, "ed25519", "a")
            .unwrap(),
        HostKeyStatus::Match
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn conflicting_legacy_aliases_never_return_match() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    db.execute("INSERT INTO trusted_host_keys(host,port,algorithm,fingerprint) VALUES ('Example.com',22,'ed25519','a')", []).unwrap();
    db.execute("INSERT INTO trusted_host_keys(host,port,algorithm,fingerprint) VALUES ('example.com.',22,'rsa','b')", []).unwrap();
    let repo = HostKeyRepository::new(&mut db);
    assert_eq!(
        repo.inspect("example.com", 22, "ed25519", "a").unwrap(),
        HostKeyStatus::Changed
    );
    assert_eq!(
        repo.inspect("EXAMPLE.COM", 22, "rsa", "b").unwrap(),
        HostKeyStatus::Changed
    );
}

#[test]
fn failed_commit_releases_write_reservation() {
    use coshell_storage::host_key::HostKeyPermit;
    let path = std::env::temp_dir().join(format!(
        "coshell-tofu-busy-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut db = Connection::open(&path).unwrap();
    migrate(&mut db).unwrap();
    let permit = HostKeyPermit::acquire_connection(db, "host", 22, "ed25519", "a", true, false)
        .unwrap_or_else(|_| panic!("permit"));
    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM trusted_host_keys;")
        .unwrap();
    let (db, result) = permit.finish(true);
    assert!(result.is_err());
    assert!(db.is_some());
    reader.execute_batch("ROLLBACK").unwrap();
    let other = Connection::open(&path).unwrap();
    other.busy_timeout(std::time::Duration::ZERO).unwrap();
    other
        .execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
        .expect("failed COMMIT left a write lock");
    drop(db);
    drop(reader);
    drop(other);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn acquire_does_not_rollback_a_caller_owned_transaction() {
    use coshell_storage::host_key::HostKeyPermit;
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    db.execute_batch("BEGIN; INSERT INTO app_settings(key,value) VALUES ('keep','value');")
        .unwrap();
    let (db, _) =
        match HostKeyPermit::acquire_connection(db, "host", 22, "ed25519", "a", true, false) {
            Ok(_) => panic!("should reject existing transaction"),
            Err(e) => e,
        };
    let db = db.unwrap();
    assert!(!db.is_autocommit());
    assert_eq!(
        db.query_row("SELECT value FROM app_settings WHERE key='keep'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "value"
    );
    db.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn known_key_disappearing_before_permit_is_rejected_and_timeout_preserved() {
    use coshell_storage::host_key::HostKeyPermit;
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    HostKeyRepository::new(&mut db)
        .insert_if_unknown("host", 22, "ed25519", "a")
        .unwrap();
    assert_eq!(
        HostKeyRepository::new(&mut db)
            .inspect("host", 22, "ed25519", "a")
            .unwrap(),
        HostKeyStatus::Match
    );
    db.execute("DELETE FROM trusted_host_keys", []).unwrap();
    db.busy_timeout(std::time::Duration::from_millis(4321))
        .unwrap();
    let (db, _) =
        match HostKeyPermit::acquire_connection(db, "host", 22, "ed25519", "a", false, true) {
            Ok(_) => panic!("missing trust must not issue permit"),
            Err(e) => e,
        };
    let db = db.unwrap();
    let timeout: u32 = db
        .pragma_query_value(None, "busy_timeout", |r| r.get(0))
        .unwrap();
    assert_eq!(timeout, 4321);
}

#[test]
fn successful_permit_restores_original_busy_timeout() {
    use coshell_storage::host_key::HostKeyPermit;
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    db.busy_timeout(std::time::Duration::from_millis(4321))
        .unwrap();
    let permit = HostKeyPermit::acquire_connection(db, "host", 22, "ed25519", "a", true, false)
        .unwrap_or_else(|_| panic!("permit"));
    let (db, result) = permit.finish(true);
    result.unwrap();
    let timeout: u32 = db
        .unwrap()
        .pragma_query_value(None, "busy_timeout", |r| r.get(0))
        .unwrap();
    assert_eq!(timeout, 4321);
}
