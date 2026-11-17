use coshell_storage::{migrate, Repository};

#[test]
fn backup_snapshot_captures_connection_and_webdav_references_together() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,port,username,authentication) VALUES ('c','host','localhost',22,'u','password')", []).unwrap();
    db.execute("INSERT INTO connection_secret_refs(connection_id,secret_type,secret_id) VALUES ('c','password','secret-id')", []).unwrap();
    db.execute("INSERT INTO webdav_profiles(id,url,remote_directory,username,credential_secret_id) VALUES ('w','https://example.org','/','u','secret-id')", []).unwrap();
    let (snapshot, webdav_refs) = Repository::new(&mut db).snapshot_for_backup().unwrap();
    assert_eq!(snapshot.secret_refs[0].secret_id, "secret-id");
    assert!(webdav_refs.contains("secret-id"));
}
