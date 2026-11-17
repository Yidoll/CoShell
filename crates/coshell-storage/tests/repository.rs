use coshell_storage::{
    migrate, ConnectionRecord, GroupRecord, HistoryFilter, HistoryRecord, Repository,
};
use rusqlite::Connection;

fn connection(id: &str, name: &str) -> ConnectionRecord {
    ConnectionRecord {
        id: id.into(),
        name: name.into(),
        host: "host".into(),
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

fn history(id: &str, connection: &str, time: &str, command: &str, outcome: &str) -> HistoryRecord {
    HistoryRecord {
        id: id.into(),
        connection_id: connection.into(),
        connection_name: connection.into(),
        production: false,
        command: command.into(),
        submitted_at: time.into(),
        outcome: outcome.into(),
        exit_code: None,
        duration_ms: None,
    }
}

#[test]
fn connection_group_setting_crud_and_unique_names() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    let mut repo = Repository::new(&mut db);
    let group = GroupRecord {
        id: "g1".into(),
        name: "servers".into(),
        parent_id: None,
    };
    repo.save_group(&group).unwrap();
    let mut first = connection("c1", "alpha");
    first.group_id = Some(group.id.clone());
    repo.save_connection(&first).unwrap();
    assert!(repo.save_connection(&connection("c2", "alpha")).is_err());
    first.notes = "updated".into();
    repo.save_connection(&first).unwrap();
    assert_eq!(repo.get_connection("c1").unwrap(), Some(first));
    assert_eq!(repo.list_groups().unwrap(), vec![group]);
    repo.set_setting("theme", "dark").unwrap();
    repo.set_setting("theme", "light").unwrap();
    assert_eq!(repo.get_setting("theme").unwrap().as_deref(), Some("light"));
    repo.delete_setting("theme").unwrap();
    assert_eq!(repo.get_setting("theme").unwrap(), None);
    repo.delete_group("g1").unwrap();
    assert_eq!(repo.get_connection("c1").unwrap().unwrap().group_id, None);
    repo.delete_connection("c1").unwrap();
    assert_eq!(repo.get_connection("c1").unwrap(), None);
}

#[test]
fn history_filters_paginates_updates_and_prunes_oldest() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    let mut repo = Repository::new(&mut db);
    for row in [
        history("h1", "a", "2026-01-01T00:00:00Z", "pwd", "succeeded"),
        history("h2", "a", "2026-01-02T00:00:00Z", "ls data", "failed"),
        history("h3", "b", "2026-01-03T00:00:00Z", "pwd", "unknown"),
    ] {
        repo.save_history(&row).unwrap();
    }
    let query = HistoryFilter {
        connection_id: Some("a".into()),
        keyword: Some("ls".into()),
        ..Default::default()
    };
    assert_eq!(repo.list_history(&query, 10, 0).unwrap()[0].id, "h2");
    assert_eq!(
        repo.list_history(&HistoryFilter::default(), 1, 1).unwrap()[0].id,
        "h2"
    );
    assert_eq!(
        repo.prune_history(Some("2026-01-02T00:00:00Z"), Some(1))
            .unwrap(),
        2
    );
    assert_eq!(
        repo.list_history(&HistoryFilter::default(), 10, 0).unwrap()[0].id,
        "h3"
    );
    assert_eq!(repo.delete_history(Some("b"), None, None).unwrap(), 1);
    assert!(repo
        .list_history(&HistoryFilter::default(), 10, 0)
        .unwrap()
        .is_empty());
}

#[test]
fn restore_snapshot_is_transactional_and_preserves_secret_references_on_failure() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    let mut repo = Repository::new(&mut db);
    repo.save_connection(&connection("c1", "alpha")).unwrap();
    repo.prepare_secret_write("seed", "c1", "password", "secret-ref")
        .unwrap();
    repo.commit_secret_write("seed").unwrap();
    let before = repo.snapshot().unwrap();
    let mut invalid = before.clone();
    invalid.connections.push(connection("c2", "alpha"));
    assert!(repo.restore_snapshot(&invalid).is_err());
    assert_eq!(repo.snapshot().unwrap(), before);
    assert_eq!(
        repo.get_secret_ref("c1", "password").unwrap().as_deref(),
        Some("secret-ref")
    );
}

#[test]
fn raw_operations_cannot_orphan_secret_references() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    let mut repo = Repository::new(&mut db);
    repo.save_connection(&connection("c1", "alpha")).unwrap();
    repo.prepare_secret_write("seed", "c1", "password", "secret-ref")
        .unwrap();
    repo.commit_secret_write("seed").unwrap();
    assert!(repo
        .set_secret_ref("c1", "password", "replacement")
        .is_err());
    assert!(repo.delete_connection("c1").is_err());
    let before = repo.snapshot().unwrap();
    let mut changed = before.clone();
    changed.secret_refs.clear();
    assert!(repo.restore_snapshot(&changed).is_err());
    assert_eq!(repo.snapshot().unwrap(), before);
}

#[test]
fn restore_never_silently_overwrites_or_drops_trusted_host_keys() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    db.execute("INSERT INTO trusted_host_keys(host,port,algorithm,fingerprint) VALUES ('Example.COM.',22,'ed25519','original')", []).unwrap();
    let mut repo = Repository::new(&mut db);
    let before = repo.snapshot().unwrap();
    let mut changed = before.clone();
    changed.host_keys[0].host = "example.com".into();
    changed.host_keys[0].fingerprint = "attacker".into();
    assert!(repo.restore_snapshot(&changed).is_err());
    assert_eq!(repo.snapshot().unwrap(), before);
    let mut absent = before.clone();
    absent.host_keys.clear();
    repo.restore_snapshot(&absent).unwrap();
    assert_eq!(repo.snapshot().unwrap().host_keys, before.host_keys);
}

#[test]
fn restore_rejects_conflicting_keys_inside_one_snapshot() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    let mut repo = Repository::new(&mut db);
    let before = repo.snapshot().unwrap();
    let mut incoming = before.clone();
    incoming.host_keys.push(coshell_storage::HostKeyRecord {
        host: "Example.com".into(),
        port: 22,
        algorithm: "ed25519".into(),
        fingerprint: "a".into(),
    });
    incoming.host_keys.push(coshell_storage::HostKeyRecord {
        host: "example.com.".into(),
        port: 22,
        algorithm: "rsa".into(),
        fingerprint: "b".into(),
    });
    assert!(repo.restore_snapshot(&incoming).is_err());
    assert_eq!(repo.snapshot().unwrap(), before);
}

#[test]
fn restore_snapshot_handles_parent_child_groups_and_preserves_foreign_keys() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    let mut repo = Repository::new(&mut db);
    repo.save_group(&GroupRecord {
        id: "parent".into(),
        name: "parent".into(),
        parent_id: None,
    })
    .unwrap();
    repo.save_group(&GroupRecord {
        id: "child".into(),
        name: "child".into(),
        parent_id: Some("parent".into()),
    })
    .unwrap();
    repo.save_group(&GroupRecord {
        id: "other".into(),
        name: "other".into(),
        parent_id: None,
    })
    .unwrap();
    repo.save_group(&GroupRecord {
        id: "other-child".into(),
        name: "child".into(),
        parent_id: Some("other".into()),
    })
    .unwrap();
    let before = repo.snapshot().unwrap();
    repo.restore_snapshot(&before).unwrap();
    assert_eq!(repo.snapshot().unwrap(), before);
}

#[test]
fn group_cycles_are_rejected_without_changing_existing_data() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    let mut repo = Repository::new(&mut db);
    let root = GroupRecord {
        id: "root".into(),
        name: "root".into(),
        parent_id: None,
    };
    let child = GroupRecord {
        id: "child".into(),
        name: "child".into(),
        parent_id: Some("root".into()),
    };
    let leaf = GroupRecord {
        id: "leaf".into(),
        name: "leaf".into(),
        parent_id: Some("child".into()),
    };
    for group in [&root, &child, &leaf] {
        repo.save_group(group).unwrap();
    }
    let original = repo.list_groups().unwrap();
    assert!(repo
        .save_group(&GroupRecord {
            parent_id: Some("root".into()),
            ..root.clone()
        })
        .is_err());
    assert_eq!(repo.list_groups().unwrap(), original);
    assert!(repo
        .save_group(&GroupRecord {
            parent_id: Some("leaf".into()),
            ..root
        })
        .is_err());
    assert_eq!(repo.list_groups().unwrap(), original);
}

#[test]
fn history_orders_filters_and_prunes_timestamps_by_instant() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    let mut repo = Repository::new(&mut db);
    for row in [
        history("whole", "a", "2026-01-01T00:00:00Z", "pwd", "succeeded"),
        history(
            "fraction",
            "a",
            "2026-01-01T00:00:00.5Z",
            "pwd",
            "succeeded",
        ),
        history(
            "offset",
            "a",
            "2026-01-01T01:00:01+01:00",
            "pwd",
            "succeeded",
        ),
    ] {
        repo.save_history(&row).unwrap();
    }
    let ids: Vec<_> = repo
        .list_history(&HistoryFilter::default(), 10, 0)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, ["offset", "fraction", "whole"]);
    let filter = HistoryFilter {
        from: Some("2025-12-31T19:00:00.5-05:00".into()),
        until: Some("2026-01-01T00:00:01Z".into()),
        ..Default::default()
    };
    let ids: Vec<_> = repo
        .list_history(&filter, 10, 0)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, ["offset", "fraction"]);
    assert_eq!(
        repo.prune_history(Some("2026-01-01T00:00:00.5Z"), Some(1))
            .unwrap(),
        2
    );
    assert_eq!(
        repo.list_history(&HistoryFilter::default(), 10, 0).unwrap()[0].id,
        "offset"
    );
}

#[test]
fn history_keeps_submillisecond_precision_and_rejects_invalid_dates() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    let mut repo = Repository::new(&mut db);
    repo.save_history(&history(
        "early",
        "a",
        "2026-01-01T00:00:00.0001Z",
        "pwd",
        "succeeded",
    ))
    .unwrap();
    repo.save_history(&history(
        "late",
        "a",
        "2026-01-01T01:00:00.0002+01:00",
        "pwd",
        "succeeded",
    ))
    .unwrap();
    assert!(repo
        .save_history(&history(
            "bad",
            "a",
            "2026-02-30T00:00:00Z",
            "pwd",
            "succeeded"
        ))
        .is_err());
    let ids: Vec<_> = repo
        .list_history(&HistoryFilter::default(), 10, 0)
        .unwrap()
        .into_iter()
        .map(|x| x.id)
        .collect();
    assert_eq!(ids, ["late", "early"]);
    assert_eq!(
        repo.prune_history(Some("2026-01-01T00:00:00.0002Z"), None)
            .unwrap(),
        1
    );
    assert_eq!(
        repo.list_history(&HistoryFilter::default(), 10, 0).unwrap()[0].id,
        "late"
    );
}

#[test]
fn restore_snapshot_rejects_group_cycle_without_changing_data() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    let mut repo = Repository::new(&mut db);
    repo.save_group(&GroupRecord {
        id: "a".into(),
        name: "a".into(),
        parent_id: None,
    })
    .unwrap();
    repo.save_group(&GroupRecord {
        id: "b".into(),
        name: "b".into(),
        parent_id: Some("a".into()),
    })
    .unwrap();
    let before = repo.snapshot().unwrap();
    let mut invalid = before.clone();
    invalid
        .groups
        .iter_mut()
        .find(|g| g.id == "a")
        .unwrap()
        .parent_id = Some("b".into());
    assert!(repo.restore_snapshot(&invalid).is_err());
    assert_eq!(repo.snapshot().unwrap(), before);
}

#[test]
fn migration_normalizes_existing_timestamps_atomically() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    reset_to_v1(&db);
    db.execute("INSERT INTO command_history (id,connection_id,connection_name,production,command,submitted_at,outcome) VALUES ('old','c','c',0,'pwd','2026-01-01T01:00:00.0002+01:00','succeeded')", []).unwrap();
    migrate(&mut db).unwrap();
    let timestamp: String = db
        .query_row(
            "SELECT submitted_at FROM command_history WHERE id='old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(timestamp, "2026-01-01T00:00:00.000200000Z");
    reset_to_v1(&db);
    db.execute("INSERT INTO command_history (id,connection_id,connection_name,production,command,submitted_at,outcome) VALUES ('bad','c','c',0,'pwd','2026-02-30T00:00:00Z','succeeded')", []).unwrap();
    assert!(migrate(&mut db).is_err());
    let unchanged: String = db
        .query_row(
            "SELECT submitted_at FROM command_history WHERE id='bad'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unchanged, "2026-02-30T00:00:00Z");
    assert_eq!(
        db.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        1
    );
}

fn reset_to_v1(db: &Connection) {
    // Later migrations attach triggers to v1 tables, so removing only the
    // added tables would leave a mixed-version fixture.
    db.execute_batch("DROP TRIGGER restore_ref_pending_insert; DROP TRIGGER restore_ref_pending_update; DROP TRIGGER restore_webdav_pending_insert; DROP TRIGGER restore_webdav_pending_update; DROP TRIGGER restore_ssh_journal_cross; DROP TRIGGER restore_webdav_journal_cross; DROP TABLE restore_secret_journal; DROP TABLE restore_secret_receipts; DROP TRIGGER backup_run_prune_aborted_profile; DROP TRIGGER backup_runs_provisional_insert; DROP TRIGGER backup_runs_provisional_update; DROP VIEW active_webdav_profiles; DROP TRIGGER webdav_provisional_block_update; DROP TRIGGER webdav_provisional_block_delete; DROP TABLE webdav_provisional_profiles; DROP TRIGGER connection_ref_webdav_pending_insert; DROP TRIGGER connection_ref_webdav_pending_update; DROP TRIGGER webdav_ref_webdav_pending_insert; DROP TRIGGER webdav_ref_webdav_pending_update; DROP TRIGGER ssh_journal_cross_operation; DROP TRIGGER webdav_journal_cross_operation; DROP TABLE webdav_secret_journal; DROP TABLE webdav_secret_receipts; DROP TRIGGER secret_ref_reject_pending_insert; DROP TRIGGER secret_ref_reject_pending_update; DROP TRIGGER webdav_secret_reject_pending_insert; DROP TRIGGER webdav_secret_reject_pending_update; DROP TABLE secret_write_journal; DROP TABLE secret_operation_receipts; DELETE FROM schema_migrations WHERE version>=2; PRAGMA user_version=1;").unwrap();
}
