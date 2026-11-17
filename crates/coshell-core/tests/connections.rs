use coshell_core::connections::ConnectionService;
use coshell_core::feature_flags::{Feature, FeatureFlags};
use coshell_storage::{ConnectionRecord, GroupRecord};

fn record(id: &str, name: &str) -> ConnectionRecord {
    ConnectionRecord {
        id: id.into(),
        name: name.into(),
        host: "example.org".into(),
        port: 22,
        username: "user".into(),
        authentication: "password".into(),
        group_id: None,
        notes: String::new(),
        production: false,
        always_confirm: false,
        version: 0,
    }
}
fn service(enabled: bool) -> ConnectionService {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    let flags = if enabled {
        FeatureFlags::default().with_enabled(Feature::Connections)
    } else {
        FeatureFlags::default()
    };
    ConnectionService::new(db, flags).unwrap()
}

#[test]
fn disabled_feature_has_no_side_effects() {
    let service = service(false);
    assert_eq!(
        service.save(&record("id", "name")).unwrap_err().code,
        "UNSUPPORTED"
    );
    assert_eq!(service.list().unwrap_err().code, "UNSUPPORTED");
}

#[test]
fn invalid_authentication_is_an_invalid_input_not_a_conflict() {
    let service = service(true);
    let mut connection = record("id", "name");
    connection.authentication = "bogus".into();
    assert_eq!(
        service.save(&connection).unwrap_err().code,
        "CONNECTION_INVALID"
    );
    assert!(service.list().unwrap().is_empty());
}

#[test]
fn metadata_crud_copy_and_group_preserve_production() {
    let service = service(true);
    service
        .save_group(&GroupRecord {
            id: "g".into(),
            name: "group".into(),
            parent_id: None,
        })
        .unwrap();
    let mut original = record("one", "server");
    original.production = true;
    original.group_id = Some("g".into());
    service.save(&original).unwrap();
    let copy = service.copy("one", "two", "copy").unwrap();
    assert!(copy.production);
    assert_eq!(copy.group_id.as_deref(), Some("g"));
    assert_eq!(service.list().unwrap().len(), 2);
    assert_eq!(service.get("one").unwrap(), Some(original));
    let mut changed = service.get("one").unwrap().unwrap();
    changed.notes = "changed".into();
    service.save(&changed).unwrap();
    assert_eq!(
        service.save(&changed).unwrap_err().code,
        "CONNECTION_CONFLICT"
    );
}

#[test]
fn independent_services_reject_stale_writes_and_duplicate_names() {
    let path = std::env::temp_dir().join(format!(
        "coshell-connections-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let flags = FeatureFlags::default().with_enabled(Feature::Connections);
    let a =
        ConnectionService::new(rusqlite::Connection::open(&path).unwrap(), flags.clone()).unwrap();
    let b = ConnectionService::new(rusqlite::Connection::open(&path).unwrap(), flags).unwrap();
    a.save(&record("one", "name")).unwrap();
    let stale = b.get("one").unwrap().unwrap();
    let mut fresh = a.get("one").unwrap().unwrap();
    fresh.production = true;
    a.save(&fresh).unwrap();
    assert_eq!(b.save(&stale).unwrap_err().code, "CONNECTION_CONFLICT");
    assert!(b.get("one").unwrap().unwrap().production);
    assert_eq!(
        b.save(&record("two", "name")).unwrap_err().code,
        "CONNECTION_CONFLICT"
    );
    drop(a);
    drop(b);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn groups_can_be_listed_and_deleted_without_deleting_connections() {
    let service = service(true);
    let group = GroupRecord {
        id: "g".into(),
        name: "group".into(),
        parent_id: None,
    };
    service.save_group(&group).unwrap();
    let mut connection = record("one", "server");
    connection.group_id = Some("g".into());
    service.save(&connection).unwrap();
    assert_eq!(service.list_groups().unwrap(), vec![group]);
    assert!(service.delete_group("g").unwrap());
    assert!(service.list_groups().unwrap().is_empty());
    assert_eq!(service.get("one").unwrap().unwrap().group_id, None);
    assert_eq!(service.get("one").unwrap().unwrap().version, 1);
    assert_eq!(
        service.save(&connection).unwrap_err().code,
        "CONNECTION_CONFLICT"
    );
    assert!(!service.delete_group("g").unwrap());
}

#[test]
fn group_delete_rolls_back_at_connection_version_limit() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute(
        "INSERT INTO connection_groups(id,name) VALUES ('g','group')",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO connections(id,name,host,port,username,authentication,group_id,version) VALUES ('one','one','host',22,'user','password','g',9007199254740991)",[]).unwrap();
    let service = ConnectionService::new(
        db,
        FeatureFlags::default().with_enabled(Feature::Connections),
    )
    .unwrap();
    assert!(service.delete_group("g").is_err());
    assert_eq!(service.list_groups().unwrap().len(), 1);
    let record = service.get("one").unwrap().unwrap();
    assert_eq!(record.group_id.as_deref(), Some("g"));
    assert_eq!(record.version, 9007199254740991);
}

#[test]
fn locked_database_reports_storage_error_not_conflict() {
    let path = std::env::temp_dir().join(format!(
        "coshell-connection-error-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = rusqlite::Connection::open(&path).unwrap();
    db.busy_timeout(std::time::Duration::ZERO).unwrap();
    let service = ConnectionService::new(
        db,
        FeatureFlags::default().with_enabled(Feature::Connections),
    )
    .unwrap();
    let locker = rusqlite::Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(
        service
            .save_group(&GroupRecord {
                id: "g".into(),
                name: "group".into(),
                parent_id: None
            })
            .unwrap_err()
            .code,
        "CONNECTION_STORAGE"
    );
    locker.execute_batch("ROLLBACK").unwrap();
    drop(locker);
    drop(service);
    std::fs::remove_file(path).unwrap();
}
