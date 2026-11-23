use super::*;
use serde_json::json;
use tauri::test::{mock_builder, mock_context, noop_assets};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const PUBLIC_KEY: &str = include_str!("../fixture/key.pub");
const SIGNATURE: &str = include_str!("../fixture/fixture.sig");
const PACKAGE: &[u8] = include_bytes!("../fixture/fixture");
const CHECK_ID: &str = "2a664ea6-a04c-4e81-8e7c-cb2ce001a8a0";
const DOWNLOAD_ID: &str = "7c90d950-164f-4879-b200-c888751cdea1";
const RETRY_ID: &str = "3d48d8e6-f274-49a8-ae24-cfd22d722511";
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

async fn request(listener: &TcpListener, path: &str) -> TcpStream {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
        }
        assert!(request.starts_with(format!("GET {path} HTTP/1.1\r\n").as_bytes()));
        stream
    })
    .await
    .expect("timed out waiting for the updater request")
}

async fn respond(stream: &mut TcpStream, headers: &str, body: &[u8]) {
    stream.write_all(headers.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
}

fn updater(endpoint: &str) -> Updater {
    let mut context = mock_context(noop_assets());
    context.config_mut().plugins.0.insert(
        "updater".into(),
        json!({
            "pubkey": PUBLIC_KEY.trim(),
            "endpoints": [endpoint],
            "dangerousInsecureTransportProtocol": true,
            "requireSignedVersion": true
        }),
    );
    let app = mock_builder()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .build(context)
        .unwrap();
    app.updater_builder()
        .no_proxy()
        .target("coshell-fixture")
        .timeout(TEST_TIMEOUT)
        .version_comparator(|current, remote| {
            is_newer_stable(&current.to_string(), &remote.version.to_string())
        })
        .build()
        .unwrap()
}

async fn serve_manifest(listener: &TcpListener, version: &str, target: &str, signature: &str) {
    let address = listener.local_addr().unwrap();
    let mut stream = request(listener, "/latest.json").await;
    let body = json!({
        "version": version,
        "notes": "更新测试",
        "platforms": {(target): {"signature": signature, "url": format!("http://{address}/package")}}
    }).to_string();
    respond(&mut stream, &format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()), body.as_bytes()).await;
}

#[tokio::test]
async fn local_manifest_returns_current_available_and_missing_platform() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let updater = updater(&format!(
        "http://{}/latest.json",
        listener.local_addr().unwrap()
    ));
    let ((), current) = tokio::join!(
        serve_manifest(&listener, "0.0.0", "coshell-fixture", SIGNATURE),
        updater.check()
    );
    assert!(current.unwrap().is_none());
    let ((), available) = tokio::join!(
        serve_manifest(&listener, "99.2.0", "coshell-fixture", SIGNATURE),
        updater.check()
    );
    let available = available.unwrap().unwrap();
    assert_eq!(available.version, "99.2.0");
    assert_eq!(available.body.as_deref(), Some("更新测试"));
    let ((), missing) = tokio::join!(
        serve_manifest(&listener, "99.2.0", "another-target", SIGNATURE),
        updater.check()
    );
    assert_eq!(
        missing.err().map(check_error),
        Some("UPDATE_PLATFORM_UNAVAILABLE")
    );
}

#[tokio::test]
async fn plugin_verifies_download_and_rejects_modified_signature_or_version() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let updater = updater(&format!(
        "http://{}/latest.json",
        listener.local_addr().unwrap()
    ));
    for (version, signature, bytes, expected) in [
        ("99.2.0", SIGNATURE, PACKAGE, None),
        (
            "99.2.0",
            SIGNATURE,
            b"modified contents".as_slice(),
            Some("UPDATE_SIGNATURE"),
        ),
        (
            "99.2.0",
            "broken minisign",
            PACKAGE,
            Some("UPDATE_SIGNATURE"),
        ),
        ("99.3.0", SIGNATURE, PACKAGE, Some("UPDATE_SIGNATURE")),
    ] {
        let ((), checked) = tokio::join!(
            serve_manifest(&listener, version, "coshell-fixture", signature),
            updater.check()
        );
        let update = checked.unwrap().unwrap();
        let serve_package = async {
            let mut stream = request(&listener, "/package").await;
            respond(
                &mut stream,
                "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n",
                bytes,
            )
            .await;
        };
        let mut received = 0;
        let download = update.download(
            |size, total| {
                received += size;
                assert_eq!(total, None);
            },
            || {},
        );
        let ((), result) = tokio::join!(serve_package, download);
        assert_eq!(received, bytes.len());
        if let Some(error) = expected {
            assert_eq!(result.err().map(update_error), Some(error));
        } else {
            assert_eq!(result.unwrap(), PACKAGE);
        }
    }
}

#[tokio::test]
async fn interrupted_download_can_retry_and_cancel_or_time_out() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let updater = updater(&format!(
        "http://{}/latest.json",
        listener.local_addr().unwrap()
    ));
    let ((), checked) = tokio::join!(
        serve_manifest(&listener, "99.2.0", "coshell-fixture", SIGNATURE),
        updater.check()
    );
    let update = checked.unwrap().unwrap();
    let cancel = Notify::new();
    let download = |timeout| {
        interruptible(&cancel, timeout, async {
            update
                .download(|_, _| {}, || {})
                .await
                .map_err(update_error)
        })
    };
    let respond_truncated = async {
        let mut stream = request(&listener, "/package").await;
        respond(
            &mut stream,
            "HTTP/1.1 200 OK\r\nContent-Length: 23\r\nConnection: close\r\n\r\n",
            b"short",
        )
        .await;
    };
    let ((), disconnected) = tokio::join!(respond_truncated, download(TEST_TIMEOUT));
    assert_eq!(disconnected, Err("UPDATE_NETWORK"));
    let cancel_after_request = async {
        let _stream = request(&listener, "/package").await;
        cancel.notify_one();
    };
    let ((), cancelled) = tokio::join!(cancel_after_request, download(TEST_TIMEOUT));
    assert_eq!(cancelled, Err(CANCELLED));
    assert_eq!(
        interruptible(
            &cancel,
            Duration::from_millis(1),
            std::future::pending::<Result<(), _>>()
        )
        .await,
        Err("UPDATE_TIMEOUT")
    );
    let serve_package = async {
        let mut stream = request(&listener, "/package").await;
        respond(
            &mut stream,
            "HTTP/1.1 200 OK\r\nContent-Length: 23\r\nConnection: close\r\n\r\n",
            PACKAGE,
        )
        .await;
    };
    let ((), retried) = tokio::join!(serve_package, download(TEST_TIMEOUT));
    assert_eq!(retried.unwrap(), PACKAGE);
}

#[test]
fn cancelled_and_late_download_completions_cannot_replace_new_operation() {
    let updater = DesktopUpdate::new("0.1.1".into());
    let mut inner = updater.lock().unwrap();
    inner.start(CHECK_ID.into(), Status::Available);
    inner.start(DOWNLOAD_ID.into(), Status::Downloading);
    assert_eq!(inner.replay(DOWNLOAD_ID), Ok(true));
    assert_eq!(inner.replay(CHECK_ID), Err("UPDATE_STALE".into()));
    assert_eq!(inner.replay(RETRY_ID), Err("UPDATE_BUSY".into()));
    inner.cancel.take().unwrap().notify_one();
    inner.finish_download(DOWNLOAD_ID, Ok(PACKAGE.to_vec()));
    assert_eq!(inner.snapshot.status, Status::Available);
    assert_eq!(inner.snapshot.error.as_deref(), Some(CANCELLED));
    assert!(inner.package.is_none());
    inner.start(RETRY_ID.into(), Status::Downloading);
    inner.finish_download(DOWNLOAD_ID, Ok(PACKAGE.to_vec()));
    assert_eq!(inner.snapshot.status, Status::Downloading);
    assert!(inner.package.is_none());
    inner.finish_download(RETRY_ID, Ok(PACKAGE.to_vec()));
    assert_eq!(inner.snapshot.status, Status::Ready);
    assert_eq!(inner.package.as_deref(), Some(PACKAGE));
}
