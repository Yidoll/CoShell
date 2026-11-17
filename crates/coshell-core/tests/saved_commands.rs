use coshell_core::saved_commands::{self, SavedCommand};
use rusqlite::Connection;

fn command() -> SavedCommand {
    SavedCommand {
        id: "one".into(),
        name: "检查主机".into(),
        content: "uname -a\npwd".into(),
        category: "默认分类".into(),
        version: 0,
    }
}

#[test]
fn commands_persist_with_cas_and_do_not_execute() {
    let mut db = Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let first = saved_commands::save(&mut db, command()).unwrap();
    let mut changed = first.clone();
    changed.name = "检查系统".into();
    let updated = saved_commands::save(&mut db, changed).unwrap();
    assert_eq!(updated.version, 1);
    assert!(saved_commands::save(&mut db, first).is_err());
    assert_eq!(saved_commands::list(&db).unwrap(), vec![updated.clone()]);
    assert!(saved_commands::delete(&mut db, &updated.id, 0).is_err());
    assert!(saved_commands::delete(&mut db, &updated.id, 1).unwrap());
    assert!(saved_commands::list(&db).unwrap().is_empty());
}

#[test]
fn rejects_control_sequences_and_duplicate_names_without_overwrite() {
    let mut db = Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    saved_commands::save(&mut db, command()).unwrap();
    let mut bad = command();
    bad.id = "other".into();
    assert!(saved_commands::save(&mut db, bad.clone()).is_err());
    bad.name = "恶意控制".into();
    bad.content = "ls\x1b[2J".into();
    assert!(saved_commands::save(&mut db, bad).is_err());
    assert_eq!(saved_commands::list(&db).unwrap().len(), 1);
}
