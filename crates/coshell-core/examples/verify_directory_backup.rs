//! Explicit opt-in real-service acceptance. Does not change local connections,
//! old remote snapshots, latest/history, or trust; creates one new snapshot.
use coshell_backup::{
    snapshot_manifest::{self, SnapshotMetadata},
    webdav::{Cancellation, WebDav},
};
use coshell_core::{
    backup_snapshot::{self, BackupOptions},
    directory_outbox::DirectoryOutbox,
    feature_flags::{Feature, FeatureFlags},
};
use coshell_platform::secret_store::{PlatformSecretStore, Secret, SecretError, SecretStore};
use std::{collections::HashMap, sync::Mutex, time::Duration};
use zeroize::Zeroizing;
#[derive(Default)]
struct TestStore(Mutex<HashMap<String, Vec<u8>>>);
impl SecretStore for TestStore {
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(id)
            .map(|v| Secret::new(v.clone())))
    }
    fn set(&self, id: &str, v: &Secret) -> Result<(), SecretError> {
        self.0
            .lock()
            .unwrap()
            .insert(id.into(), v.expose().to_vec());
        Ok(())
    }
    fn delete(&self, id: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
}
#[tokio::main]
async fn main() {
    if std::env::var("COSHELL_LIVE_SNAPSHOT_ACCEPTANCE").as_deref() != Ok("1") {
        eprintln!("Set COSHELL_LIVE_SNAPSHOT_ACCEPTANCE=1 for an explicitly authorized real-service acceptance run.");
        std::process::exit(2)
    }
    if let Err(step) = run().await {
        eprintln!("Acceptance stopped safely at: {step}");
        std::process::exit(1)
    }
}
async fn run() -> Result<(), &'static str> {
    let dir = coshell_platform::data_dir::default_data_dir()
        .map_err(|_| "data directory")?
        .canonicalize()
        .map_err(|_| "data directory")?;
    let path = dir.join("coshell.sqlite");
    let flags = FeatureFlags::default().with_enabled(Feature::WebDavBackup);
    let config = coshell_core::webdav_config::get_profile(&path, &flags, "default")
        .map_err(|_| "config read")?
        .ok_or("missing config")?;
    let db =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| "database read")?;
    let credential: Option<String> = db
        .query_row(
            "SELECT credential_secret_id FROM active_webdav_profiles WHERE id='default'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "credential reference")?;
    let store = PlatformSecretStore::for_data_dir(&dir).map_err(|_| "OS store")?;
    let credential = credential
        .map(|id| {
            store
                .get(&id)
                .map_err(|_| "OS credential read")?
                .ok_or("missing OS credential")
        })
        .transpose()?;
    let password = credential
        .as_ref()
        .map(|v| std::str::from_utf8(v.expose()).map_err(|_| "invalid OS credential"))
        .transpose()?
        .unwrap_or("");
    let root = WebDav::new(
        &config.url,
        &config.username,
        password,
        Duration::from_secs(60),
    )
    .map_err(|_| "client policy")?;
    let cancel = Cancellation::new();
    root.ensure_directory(&config.remote_directory, &cancel)
        .await
        .map_err(|_| "configured collection")?;
    let base = format!(
        "{}/{}/",
        config.url.trim_end_matches('/'),
        config.remote_directory.trim_matches('/')
    );
    let client = WebDav::new(&base, &config.username, password, Duration::from_secs(60))
        .map_err(|_| "backup client")?;
    let now = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now());
    let directory = now.format("%Y%m%d%H%M%S").to_string();
    let created = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut random = [0u8; 16];
    getrandom::getrandom(&mut random).map_err(|_| "randomness")?;
    let operation = format!(
        "acceptance-{}",
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let artifact = backup_snapshot::create_directory_snapshot(
        &path,
        &store,
        b"",
        BackupOptions {
            include_secrets: true,
        },
        &flags,
        SnapshotMetadata {
            snapshot_id: &operation,
            created_at: &created,
            app_version: env!("CARGO_PKG_VERSION"),
        },
    )
    .map_err(|_| "build snapshot")?;
    let mut queue_db = rusqlite::Connection::open(dir.join("snapshot-acceptance-outbox.sqlite"))
        .map_err(|_| "acceptance outbox")?;
    let key = {
        let _lock = coshell_core::secret_compensation::SecretOperationLock::try_acquire(&dir)
            .map_err(|_| "OS key lock")?;
        const ID: &str = "coshell-snapshot-acceptance-key-v1";
        let mut key = Zeroizing::new([0u8; 32]);
        if let Some(existing) = store.get(ID).map_err(|_| "OS key read")? {
            if existing.expose().len() != 32 {
                return Err("invalid OS key");
            }
            key.copy_from_slice(existing.expose());
        } else {
            if coshell_core::directory_outbox::has_directory_rows(&queue_db)
                .map_err(|_| "outbox check")?
            {
                return Err("OS key missing");
            };
            getrandom::getrandom(key.as_mut()).map_err(|_| "OS key randomness")?;
            store
                .set(ID, &Secret::new(key.to_vec()))
                .map_err(|_| "OS key write")?;
        }
        key
    };
    let mut queue = DirectoryOutbox::new(&mut queue_db, key).map_err(|_| "outbox initialize")?;
    queue
        .prepare_account(
            &operation,
            &base,
            &directory,
            &artifact.pack().map_err(|_| "pack")?,
            &config.username,
        )
        .map_err(|_| "durable prepare")?;
    if !queue.claim(&operation).map_err(|_| "claim")? {
        return Err("not fresh");
    };
    if client
        .publish_snapshot(&directory, &artifact, &operation, &cancel)
        .await
        .is_err()
    {
        queue
            .finish(&operation, "unknown", Some("real_service_unconfirmed"))
            .map_err(|_| "unknown status")?;
        return Err("publish (inspect the protected acceptance outbox; no automatic retry)");
    };
    queue
        .finish(&operation, "succeeded", None)
        .map_err(|_| "success receipt")?;
    let page = client
        .list_snapshots(None, &cancel)
        .await
        .map_err(|_| "list snapshots")?;
    let card = page
        .snapshots
        .iter()
        .find(|v| v.id == format!("snapshot:{directory}"))
        .ok_or("new snapshot missing from first page")?;
    if !card.restorable {
        return Err("snapshot structure unavailable");
    }
    let remote = client
        .download_snapshot(&directory, &cancel)
        .await
        .map_err(|_| "download snapshot")?;
    let (inspection, secrets) = coshell_core::backup_inspection::inspect_directory_for_restore(
        &remote.manifest,
        &remote.complete,
        &remote.connections_package,
        remote.credentials_package.as_deref().map(|p| p.as_slice()),
        b"",
    )
    .map_err(|_| "full validation")?;
    client
        .verify_snapshot(&directory, &artifact, &operation, &cancel)
        .await
        .map_err(|_| "exact reconciliation")?;
    let scratch = std::env::temp_dir().join(format!("coshell-restore-{operation}"));
    std::fs::create_dir(&scratch).map_err(|_| "scratch directory")?;
    let target = scratch.join("restore.sqlite");
    let mut target_db = rusqlite::Connection::open(&target).map_err(|_| "scratch database")?;
    coshell_storage::migrate(&mut target_db).map_err(|_| "scratch schema")?;
    let expected = coshell_storage::Repository::new(&mut target_db)
        .snapshot()
        .map_err(|_| "scratch snapshot")?;
    let choices = inspection
        .connections
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                coshell_core::backup_inspection::RestoreChoice::Replace,
            )
        })
        .collect();
    let desired = coshell_core::backup_inspection::prepare_restore_with_groups(
        &inspection,
        &expected,
        &choices,
        true,
    )
    .map_err(|_| "scratch restore plan")?;
    let test_store = TestStore::default();
    if secrets.is_empty() {
        coshell_storage::Repository::new(&mut target_db)
            .restore_snapshot_if_matches(&expected, &desired)
            .map_err(|_| "scratch metadata restore")?;
    } else {
        let incoming = secrets
            .iter()
            .map(|s| coshell_core::restore_secrets::IncomingSecret {
                connection_id: &s.connection_id,
                secret_type: &s.secret_type,
                value: s.value(),
            })
            .collect::<Vec<_>>();
        coshell_core::restore_secrets::apply(
            &target,
            &test_store,
            &operation,
            &expected,
            &desired,
            &incoming,
        )
        .map_err(|_| "scratch credential restore")?;
    }
    let restored = coshell_storage::Repository::new(&mut target_db)
        .snapshot()
        .map_err(|_| "scratch verification")?;
    if restored.connections.len() != inspection.connections.len()
        || restored.secret_refs.len() != secrets.len()
    {
        return Err("scratch counts mismatch");
    };
    drop(target_db);
    std::fs::remove_dir_all(scratch).map_err(|_| "scratch cleanup")?;
    println!(
        "{}",
        serde_json::json!({"result":"passed","directory":directory,"operationId":operation,"manifestSha256":snapshot_manifest::sha256(&artifact.manifest),"connectionCount":inspection.connections.len(),"groupCount":inspection.groups.len(),"credentialCount":secrets.len(),"cardState":card.state,"oldRemoteDataModified":false,"localConnectionsModified":false,"restoreTarget":"isolated temporary database with fake secret store","remoteCleanup":"retained user backup; no deletion"})
    );
    Ok(())
}
