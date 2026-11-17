use coshell_storage::{
    migrate, secret_receipt::SecretReceiptRepository,
    webdav_secret_receipt::WebDavSecretReceiptRepository,
};

fn database() -> rusqlite::Connection {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    db.execute_batch("INSERT INTO connections(id,name,host,username,authentication) VALUES ('c','host','host','u','password'); INSERT INTO webdav_profiles(id,url,remote_directory,username) VALUES ('p','https://example.org','/','u'), ('q','https://example.org','/','u');").unwrap();
    db
}

// Remove v8+ objects before constructing any v3/v5/v6 migration fixture.
// Several triggers are attached to older tables and survive table deletion.
fn remove_v8_and_later(db: &rusqlite::Connection) {
    db.execute_batch("DROP TRIGGER restore_ref_pending_insert; DROP TRIGGER restore_ref_pending_update; DROP TRIGGER restore_webdav_pending_insert; DROP TRIGGER restore_webdav_pending_update; DROP TRIGGER restore_ssh_journal_cross; DROP TRIGGER restore_webdav_journal_cross; DROP TABLE restore_secret_journal; DROP TABLE restore_secret_receipts; DELETE FROM schema_migrations WHERE version>=8;").unwrap();
}

#[test]
fn pending_cross_type_ids_and_direct_reference_writes_are_rejected() {
    let mut db = database();
    SecretReceiptRepository::new(&mut db)
        .prepare("ssh-op", "c", "password", "ssh-id")
        .unwrap();
    assert!(WebDavSecretReceiptRepository(&mut db)
        .prepare("ssh-op", "p", "webdav_password", "other-id")
        .is_err());
    assert!(WebDavSecretReceiptRepository(&mut db)
        .prepare("dav-op", "p", "webdav_password", "ssh-id")
        .is_err());
    assert!(db
        .execute(
            "UPDATE webdav_profiles SET credential_secret_id='ssh-id' WHERE id='p'",
            []
        )
        .is_err());
    WebDavSecretReceiptRepository(&mut db)
        .prepare("dav-op", "p", "webdav_password", "dav-id")
        .unwrap();
    assert!(SecretReceiptRepository::new(&mut db)
        .prepare("dav-op", "c", "private_key", "other-id")
        .is_err());
    assert!(SecretReceiptRepository::new(&mut db)
        .prepare("ssh-op2", "c", "private_key", "dav-id")
        .is_err());
    assert!(db.execute("INSERT INTO connection_secret_refs(connection_id,secret_type,secret_id) VALUES ('c','private_key','dav-id')",[]).is_err());
    assert!(db
        .execute(
            "UPDATE webdav_profiles SET credential_secret_id='dav-id' WHERE id='q'",
            []
        )
        .is_err());
}

#[test]
fn v3_to_v4_preserves_pending_ssh_receipts_and_journal() {
    let mut db = database();
    remove_v8_and_later(&db);
    SecretReceiptRepository::new(&mut db)
        .prepare("ssh-op", "c", "password", "ssh-id")
        .unwrap();
    db.execute_batch("DROP TRIGGER backup_run_prune_aborted_profile; DROP TRIGGER backup_runs_provisional_insert; DROP TRIGGER backup_runs_provisional_update; DROP VIEW active_webdav_profiles; DROP TRIGGER webdav_provisional_block_update; DROP TRIGGER webdav_provisional_block_delete; DROP TABLE webdav_provisional_profiles; DROP TRIGGER connection_ref_webdav_pending_insert; DROP TRIGGER connection_ref_webdav_pending_update; DROP TRIGGER webdav_ref_webdav_pending_insert; DROP TRIGGER webdav_ref_webdav_pending_update; DROP TRIGGER ssh_journal_cross_operation; DROP TRIGGER webdav_journal_cross_operation; DROP TABLE webdav_secret_journal; DROP TABLE webdav_secret_receipts; DELETE FROM schema_migrations WHERE version>=4; PRAGMA user_version=3;").unwrap();
    migrate(&mut db).unwrap();
    let receipt = SecretReceiptRepository::new(&mut db)
        .get("ssh-op")
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.state,
        coshell_storage::secret_receipt::ReceiptState::Staged
    );
    let (id, kind): (String, String) = db
        .query_row(
            "SELECT secret_id,kind FROM secret_write_journal WHERE operation_id='ssh-op'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((id.as_str(), kind.as_str()), ("ssh-id", "staged"));
    assert!(WebDavSecretReceiptRepository(&mut db)
        .prepare("dav-op", "p", "webdav_password", "ssh-id")
        .is_err());
}

#[test]
fn new_profile_and_staged_receipt_are_atomic_and_abort_removes_profile() {
    let mut db = database();
    let mut repo = WebDavSecretReceiptRepository(&mut db);
    assert!(repo
        .prepare_new_profile(
            "op",
            "new",
            "https://example.org/",
            "backups",
            "alice",
            false,
            "new-secret"
        )
        .is_ok());
    assert!(repo.get("op").unwrap().is_some());
    repo.finish("op", "new-secret").unwrap();
    assert_eq!(
        repo.get("op").unwrap().unwrap().state,
        coshell_storage::secret_receipt::ReceiptState::Aborted
    );
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM webdav_profiles WHERE id='new'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    let mut repo = WebDavSecretReceiptRepository(&mut db);
    assert!(repo
        .prepare_new_profile(
            "another",
            "p",
            "https://example.org/",
            "backups",
            "alice",
            false,
            "another-secret"
        )
        .is_err());
    assert!(repo.get("another").unwrap().is_none());
}

#[test]
fn v5_pending_profile_with_backup_runs_can_be_recovered_without_losing_runs() {
    let mut db = database();
    WebDavSecretReceiptRepository(&mut db)
        .prepare_new_profile(
            "legacy",
            "pending",
            "https://example.org/",
            "backups",
            "alice",
            false,
            "secret",
        )
        .unwrap();
    remove_v8_and_later(&db);
    db.execute_batch("DROP TRIGGER backup_run_prune_aborted_profile; DROP TRIGGER backup_runs_provisional_insert; DROP TRIGGER backup_runs_provisional_update; DROP VIEW active_webdav_profiles; DELETE FROM schema_migrations WHERE version>=6; PRAGMA user_version=5; INSERT INTO backup_runs(id,profile_id,snapshot_id,state,started_at) VALUES ('run','pending','snap','failed','now'),('run2','pending','snap2','failed','now');").unwrap();
    migrate(&mut db).unwrap();
    WebDavSecretReceiptRepository(&mut db)
        .finish("legacy", "secret")
        .unwrap();
    assert_eq!(
        WebDavSecretReceiptRepository(&mut db)
            .get("legacy")
            .unwrap()
            .unwrap()
            .state,
        coshell_storage::secret_receipt::ReceiptState::Aborted
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM backup_runs WHERE id='run'", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM active_webdav_profiles WHERE id='pending'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(db.execute("INSERT INTO backup_runs(id,profile_id,snapshot_id,state,started_at) VALUES ('another','pending','snap2','queued','now')",[]).is_err());
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM webdav_provisional_profiles WHERE operation_id='legacy'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    // Deleting one of several historical runs must not remove the profile.
    db.execute("DELETE FROM backup_runs WHERE id='run'", [])
        .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM webdav_provisional_profiles WHERE operation_id='legacy'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    db.execute("DELETE FROM backup_runs WHERE id='run2'", [])
        .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM webdav_provisional_profiles WHERE operation_id='legacy'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM webdav_profiles WHERE id='pending'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        WebDavSecretReceiptRepository(&mut db)
            .get("legacy")
            .unwrap()
            .unwrap()
            .state,
        coshell_storage::secret_receipt::ReceiptState::Aborted
    );
}

#[test]
fn v6_to_v7_reclaims_aborted_profiles_with_no_remaining_runs() {
    let mut db = database();
    WebDavSecretReceiptRepository(&mut db)
        .prepare_new_profile(
            "op",
            "temporary",
            "https://example.org/",
            "backups",
            "alice",
            false,
            "secret",
        )
        .unwrap();
    db.execute_batch("DROP TRIGGER backup_runs_provisional_insert; INSERT INTO backup_runs(id,profile_id,snapshot_id,state,started_at) VALUES ('run','temporary','snap','failed','now'); CREATE TRIGGER backup_runs_provisional_insert BEFORE INSERT ON backup_runs WHEN EXISTS(SELECT 1 FROM webdav_provisional_profiles WHERE profile_id=NEW.profile_id) BEGIN SELECT RAISE(ABORT,'profile creation pending'); END;").unwrap();
    WebDavSecretReceiptRepository(&mut db)
        .finish("op", "secret")
        .unwrap();
    remove_v8_and_later(&db);
    db.execute_batch("DROP TRIGGER backup_run_prune_aborted_profile; DELETE FROM backup_runs WHERE id='run'; DELETE FROM schema_migrations WHERE version=7; PRAGMA user_version=6;").unwrap();
    migrate(&mut db).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM webdav_profiles WHERE id='temporary'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        WebDavSecretReceiptRepository(&mut db)
            .get("op")
            .unwrap()
            .unwrap()
            .state,
        coshell_storage::secret_receipt::ReceiptState::Aborted
    );
}

#[test]
fn cleanup_preserves_staged_credentialed_and_regular_profiles() {
    let mut db = database();
    WebDavSecretReceiptRepository(&mut db)
        .prepare_new_profile(
            "staged-op",
            "staged",
            "https://example.org/",
            "backups",
            "u",
            false,
            "staged-id",
        )
        .unwrap();
    db.execute_batch("INSERT INTO webdav_profiles(id,url,remote_directory,username,credential_secret_id) VALUES ('credentialed','https://example.org/','/','u','existing-id'); INSERT INTO webdav_secret_receipts(operation_id,profile_id,secret_id,state,cleanup_pending) VALUES ('aborted-op','credentialed','prior-id','aborted',0); INSERT INTO webdav_provisional_profiles(operation_id,profile_id) VALUES ('aborted-op','credentialed'); DROP TRIGGER backup_runs_provisional_insert; INSERT INTO backup_runs(id,profile_id,snapshot_id,state,started_at) VALUES ('r1','staged','s1','failed','now'),('r2','credentialed','s2','failed','now'),('r3','p','s3','failed','now');").unwrap();
    db.execute_batch("DELETE FROM backup_runs WHERE id IN ('r1','r2','r3');")
        .unwrap();
    for id in ["staged", "credentialed", "p"] {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM webdav_profiles WHERE id=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1,
            "{id}"
        );
    }
    assert_eq!(db.query_row("SELECT COUNT(*) FROM webdav_provisional_profiles WHERE profile_id IN ('staged','credentialed')",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    assert_eq!(
        db.query_row(
            "SELECT credential_secret_id FROM webdav_profiles WHERE id='credentialed'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "existing-id"
    );
}

#[test]
fn v7_migration_reclaims_only_unreferenced_aborted_profiles_in_mixed_data() {
    let mut db = database();
    remove_v8_and_later(&db);
    WebDavSecretReceiptRepository(&mut db)
        .prepare_new_profile(
            "staged-op",
            "staged",
            "https://example.org/",
            "backups",
            "u",
            false,
            "staged-id",
        )
        .unwrap();
    db.execute_batch("INSERT INTO webdav_profiles(id,url,remote_directory,username,credential_secret_id) VALUES ('credentialed','https://example.org/','/','u','existing-id'); INSERT INTO webdav_profiles(id,url,remote_directory,username) VALUES ('empty','https://example.org/','/','u'),('retained','https://example.org/','/','u'); INSERT INTO webdav_secret_receipts(operation_id,profile_id,secret_id,state,cleanup_pending) VALUES ('cred-op','credentialed','prior-id','aborted',0),('empty-op','empty','empty-id','aborted',0),('retained-op','retained','retained-id','aborted',0); INSERT INTO webdav_provisional_profiles(operation_id,profile_id) VALUES ('cred-op','credentialed'),('empty-op','empty'),('retained-op','retained'); DROP TRIGGER backup_runs_provisional_insert; INSERT INTO backup_runs(id,profile_id,snapshot_id,state,started_at) VALUES ('legacy','retained','snap','failed','now'); CREATE TRIGGER backup_runs_provisional_insert BEFORE INSERT ON backup_runs WHEN EXISTS(SELECT 1 FROM webdav_provisional_profiles WHERE profile_id=NEW.profile_id) BEGIN SELECT RAISE(ABORT,'profile creation pending'); END; DROP TRIGGER backup_run_prune_aborted_profile; DELETE FROM schema_migrations WHERE version=7; PRAGMA user_version=6;").unwrap();
    migrate(&mut db).unwrap();
    for id in ["staged", "credentialed", "retained", "p"] {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM webdav_profiles WHERE id=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1,
            "{id}"
        );
    }
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM webdav_profiles WHERE id='empty'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM backup_runs WHERE id='legacy'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(db.query_row("SELECT COUNT(*) FROM webdav_secret_receipts WHERE operation_id='empty-op' AND state='aborted'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
}
