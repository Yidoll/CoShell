use coshell_core::desktop_runtime::{ConnectionDto, DesktopRuntime, OpenResult, SubmitResult};
use coshell_core::feature_flags::{Feature, FeatureFlags};

#[tokio::test]
async fn desktop_connection_metadata_persists_and_validates() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let runtime = DesktopRuntime::from_connection(
        db,
        FeatureFlags::default().with_enabled(Feature::Connections),
    )
    .unwrap();
    assert!(runtime.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn groups_copy_and_idle_delete_preserve_production_and_do_not_copy_secrets() {
    use coshell_core::desktop_runtime::GroupDto;
    let db = rusqlite::Connection::open_in_memory().unwrap();
    let runtime = DesktopRuntime::from_connection(
        db,
        FeatureFlags::default().with_enabled(Feature::Connections),
    )
    .unwrap();
    runtime
        .save_group(GroupDto {
            id: "g".into(),
            name: "group".into(),
            parent_id: None,
        })
        .await
        .unwrap();
    let record = ConnectionDto {
        id: "one".into(),
        name: "one".into(),
        host: "example.org".into(),
        port: 22,
        username: "x".into(),
        authentication: "password".into(),
        group_id: Some("g".into()),
        notes: String::new(),
        production: true,
        always_confirm: false,
        version: 0,
    };
    runtime.save(record).await.unwrap();
    let copy = runtime.copy("one", "two", "copy").await.unwrap();
    assert!(copy.production);
    assert_eq!(copy.group_id.as_deref(), Some("g"));
    assert_eq!(runtime.groups().await.unwrap().len(), 1);
    assert!(runtime.delete("one").await.unwrap());
    assert_eq!(runtime.list().await.unwrap().len(), 1);
    assert!(runtime.delete("two").await.unwrap());
    assert!(runtime.delete_group("g").await.unwrap());
}

#[tokio::test]
async fn connection_with_saved_secret_cannot_be_deleted_or_copied_with_secret() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let record = coshell_storage::ConnectionRecord {
        id: "one".into(),
        name: "one".into(),
        host: "example.org".into(),
        port: 22,
        username: "x".into(),
        authentication: "password".into(),
        group_id: None,
        notes: String::new(),
        production: true,
        always_confirm: false,
        version: 0,
    };
    let mut repo = coshell_storage::Repository::new(&mut db);
    repo.save_connection(&record).unwrap();
    repo.prepare_secret_write("op", "one", "password", "secret-id")
        .unwrap();
    repo.commit_secret_write("op").unwrap();
    drop(repo);
    let runtime = DesktopRuntime::from_connection(
        db,
        FeatureFlags::default().with_enabled(Feature::Connections),
    )
    .unwrap();
    assert_eq!(
        runtime.delete("one").await.unwrap_err(),
        "CONNECTION_HAS_SECRETS"
    );
    runtime.copy("one", "two", "copy").await.unwrap();
    assert!(runtime.delete("two").await.unwrap());
    assert_eq!(runtime.list().await.unwrap().len(), 1);
}

const KEY:&str="-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jYmMAAAAGYmNyeXB0AAAAGAAAABDLGyfA39\nJ2FcJygtYqi5ISAAAAEAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIN+Wjn4+4Fcvl2Jl\nKpggT+wCRxpSvtqqpVrQrKN1/A22AAAAkOHDLnYZvYS6H9Q3S3Nk4ri3R2jAZlQlBbUos5\nFkHpYgNw65KCWCTXtP7ye2czMC3zjn2r98pJLobsLYQgRiHIv/CUdAdsqbvMPECB+wl/UQ\ne+JpiSq66Z6GIt0801skPh20jxOO3F52SoX1IeO5D5PXfZrfSZlw6S8c7bwyp2FHxDewRx\n7/wNsnDM0T7nLv/Q==\n-----END OPENSSH PRIVATE KEY-----";
struct Server(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl russh::server::Handler for Server {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        _: &str,
        password: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        Ok(if password == "ok" {
            russh::server::Auth::Accept
        } else {
            russh::server::Auth::reject()
        })
    }
    async fn auth_publickey(
        &mut self,
        _: &str,
        key: &russh::keys::PublicKey,
    ) -> Result<russh::server::Auth, Self::Error> {
        let expected = russh::keys::decode_secret_key(KEY, Some("blabla")).unwrap();
        Ok(if key.key_data() == expected.public_key().key_data() {
            russh::server::Auth::Accept
        } else {
            russh::server::Auth::reject()
        })
    }
    async fn channel_open_session(
        &mut self,
        _: russh::Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn pty_request(
        &mut self,
        id: russh::ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        s.channel_success(id)?;
        Ok(())
    }
    async fn shell_request(
        &mut self,
        id: russh::ChannelId,
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        s.channel_success(id)?;
        s.data(id, b"ready\n".to_vec())?;
        Ok(())
    }
    async fn data(
        &mut self,
        _: russh::ChannelId,
        data: &[u8],
        _: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(())
    }
    async fn exec_request(
        &mut self,
        id: russh::ChannelId,
        data: &[u8],
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.0.lock().unwrap().extend_from_slice(data);
        s.channel_success(id)?;
        s.data(id, b"exec-result\n".to_vec())?;
        s.exit_status_request(id, 0)?;
        s.eof(id)?;
        s.close(id)?;
        Ok(())
    }
}

#[tokio::test]
async fn encrypted_private_key_authentication_is_temporary_and_errors_are_redacted() {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let cfg = Arc::new(russh::server::Config {
            keys: vec![russh::keys::decode_secret_key(KEY, Some("blabla")).unwrap()],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let c = cfg.clone();
            tokio::spawn(async move {
                if let Ok(s) =
                    russh::server::run_stream(c, socket, Server(Arc::new(Mutex::new(Vec::new()))))
                        .await
                {
                    let _ = s.await;
                }
            });
        }
    });
    let dir = std::env::temp_dir().join(format!(
        "coshell-key-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let runtime = DesktopRuntime::open(&path).unwrap();
    runtime
        .save(ConnectionDto {
            id: "key".into(),
            name: "key".into(),
            host: "127.0.0.1".into(),
            port,
            username: "test".into(),
            authentication: "private_key".into(),
            group_id: None,
            notes: String::new(),
            production: false,
            always_confirm: false,
            version: 0,
        })
        .await
        .unwrap();
    let fp = match runtime
        .open_ssh(
            "key".into(),
            None,
            None,
            "key1".into(),
            Some(KEY.into()),
            Some("blabla".into()),
        )
        .await
        .unwrap()
    {
        OpenResult::HostKeyConfirmation { fingerprint, .. } => fingerprint,
        _ => panic!(),
    };
    let session = match runtime
        .open_ssh(
            "key".into(),
            None,
            Some(fp),
            "key2".into(),
            Some(KEY.into()),
            Some("blabla".into()),
        )
        .await
        .unwrap()
    {
        OpenResult::Connected { session_id } => session_id,
        _ => panic!(),
    };
    runtime.close(&session).await.unwrap();
    let err = match runtime
        .open_ssh(
            "key".into(),
            None,
            None,
            "key3".into(),
            Some(KEY.into()),
            Some("bad-secret-phrase".into()),
        )
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("wrong passphrase must fail"),
    };
    assert!(!err.contains("bad-secret-phrase") && !err.contains("PRIVATE KEY"));
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.windows("blabla".len()).any(|x| x == b"blabla"));
    assert!(!bytes
        .windows("OPENSSH PRIVATE KEY".len())
        .any(|x| x == b"OPENSSH PRIVATE KEY"));
    drop(runtime);
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn real_ssh_tofu_and_production_confirmation_precede_all_command_bytes() {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let data = Arc::new(Mutex::new(Vec::new()));
    let received = data.clone();
    let server = tokio::spawn(async move {
        let config = Arc::new(russh::server::Config {
            keys: vec![russh::keys::decode_secret_key(KEY, Some("blabla")).unwrap()],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let cfg = config.clone();
            let d = received.clone();
            tokio::spawn(async move {
                if let Ok(s) = russh::server::run_stream(cfg, socket, Server(d)).await {
                    let _ = s.await;
                }
            });
        }
    });
    let dir = std::env::temp_dir().join(format!(
        "coshell-runtime-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let runtime = DesktopRuntime::open(&dir.join("db.sqlite")).unwrap();
    runtime
        .save_group(coshell_core::desktop_runtime::GroupDto {
            id: "active-group".into(),
            name: "active-group".into(),
            parent_id: None,
        })
        .await
        .unwrap();
    let mut record = ConnectionDto {
        id: "server".into(),
        name: "server".into(),
        host: "127.0.0.1".into(),
        port,
        username: "test".into(),
        authentication: "password".into(),
        group_id: Some("active-group".into()),
        notes: String::new(),
        production: true,
        always_confirm: false,
        version: 0,
    };
    runtime.save(record.clone()).await.unwrap();
    let fingerprint = match runtime
        .open_ssh(
            "server".into(),
            Some("ok".into()),
            None,
            "open1".into(),
            None,
            None,
        )
        .await
        .unwrap()
    {
        OpenResult::HostKeyConfirmation { fingerprint, .. } => fingerprint,
        _ => panic!("expected challenge"),
    };
    let session = match runtime
        .open_ssh(
            "server".into(),
            Some("ok".into()),
            Some(fingerprint),
            "open2".into(),
            None,
            None,
        )
        .await
        .unwrap()
    {
        OpenResult::Connected { session_id } => session_id,
        _ => panic!("expected connection"),
    };
    let saved = runtime
        .saved_command_save(coshell_core::saved_commands::SavedCommand {
            id: "saved".into(),
            name: "Danger".into(),
            content: "rm -rf /tmp/saved".into(),
            category: "".into(),
            version: 0,
        })
        .unwrap();
    let saved_token = match runtime.run_saved(&session, "saved", 0, None).await.unwrap() {
        coshell_core::desktop_runtime::SavedRunResult::ConfirmationRequired { token, .. } => token,
        _ => panic!("expected saved command confirmation"),
    };
    assert!(data.lock().unwrap().is_empty());
    let mut changed = saved.clone();
    changed.content = "rm -rf /tmp/changed".into();
    runtime.saved_command_save(changed).unwrap();
    assert!(runtime.run_saved(&session, "saved", 0, None).await.is_err());
    assert!(runtime
        .run_saved(&session, "saved", 0, Some(saved_token))
        .await
        .is_err());
    assert!(data.lock().unwrap().is_empty());
    let token2 = match runtime.run_saved(&session, "saved", 1, None).await.unwrap() {
        coshell_core::desktop_runtime::SavedRunResult::ConfirmationRequired { token, .. } => token,
        _ => panic!("expected confirmation"),
    };
    assert!(matches!(
        runtime
            .run_saved(&session, "saved", 1, Some(token2.clone()))
            .await
            .unwrap(),
        coshell_core::desktop_runtime::SavedRunResult::Sent { .. }
    ));
    assert!(runtime
        .run_saved(&session, "saved", 1, Some(token2))
        .await
        .is_err());
    assert_eq!(&*data.lock().unwrap(), b"rm -rf /tmp/changed");
    data.lock().unwrap().clear();
    let token = match runtime
        .submit(&session, "rm -rf /tmp/demo".into(), None)
        .await
        .unwrap()
    {
        SubmitResult::ConfirmationRequired { token, .. } => token,
        _ => panic!("expected confirmation"),
    };
    assert!(runtime
        .write_terminal(&session, "unsafe".into())
        .await
        .is_err());
    assert!(runtime.resize_terminal(&session, 80, 24).await.is_err());
    assert!(data.lock().unwrap().is_empty());
    assert_eq!(
        runtime.delete("server").await.unwrap_err(),
        "CONNECTION_ACTIVE"
    );
    assert_eq!(
        runtime.delete_group("active-group").await.unwrap_err(),
        "CONNECTION_ACTIVE"
    );
    assert!(runtime
        .submit(&session, "rm -rf /tmp/changed".into(), Some(token.clone()))
        .await
        .is_err());
    assert!(data.lock().unwrap().is_empty());
    assert!(matches!(
        runtime
            .submit(&session, "rm -rf /tmp/demo".into(), Some(token.clone()))
            .await
            .unwrap(),
        SubmitResult::Sent
    ));
    assert!(runtime
        .submit(&session, "rm -rf /tmp/demo".into(), Some(token))
        .await
        .is_err());
    assert_eq!(&*data.lock().unwrap(), b"rm -rf /tmp/demo");
    assert!(runtime
        .read(&session)
        .await
        .unwrap()
        .output
        .contains("exec-result"));
    data.lock().unwrap().clear();
    let pending = match runtime
        .submit(&session, "shutdown now".into(), None)
        .await
        .unwrap()
    {
        SubmitResult::ConfirmationRequired { token, .. } => token,
        _ => panic!("expected confirmation"),
    };
    record.production = false;
    record = runtime.save(record).await.unwrap();
    assert!(runtime
        .submit(&session, "shutdown now".into(), Some(pending))
        .await
        .is_err());
    assert!(runtime
        .submit(&session, "echo changed".into(), None)
        .await
        .is_err());
    assert!(data.lock().unwrap().is_empty());
    runtime.close(&session).await.unwrap();
    let session = match runtime
        .open_ssh(
            "server".into(),
            Some("ok".into()),
            None,
            "open3".into(),
            None,
            None,
        )
        .await
        .unwrap()
    {
        OpenResult::Connected { session_id } => session_id,
        _ => panic!("expected known host"),
    };
    runtime
        .write_terminal(&session, "echo raw\t".into())
        .await
        .unwrap();
    runtime.resize_terminal(&session, 100, 30).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while data.lock().unwrap().len() < 9 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(&*data.lock().unwrap(), b"echo raw\t");
    data.lock().unwrap().clear();
    assert!(matches!(
        runtime
            .submit(&session, "rm -rf /tmp/demo".into(), None)
            .await
            .unwrap(),
        SubmitResult::Sent
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !data.lock().unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(&*data.lock().unwrap(), b"rm -rf /tmp/demo\n");
    record.always_confirm = true;
    record = runtime.save(record).await.unwrap();
    assert!(runtime
        .write_terminal(&session, "bypass".into())
        .await
        .is_err());
    runtime.close(&session).await.unwrap();
    let session = match runtime
        .open_ssh(
            "server".into(),
            Some("ok".into()),
            None,
            "open4".into(),
            None,
            None,
        )
        .await
        .unwrap()
    {
        OpenResult::Connected { session_id } => session_id,
        _ => panic!("expected known host"),
    };
    assert!(runtime
        .write_terminal(&session, "bypass".into())
        .await
        .is_err());
    assert_eq!(&*data.lock().unwrap(), b"rm -rf /tmp/demo\n");
    record.name = "changed".into();
    record = runtime.save(record).await.unwrap();
    assert!(runtime.write_terminal(&session, "x".into()).await.is_err());
    assert!(runtime
        .submit(&session, "rm -rf /tmp/other".into(), None)
        .await
        .is_err());
    assert!(runtime
        .submit(&session, "echo after change".into(), None)
        .await
        .is_err());
    assert_eq!(&*data.lock().unwrap(), b"rm -rf /tmp/demo\n");
    assert_eq!(record.version, 3);
    runtime.close(&session).await.unwrap();
    drop(runtime);
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn cancellation_before_and_during_handshake_stops_open_and_leaves_no_session() {
    use std::{sync::Arc, time::Duration};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let entered = Arc::new(tokio::sync::Notify::new());
    let notify = entered.clone();
    let task = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        notify.notify_one();
        std::future::pending::<()>().await;
    });
    let dir = std::env::temp_dir().join(format!(
        "coshell-cancel-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let runtime = Arc::new(DesktopRuntime::open(&dir.join("db.sqlite")).unwrap());
    assert!(DesktopRuntime::open(&dir.join("db.sqlite")).is_err());
    runtime
        .save(ConnectionDto {
            id: "x".into(),
            name: "x".into(),
            host: "127.0.0.1".into(),
            port,
            username: "test".into(),
            authentication: "password".into(),
            group_id: None,
            notes: String::new(),
            production: false,
            always_confirm: false,
            version: 0,
        })
        .await
        .unwrap();
    runtime.cancel_open("before").await.unwrap();
    assert!(runtime
        .open_ssh(
            "x".into(),
            Some("ok".into()),
            None,
            "before".into(),
            None,
            None
        )
        .await
        .is_err());
    assert!(!runtime.delete("missing").await.unwrap());
    let r = runtime.clone();
    let open = tokio::spawn(async move {
        r.open_ssh(
            "x".into(),
            Some("ok".into()),
            None,
            "during".into(),
            None,
            None,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    assert_eq!(runtime.delete("x").await.unwrap_err(), "CONNECTION_ACTIVE");
    tokio::time::timeout(Duration::from_secs(2), runtime.cancel_open("during"))
        .await
        .unwrap()
        .unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(2), open)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert!(runtime.delete("x").await.unwrap());
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(dir).unwrap();
}
