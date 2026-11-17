use coshell_core::backup_inspection::{inspect, InspectionError};

#[test]
fn secret_payload_is_available_only_to_trusted_restore_and_never_in_preview() {
    let clear = br#"{"version":1,"groups":[],"connections":[{"id":"c","name":"host","host":"host","port":22,"username":"u","authentication":"password","group_id":null,"notes":"","production":false,"always_confirm":false,"version":0}],"host_keys":[],"secrets":[{"connection_id":"c","secret_type":"password","value":[115,101,99,114,101,116]}]}"#;
    let encoded = coshell_backup::encode_optional(clear, b"").unwrap();
    let (preview, secrets) =
        coshell_core::backup_inspection::inspect_for_restore(&encoded, b"").unwrap();
    assert_eq!(secrets.len(), 1);
    assert_eq!(secrets[0].value(), b"secret");
    assert!(!serde_json::to_string(&preview).unwrap().contains("secret"));
    assert!(!format!("{:?}", secrets[0]).contains("[115, 101, 99, 114, 101, 116]"));
}

#[test]
fn unencrypted_snapshot_can_be_inspected_with_empty_password() {
    let clear = br#"{"version":1,"groups":[],"connections":[],"host_keys":[],"secrets":[]}"#;
    let bytes = coshell_backup::encode_optional(clear, b"").unwrap();
    assert!(inspect(&bytes, b"").unwrap().connections.is_empty());
    assert_eq!(
        inspect(&bytes, b"nonempty"),
        Err(InspectionError::Authentication)
    );
}

#[test]
fn authenticated_preview_rejects_wrong_password_and_secret_payload() {
    let clear = br#"{"version":1,"groups":[],"connections":[],"host_keys":[],"secrets":[]}"#;
    let bytes = coshell_backup::encrypt(clear, b"pass").unwrap();
    let preview = inspect(&bytes, b"pass").unwrap();
    assert!(preview.connections.is_empty());
    assert_eq!(
        inspect(&bytes, b"wrong"),
        Err(InspectionError::Authentication)
    );
    let secret =
        br#"{"version":1,"groups":[],"connections":[],"host_keys":[],"secrets":[{"value":[1]}]}"#;
    let bytes = coshell_backup::encrypt(secret, b"pass").unwrap();
    assert_eq!(
        inspect(&bytes, b"pass"),
        Err(InspectionError::SecretsUnsupported)
    );
}

#[test]
fn rejects_duplicate_connection_ids_in_untrusted_backup() {
    let clear = br#"{"version":1,"groups":[],"connections":[{"id":"c","name":"a","host":"a","port":22,"username":"u","authentication":"password","group_id":null,"notes":"","production":false,"always_confirm":false,"version":0},{"id":"c","name":"b","host":"b","port":22,"username":"u","authentication":"password","group_id":null,"notes":"","production":true,"always_confirm":false,"version":0}],"host_keys":[],"secrets":[]}"#;
    let bytes = coshell_backup::encrypt(clear, b"pass").unwrap();
    assert_eq!(
        inspect(&bytes, b"pass"),
        Err(InspectionError::InvalidPayload)
    );
}

#[test]
fn preview_marks_existing_connections_without_changing_them() {
    use coshell_core::backup_inspection::compare_connections;
    let clear = br#"{"version":1,"groups":[],"connections":[{"id":"c","name":"remote","host":"example.org","port":22,"username":"u","authentication":"password","group_id":null,"notes":"","production":true,"always_confirm":false,"version":0}],"host_keys":[],"secrets":[]}"#;
    let bytes = coshell_backup::encrypt(clear, b"pass").unwrap();
    let incoming = inspect(&bytes, b"pass").unwrap();
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let mut repo = coshell_storage::Repository::new(&mut db);
    let record = coshell_storage::ConnectionRecord {
        id: "c".into(),
        name: "local".into(),
        host: "localhost".into(),
        port: 22,
        username: "u".into(),
        authentication: "password".into(),
        group_id: None,
        notes: "".into(),
        production: false,
        always_confirm: false,
        version: 3,
    };
    repo.save_connection(&record).unwrap();
    let result = compare_connections(&incoming, &repo.list_connections().unwrap());
    assert_eq!(result[0].status, "conflict");
    assert_eq!(repo.get_connection("c").unwrap(), Some(record));
}

#[test]
fn restore_choices_are_prepared_without_writes_and_applied_with_guard() {
    use coshell_core::backup_inspection::{prepare_restore, RestoreChoice};
    let clear = br#"{"version":1,"groups":[],"connections":[{"id":"c","name":"remote","host":"example.org","port":22,"username":"u","authentication":"password","group_id":null,"notes":"","production":true,"always_confirm":false,"version":0}],"host_keys":[],"secrets":[]}"#;
    let incoming = inspect(&coshell_backup::encrypt(clear, b"pass").unwrap(), b"pass").unwrap();
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let mut repo = coshell_storage::Repository::new(&mut db);
    let local = coshell_storage::ConnectionRecord {
        id: "c".into(),
        name: "local".into(),
        host: "localhost".into(),
        port: 22,
        username: "u".into(),
        authentication: "password".into(),
        group_id: None,
        notes: "".into(),
        production: false,
        always_confirm: false,
        version: 3,
    };
    repo.save_connection(&local).unwrap();
    let before = repo.snapshot().unwrap();
    let mut choices = std::collections::HashMap::new();
    choices.insert("c".into(), RestoreChoice::Keep);
    assert_eq!(
        prepare_restore(&incoming, &before, &choices).unwrap(),
        before
    );
    choices.insert(
        "c".into(),
        RestoreChoice::Copy {
            id: "copy".into(),
            name: "copy".into(),
        },
    );
    let desired = prepare_restore(&incoming, &before, &choices).unwrap();
    assert_eq!(repo.snapshot().unwrap(), before);
    repo.restore_snapshot_if_matches(&before, &desired).unwrap();
    assert_eq!(repo.get_connection("c").unwrap(), Some(local));
    assert!(repo.get_connection("copy").unwrap().unwrap().production);
    let now = repo.snapshot().unwrap();
    choices.insert("c".into(), RestoreChoice::Replace);
    let desired = prepare_restore(&incoming, &now, &choices).unwrap();
    repo.restore_snapshot_if_matches(&now, &desired).unwrap();
    assert_eq!(repo.get_connection("c").unwrap().unwrap().version, 4);
}

#[test]
fn restore_refuses_to_move_saved_credential_to_another_host() {
    use coshell_core::backup_inspection::{prepare_restore, RestoreChoice};
    let clear = br#"{"version":1,"groups":[],"connections":[{"id":"c","name":"remote","host":"evil.example","port":22,"username":"u","authentication":"password","group_id":null,"notes":"","production":false,"always_confirm":false,"version":0}],"host_keys":[],"secrets":[]}"#;
    let incoming = inspect(&coshell_backup::encrypt(clear, b"pass").unwrap(), b"pass").unwrap();
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let mut repo = coshell_storage::Repository::new(&mut db);
    repo.save_connection(&coshell_storage::ConnectionRecord {
        id: "c".into(),
        name: "local".into(),
        host: "good.example".into(),
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
    repo.prepare_secret_write("op", "c", "password", "secret-id")
        .unwrap();
    repo.commit_secret_write("op").unwrap();
    let before = repo.snapshot().unwrap();
    let choices = [("c".into(), RestoreChoice::Replace)].into_iter().collect();
    assert_eq!(
        prepare_restore(&incoming, &before, &choices),
        Err(InspectionError::SecretsUnsupported)
    );
    assert_eq!(repo.snapshot().unwrap(), before);
}

#[test]
fn restore_never_overwrites_local_host_key() {
    use coshell_core::backup_inspection::prepare_restore_with_groups;
    let clear = br#"{"version":1,"groups":[{"id":"g","name":"group","parent_id":null}],"connections":[],"host_keys":[{"host":"example.org","port":22,"algorithm":"ssh-ed25519","fingerprint":"new"}],"secrets":[]}"#;
    let incoming = inspect(&coshell_backup::encrypt(clear, b"pass").unwrap(), b"pass").unwrap();
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute("INSERT INTO trusted_host_keys(host,port,algorithm,fingerprint) VALUES('example.org',22,'ssh-ed25519','old')", []).unwrap();
    let mut repo = coshell_storage::Repository::new(&mut db);
    let before = repo.snapshot().unwrap();
    let desired =
        prepare_restore_with_groups(&incoming, &before, &Default::default(), true).unwrap();
    repo.restore_snapshot_if_matches(&before, &desired).unwrap();
    assert_eq!(repo.snapshot().unwrap().host_keys, before.host_keys);
}

#[test]
fn restore_does_not_silently_remap_conflicting_group() {
    use coshell_core::backup_inspection::prepare_restore;
    let clear = br#"{"version":1,"groups":[{"id":"g","name":"remote","parent_id":null}],"connections":[],"host_keys":[],"secrets":[]}"#;
    let incoming = inspect(&coshell_backup::encrypt(clear, b"pass").unwrap(), b"pass").unwrap();
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let mut repo = coshell_storage::Repository::new(&mut db);
    repo.save_group(&coshell_storage::GroupRecord {
        id: "g".into(),
        name: "local".into(),
        parent_id: None,
    })
    .unwrap();
    let before = repo.snapshot().unwrap();
    assert_eq!(
        prepare_restore(&incoming, &before, &Default::default()),
        Err(InspectionError::InvalidPayload)
    );
    assert_eq!(repo.snapshot().unwrap(), before);
}

#[test]
fn restore_does_not_silently_trust_backup_host_keys() {
    use coshell_core::backup_inspection::prepare_restore;
    let clear = br#"{"version":1,"groups":[],"connections":[],"host_keys":[{"host":"example.org","port":22,"algorithm":"ssh-ed25519","fingerprint":"unverified"}],"secrets":[]}"#;
    let incoming = inspect(&coshell_backup::encrypt(clear, b"pass").unwrap(), b"pass").unwrap();
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let current = coshell_storage::Repository::new(&mut db)
        .snapshot()
        .unwrap();
    let desired = prepare_restore(&incoming, &current, &Default::default()).unwrap();
    assert!(desired.host_keys.is_empty());
}

#[test]
fn restore_rejects_unconfirmed_new_groups() {
    use coshell_core::backup_inspection::{prepare_restore, prepare_restore_with_groups};
    let clear = br#"{"version":1,"groups":[{"id":"g","name":"new group","parent_id":null}],"connections":[],"host_keys":[],"secrets":[]}"#;
    let incoming = inspect(&coshell_backup::encrypt(clear, b"pass").unwrap(), b"pass").unwrap();
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let current = coshell_storage::Repository::new(&mut db)
        .snapshot()
        .unwrap();
    assert_eq!(
        prepare_restore(&incoming, &current, &Default::default()),
        Err(InspectionError::InvalidPayload)
    );
    assert_eq!(incoming.groups[0].name, "new group");
    assert_eq!(
        prepare_restore_with_groups(&incoming, &current, &Default::default(), true)
            .unwrap()
            .groups
            .len(),
        1
    );
}
