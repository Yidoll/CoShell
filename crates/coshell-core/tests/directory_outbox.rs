use coshell_core::directory_outbox::{DirectoryOutbox, QueueError};
use zeroize::Zeroizing;
fn package() -> Zeroizing<Vec<u8>> {
    coshell_backup::snapshot_manifest::build_manifest(
        "snap",
        "2026-11-16T03:18:18Z",
        "0.1.0",
        coshell_backup::snapshot_manifest::SnapshotProtection::Plaintext,
        b"SENSITIVE_DIRECTORY_PACKAGE",
        None,
        0,
        0,
    )
    .unwrap()
    .pack()
    .unwrap()
}

#[test]
fn durable_package_is_protected_and_operation_bound() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    let key = Zeroizing::new([19; 32]);
    let body = package();
    {
        let mut queue = DirectoryOutbox::new(&mut db, key).unwrap();
        queue
            .prepare(
                "operation",
                "https://example.org/backup/",
                "20261116031818",
                &body,
            )
            .unwrap();
        assert!(queue.has_protected_rows().unwrap());
        let loaded = queue.load("operation").unwrap().unwrap();
        assert_eq!(loaded.body.as_slice(), body.as_slice());
        assert_eq!(loaded.state, "prepared");
        assert_eq!(
            queue.prepare(
                "operation",
                "https://other.example/",
                "20261116031818",
                &body
            ),
            Err(QueueError::Conflict)
        );
        assert!(queue.claim("operation").unwrap());
        assert!(!queue.claim("operation").unwrap());
    }
    let raw: Vec<u8> = db
        .query_row("SELECT package FROM directory_publications", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains("SENSITIVE_DIRECTORY_PACKAGE"));
    let mut reopened = DirectoryOutbox::new(&mut db, Zeroizing::new([19; 32])).unwrap();
    assert_eq!(
        reopened.load("operation").unwrap().unwrap().state,
        "running"
    );
    assert!(!reopened.claim("operation").unwrap());
    reopened.finish("operation", "succeeded", None).unwrap();
    assert_eq!(
        reopened.status("operation").unwrap().unwrap().state,
        "succeeded"
    );
}

#[test]
fn wrong_key_and_cross_directory_tampering_are_rejected() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    let body = package();
    DirectoryOutbox::new(&mut db, Zeroizing::new([1; 32]))
        .unwrap()
        .prepare("op", "https://example.org/", "20261116031818", &body)
        .unwrap();
    assert!(DirectoryOutbox::new(&mut db, Zeroizing::new([2; 32]))
        .unwrap()
        .load("op")
        .is_err());
    db.execute(
        "UPDATE directory_publications SET directory='20261116031819'",
        [],
    )
    .unwrap();
    assert!(DirectoryOutbox::new(&mut db, Zeroizing::new([1; 32]))
        .unwrap()
        .load("op")
        .is_err());
}

#[test]
fn invalid_artifacts_and_equivalent_targets_cannot_bypass_the_queue() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    let body = package();
    let mut queue = DirectoryOutbox::new(&mut db, Zeroizing::new([4; 32])).unwrap();
    assert_eq!(
        queue.prepare(
            "invalid",
            "https://example.org/",
            "20261116031818",
            b"not-an-artifact"
        ),
        Err(QueueError::Invalid)
    );
    queue
        .prepare(
            "first",
            "https://EXAMPLE.org:443/backup/",
            "20261116031818",
            &body,
        )
        .unwrap();
    assert!(queue.claim("first").unwrap());
    queue
        .prepare(
            "second",
            "https://example.org/backup/",
            "20261116031819",
            &body,
        )
        .unwrap();
    assert_eq!(queue.claim("second"), Err(QueueError::Conflict));
}

#[test]
fn protected_packages_do_not_leak_into_database_or_wal() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-dir-queue-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("outbox.sqlite");
    let mut db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    DirectoryOutbox::new(&mut db, Zeroizing::new([21; 32]))
        .unwrap()
        .prepare("op", "https://example.org/", "20261116031818", &package())
        .unwrap();
    for p in [&path, &dir.join("outbox.sqlite-wal")] {
        let bytes = std::fs::read(p).unwrap();
        assert!(!bytes
            .windows(b"SENSITIVE_DIRECTORY_PACKAGE".len())
            .any(|w| w == b"SENSITIVE_DIRECTORY_PACKAGE"));
    }
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}
