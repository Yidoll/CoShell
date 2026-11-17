//! Independent snapshot publication and bounded, read-only discovery.
use super::*;
use crate::snapshot_manifest::{self as format, SnapshotArtifact, SnapshotProtection};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const MAX_LIST: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 1000;
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotCard {
    pub id: String,
    pub directory: String,
    pub name: String,
    pub created_at: Option<String>,
    pub app_version: Option<String>,
    pub format_version: Option<u32>,
    pub size_bytes: Option<u64>,
    pub protection: String,
    pub contains_credentials: Option<bool>,
    pub state: String,
    pub reason: Option<String>,
    pub restorable: bool,
    pub source: String,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotPage {
    pub snapshots: Vec<SnapshotCard>,
    pub next_cursor: Option<String>,
    pub scanned_count: usize,
}
pub struct DownloadedSnapshot {
    pub manifest: Vec<u8>,
    pub complete: Vec<u8>,
    pub connections_package: Zeroizing<Vec<u8>>,
    pub credentials_package: Option<Zeroizing<Vec<u8>>>,
}
impl std::fmt::Debug for DownloadedSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DownloadedSnapshot([PACKAGES REDACTED])")
    }
}
fn valid_directory(s: &str) -> bool {
    s.len() == 14 && s.bytes().all(|b| b.is_ascii_digit())
}
fn valid_operation(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn snapshot_url(base: &Url, directory: &str, file: &str) -> Result<Url, WebDavError> {
    if !valid_directory(directory)
        || ![
            format::MANIFEST_FILE,
            format::COMPLETE_MARKER,
            format::CONNECTIONS_PACKAGE,
            format::CREDENTIALS_PACKAGE,
        ]
        .contains(&file)
    {
        return Err(WebDavError::InvalidInput);
    }
    base.join(&format!("{directory}/{file}"))
        .map_err(|_| WebDavError::InvalidInput)
}
fn directory_time(s: &str) -> Option<String> {
    if !valid_directory(s) {
        return None;
    }
    let number = |a: usize, b: usize| s[a..b].parse::<u32>().ok();
    let (year, month, day) = (number(0, 4)?, number(4, 6)?, number(6, 8)?);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 0,
    };
    if year == 0
        || day == 0
        || day > days
        || number(8, 10)? >= 24
        || number(10, 12)? >= 60
        || number(12, 14)? >= 60
    {
        return None;
    }
    Some(format!(
        "{}-{}-{}T{}:{}:{}Z",
        &s[..4],
        &s[4..6],
        &s[6..8],
        &s[8..10],
        &s[10..12],
        &s[12..14]
    ))
}
fn card(directory: &str) -> SnapshotCard {
    SnapshotCard {
        id: format!("snapshot:{directory}"),
        directory: directory.into(),
        name: directory.into(),
        created_at: directory_time(directory),
        app_version: None,
        format_version: None,
        size_bytes: None,
        protection: "unknown".into(),
        contains_credentials: None,
        state: "missing_manifest".into(),
        reason: Some("缺少 manifest.json".into()),
        restorable: false,
        source: "independent".into(),
    }
}
fn failed(mut c: SnapshotCard, state: &str, reason: &str) -> SnapshotCard {
    c.state = state.into();
    c.reason = Some(reason.into());
    c.restorable = false;
    c
}

impl WebDav {
    async fn snapshot_read(
        &self,
        url: Url,
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<u8>, WebDavError> {
        let response = self
            .request(Method::GET, url, HeaderMap::new(), Vec::new(), cancel)
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(WebDavError::NotFound);
        }
        ensure_success(response.status())?;
        self.response_bounded(response, limit, cancel).await
    }
    /// Caller MUST persist directory, operation and complete artifact before use.
    /// This method never adopts an existing directory, removes remote data, or
    /// changes the legacy latest/history index.
    pub async fn publish_snapshot(
        &self,
        directory: &str,
        artifact: &SnapshotArtifact,
        operation: &str,
        cancel: &Cancellation,
    ) -> Result<(), WebDavError> {
        if !valid_directory(directory) || !valid_operation(operation) {
            return Err(WebDavError::InvalidInput);
        }
        let manifest =
            format::parse_manifest(&artifact.manifest).map_err(|_| WebDavError::InvalidInput)?;
        format::verify_complete(&artifact.manifest, &artifact.complete)
            .map_err(|_| WebDavError::InvalidInput)?;
        if manifest.includes_credentials != artifact.credentials_package.is_some() {
            return Err(WebDavError::InvalidInput);
        }
        format::verify_package(&manifest.files[0], &artifact.connections_package)
            .map_err(|_| WebDavError::InvalidInput)?;
        if let Some(bytes) = &artifact.credentials_package {
            format::verify_package(&manifest.files[1], bytes)
                .map_err(|_| WebDavError::InvalidInput)?;
        }
        tokio::time::timeout(self.timeout, async {
            let url = self
                .base
                .join(&format!("{directory}/"))
                .map_err(|_| WebDavError::InvalidInput)?;
            let created = self
                .request(
                    Method::from_bytes(b"MKCOL").unwrap(),
                    url,
                    HeaderMap::new(),
                    Vec::new(),
                    cancel,
                )
                .await
                .map_err(|_| WebDavError::OutcomeUnknown)?;
            if created.status() != StatusCode::CREATED {
                return Err(if created.status() == StatusCode::METHOD_NOT_ALLOWED {
                    WebDavError::Conflict
                } else if created.status().is_success() {
                    WebDavError::Unsupported
                } else {
                    ensure_success(created.status()).unwrap_err()
                });
            }
            let put = async {
                self.put_snapshot_file(
                    directory,
                    format::CONNECTIONS_PACKAGE,
                    &artifact.connections_package,
                    cancel,
                )
                .await?;
                if let Some(bytes) = &artifact.credentials_package {
                    self.put_snapshot_file(directory, format::CREDENTIALS_PACKAGE, bytes, cancel)
                        .await?;
                }
                self.put_snapshot_file(
                    directory,
                    format::MANIFEST_FILE,
                    &artifact.manifest,
                    cancel,
                )
                .await?;
                // Complete is always the last remote write and binds the exact manifest.
                self.put_snapshot_file(
                    directory,
                    format::COMPLETE_MARKER,
                    &artifact.complete,
                    cancel,
                )
                .await?;
                self.verify_snapshot(directory, artifact, operation, cancel)
                    .await
            }
            .await;
            // Once our collection exists, any unverified failure may have
            // happened after a committed remote write (including a 401 during
            // readback). Never report a definite no-write failure.
            put.map_err(|_| WebDavError::OutcomeUnknown)
        })
        .await
        .unwrap_or(Err(WebDavError::OutcomeUnknown))
    }
    async fn put_snapshot_file(
        &self,
        directory: &str,
        name: &str,
        bytes: &[u8],
        cancel: &Cancellation,
    ) -> Result<(), WebDavError> {
        let mut headers = HeaderMap::new();
        headers.insert(IF_NONE_MATCH, HeaderValue::from_static("*"));
        let url = snapshot_url(&self.base, directory, name)?;
        let response = self
            .request(Method::PUT, url.clone(), headers, bytes.to_vec(), cancel)
            .await?;
        ensure_success(response.status())?;
        let result = Zeroizing::new(self.snapshot_read(url, bytes.len(), cancel).await?);
        if result.as_slice() != bytes {
            return Err(WebDavError::Integrity);
        }
        Ok(())
    }
    /// Read-only reconciliation: exact artifact equality, never directory name or
    /// an untrusted completion marker alone, establishes this attempt's outcome.
    pub async fn verify_snapshot(
        &self,
        directory: &str,
        artifact: &SnapshotArtifact,
        operation: &str,
        cancel: &Cancellation,
    ) -> Result<(), WebDavError> {
        if !valid_operation(operation) {
            return Err(WebDavError::InvalidInput);
        }
        let remote = self.download_snapshot(directory, cancel).await?;
        if remote.manifest != artifact.manifest
            || remote.complete != artifact.complete
            || remote.connections_package.as_slice() != artifact.connections_package.as_slice()
            || remote.credentials_package.as_deref().map(|b| b.as_slice())
                != artifact
                    .credentials_package
                    .as_deref()
                    .map(|b| b.as_slice())
        {
            return Err(WebDavError::Integrity);
        }
        Ok(())
    }
    pub async fn download_snapshot(
        &self,
        directory: &str,
        cancel: &Cancellation,
    ) -> Result<DownloadedSnapshot, WebDavError> {
        tokio::time::timeout(self.timeout, async {
            let manifest = self
                .snapshot_read(
                    snapshot_url(&self.base, directory, format::MANIFEST_FILE)?,
                    format::MAX_MANIFEST_BYTES,
                    cancel,
                )
                .await?;
            let parsed = format::parse_manifest(&manifest).map_err(|_| WebDavError::Integrity)?;
            let complete = self
                .snapshot_read(
                    snapshot_url(&self.base, directory, format::COMPLETE_MARKER)?,
                    format::MAX_MANIFEST_BYTES,
                    cancel,
                )
                .await?;
            format::verify_complete(&manifest, &complete).map_err(|_| WebDavError::Integrity)?;
            let connections_package = Zeroizing::new(
                self.snapshot_read(
                    snapshot_url(&self.base, directory, format::CONNECTIONS_PACKAGE)?,
                    format::MAX_PACKAGE_BYTES,
                    cancel,
                )
                .await?,
            );
            format::verify_package(&parsed.files[0], &connections_package)
                .map_err(|_| WebDavError::Integrity)?;
            let credentials_package = if parsed.includes_credentials {
                let bytes = Zeroizing::new(
                    self.snapshot_read(
                        snapshot_url(&self.base, directory, format::CREDENTIALS_PACKAGE)?,
                        format::MAX_PACKAGE_BYTES,
                        cancel,
                    )
                    .await?,
                );
                format::verify_package(&parsed.files[1], &bytes)
                    .map_err(|_| WebDavError::Integrity)?;
                Some(bytes)
            } else {
                None
            };
            Ok(DownloadedSnapshot {
                manifest,
                complete,
                connections_package,
                credentials_package,
            })
        })
        .await
        .unwrap_or(Err(WebDavError::Timeout))
    }
    async fn snapshot_card(&self, directory: &str, cancel: &Cancellation) -> SnapshotCard {
        let mut c = card(directory);
        let manifest = match self
            .snapshot_read(
                snapshot_url(&self.base, directory, format::MANIFEST_FILE).unwrap(),
                format::MAX_MANIFEST_BYTES,
                cancel,
            )
            .await
        {
            Ok(v) => v,
            Err(WebDavError::NotFound) => return c,
            Err(_) => return failed(c, "unavailable", "暂不可读取清单"),
        };
        // A known unsupported format remains visible without borrowing today's app version.
        #[derive(Deserialize)]
        struct Version {
            #[serde(rename = "formatVersion")]
            format_version: u32,
        }
        if let Ok(v) = serde_json::from_slice::<Version>(&manifest) {
            c.format_version = Some(v.format_version);
            // A newer schema may not have today's required fields at all.
            // Identify its version before attempting the current full shape.
            if v.format_version != format::FORMAT_VERSION {
                return failed(c, "unsupported_format", "备份格式不支持");
            }
        }
        let parsed = match format::parse_manifest(&manifest) {
            Ok(v) => v,
            Err(crate::BackupError::UnsupportedVersion) => {
                return failed(c, "unsupported_format", "备份格式不支持")
            }
            Err(_) => return failed(c, "invalid_manifest", "manifest 无效或不可解析"),
        };
        c.created_at = Some(parsed.created_at.clone());
        c.app_version = Some(parsed.app_version.clone());
        c.protection = match parsed.protection {
            SnapshotProtection::Encrypted => "encrypted",
            SnapshotProtection::Plaintext => "none",
        }
        .into();
        c.contains_credentials = Some(parsed.includes_credentials);
        c.size_bytes = Some(parsed.files.iter().map(|f| f.length).sum());
        let complete = match self
            .snapshot_read(
                snapshot_url(&self.base, directory, format::COMPLETE_MARKER).unwrap(),
                format::MAX_MANIFEST_BYTES,
                cancel,
            )
            .await
        {
            Ok(v) => v,
            Err(WebDavError::NotFound) => {
                return failed(c, "incomplete", "缺少 complete.json，上传未完成")
            }
            Err(_) => return failed(c, "unavailable", "暂不可读取完成标记"),
        };
        if format::verify_complete(&manifest, &complete).is_err() {
            return failed(c, "integrity_mismatch", "完成标记与清单不一致");
        }
        for file in &parsed.files {
            let response = match self
                .request(
                    Method::HEAD,
                    snapshot_url(&self.base, directory, &file.path).unwrap(),
                    HeaderMap::new(),
                    Vec::new(),
                    cancel,
                )
                .await
            {
                Ok(v) => v,
                Err(_) => return failed(c, "unavailable", "暂不可检查必需文件"),
            };
            if response.status() == StatusCode::NOT_FOUND {
                return if file.path == format::CREDENTIALS_PACKAGE {
                    failed(c, "missing_credentials", "缺少凭据包")
                } else {
                    failed(c, "missing_connections", "缺少连接资料包")
                };
            }
            if ensure_success(response.status()).is_err() {
                return failed(c, "unavailable", "无法检查必需文件权限或响应");
            }
            match response
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
            {
                Some(n) if n == file.length => {}
                Some(_) => return failed(c, "integrity_mismatch", "文件大小与清单不一致"),
                None => return failed(c, "unavailable", "服务器未提供必需文件大小"),
            }
        }
        c.state = "structurally_complete".into();
        c.reason = Some("结构完整；恢复前仍需完整校验".into());
        c.restorable = true;
        c
    }
    async fn legacy_evidence(
        &self,
        directory: &str,
        cancel: &Cancellation,
    ) -> Result<bool, WebDavError> {
        let target = self
            .base
            .join(&format!("{directory}/"))
            .map_err(|_| WebDavError::InvalidInput)?;
        let mut headers = HeaderMap::new();
        headers.insert("Depth", HeaderValue::from_static("1"));
        let response = self
            .request(
                Method::from_bytes(b"PROPFIND").unwrap(),
                target.clone(),
                headers,
                Vec::new(),
                cancel,
            )
            .await?;
        if response.status() != StatusCode::MULTI_STATUS {
            return Ok(false);
        }
        let bytes = self.response_bounded(response, 65536, cancel).await?;
        Ok(parse_listing(&bytes, &target)?
            .iter()
            .any(|(name, collection)| {
                !collection
                    && name.starts_with("attempt-")
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            }))
    }
    async fn checked_card(&self, directory: &str, cancel: &Cancellation) -> SnapshotCard {
        let budget = (self.timeout / 4)
            .min(Duration::from_secs(3))
            .max(Duration::from_millis(10));
        tokio::time::timeout(budget, async {
            let mut item = self.snapshot_card(directory, cancel).await;
            if item.state == "missing_manifest"
                && self
                    .legacy_evidence(directory, cancel)
                    .await
                    .unwrap_or(false)
            {
                item.state = "legacy_unconfirmed".into();
                item.reason = Some("旧格式待确认；无可证明已发布的索引".into());
            }
            item
        })
        .await
        .unwrap_or_else(|_| failed(card(directory), "unavailable", "条目检查超时，请重试"))
    }
    async fn legacy_card(
        &self,
        old: &IndexRecord,
        index: usize,
        cancel: &Cancellation,
    ) -> SnapshotCard {
        let directory = old.object.split_once('/').map_or("", |(d, _)| d);
        let c = SnapshotCard {
            id: format!("legacy:{}", old.snapshot_id),
            directory: directory.into(),
            name: if directory.is_empty() {
                old.snapshot_id.clone()
            } else {
                directory.into()
            },
            created_at: directory_time(directory),
            app_version: None,
            format_version: Some(old.version as u32),
            size_bytes: None,
            protection: "unknown".into(),
            contains_credentials: None,
            state: "legacy".into(),
            reason: Some(
                if index == 0 {
                    "旧格式已发布快照；恢复前需完整校验"
                } else {
                    "旧格式历史快照；恢复前需完整校验"
                }
                .into(),
            ),
            restorable: true,
            source: "legacy".into(),
        };
        let budget = (self.timeout / 4)
            .min(Duration::from_secs(3))
            .max(Duration::from_millis(10));
        tokio::time::timeout(budget, async {
            let url = match self.published_object(&old.object) {
                Ok(url) => url,
                Err(_) => return failed(c.clone(), "unavailable", "旧格式对象路径无效"),
            };
            let response = match self
                .request(Method::HEAD, url, HeaderMap::new(), Vec::new(), cancel)
                .await
            {
                Ok(response) => response,
                Err(_) => return failed(c.clone(), "unavailable", "暂不可检查旧格式备份"),
            };
            if response.status() == StatusCode::NOT_FOUND {
                return failed(c.clone(), "missing_connections", "旧格式连接资料包缺失");
            }
            if ensure_success(response.status()).is_err() {
                return failed(c.clone(), "unavailable", "旧格式备份不可访问");
            }
            let size = response
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            match size {
                Some(size) if size > 0 && size <= MAX_OBJECT as u64 => {
                    let mut c = c.clone();
                    c.size_bytes = Some(size);
                    c
                }
                Some(_) => failed(c.clone(), "integrity_mismatch", "旧格式备份大小无效"),
                None => failed(c.clone(), "unavailable", "旧格式备份大小未知"),
            }
        })
        .await
        .unwrap_or_else(|_| failed(c, "unavailable", "旧格式备份检查超时"))
    }
    pub async fn list_snapshots(
        &self,
        cursor: Option<&str>,
        cancel: &Cancellation,
    ) -> Result<SnapshotPage, WebDavError> {
        tokio::time::timeout(self.timeout,async{
   let mut headers=HeaderMap::new();headers.insert("Depth",HeaderValue::from_static("1"));headers.insert("Content-Type",HeaderValue::from_static("application/xml; charset=utf-8"));
   let response=self.request(Method::from_bytes(b"PROPFIND").unwrap(),self.base.clone(),headers,b"<?xml version=\"1.0\"?><d:propfind xmlns:d=\"DAV:\"><d:prop><d:resourcetype/></d:prop></d:propfind>".to_vec(),cancel).await?;
   if response.status()!=StatusCode::MULTI_STATUS{return Err(if response.status()==StatusCode::NOT_FOUND{WebDavError::NotFound}else{ensure_success(response.status()).err().unwrap_or(WebDavError::Unsupported)});}
   let body=self.response_bounded(response,MAX_LIST,cancel).await?;let directories=parse_directories(&body,&self.base)?;
   let mut legacy_error=None;
   let legacy=match self.snapshot_read(self.object("latest")?,MAX_INDEX,cancel).await {Err(WebDavError::NotFound)=>Vec::new(),Ok(_)=>match self.published_versions(cancel).await{Ok(v)=>v,Err(WebDavError::Cancelled)=>return Err(WebDavError::Cancelled),Err(_)=>{legacy_error=Some("旧格式索引无效或暂不可读取");Vec::new()}},Err(WebDavError::Cancelled)=>return Err(WebDavError::Cancelled),Err(_)=>{legacy_error=Some("旧格式索引暂不可读取");Vec::new()}};
   // Scan metadata with bounded concurrency, then order by validated manifest time.
   // This permits individual bad directories without hiding valid snapshots.
   let mut snapshots=Vec::new();let mut chunks=directories.chunks(8);
   while let Some(batch)=chunks.next(){let results=tokio::join!(
    async{if let Some(d)=batch.first(){Some(self.checked_card(d,cancel).await)}else{None}},
    async{if let Some(d)=batch.get(1){Some(self.checked_card(d,cancel).await)}else{None}},
    async{if let Some(d)=batch.get(2){Some(self.checked_card(d,cancel).await)}else{None}},
    async{if let Some(d)=batch.get(3){Some(self.checked_card(d,cancel).await)}else{None}},
    async{if let Some(d)=batch.get(4){Some(self.checked_card(d,cancel).await)}else{None}},
    async{if let Some(d)=batch.get(5){Some(self.checked_card(d,cancel).await)}else{None}},
    async{if let Some(d)=batch.get(6){Some(self.checked_card(d,cancel).await)}else{None}},
    async{if let Some(d)=batch.get(7){Some(self.checked_card(d,cancel).await)}else{None}});
    for item in [results.0,results.1,results.2,results.3,results.4,results.5,results.6,results.7].into_iter().flatten(){snapshots.push(item);}
   }
   for (index,old) in legacy.iter().enumerate(){let directory=old.object.split_once('/').map_or("",|(d,_)|d);if !directory.is_empty(){snapshots.retain(|c|!(c.directory==directory&&matches!(c.state.as_str(),"missing_manifest"|"legacy_unconfirmed")));}snapshots.push(self.legacy_card(old,index,cancel).await);}
   if let Some(reason)=legacy_error{let mut c=card("旧格式索引");c.id="legacy:unavailable".into();c.created_at=None;c.source="legacy".into();c.state="unavailable".into();c.reason=Some(reason.into());snapshots.push(c);}
   snapshots.sort_by(|a,b|b.created_at.cmp(&a.created_at).then_with(||b.id.cmp(&a.id)));
   let digest=format::sha256(serde_json::to_vec(&snapshots).map_err(|_|WebDavError::Integrity)?.as_slice());
   let offset=match cursor{None=>0,Some(value)=>{let(head,offset)=value.split_once(':').ok_or(WebDavError::InvalidInput)?;if head!=digest{return Err(WebDavError::Conflict);}offset.parse::<usize>().map_err(|_|WebDavError::InvalidInput)?}};
   if offset>snapshots.len(){return Err(WebDavError::InvalidInput);}let end=(offset+50).min(snapshots.len());let scanned_count=snapshots.len();let snapshots=snapshots[offset..end].to_vec();if *cancel.0.subscribe().borrow(){return Err(WebDavError::Cancelled);}Ok(SnapshotPage{snapshots,next_cursor:(end<scanned_count).then(||format!("{digest}:{end}")),scanned_count})
  }).await.unwrap_or(Err(WebDavError::Timeout))
    }
    async fn response_bounded(
        &self,
        mut response: reqwest::Response,
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<u8>, WebDavError> {
        if response.content_length().is_some_and(|n| n > limit as u64) {
            return Err(WebDavError::Integrity);
        }
        let mut result = Vec::new();
        loop {
            let mut receiver = cancel.0.subscribe();
            if *receiver.borrow() {
                return Err(WebDavError::Cancelled);
            }
            let chunk = tokio::select! {biased;_=receiver.changed()=>return Err(WebDavError::Cancelled),chunk=response.chunk()=>chunk.map_err(|_|WebDavError::Transport)?};
            match chunk {
                None => return Ok(result),
                Some(bytes) => {
                    if result.len() + bytes.len() > limit {
                        return Err(WebDavError::Integrity);
                    }
                    result.extend_from_slice(&bytes);
                }
            }
        }
    }
}

fn parse_directories(body: &[u8], base: &Url) -> Result<Vec<String>, WebDavError> {
    Ok(parse_listing(body, base)?
        .into_iter()
        .filter(|(name, collection)| *collection && valid_directory(name))
        .map(|(name, _)| name)
        .collect())
}
fn parse_listing(body: &[u8], base: &Url) -> Result<Vec<(String, bool)>, WebDavError> {
    use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
    let mut reader = NsReader::from_reader(body);
    reader.config_mut().expand_empty_elements = true;
    let mut path = Vec::new();
    let mut href = String::new();
    let mut href_count = 0;
    let mut status = String::new();
    let mut status_count = 0;
    let mut collection = false;
    let mut valid_collection = false;
    let mut valid_properties = false;
    let mut result = Vec::new();
    let mut responses = 0;
    let mut roots = 0;
    let mut response_status = false;
    loop {
        let (ns, event) = reader
            .read_resolved_event()
            .map_err(|_| WebDavError::Integrity)?;
        match event {
            Event::Start(e) => {
                let name = if matches!(ns,ResolveResult::Bound(ns)if ns.as_ref()=="DAV:") {
                    e.local_name().as_ref().to_string()
                } else {
                    "#".into()
                };
                if path.is_empty() {
                    roots += 1;
                    if name != "multistatus" || roots != 1 {
                        return Err(WebDavError::Integrity);
                    }
                }
                path.push(name);
                if path == ["multistatus", "response"] {
                    responses += 1;
                    if responses > MAX_ENTRIES + 1 {
                        return Err(WebDavError::Integrity);
                    }
                    href.clear();
                    href_count = 0;
                    valid_collection = false;
                    valid_properties = false;
                    response_status = false;
                }
                if path == ["multistatus", "response", "href"] {
                    href_count += 1;
                    if href_count > 1 {
                        return Err(WebDavError::Integrity);
                    }
                }
                if path == ["multistatus", "response", "status"] {
                    response_status = true;
                }
                if path == ["multistatus", "response", "propstat"] {
                    status.clear();
                    status_count = 0;
                    collection = false;
                }
                if path == ["multistatus", "response", "propstat", "status"] {
                    status_count += 1;
                    if status_count > 1 {
                        return Err(WebDavError::Integrity);
                    }
                }
                if path
                    == [
                        "multistatus",
                        "response",
                        "propstat",
                        "prop",
                        "resourcetype",
                        "collection",
                    ]
                {
                    collection = true;
                }
            }
            Event::Text(t) => {
                if t.as_ref().contains('&') {
                    return Err(WebDavError::Integrity);
                }
                if path == ["multistatus", "response", "href"] {
                    href.push_str(t.as_ref());
                } else if path == ["multistatus", "response", "propstat", "status"] {
                    status.push_str(t.as_ref());
                } else if path.is_empty() && !t.as_ref().trim().is_empty() {
                    return Err(WebDavError::Integrity);
                }
            }
            Event::End(_) => {
                if path == ["multistatus", "response", "propstat"]
                    && status_count == 1
                    && is_success_status_line(&status)
                {
                    valid_properties = true;
                    if collection {
                        valid_collection = true;
                    }
                }
                if path == ["multistatus", "response"] {
                    if href_count != 1 || href.trim().is_empty() {
                        return Err(WebDavError::Integrity);
                    }
                    let url = base.join(href.trim()).map_err(|_| WebDavError::Integrity)?;
                    if url.origin() != base.origin()
                        || url.query().is_some()
                        || url.fragment().is_some()
                        || !url.username().is_empty()
                        || url.password().is_some()
                    {
                        return Err(WebDavError::Integrity);
                    }
                    if valid_properties && !response_status {
                        let base_path = base.path();
                        if url.path().trim_end_matches('/') != base_path.trim_end_matches('/') {
                            let tail = url
                                .path()
                                .strip_prefix(base_path)
                                .ok_or(WebDavError::Integrity)?
                                .trim_end_matches('/');
                            if !tail.is_empty()
                                && !tail.contains('/')
                                && !tail.contains('%')
                                && !tail.contains('\\')
                            {
                                result.push((tail.to_owned(), valid_collection));
                            }
                        }
                    }
                }
                if path.pop().is_none() {
                    return Err(WebDavError::Integrity);
                }
            }
            Event::Eof => break,
            Event::Decl(_) | Event::Comment(_) => {}
            _ => return Err(WebDavError::Integrity),
        }
    }
    if !path.is_empty() || roots != 1 {
        return Err(WebDavError::Integrity);
    }
    result.sort_by(|a, b| b.0.cmp(&a.0));
    result.dedup();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn list_xml_requires_dav_root_unambiguous_target_and_bounded_entries() {
        let base = Url::parse("https://example.org/backup/").unwrap();
        for bad in [
            "",
            "<garbage/>",
            "<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href/></d:response></d:multistatus>",
            "<!DOCTYPE a><d:multistatus xmlns:d=\"DAV:\"/>",
        ] {
            assert!(parse_directories(bad.as_bytes(), &base).is_err(), "{bad}");
        }
    }
}
