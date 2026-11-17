use coshell_storage::{Repository, SecretReference};

#[test]
fn batch_restore_switches_only_staged_secret_in_same_transaction() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,port,username,authentication) VALUES('c','n','h',22,'u','password')", []).unwrap();
    let expected = Repository::new(&mut db).snapshot().unwrap();
    let mut desired = expected.clone();
    desired.secret_refs.push(SecretReference {
        connection_id: "c".into(),
        secret_type: "password".into(),
        secret_id: "new-secret".into(),
    });
    assert!(Repository::new(&mut db)
        .restore_snapshot_with_staged_secrets(&expected, &desired, "batch")
        .is_err());
    db.execute(
        "INSERT INTO restore_secret_receipts VALUES('batch','staged')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO restore_secret_journal VALUES('batch','c','password','new-secret','staged')",
        [],
    )
    .unwrap();
    Repository::new(&mut db)
        .restore_snapshot_with_staged_secrets(&expected, &desired, "batch")
        .unwrap();
    assert_eq!(Repository::new(&mut db).snapshot().unwrap(), desired);
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM restore_secret_journal", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn v8_upgrade_allows_cleanup_of_own_committed_ssh_secret_but_not_staged_reuse() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute_batch("DROP TRIGGER restore_cleanup_owned; DELETE FROM schema_migrations WHERE version>=9; PRAGMA user_version=8;").unwrap();
    db.execute_batch("DROP TRIGGER restore_journal_cross; CREATE TRIGGER restore_journal_cross BEFORE INSERT ON restore_secret_journal WHEN EXISTS(SELECT 1 FROM secret_operation_receipts WHERE operation_id=NEW.batch_id OR secret_id=NEW.secret_id) BEGIN SELECT RAISE(ABORT,'old conflict'); END;").unwrap();
    db.execute("INSERT INTO connections(id,name,host,port,username,authentication) VALUES('c','n','h',22,'u','private_key')", []).unwrap();
    db.execute("INSERT INTO secret_operation_receipts(operation_id,connection_id,secret_type,secret_id,state,cleanup_pending) VALUES('ssh-op','c','ssh_private_key_bundle_v1','old-id','committed',0)", []).unwrap();
    db.execute(
        "INSERT INTO connection_secret_refs VALUES('c','ssh_private_key_bundle_v1','old-id')",
        [],
    )
    .unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    assert!(db.execute("INSERT INTO restore_secret_journal VALUES('batch','c','ssh_private_key_bundle_v1','old-id','cleanup')", []).is_ok());
    assert!(db.execute("INSERT INTO restore_secret_journal VALUES('other','c','ssh_private_key_bundle_v1','old-id','staged')", []).is_err());
}

#[test]
fn v9_upgrade_requires_exact_current_owner_for_cleanup() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute_batch("DROP TRIGGER restore_cleanup_owned; DELETE FROM schema_migrations WHERE version>=10; PRAGMA user_version=9;").unwrap();
    db.execute("INSERT INTO connections(id,name,host,port,username,authentication) VALUES('a','a','h',22,'u','password'),('b','b','h',22,'u','password')", []).unwrap();
    db.execute(
        "INSERT INTO connection_secret_refs VALUES('a','password','owned')",
        [],
    )
    .unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    for (connection, kind, id) in [
        ("b", "password", "owned"),
        ("a", "other", "owned"),
        ("a", "password", "missing"),
    ] {
        assert!(db
            .execute(
                "INSERT INTO restore_secret_journal VALUES('batch',?1,?2,?3,'cleanup')",
                rusqlite::params![connection, kind, id]
            )
            .is_err());
    }
    assert!(db
        .execute(
            "INSERT INTO restore_secret_journal VALUES('batch','a','password','owned','cleanup')",
            []
        )
        .is_ok());
}

#[test]
fn v9_upgrade_fails_closed_on_existing_unowned_cleanup() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute_batch("DROP TRIGGER restore_cleanup_owned; DELETE FROM schema_migrations WHERE version>=10; PRAGMA user_version=9;").unwrap();
    db.execute("INSERT INTO restore_secret_journal VALUES('batch','victim','password','unknown-id','cleanup')", []).unwrap();
    assert!(coshell_storage::migrate(&mut db).is_err());
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        9
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM restore_secret_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn v10_upgrade_rechecks_existing_unowned_cleanup_and_preserves_data() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute_batch("DROP TRIGGER restore_cleanup_owned; DELETE FROM schema_migrations WHERE version=11; PRAGMA user_version=10;").unwrap();
    db.execute("INSERT INTO restore_secret_journal VALUES('batch','victim','password','unknown-id','cleanup')", []).unwrap();
    assert!(coshell_storage::migrate(&mut db).is_err());
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        10
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM restore_secret_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn v10_upgrade_preserves_cleanup_with_matching_committed_receipt() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute_batch("DROP TRIGGER restore_cleanup_owned; DELETE FROM schema_migrations WHERE version=11; PRAGMA user_version=10;").unwrap();
    db.execute("INSERT INTO connections(id,name,host,port,username,authentication) VALUES('c','n','h',22,'u','private_key')", []).unwrap();
    db.execute("INSERT INTO secret_operation_receipts(operation_id,connection_id,secret_type,secret_id,state,cleanup_pending) VALUES('ssh-op','c','ssh_private_key_bundle_v1','old-id','committed',0)", []).unwrap();
    db.execute("INSERT INTO restore_secret_journal VALUES('batch','c','ssh_private_key_bundle_v1','old-id','cleanup')", []).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        11
    );
    let entry: (String, String, String, String, String) = db
        .query_row(
            "SELECT batch_id,connection_id,secret_type,secret_id,kind FROM restore_secret_journal",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(
        entry,
        (
            "batch".into(),
            "c".into(),
            "ssh_private_key_bundle_v1".into(),
            "old-id".into(),
            "cleanup".into()
        )
    );
}
