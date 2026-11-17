use coshell_drivers::ssh::{
    Authentication, ConnectionRequest, HostKey, HostKeyDecision, HostKeyVerification,
    HostKeyVerifier, RusshConnector, SshConnector, SshFuture,
};
use coshell_platform::secret_store::Secret;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
const KEY:&str="-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jYmMAAAAGYmNyeXB0AAAAGAAAABDLGyfA39\nJ2FcJygtYqi5ISAAAAEAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIN+Wjn4+4Fcvl2Jl\nKpggT+wCRxpSvtqqpVrQrKN1/A22AAAAkOHDLnYZvYS6H9Q3S3Nk4ri3R2jAZlQlBbUos5\nFkHpYgNw65KCWCTXtP7ye2czMC3zjn2r98pJLobsLYQgRiHIv/CUdAdsqbvMPECB+wl/UQ\ne+JpiSq66Z6GIt0801skPh20jxOO3F52SoX1IeO5D5PXfZrfSZlw6S8c7bwyp2FHxDewRx\n7/wNsnDM0T7nLv/Q==\n-----END OPENSSH PRIVATE KEY-----";
struct Verify;
impl HostKeyVerifier for Verify {
    fn verify<'a>(&'a self, _: &'a HostKey) -> SshFuture<'a, HostKeyVerification> {
        Box::pin(async { Ok(HostKeyVerification::decision(HostKeyDecision::AcceptOnce)) })
    }
}
fn num(v: &mut Vec<u8>, n: u32) {
    v.extend_from_slice(&n.to_be_bytes())
}
fn strput(v: &mut Vec<u8>, s: &[u8]) {
    num(v, s.len() as u32);
    v.extend_from_slice(s)
}
struct Server {
    buf: Vec<u8>,
    writes: Arc<Mutex<Vec<u8>>>,
    directory_read: bool,
}
impl russh::server::Handler for Server {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        Ok(russh::server::Auth::Accept)
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
    async fn subsystem_request(
        &mut self,
        id: russh::ChannelId,
        name: &str,
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        assert_eq!(name, "sftp");
        s.channel_success(id)?;
        Ok(())
    }
    async fn data(
        &mut self,
        id: russh::ChannelId,
        data: &[u8],
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.buf.extend_from_slice(data);
        while self.buf.len() >= 4 {
            let n = u32::from_be_bytes(self.buf[..4].try_into().unwrap()) as usize;
            if self.buf.len() < n + 4 {
                break;
            }
            let p = self.buf[4..n + 4].to_vec();
            self.buf.drain(..n + 4);
            let mut r = Vec::new();
            if p[0] == 1 {
                r.extend_from_slice(&[2, 0, 0, 0, 3]);
            } else {
                let request = &p[1..5];
                match p[0] {
                    11 | 3 => {
                        r.push(102);
                        r.extend_from_slice(request);
                        strput(&mut r, b"h");
                    }
                    12 if !self.directory_read => {
                        self.directory_read = true;
                        r.push(104);
                        r.extend_from_slice(request);
                        num(&mut r, 1);
                        strput(&mut r, b"file");
                        strput(&mut r, b"file");
                        num(&mut r, 5);
                        r.extend_from_slice(&3u64.to_be_bytes());
                        num(&mut r, 0o100644);
                    }
                    12 => {
                        r.push(101);
                        r.extend_from_slice(request);
                        num(&mut r, 1);
                    }
                    5 => {
                        r.push(103);
                        r.extend_from_slice(request);
                        strput(&mut r, b"abc");
                    }
                    6 => {
                        let start = 5 + 4 + 1 + 8;
                        let n =
                            u32::from_be_bytes(p[start..start + 4].try_into().unwrap()) as usize;
                        self.writes
                            .lock()
                            .unwrap()
                            .extend_from_slice(&p[start + 4..start + 4 + n]);
                        r.push(101);
                        r.extend_from_slice(request);
                        num(&mut r, 0);
                    }
                    4 | 14 | 13 | 15 | 18 | 9 => {
                        r.push(101);
                        r.extend_from_slice(request);
                        num(&mut r, 0);
                    }
                    _ => panic!("unexpected request {}", p[0]),
                }
            }
            let mut framed = Vec::new();
            num(&mut framed, r.len() as u32);
            framed.extend_from_slice(&r);
            s.data(id, framed)?;
        }
        Ok(())
    }
}
#[tokio::test]
async fn subsystem_runs_over_real_authenticated_ssh() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let writes = Arc::new(Mutex::new(Vec::new()));
    let captured = writes.clone();
    let task = tokio::spawn(async move {
        let config = Arc::new(russh::server::Config {
            keys: vec![russh::keys::decode_secret_key(KEY, Some("blabla")).unwrap()],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let (socket, _) = listener.accept().await.unwrap();
        let session = russh::server::run_stream(
            config,
            socket,
            Server {
                buf: vec![],
                writes: captured,
                directory_read: false,
            },
        )
        .await
        .unwrap();
        let _ = session.await;
    });
    let c = RusshConnector::default()
        .connect(
            ConnectionRequest {
                host: "127.0.0.1".into(),
                port,
                username: "test".into(),
                authentication: Authentication::Password(Secret::new(b"test".to_vec())),
            },
            &Verify,
        )
        .await
        .unwrap();
    let mut s = c.open_sftp().await.unwrap();
    let entries = s.list("/").await.unwrap();
    assert_eq!(entries[0].name, "file");
    assert_eq!(s.read("/file", 0, 3).await.unwrap(), b"abc");
    s.write_file("/new", b"hello", false).await.unwrap();
    assert_eq!(&*writes.lock().unwrap(), b"hello");
    drop(s);
    c.close().await.unwrap();
    task.abort();
}
