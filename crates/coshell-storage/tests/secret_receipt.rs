use coshell_storage::{
    migrate,
    secret_receipt::{ReceiptState, SecretReceiptRepository},
};
use rusqlite::Connection;

#[test]
fn receipt_transitions_are_durable_and_idempotent() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,username,authentication) VALUES ('c','name','host','user','password')",[]).unwrap();
    let mut repo = SecretReceiptRepository::new(&mut db);
    let first = repo.prepare("op", "c", "password", "new").unwrap();
    assert_eq!(first.state, ReceiptState::Staged);
    assert_eq!(
        repo.prepare("op", "c", "password", "different").unwrap(),
        first
    );
    assert!(repo
        .prepare("op", "other", "password", "different")
        .is_err());
    repo.commit("op").unwrap();
    let receipt = repo.get("op").unwrap().unwrap();
    assert_eq!(receipt.state, ReceiptState::Committed);
    assert!(!receipt.cleanup_pending);
    assert_eq!(
        repo.prepare("op", "c", "password", "unused").unwrap(),
        receipt
    );
}

#[test]
fn staged_cleanup_and_committed_old_cleanup_update_receipt() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,username,authentication) VALUES ('c','name','host','user','password')",[]).unwrap();
    let mut repo = SecretReceiptRepository::new(&mut db);
    repo.prepare("abort", "c", "password", "unused").unwrap();
    repo.finish_cleanup("abort", "unused").unwrap();
    assert_eq!(
        repo.get("abort").unwrap().unwrap().state,
        ReceiptState::Aborted
    );
    repo.prepare("first", "c", "password", "old").unwrap();
    repo.commit("first").unwrap();
    repo.prepare("second", "c", "password", "new").unwrap();
    repo.commit("second").unwrap();
    assert!(repo.get("second").unwrap().unwrap().cleanup_pending);
    repo.finish_cleanup("second", "old").unwrap();
    let receipt = repo.get("second").unwrap().unwrap();
    assert_eq!(receipt.state, ReceiptState::Committed);
    assert!(!receipt.cleanup_pending);
    repo.finish_cleanup("second", "old").unwrap();
}

#[test]
fn legacy_journal_entrypoints_cannot_bypass_receipt_state() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,username,authentication) VALUES ('c','name','host','user','password')",[]).unwrap();
    SecretReceiptRepository::new(&mut db)
        .prepare("op", "c", "password", "new")
        .unwrap();
    {
        let mut legacy = coshell_storage::Repository::new(&mut db);
        assert!(legacy.commit_secret_write("op").is_err());
        assert!(legacy.finish_secret_cleanup("op", "new").is_err());
    }
    assert_eq!(
        SecretReceiptRepository::new(&mut db)
            .get("op")
            .unwrap()
            .unwrap()
            .state,
        ReceiptState::Staged
    );
}

#[test]
fn completed_or_aborted_receipt_secret_id_cannot_be_reused_by_legacy_api() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,username,authentication) VALUES ('c','name','host','user','password')",[]).unwrap();
    SecretReceiptRepository::new(&mut db)
        .prepare("op", "c", "password", "retired")
        .unwrap();
    SecretReceiptRepository::new(&mut db)
        .finish_cleanup("op", "retired")
        .unwrap();
    let mut legacy = coshell_storage::Repository::new(&mut db);
    assert!(legacy
        .prepare_secret_write("other", "c", "password", "retired")
        .is_err());
    assert!(legacy.set_secret_ref("c", "password", "retired").is_err());
}

#[test]
fn legacy_prepare_cannot_reuse_a_committed_operation_id() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,username,authentication) VALUES ('c','name','host','user','password')",[]).unwrap();
    let mut receipts = SecretReceiptRepository::new(&mut db);
    receipts.prepare("op", "c", "password", "first").unwrap();
    receipts.commit("op").unwrap();
    let mut legacy = coshell_storage::Repository::new(&mut db);
    assert!(legacy
        .prepare_secret_write("op", "c", "password", "other")
        .is_err());
    assert!(legacy.pending_secret_cleanup().unwrap().is_empty());
    assert_eq!(
        SecretReceiptRepository::new(&mut db)
            .get("op")
            .unwrap()
            .unwrap()
            .state,
        ReceiptState::Committed
    );
}
