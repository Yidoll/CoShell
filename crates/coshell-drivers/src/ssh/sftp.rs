//! Bounded SFTP v3 protocol, using an existing authenticated SSH connection.
use super::{SshError, SshFuture};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_PACKET: usize = 1024 * 1024;
pub const MAX_CHUNK: usize = 64 * 1024;
const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub modified: Option<u32>,
}

pub trait Sftp: Send {
    fn realpath<'a>(&'a mut self, path: &'a str) -> SshFuture<'a, String>;
    fn list<'a>(&'a mut self, path: &'a str) -> SshFuture<'a, Vec<Entry>>;
    fn stat<'a>(&'a mut self, path: &'a str) -> SshFuture<'a, Entry>;
    fn read_file<'a>(&'a mut self, path: &'a str, limit: usize) -> SshFuture<'a, (Vec<u8>, Entry)>;
    fn read<'a>(&'a mut self, path: &'a str, offset: u64, length: u32) -> SshFuture<'a, Vec<u8>>;
    fn write<'a>(
        &'a mut self,
        path: &'a str,
        offset: u64,
        data: &'a [u8],
        create: bool,
    ) -> SshFuture<'a, ()>;
    fn write_file<'a>(
        &'a mut self,
        path: &'a str,
        data: &'a [u8],
        overwrite: bool,
    ) -> SshFuture<'a, ()>;
    fn mkdir<'a>(&'a mut self, path: &'a str) -> SshFuture<'a, ()>;
    fn remove<'a>(&'a mut self, path: &'a str, directory: bool) -> SshFuture<'a, ()>;
    fn rename<'a>(&'a mut self, path: &'a str, destination: &'a str) -> SshFuture<'a, ()>;
    fn chmod<'a>(&'a mut self, path: &'a str, mode: u32) -> SshFuture<'a, ()>;
}

fn u32put(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn u64put(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn string(out: &mut Vec<u8>, value: &[u8]) {
    u32put(out, value.len() as u32);
    out.extend_from_slice(value);
}
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SshError> {
        let end = self.pos.checked_add(n).ok_or(SshError::Transport)?;
        let b = self.bytes.get(self.pos..end).ok_or(SshError::Transport)?;
        self.pos = end;
        Ok(b)
    }
    fn u32(&mut self) -> Result<u32, SshError> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| SshError::Transport)?,
        ))
    }
    fn u64(&mut self) -> Result<u64, SshError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().map_err(|_| SshError::Transport)?,
        ))
    }
    fn string(&mut self) -> Result<&'a [u8], SshError> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    fn attrs(&mut self, name: String) -> Result<Entry, SshError> {
        let flags = self.u32()?;
        if flags & !0x8000_000f != 0 {
            return Err(SshError::Transport);
        };
        let size = if flags & 1 != 0 {
            Some(self.u64()?)
        } else {
            None
        };
        if flags & 2 != 0 {
            self.take(8)?;
        }
        let permissions = if flags & 4 != 0 {
            Some(self.u32()?)
        } else {
            None
        };
        let modified = if flags & 8 != 0 {
            self.u32()?;
            Some(self.u32()?)
        } else {
            None
        };
        if flags & 0x8000_0000 != 0 {
            let n = self.u32()?;
            if n > 1024 {
                return Err(SshError::Transport);
            };
            for _ in 0..n {
                self.string()?;
                self.string()?;
            }
        }
        Ok(Entry {
            name,
            size,
            permissions,
            modified,
        })
    }
}

pub struct Client<T> {
    stream: T,
    next: u32,
}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Client<T> {
    pub async fn init(stream: T) -> Result<Self, SshError> {
        let mut c = Self { stream, next: 1 };
        c.packet(&[1, 0, 0, 0, 3]).await?;
        let p = c.receive().await?;
        if p.len() < 5
            || p[0] != 2
            || u32::from_be_bytes(p[1..5].try_into().map_err(|_| SshError::Transport)?) != 3
        {
            return Err(SshError::Transport);
        };
        Ok(c)
    }
    async fn packet(&mut self, p: &[u8]) -> Result<(), SshError> {
        if p.len() > MAX_PACKET {
            return Err(SshError::Transport);
        };
        self.stream
            .write_all(&(p.len() as u32).to_be_bytes())
            .await
            .map_err(|_| SshError::Transport)?;
        self.stream
            .write_all(p)
            .await
            .map_err(|_| SshError::Transport)?;
        self.stream.flush().await.map_err(|_| SshError::Transport)
    }
    async fn receive(&mut self) -> Result<Vec<u8>, SshError> {
        let n = self
            .stream
            .read_u32()
            .await
            .map_err(|_| SshError::Transport)? as usize;
        if !(5..=MAX_PACKET).contains(&n) {
            return Err(SshError::Transport);
        };
        let mut p = vec![0; n];
        self.stream
            .read_exact(&mut p)
            .await
            .map_err(|_| SshError::Transport)?;
        Ok(p)
    }
    async fn request(&mut self, kind: u8, body: Vec<u8>) -> Result<(u8, Vec<u8>), SshError> {
        let id = self.next;
        self.next = self.next.checked_add(1).ok_or(SshError::Transport)?;
        let mut p = vec![kind];
        u32put(&mut p, id);
        p.extend_from_slice(&body);
        self.packet(&p).await?;
        let reply = self.receive().await?;
        if reply.len() < 5
            || u32::from_be_bytes(reply[1..5].try_into().map_err(|_| SshError::Transport)?) != id
        {
            return Err(SshError::Transport);
        };
        Ok((reply[0], reply[5..].to_vec()))
    }
    async fn status(&mut self, kind: u8, body: Vec<u8>) -> Result<(), SshError> {
        let (t, p) = self.request(kind, body).await?;
        if t != 101 {
            return Err(SshError::Transport);
        };
        let mut c = Cursor { bytes: &p, pos: 0 };
        if c.u32()? == 0 {
            Ok(())
        } else {
            Err(SshError::Transport)
        }
    }
    async fn handle(&mut self, kind: u8, body: Vec<u8>) -> Result<Vec<u8>, SshError> {
        let (t, p) = self.request(kind, body).await?;
        if t != 102 {
            return Err(SshError::Transport);
        };
        let mut c = Cursor { bytes: &p, pos: 0 };
        let h = c.string()?.to_vec();
        if h.len() > 65536 {
            return Err(SshError::Transport);
        };
        Ok(h)
    }
    async fn close_handle(&mut self, h: &[u8]) -> Result<(), SshError> {
        let mut p = vec![];
        string(&mut p, h);
        self.status(4, p).await
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Sftp for Client<T> {
    fn read_file<'a>(&'a mut self, path: &'a str, limit: usize) -> SshFuture<'a, (Vec<u8>, Entry)> {
        Box::pin(async move {
            if limit == 0 || limit > 16 * 1024 * 1024 {
                return Err(SshError::Transport);
            }
            let before = self.stat(path).await?;
            if !before.permissions.is_some_and(|p| p & 0o170000 == 0o100000)
                || before.size.is_none_or(|n| n > limit as u64)
            {
                return Err(SshError::Transport);
            }
            let mut request = vec![];
            string(&mut request, path.as_bytes());
            u32put(&mut request, 1);
            u32put(&mut request, 0);
            let handle = self.handle(3, request).await?;
            let result = async {
                let mut request = vec![];
                string(&mut request, &handle);
                let (t, p) = self.request(8, request).await?;
                if t != 105 {
                    return Err(SshError::Transport);
                };
                let first = Cursor { bytes: &p, pos: 0 }.attrs(path.into())?;
                if first != before {
                    return Err(SshError::Transport);
                }
                let size = first.size.ok_or(SshError::Transport)? as usize;
                let mut data = Vec::with_capacity(size);
                while data.len() < size {
                    let mut request = vec![];
                    string(&mut request, &handle);
                    u64put(&mut request, data.len() as u64);
                    let n = (size - data.len()).min(MAX_CHUNK);
                    u32put(&mut request, n as u32);
                    let (t, p) = self.request(5, request).await?;
                    if t != 103 {
                        return Err(SshError::Transport);
                    }
                    let mut c = Cursor { bytes: &p, pos: 0 };
                    let bytes = c.string()?;
                    if bytes.is_empty() || bytes.len() > n {
                        return Err(SshError::Transport);
                    }
                    data.extend_from_slice(bytes);
                }
                let mut request = vec![];
                string(&mut request, &handle);
                let (t, p) = self.request(8, request).await?;
                if t != 105 {
                    return Err(SshError::Transport);
                };
                let last = Cursor { bytes: &p, pos: 0 }.attrs(path.into())?;
                if last != first {
                    return Err(SshError::Transport);
                }
                Ok((data, last))
            }
            .await;
            let closed = self.close_handle(&handle).await;
            let (data, last) = result?;
            closed?;
            let after = self.stat(path).await?;
            if after != last {
                return Err(SshError::Transport);
            }
            Ok((data, after))
        })
    }
    fn realpath<'a>(&'a mut self, path: &'a str) -> SshFuture<'a, String> {
        Box::pin(async move {
            let mut b = vec![];
            string(&mut b, path.as_bytes());
            let (t, p) = self.request(16, b).await?;
            if t != 104 {
                return Err(SshError::Transport);
            };
            let mut c = Cursor { bytes: &p, pos: 0 };
            if c.u32()? != 1 {
                return Err(SshError::Transport);
            };
            let name = std::str::from_utf8(c.string()?)
                .map_err(|_| SshError::Transport)?
                .to_owned();
            c.string()?;
            c.attrs(String::new())?;
            if !name.starts_with('/') || name.len() > 4096 || name.contains('\0') {
                return Err(SshError::Transport);
            };
            Ok(name)
        })
    }
    fn list<'a>(&'a mut self, path: &'a str) -> SshFuture<'a, Vec<Entry>> {
        Box::pin(async move {
            let mut b = vec![];
            string(&mut b, path.as_bytes());
            let h = self.handle(11, b).await?;
            let mut all = Vec::new();
            let mut seen = 0usize;
            let result = async {
                loop {
                    let mut b = vec![];
                    string(&mut b, &h);
                    let (t, p) = self.request(12, b).await?;
                    if t == 101 {
                        let mut c = Cursor { bytes: &p, pos: 0 };
                        return if c.u32()? == 1 {
                            Ok(())
                        } else {
                            Err(SshError::Transport)
                        };
                    }
                    if t != 104 {
                        return Err(SshError::Transport);
                    };
                    let mut c = Cursor { bytes: &p, pos: 0 };
                    let n = c.u32()? as usize;
                    if n == 0 || n > MAX_ENTRIES.saturating_sub(seen) {
                        return Err(SshError::Transport);
                    };
                    seen += n;
                    for _ in 0..n {
                        let name = std::str::from_utf8(c.string()?)
                            .map_err(|_| SshError::Transport)?
                            .to_owned();
                        c.string()?;
                        let entry = c.attrs(name)?;
                        if entry.name == "." || entry.name == ".." {
                            continue;
                        }
                        if entry.name.is_empty()
                            || entry.name.len() > 255
                            || entry.name.contains(['/', '\\', '\0'])
                        {
                            return Err(SshError::Transport);
                        };
                        all.push(entry);
                    }
                }
            }
            .await;
            let close = self.close_handle(&h).await;
            result?;
            close?;
            Ok(all)
        })
    }
    fn stat<'a>(&'a mut self, path: &'a str) -> SshFuture<'a, Entry> {
        Box::pin(async move {
            let mut b = vec![];
            string(&mut b, path.as_bytes());
            let (t, p) = self.request(7, b).await?;
            if t != 105 {
                return Err(SshError::Transport);
            };
            Cursor { bytes: &p, pos: 0 }.attrs(path.to_owned())
        })
    }
    fn read<'a>(&'a mut self, path: &'a str, offset: u64, length: u32) -> SshFuture<'a, Vec<u8>> {
        Box::pin(async move {
            if length == 0 || length as usize > MAX_CHUNK {
                return Err(SshError::Transport);
            };
            let mut b = vec![];
            string(&mut b, path.as_bytes());
            u32put(&mut b, 1);
            u32put(&mut b, 0);
            let h = self.handle(3, b).await?;
            let mut b = vec![];
            string(&mut b, &h);
            u64put(&mut b, offset);
            u32put(&mut b, length);
            let result = async {
                let (t, p) = self.request(5, b).await?;
                let mut c = Cursor { bytes: &p, pos: 0 };
                match t {
                    103 => {
                        let data = c.string()?;
                        if data.len() > length as usize {
                            return Err(SshError::Transport);
                        };
                        Ok(data.to_vec())
                    }
                    101 if c.u32()? == 1 => Ok(Vec::new()),
                    _ => Err(SshError::Transport),
                }
            }
            .await;
            let close = self.close_handle(&h).await;
            let result = result?;
            close?;
            Ok(result)
        })
    }
    fn write<'a>(
        &'a mut self,
        path: &'a str,
        offset: u64,
        data: &'a [u8],
        create: bool,
    ) -> SshFuture<'a, ()> {
        Box::pin(async move {
            if data.len() > MAX_CHUNK {
                return Err(SshError::Transport);
            };
            let mut b = vec![];
            string(&mut b, path.as_bytes());
            u32put(&mut b, if create { 2 | 8 | 32 } else { 2 });
            u32put(&mut b, 0);
            let h = self.handle(3, b).await?;
            let mut b = vec![];
            string(&mut b, &h);
            u64put(&mut b, offset);
            string(&mut b, data);
            let result = self.status(6, b).await;
            let close = self.close_handle(&h).await;
            result?;
            close
        })
    }
    fn write_file<'a>(
        &'a mut self,
        path: &'a str,
        data: &'a [u8],
        overwrite: bool,
    ) -> SshFuture<'a, ()> {
        Box::pin(async move {
            if data.len() > 16 * 1024 * 1024 {
                return Err(SshError::Transport);
            };
            let mut b = vec![];
            string(&mut b, path.as_bytes());
            u32put(&mut b, if overwrite { 2 | 8 | 16 } else { 2 | 8 | 32 });
            u32put(&mut b, 0);
            let h = self.handle(3, b).await?;
            let result = async {
                for (index, chunk) in data.chunks(MAX_CHUNK).enumerate() {
                    let mut b = vec![];
                    string(&mut b, &h);
                    u64put(&mut b, (index * MAX_CHUNK) as u64);
                    string(&mut b, chunk);
                    self.status(6, b).await?;
                }
                Ok(())
            }
            .await;
            let close = self.close_handle(&h).await;
            result?;
            close
        })
    }
    fn mkdir<'a>(&'a mut self, path: &'a str) -> SshFuture<'a, ()> {
        Box::pin(async move {
            let mut b = vec![];
            string(&mut b, path.as_bytes());
            u32put(&mut b, 0);
            self.status(14, b).await
        })
    }
    fn remove<'a>(&'a mut self, path: &'a str, directory: bool) -> SshFuture<'a, ()> {
        Box::pin(async move {
            let mut b = vec![];
            string(&mut b, path.as_bytes());
            self.status(if directory { 15 } else { 13 }, b).await
        })
    }
    fn rename<'a>(&'a mut self, path: &'a str, destination: &'a str) -> SshFuture<'a, ()> {
        Box::pin(async move {
            let mut b = vec![];
            string(&mut b, path.as_bytes());
            string(&mut b, destination.as_bytes());
            self.status(18, b).await
        })
    }
    fn chmod<'a>(&'a mut self, path: &'a str, mode: u32) -> SshFuture<'a, ()> {
        Box::pin(async move {
            let mut b = vec![];
            string(&mut b, path.as_bytes());
            u32put(&mut b, 4);
            u32put(&mut b, mode);
            self.status(9, b).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn complete_file_read_uses_one_handle_and_checks_metadata() {
        let (client, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            async fn receive(s: &mut tokio::io::DuplexStream) -> Vec<u8> {
                let n = s.read_u32().await.unwrap();
                let mut p = vec![0; n as usize];
                s.read_exact(&mut p).await.unwrap();
                p
            }
            async fn send(s: &mut tokio::io::DuplexStream, p: &[u8]) {
                s.write_u32(p.len() as u32).await.unwrap();
                s.write_all(p).await.unwrap();
            }
            receive(&mut server).await;
            send(&mut server, &[2, 0, 0, 0, 3]).await;
            let p = receive(&mut server).await;
            assert_eq!(p[0], 7);
            let mut r = vec![105];
            r.extend_from_slice(&p[1..5]);
            u32put(&mut r, 5);
            u64put(&mut r, 3);
            u32put(&mut r, 0o100644);
            send(&mut server, &r).await;
            let p = receive(&mut server).await;
            assert_eq!(p[0], 3);
            let mut r = vec![102];
            r.extend_from_slice(&p[1..5]);
            string(&mut r, b"h");
            send(&mut server, &r).await;
            for expected in [8, 5, 8, 4, 7] {
                let p = receive(&mut server).await;
                assert_eq!(p[0], expected);
                let mut r = vec![if expected == 8 || expected == 7 {
                    105
                } else if expected == 5 {
                    103
                } else {
                    101
                }];
                r.extend_from_slice(&p[1..5]);
                if expected == 8 || expected == 7 {
                    u32put(&mut r, 5);
                    u64put(&mut r, 3);
                    u32put(&mut r, 0o100644);
                } else if expected == 5 {
                    string(&mut r, b"abc");
                } else {
                    u32put(&mut r, 0);
                }
                send(&mut server, &r).await;
            }
        });
        let mut c = Client::init(client).await.unwrap();
        let (data, meta) = c.read_file("/f", 16).await.unwrap();
        assert_eq!(data, b"abc");
        assert_eq!(meta.size, Some(3));
        task.await.unwrap();
    }
    #[tokio::test]
    async fn list_decodes_attributes_and_closes_directory() {
        let (client, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            async fn recv(s: &mut tokio::io::DuplexStream) -> Vec<u8> {
                let n = s.read_u32().await.unwrap();
                let mut p = vec![0; n as usize];
                s.read_exact(&mut p).await.unwrap();
                p
            }
            async fn send(s: &mut tokio::io::DuplexStream, p: &[u8]) {
                s.write_u32(p.len() as u32).await.unwrap();
                s.write_all(p).await.unwrap();
            }
            assert_eq!(recv(&mut server).await[0], 1);
            send(&mut server, &[2, 0, 0, 0, 3]).await;
            let p = recv(&mut server).await;
            assert_eq!(p[0], 11);
            let mut v = vec![102];
            v.extend_from_slice(&p[1..5]);
            string(&mut v, b"handle");
            send(&mut server, &v).await;
            let p = recv(&mut server).await;
            assert_eq!(p[0], 12);
            let mut v = vec![104];
            v.extend_from_slice(&p[1..5]);
            u32put(&mut v, 1);
            string(&mut v, b"file.txt");
            string(&mut v, b"ignored");
            u32put(&mut v, 5);
            u64put(&mut v, 12);
            u32put(&mut v, 0o100644);
            send(&mut server, &v).await;
            let p = recv(&mut server).await;
            let mut v = vec![101];
            v.extend_from_slice(&p[1..5]);
            u32put(&mut v, 1);
            send(&mut server, &v).await;
            let p = recv(&mut server).await;
            assert_eq!(p[0], 4);
            let mut v = vec![101];
            v.extend_from_slice(&p[1..5]);
            u32put(&mut v, 0);
            send(&mut server, &v).await;
        });
        let mut c = Client::init(client).await.unwrap();
        let entries = c.list("/").await.unwrap();
        assert_eq!(
            entries,
            vec![Entry {
                name: "file.txt".into(),
                size: Some(12),
                permissions: Some(0o100644),
                modified: None
            }]
        );
        task.await.unwrap();
    }
    #[tokio::test]
    async fn rejects_oversized_or_mismatched_server_packets() {
        let (client, mut server) = tokio::io::duplex(64);
        tokio::spawn(async move {
            let n = server.read_u32().await.unwrap();
            let mut p = vec![0; n as usize];
            server.read_exact(&mut p).await.unwrap();
            server.write_u32((MAX_PACKET + 1) as u32).await.unwrap();
        });
        assert!(Client::init(client).await.is_err());
    }
}
