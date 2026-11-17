use coshell_storage::{ConnectionRecord, Repository};

#[test]
fn restore_rejects_stale_preview_without_changing_newer_data() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let mut repo = Repository::new(&mut db);
    let expected = repo.snapshot().unwrap();
    let desired = expected.clone();
    repo.save_connection(&ConnectionRecord {
        id: "new".into(),
        name: "new".into(),
        host: "host".into(),
        port: 22,
        username: "u".into(),
        authentication: "password".into(),
        group_id: None,
        notes: "".into(),
        production: false,
        always_confirm: false,
        version: 0,
    })
    .unwrap();
    assert!(repo
        .restore_snapshot_if_matches(&expected, &desired)
        .is_err());
    assert!(repo.get_connection("new").unwrap().is_some());
}

#[test]
fn independent_connections_cannot_apply_a_stale_preview() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-restore-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("test.sqlite");
    let mut first = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut first).unwrap();
    let mut second = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut second).unwrap();
    let expected = Repository::new(&mut first).snapshot().unwrap();
    let newer = ConnectionRecord {
        id: "other".into(),
        name: "other".into(),
        host: "host".into(),
        port: 22,
        username: "u".into(),
        authentication: "password".into(),
        group_id: None,
        notes: "".into(),
        production: false,
        always_confirm: false,
        version: 0,
    };
    Repository::new(&mut second)
        .save_connection(&newer)
        .unwrap();
    assert!(Repository::new(&mut first)
        .restore_snapshot_if_matches(&expected, &expected)
        .is_err());
    assert_eq!(
        Repository::new(&mut first).get_connection("other").unwrap(),
        Some(newer)
    );
    drop(first);
    drop(second);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn guarded_restore_cannot_retarget_a_saved_credential() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let mut repo = Repository::new(&mut db);
    repo.save_connection(&ConnectionRecord {
        id: "c".into(),
        name: "c".into(),
        host: "original.example".into(),
        port: 22,
        username: "u".into(),
        authentication: "password".into(),
        group_id: None,
        notes: "".into(),
        production: false,
        always_confirm: false,
        version: 0,
    })
    .unwrap();
    repo.prepare_secret_write("op", "c", "password", "secret")
        .unwrap();
    repo.commit_secret_write("op").unwrap();
    let before = repo.snapshot().unwrap();
    let mut desired = before.clone();
    desired.connections[0].host = "other.example".into();
    assert!(repo.restore_snapshot_if_matches(&before, &desired).is_err());
    assert!(repo.restore_snapshot(&desired).is_err());
    assert_eq!(repo.snapshot().unwrap(), before);
}
