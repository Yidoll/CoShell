use coshell_storage::{migrate, ConnectionRecord, Repository, SecretCleanupKind};
use rusqlite::Connection;

fn setup() -> Connection {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    Repository::new(&mut db)
        .save_connection(&ConnectionRecord {
            id: "c".into(),
            name: "c".into(),
            host: "host".into(),
            port: 22,
            username: "user".into(),
            authentication: "password".into(),
            group_id: None,
            notes: String::new(),
            production: false,
            always_confirm: false,
            version: 0,
        })
        .unwrap();
    db
}

fn seed_ref(db: &mut Connection, id: &str, secret: &str) {
    let mut repo = Repository::new(db);
    repo.prepare_secret_write("seed", "c", id, secret).unwrap();
    repo.commit_secret_write("seed").unwrap();
}

#[test]
fn staged_and_cleanup_survive_repository_restart_and_finish_idempotently() {
    let mut db = setup();
    seed_ref(&mut db, "password", "old");
    {
        let mut repo = Repository::new(&mut db);
        repo.prepare_secret_write("op", "c", "password", "new")
            .unwrap();
        assert_eq!(
            repo.get_secret_ref("c", "password").unwrap().as_deref(),
            Some("old")
        );
    }
    {
        let mut repo = Repository::new(&mut db);
        let pending = repo.pending_secret_cleanup().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].secret_id, "new");
        assert_eq!(pending[0].kind, SecretCleanupKind::Staged);
        repo.commit_secret_write("op").unwrap();
        assert_eq!(
            repo.get_secret_ref("c", "password").unwrap().as_deref(),
            Some("new")
        );
    }
    let mut repo = Repository::new(&mut db);
    let pending = repo.pending_secret_cleanup().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].secret_id, "old");
    assert_eq!(pending[0].kind, SecretCleanupKind::Cleanup);
    repo.finish_secret_cleanup("op", "old").unwrap();
    repo.finish_secret_cleanup("op", "old").unwrap();
    assert!(repo.pending_secret_cleanup().unwrap().is_empty());
}

#[test]
fn prepare_rejects_missing_duplicate_and_active_target_without_side_effects() {
    let mut db = setup();
    let mut repo = Repository::new(&mut db);
    assert!(repo
        .prepare_secret_write("bad", "missing", "password", "new")
        .is_err());
    repo.prepare_secret_write("op", "c", "password", "new")
        .unwrap();
    assert!(repo
        .prepare_secret_write("op", "c", "private_key", "other")
        .is_err());
    assert!(repo
        .prepare_secret_write("other", "c", "password", "other")
        .is_err());
    assert_eq!(repo.pending_secret_cleanup().unwrap().len(), 1);
    assert_eq!(repo.get_secret_ref("c", "password").unwrap(), None);
}

#[test]
fn first_write_has_no_old_secret_to_cleanup_and_commit_failure_is_atomic() {
    let mut db = setup();
    let mut repo = Repository::new(&mut db);
    repo.prepare_secret_write("op", "c", "password", "new")
        .unwrap();
    repo.commit_secret_write("op").unwrap();
    assert!(repo.pending_secret_cleanup().unwrap().is_empty());
    assert!(repo.commit_secret_write("missing").is_err());
    assert_eq!(
        repo.get_secret_ref("c", "password").unwrap().as_deref(),
        Some("new")
    );
}

#[test]
fn commit_failure_preserves_staged_entry_and_original_reference() {
    let mut db = setup();
    seed_ref(&mut db, "password", "old");
    {
        let mut repo = Repository::new(&mut db);
        repo.prepare_secret_write("op", "c", "password", "new")
            .unwrap();
        // A conflicting reference cannot be inserted while the ID is staged.
        assert!(repo.set_secret_ref("c", "private_key", "new").is_err());
    }
    db.execute_batch("CREATE TRIGGER reject_secret BEFORE UPDATE ON connection_secret_refs BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
    {
        let mut repo = Repository::new(&mut db);
        assert!(repo.commit_secret_write("op").is_err());
        assert_eq!(
            repo.get_secret_ref("c", "password").unwrap().as_deref(),
            Some("old")
        );
        let pending = repo.pending_secret_cleanup().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, SecretCleanupKind::Staged);
    }
}

#[test]
fn reused_secret_ids_cannot_be_staged_or_cleaned() {
    let mut db = setup();
    seed_ref(&mut db, "password", "existing");
    let mut repo = Repository::new(&mut db);
    assert!(repo
        .prepare_secret_write("op1", "c", "password", "existing")
        .is_err());
    repo.prepare_secret_write("op2", "c", "password", "new")
        .unwrap();
    assert!(repo.set_secret_ref("c", "private_key", "new").is_err());
    assert!(repo
        .prepare_secret_write("op3", "c", "private_key", "new")
        .is_err());
    assert!(repo.secret_id_is_referenced("existing").unwrap());
    assert!(!repo.secret_id_is_referenced("new").unwrap());
    assert!(repo.finish_secret_cleanup("op2", "different").is_err());
    assert_eq!(repo.pending_secret_cleanup().unwrap().len(), 1);
}

#[test]
fn webdav_references_block_secret_cleanup() {
    let mut db = setup();
    db.execute("INSERT INTO webdav_profiles(id,url,remote_directory,username,credential_secret_id) VALUES ('w','https://example.org','/','u','webdav-secret')", []).unwrap();
    let mut repo = Repository::new(&mut db);
    assert!(repo.secret_id_is_referenced("webdav-secret").unwrap());
    assert!(repo
        .prepare_secret_write("op", "c", "password", "webdav-secret")
        .is_err());
}

#[test]
fn raw_secret_reference_cannot_create_unverified_or_retired_reference() {
    let mut db = setup();
    let mut repo = Repository::new(&mut db);
    assert!(repo.set_secret_ref("c", "password", "unverified").is_err());
    assert_eq!(repo.get_secret_ref("c", "password").unwrap(), None);
}

#[test]
fn pending_cleanup_secret_cannot_be_reintroduced_by_raw_reference() {
    let mut db = setup();
    seed_ref(&mut db, "password", "old");
    let mut repo = Repository::new(&mut db);
    repo.prepare_secret_write("op", "c", "password", "new")
        .unwrap();
    repo.commit_secret_write("op").unwrap();
    assert!(repo.set_secret_ref("c", "private_key", "old").is_err());
    assert!(!repo.secret_id_is_referenced("old").unwrap());
}

#[test]
fn acknowledging_identical_reference_is_idempotent_and_has_no_side_effects() {
    let mut db = setup();
    seed_ref(&mut db, "password", "existing");
    let mut repo = Repository::new(&mut db);
    for _ in 0..2 {
        repo.set_secret_ref("c", "password", "existing").unwrap();
    }
    assert_eq!(
        repo.get_secret_ref("c", "password").unwrap().as_deref(),
        Some("existing")
    );
    assert!(repo.pending_secret_cleanup().unwrap().is_empty());
    assert!(repo.set_secret_ref("c", "password", "different").is_err());
    assert_eq!(
        repo.get_secret_ref("c", "password").unwrap().as_deref(),
        Some("existing")
    );
}

#[test]
fn sql_triggers_reject_staged_and_cleanup_ids_on_insert_and_update() {
    let mut db = setup();
    seed_ref(&mut db, "password", "old");
    db.execute("INSERT INTO webdav_profiles(id,url,remote_directory,username,credential_secret_id) VALUES ('w','https://example.org','/','u','webdav')", []).unwrap();
    Repository::new(&mut db)
        .prepare_secret_write("op", "c", "password", "new")
        .unwrap();
    fn assert_blocked(db: &Connection, id: &str) {
        let statements = [
            "INSERT INTO connection_secret_refs(connection_id,secret_type,secret_id) VALUES ('c','private_key',?1)",
            "UPDATE connection_secret_refs SET secret_id=?1 WHERE connection_id='c' AND secret_type='password'",
            "INSERT INTO webdav_profiles(id,url,remote_directory,username,credential_secret_id) VALUES ('other','https://example.org','/','u',?1)",
            "UPDATE webdav_profiles SET credential_secret_id=?1 WHERE id='w'",
        ];
        for sql in statements {
            let error = db.execute(sql, [id]).unwrap_err();
            assert!(
                error.to_string().contains("secret pending cleanup"),
                "{sql}: {error}"
            );
        }
    }
    assert_blocked(&db, "new");
    Repository::new(&mut db).commit_secret_write("op").unwrap();
    assert_blocked(&db, "old");
    assert_eq!(
        Repository::new(&mut db)
            .get_secret_ref("c", "password")
            .unwrap()
            .as_deref(),
        Some("new")
    );
    assert_eq!(
        db.query_row(
            "SELECT credential_secret_id FROM webdav_profiles WHERE id='w'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "webdav"
    );
}
