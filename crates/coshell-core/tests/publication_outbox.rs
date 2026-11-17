use coshell_backup::webdav::PublishCondition;
use coshell_core::publication_outbox::{Outbox, OutboxError};

#[test]
fn plaintext_remote_package_is_encrypted_before_local_outbox_persistence() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    let plain = coshell_backup::encode_optional(br#"{"private_key":"SENSITIVE_KEY_FIXTURE"}"#, b"")
        .unwrap();
    assert!(Outbox::new(&mut db)
        .unwrap()
        .prepare(
            "op",
            "https://example.org/backup/",
            "s1",
            &plain,
            b"",
            PublishCondition::Absent
        )
        .is_err());
    let key = [7u8; 32];
    {
        let mut outbox = Outbox::new_protected(&mut db, zeroize::Zeroizing::new(key)).unwrap();
        outbox
            .prepare(
                "op",
                "https://example.org/backup/",
                "s1",
                &plain,
                b"",
                PublishCondition::Absent,
            )
            .unwrap();
        assert_eq!(outbox.load("op").unwrap().unwrap().ciphertext, plain);
        outbox
            .prepare(
                "op",
                "https://example.org/backup/",
                "s1",
                &plain,
                b"",
                PublishCondition::Absent,
            )
            .unwrap();
    }
    let stored: Vec<u8> = db
        .query_row(
            "SELECT ciphertext FROM publication_outbox WHERE operation_id='op'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains("SENSITIVE_KEY_FIXTURE"));
    assert!(Outbox::new(&mut db).unwrap().load("op").is_err());
    assert!(
        Outbox::new_protected(&mut db, zeroize::Zeroizing::new([8; 32]))
            .unwrap()
            .load("op")
            .is_err()
    );
    assert_eq!(
        Outbox::new_protected(&mut db, zeroize::Zeroizing::new(key))
            .unwrap()
            .load("op")
            .unwrap()
            .unwrap()
            .ciphertext,
        plain
    );
}

#[test]
fn protected_plaintext_never_appears_in_database_or_wal() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-outbox-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("queue.sqlite");
    let mut db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    let plain =
        coshell_backup::encode_optional(br#"{"private_key":"UNIQUE_PLAINTEXT_KEY_SENTINEL"}"#, b"")
            .unwrap();
    let mut outbox = Outbox::new_protected(&mut db, zeroize::Zeroizing::new([17; 32])).unwrap();
    outbox
        .prepare(
            "op",
            "https://example.org/backup/",
            "s",
            &plain,
            b"",
            PublishCondition::Absent,
        )
        .unwrap();
    assert!(outbox.has_protected_rows().unwrap());
    for path in [&path, &dir.join("queue.sqlite-wal")] {
        if path.exists() {
            assert!(!std::fs::read(path)
                .unwrap()
                .windows(b"UNIQUE_PLAINTEXT_KEY_SENTINEL".len())
                .any(|window| window == b"UNIQUE_PLAINTEXT_KEY_SENTINEL"));
        }
    }
    drop(outbox);
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn legacy_plaintext_is_rejected_without_migration_or_copy() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE publication_outbox (operation_id TEXT PRIMARY KEY, target TEXT NOT NULL, snapshot_id TEXT NOT NULL, ciphertext BLOB NOT NULL, condition_json TEXT NOT NULL, attempt_json TEXT NOT NULL, state TEXT NOT NULL);").unwrap();
    let plain = coshell_backup::encode_optional(br#"{"key":"secret"}"#, b"").unwrap();
    let attempt = coshell_backup::webdav::PublicationAttempt::new(
        "op",
        "https://example.org/backup/",
        "s",
        &plain,
        PublishCondition::Absent,
    )
    .unwrap();
    let attempt = serde_json::to_string(&attempt).unwrap();
    db.execute(
        "INSERT INTO publication_outbox VALUES('op','target','s',?1,'\"Absent\"',?2,'prepared')",
        rusqlite::params![plain, attempt],
    )
    .unwrap();
    assert!(Outbox::new(&mut db).is_err());
    let original: Vec<u8> = db
        .query_row(
            "SELECT ciphertext FROM publication_outbox WHERE operation_id='op'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(original, plain);
}

#[test]
fn operation_is_persisted_and_retries_cannot_change_ciphertext() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    let mut outbox = Outbox::new(&mut db).unwrap();
    let ciphertext = coshell_backup::encrypt(b"data", b"pass").unwrap();
    let other = coshell_backup::encrypt(b"other", b"pass").unwrap();
    let first = outbox
        .prepare(
            "op",
            "https://example.org/backup/",
            "s1",
            &ciphertext,
            b"pass",
            PublishCondition::Absent,
        )
        .unwrap();
    outbox
        .prepare(
            "op2",
            "https://example.org/backup",
            "s2",
            &ciphertext,
            b"pass",
            PublishCondition::Absent,
        )
        .unwrap();
    assert_eq!(outbox.load("op").unwrap().unwrap().ciphertext, ciphertext);
    assert_eq!(
        outbox
            .prepare(
                "op",
                "https://example.org/backup/",
                "s1",
                &other,
                b"pass",
                PublishCondition::Absent
            )
            .unwrap_err(),
        OutboxError::Conflict
    );
    assert_eq!(
        outbox.load("op").unwrap().unwrap().attempt.attempt_id(),
        first.attempt.attempt_id()
    );
    assert_eq!(outbox.claim("op").unwrap(), true);
    assert_eq!(outbox.claim("op2"), Err(OutboxError::TargetBusy));
    assert!(outbox.status("op").unwrap().unwrap().cleanup_pending);
    assert_eq!(outbox.claim("op"), Err(OutboxError::OutcomeUnknown));
    outbox.finish("op", true).unwrap();
    assert_eq!(outbox.claim("op").unwrap(), false);
    assert_eq!(
        outbox
            .prepare(
                "bad",
                "https://example.org/backup/",
                "s",
                b"plain",
                b"pass",
                PublishCondition::Absent
            )
            .unwrap_err(),
        OutboxError::Invalid
    );
}

#[test]
fn legacy_schema_migrates_without_losing_recovery_information() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE publication_outbox (operation_id TEXT PRIMARY KEY, target TEXT NOT NULL, snapshot_id TEXT NOT NULL, ciphertext BLOB NOT NULL, condition_json TEXT NOT NULL, attempt_json TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'prepared' CHECK(state IN ('prepared','running','succeeded','unknown')));").unwrap();
    let encrypted = coshell_backup::encrypt(b"data", b"pass").unwrap();
    let attempt = coshell_backup::webdav::PublicationAttempt::new(
        "op",
        "https://example.org/backup",
        "s1",
        &encrypted,
        PublishCondition::Absent,
    )
    .unwrap();
    db.execute(
        "INSERT INTO publication_outbox VALUES (?1,?2,?3,?4,?5,?6,'unknown')",
        rusqlite::params![
            "op",
            "https://example.org/backup",
            "s1",
            encrypted,
            serde_json::to_string(&PublishCondition::Absent).unwrap(),
            serde_json::to_string(&attempt).unwrap()
        ],
    )
    .unwrap();
    let outbox = Outbox::new(&mut db).unwrap();
    let status = outbox.status("op").unwrap().unwrap();
    assert_eq!(status.state, "unknown");
    assert!(status.cleanup_pending);
    assert_eq!(status.possible_objects, attempt.possible_objects());
    assert_eq!(
        outbox.load("op").unwrap().unwrap().target,
        "https://example.org/backup/"
    );
}

#[tokio::test]
async fn cancelled_publication_reports_unknown_and_preserves_details() {
    use coshell_backup::webdav::{Cancellation, WebDav};
    use std::time::Duration;
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    let mut outbox = Outbox::new(&mut db).unwrap();
    let encrypted = coshell_backup::encrypt(b"data", b"pass").unwrap();
    outbox
        .prepare(
            "op",
            "http://127.0.0.1:9/",
            "s1",
            &encrypted,
            b"pass",
            PublishCondition::Absent,
        )
        .unwrap();
    let client = WebDav::new("http://127.0.0.1:9/", "u", "p", Duration::from_secs(1)).unwrap();
    let cancel = Cancellation::new();
    cancel.cancel();
    assert_eq!(
        outbox.publish_once("op", &client, &cancel).await,
        Err(OutboxError::OutcomeUnknown)
    );
    let status = outbox.status("op").unwrap().unwrap();
    assert_eq!(status.state, "unknown");
    assert_eq!(status.error.as_deref(), Some("Cancelled"));
    assert!(status.cleanup_pending);
    assert!(!status.possible_objects.is_empty());
    assert_eq!(outbox.recent().unwrap()[0].operation_id, "op");
}

#[test]
fn interrupted_claim_survives_reopen_and_blocks_target() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-outbox-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("outbox.sqlite");
    let encrypted = coshell_backup::encrypt(b"data", b"pass").unwrap();
    {
        let mut db = rusqlite::Connection::open(&path).unwrap();
        let mut outbox = Outbox::new(&mut db).unwrap();
        outbox
            .prepare(
                "first",
                "https://example.org/",
                "s1",
                &encrypted,
                b"pass",
                PublishCondition::Absent,
            )
            .unwrap();
        outbox
            .prepare(
                "second",
                "https://example.org/",
                "s2",
                &encrypted,
                b"pass",
                PublishCondition::Absent,
            )
            .unwrap();
        assert!(outbox.claim("first").unwrap());
    }
    {
        let mut db = rusqlite::Connection::open(&path).unwrap();
        let mut outbox = Outbox::new(&mut db).unwrap();
        assert_eq!(outbox.claim("first"), Err(OutboxError::OutcomeUnknown));
        assert_eq!(outbox.claim("second"), Err(OutboxError::TargetBusy));
        let status = outbox.status("first").unwrap().unwrap();
        assert!(status.cleanup_pending);
        assert!(!status.possible_objects.is_empty());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_legacy_attempt_aborts_migration_without_losing_rows() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE publication_outbox (operation_id TEXT PRIMARY KEY, target TEXT NOT NULL, snapshot_id TEXT NOT NULL, ciphertext BLOB NOT NULL, condition_json TEXT NOT NULL, attempt_json TEXT NOT NULL, state TEXT NOT NULL); INSERT INTO publication_outbox VALUES('op','target','s',x'00','{}','bad','unknown');").unwrap();
    assert!(matches!(Outbox::new(&mut db), Err(OutboxError::Storage)));
    let attempt: String = db
        .query_row(
            "SELECT attempt_json FROM publication_outbox WHERE operation_id='op'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(attempt, "bad");
}
