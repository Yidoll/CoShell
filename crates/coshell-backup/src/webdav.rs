//! WebDAV transport for opaque encrypted backups. No plaintext is handled here.
use chacha20poly1305::aead::{rand_core::RngCore, OsRng};
use reqwest::{
    header::{HeaderMap, HeaderValue, ETAG, IF_MATCH, IF_NONE_MATCH},
    redirect::Policy,
    Client, Method, StatusCode, Url,
};
use sha2::{Digest, Sha256};
use std::{net::IpAddr, time::Duration};
use tokio::sync::watch;
mod snapshots;
pub use snapshots::{DownloadedSnapshot, SnapshotCard, SnapshotPage};

const MAX_OBJECT: usize = 20 * 1024 * 1024;
const MAX_INDEX: usize = 1024;
const MAX_ETAG: usize = 128;

fn valid_condition(condition: &PublishCondition) -> bool {
    match condition {
        PublishCondition::Absent => true,
        PublishCondition::Match(etag) => {
            etag.len() >= 2
                && etag.len() <= MAX_ETAG
                && !etag.starts_with("W/")
                && etag.starts_with('"')
                && etag.ends_with('"')
                && HeaderValue::from_str(etag).is_ok()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebDavError {
    InvalidUrl,
    InvalidInput,
    Authentication,
    NotFound,
    Conflict,
    Unsupported,
    /// Server failed the concurrent conditional-create check.
    UnsupportedAtomicCreate,
    /// Server failed the conditional-update check.
    UnsupportedAtomicUpdate,
    /// A probe object could not be read back intact.
    UnsupportedReadback,
    /// A strong ETag was absent or invalid.
    UnsupportedStrongEtag,
    Timeout,
    Cancelled,
    Transport,
    Integrity,
    Server,
    OutcomeUnknown,
}

impl WebDavError {
    /// A retry is safe only for an operation that the caller can repeat using
    /// the same idempotency condition; a failed write may have completed remotely.
    pub fn retryable(self) -> bool {
        matches!(self, Self::Timeout | Self::Transport | Self::Server)
    }
}

#[derive(Clone)]
pub struct Cancellation(watch::Sender<bool>);
impl Default for Cancellation {
    fn default() -> Self {
        Self::new()
    }
}
impl Cancellation {
    pub fn new() -> Self {
        Self(watch::channel(false).0)
    }
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PublishCondition {
    Absent,
    Match(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub supports_move: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishMode {
    Move,
    ImmutableFallback,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexRecord {
    version: u8,
    snapshot_id: String,
    object: String,
    operation_token: String,
    condition: PublishCondition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupVersion {
    pub id: String,
    pub timestamp: Option<String>,
    pub latest: bool,
}
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupVersionPage {
    pub versions: Vec<BackupVersion>,
    pub next_cursor: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct OperationReceipt {
    version: u8,
    operation_token: String,
    target: String,
    snapshot_id: String,
    digest: [u8; 32],
    condition: PublishCondition,
}

/// Persist this value before starting a write; use a fresh attempt after a
/// failure, retaining the same operation token. Its ID is not a credential.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationAttempt {
    operation_token: String,
    attempt_id: String,
    target: String,
    snapshot_id: String,
    digest: [u8; 32],
    probe_id: String,
    condition: PublishCondition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    directory: Option<String>,
}

impl PublicationAttempt {
    pub fn new(
        operation_token: &str,
        target: &str,
        snapshot_id: &str,
        encrypted: &[u8],
        condition: PublishCondition,
    ) -> Result<Self, WebDavError> {
        if !valid_condition(&condition)
            || operation_token.is_empty()
            || operation_token.len() > 128
            || !operation_token
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(WebDavError::InvalidInput);
        }
        let mut random = [0u8; 16];
        OsRng
            .try_fill_bytes(&mut random)
            .map_err(|_| WebDavError::Transport)?;
        Ok(Self {
            operation_token: operation_token.into(),
            attempt_id: format!(
                "attempt-{}",
                random
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ),
            target: canonical_target(target)?,
            snapshot_id: snapshot_id.to_owned(),
            digest: Sha256::digest(encrypted).into(),
            probe_id: format!(
                "probe-{}",
                random
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ),
            condition,
            directory: None,
        })
    }
    pub fn new_timestamped(
        operation_token: &str,
        target: &str,
        snapshot_id: &str,
        encrypted: &[u8],
        condition: PublishCondition,
        directory: &str,
    ) -> Result<Self, WebDavError> {
        if directory.len() != 14 || !directory.bytes().all(|b| b.is_ascii_digit()) {
            return Err(WebDavError::InvalidInput);
        }
        let mut attempt = Self::new(operation_token, target, snapshot_id, encrypted, condition)?;
        attempt.directory = Some(directory.into());
        Ok(attempt)
    }
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    /// Canonical target used when binding attempts and serializing publications.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Persist the full attempt before use. These names may exist after a lost
    /// response; published objects must never be removed while referenced.
    pub fn possible_objects(&self) -> Vec<String> {
        vec![
            self.directory.as_ref().map_or_else(
                || self.attempt_id.clone(),
                |d| format!("{d}/{}", self.attempt_id),
            ),
            self.probe_id.clone(),
            format!("{}-create", self.probe_id),
            self.receipt_probe_name(),
        ]
    }

    fn receipt_probe_name(&self) -> String {
        format!(
            "receipt-{}",
            Sha256::digest(format!("sentinel:{}", self.probe_id).as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    }

    fn receipt_name(&self) -> String {
        format!(
            "receipt-{}",
            Sha256::digest(self.operation_token.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    }
}

#[derive(Debug)]
pub struct PublicationFailure {
    pub operation_token: String,
    pub attempt_id: String,
    /// Conservative: callers persist this even if the request was cancelled
    /// before the server acknowledged creation. Do not delete blindly.
    pub cleanup_pending: bool,
    pub possible_objects: Vec<String>,
    pub error: WebDavError,
}

#[derive(Debug)]
pub struct PublicationSuccess {
    pub mode: PublishMode,
    pub operation_token: String,
    pub attempt_id: String,
    pub cleanup_pending: bool,
    pub possible_objects: Vec<String>,
}

pub fn canonical_target(target: &str) -> Result<String, WebDavError> {
    let mut url = Url::parse(target).map_err(|_| WebDavError::InvalidUrl)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(WebDavError::InvalidUrl);
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url.to_string())
}

pub struct WebDav {
    client: Client,
    base: Url,
    username: String,
    password: zeroize::Zeroizing<String>,
    timeout: Duration,
}

impl WebDav {
    /// Create missing, validated child collections. Existing collections are
    /// accepted only after an authenticated depth-zero PROPFIND.
    pub async fn ensure_directory(
        &self,
        directory: &str,
        cancel: &Cancellation,
    ) -> Result<(), WebDavError> {
        let mut url = self.base.clone();
        if directory.is_empty() || directory == "/" {
            return Ok(());
        }
        for segment in directory.trim_matches('/').split('/') {
            if segment.is_empty()
                || segment == "."
                || segment == ".."
                || !segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
            {
                return Err(WebDavError::InvalidInput);
            }
            url = url
                .join(&format!("{segment}/"))
                .map_err(|_| WebDavError::InvalidInput)?;
            let method = Method::from_bytes(b"MKCOL").map_err(|_| WebDavError::InvalidInput)?;
            let response = self
                .request(method, url.clone(), HeaderMap::new(), Vec::new(), cancel)
                .await?;
            if response.status() == StatusCode::CREATED {
                continue;
            }
            if response.status() != StatusCode::METHOD_NOT_ALLOWED {
                return Err(if response.status().is_success() {
                    WebDavError::Unsupported
                } else {
                    ensure_success(response.status()).unwrap_err()
                });
            }
            let mut headers = HeaderMap::new();
            headers.insert("Depth", HeaderValue::from_static("0"));
            let check = self
                .request(
                    Method::from_bytes(b"PROPFIND").unwrap(),
                    url.clone(),
                    headers,
                    Vec::new(),
                    cancel,
                )
                .await?;
            if check.status() != StatusCode::MULTI_STATUS {
                return Err(WebDavError::Unsupported);
            }
            let mut check = check;
            let mut body = Vec::new();
            if check.content_length().is_some_and(|n| n > 65536) {
                return Err(WebDavError::Unsupported);
            }
            loop {
                let mut rx = cancel.0.subscribe();
                if *rx.borrow() {
                    return Err(WebDavError::Cancelled);
                }
                let chunk = tokio::select! { biased; _=rx.changed()=>return Err(WebDavError::Cancelled), r=tokio::time::timeout(self.timeout,check.chunk())=>r.map_err(|_|WebDavError::Timeout)?.map_err(|_|WebDavError::Transport)? };
                let Some(chunk) = chunk else {
                    break;
                };
                if body.len().saturating_add(chunk.len()) > 65536 {
                    return Err(WebDavError::Unsupported);
                }
                body.extend_from_slice(&chunk);
            }
            if !is_collection_response(&body, &url) {
                return Err(WebDavError::Unsupported);
            }
        }
        Ok(())
    }
    fn published_object(&self, name: &str) -> Result<Url, WebDavError> {
        let object = if let Some((dir, object)) = name.split_once('/') {
            if dir.len() != 14 || !dir.bytes().all(|b| b.is_ascii_digit()) || object.contains('/') {
                return Err(WebDavError::InvalidInput);
            }
            object
        } else {
            name
        };
        if !object.starts_with("attempt-")
            || object.len() > 128
            || !object
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(WebDavError::InvalidInput);
        }
        self.base.join(name).map_err(|_| WebDavError::InvalidInput)
    }
    /// Sends a read-only PROPFIND to the configured collection without creating
    /// objects. A 207 response shows that the server responded with the WebDAV
    /// multistatus code; its contents and write permissions are not verified.
    pub async fn check_connection(&self, cancel: &Cancellation) -> Result<(), WebDavError> {
        let mut headers = HeaderMap::new();
        headers.insert("Depth", HeaderValue::from_static("0"));
        let method = Method::from_bytes(b"PROPFIND").map_err(|_| WebDavError::InvalidInput)?;
        let response = self
            .request(method, self.base.clone(), headers, Vec::new(), cancel)
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(WebDavError::NotFound);
        }
        if response.status() == StatusCode::MULTI_STATUS {
            Ok(())
        } else if response.status().is_success() {
            Err(WebDavError::Unsupported)
        } else {
            ensure_success(response.status())
        }
    }
    pub fn new(
        base: &str,
        username: &str,
        password: &str,
        timeout: Duration,
    ) -> Result<Self, WebDavError> {
        let mut url = Url::parse(base).map_err(|_| WebDavError::InvalidUrl)?;
        let loopback = url.host_str().is_some_and(|host| {
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        });
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.host().is_none()
        {
            return Err(WebDavError::InvalidUrl);
        }
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .timeout(timeout)
            .build()
            .map_err(|_| WebDavError::Transport)?;
        Ok(Self {
            client,
            base: url,
            username: username.into(),
            password: zeroize::Zeroizing::new(password.into()),
            timeout,
        })
    }

    fn object(&self, name: &str) -> Result<Url, WebDavError> {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(WebDavError::InvalidInput);
        }
        self.base.join(name).map_err(|_| WebDavError::InvalidInput)
    }

    async fn request(
        &self,
        method: Method,
        url: Url,
        headers: HeaderMap,
        body: Vec<u8>,
        cancel: &Cancellation,
    ) -> Result<reqwest::Response, WebDavError> {
        let mut rx = cancel.0.subscribe();
        if *rx.borrow() {
            return Err(WebDavError::Cancelled);
        }
        let request = self
            .client
            .request(method, url)
            .basic_auth(&self.username, Some(self.password.as_str()))
            .headers(headers)
            .body(body)
            .send();
        tokio::select! { biased;
            _ = rx.changed() => Err(WebDavError::Cancelled),
            result = tokio::time::timeout(self.timeout, request) => match result { Err(_) => Err(WebDavError::Timeout), Ok(Err(e)) if e.is_timeout() => Err(WebDavError::Timeout), Ok(Err(_)) => Err(WebDavError::Transport), Ok(Ok(response)) => Ok(response) }
        }
    }

    async fn read_bounded(
        &self,
        url: Url,
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<u8>, WebDavError> {
        let mut response = self
            .request(Method::GET, url, HeaderMap::new(), Vec::new(), cancel)
            .await?;
        ensure_success(response.status())?;
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(WebDavError::Integrity);
        }
        let mut bytes = Vec::new();
        loop {
            let mut rx = cancel.0.subscribe();
            if *rx.borrow() {
                return Err(WebDavError::Cancelled);
            }
            let chunk = tokio::select! { biased; _ = rx.changed() => return Err(WebDavError::Cancelled), r = tokio::time::timeout(self.timeout, response.chunk()) => r.map_err(|_| WebDavError::Timeout)?.map_err(|_| WebDavError::Transport)? };
            match chunk {
                Some(chunk) => {
                    if bytes.len().saturating_add(chunk.len()) > limit {
                        return Err(WebDavError::Integrity);
                    }
                    bytes.extend_from_slice(&chunk);
                }
                None => return Ok(bytes),
            }
        }
    }

    pub async fn probe_tracked(
        &self,
        attempt: &PublicationAttempt,
        cancel: &Cancellation,
    ) -> Result<Capabilities, PublicationFailure> {
        if attempt.target != self.base.as_str() {
            return Err(PublicationFailure {
                operation_token: attempt.operation_token.clone(),
                attempt_id: attempt.attempt_id.clone(),
                cleanup_pending: false,
                possible_objects: vec![],
                error: WebDavError::InvalidInput,
            });
        }
        self.probe_until(
            cancel,
            tokio::time::Instant::now() + self.timeout,
            &attempt.probe_id,
            &attempt.receipt_probe_name(),
        )
        .await
        .map_err(|error| PublicationFailure {
            operation_token: attempt.operation_token.clone(),
            attempt_id: attempt.attempt_id.clone(),
            cleanup_pending: true,
            possible_objects: vec![
                attempt.probe_id.clone(),
                format!("{}-create", attempt.probe_id),
                attempt.receipt_probe_name(),
            ],
            error,
        })
    }

    async fn probe_until(
        &self,
        cancel: &Cancellation,
        deadline: tokio::time::Instant,
        name: &str,
        receipt_sentinel: &str,
    ) -> Result<Capabilities, WebDavError> {
        let response = tokio::time::timeout_at(
            deadline,
            self.request(
                Method::OPTIONS,
                self.base.clone(),
                HeaderMap::new(),
                Vec::new(),
                cancel,
            ),
        )
        .await
        .map_err(|_| WebDavError::Timeout)??;
        ensure_success(response.status())?;
        let supports_move = response
            .headers()
            .get("allow")
            .and_then(|h| h.to_str().ok())
            .is_some_and(|v| v.split(',').any(|m| m.trim().eq_ignore_ascii_case("MOVE")));
        if !name.starts_with("probe-")
            || name.len() != 38
            || !name[6..].bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(WebDavError::InvalidInput);
        }
        let url = self.object(name)?;
        let create_url = self.object(&format!("{name}-create"))?;
        let result = tokio::time::timeout_at(deadline, async {
            let sentinel = self.object(receipt_sentinel)?;
            let mut initial = HeaderMap::new(); initial.insert(IF_NONE_MATCH, HeaderValue::from_static("*"));
            let (a,b)=tokio::join!(self.request(Method::PUT,sentinel.clone(),initial.clone(),b"first".to_vec(),cancel),self.request(Method::PUT,sentinel,initial,b"second".to_vec(),cancel));
            let (a,b)=(a?.status(),b?.status());
            if !((a.is_success() && b==StatusCode::PRECONDITION_FAILED)||(b.is_success() && a==StatusCode::PRECONDITION_FAILED)) { return Err(WebDavError::UnsupportedAtomicCreate); }
            let actual = self.read_bounded(self.object(receipt_sentinel)?, 6, cancel).await.map_err(|e| match e { WebDavError::Integrity | WebDavError::NotFound => WebDavError::UnsupportedReadback, other => other })?;
            let expected: &[u8] = if a.is_success() { b"first" } else { b"second" };
            if actual != expected { return Err(WebDavError::UnsupportedReadback); }
            let mut initial = HeaderMap::new(); initial.insert(IF_NONE_MATCH, HeaderValue::from_static("*"));
            let (first, second) = tokio::join!(
                self.request(Method::PUT, create_url.clone(), initial.clone(), b"first".to_vec(), cancel),
                self.request(Method::PUT, create_url.clone(), initial, b"second".to_vec(), cancel)
            );
            let (first, second) = (first?.status(), second?.status());
            if !((first.is_success() && second == StatusCode::PRECONDITION_FAILED) || (second.is_success() && first == StatusCode::PRECONDITION_FAILED)) { return Err(WebDavError::UnsupportedAtomicCreate); }
            let mut headers = HeaderMap::new();
            headers.insert(IF_NONE_MATCH, HeaderValue::from_static("*"));
            let created = self
                .request(
                    Method::PUT,
                    url.clone(),
                    headers.clone(),
                    b"probe".to_vec(),
                    cancel,
                )
                .await?;
            ensure_success(created.status())?;
            let denied = self
                .request(
                    Method::PUT,
                    url.clone(),
                    headers,
                    b"changed".to_vec(),
                    cancel,
                )
                .await?;
            if denied.status() != StatusCode::PRECONDITION_FAILED {
                return Err(WebDavError::UnsupportedAtomicCreate);
            }
            let mut headers = HeaderMap::new();
            headers.insert(
                IF_MATCH,
                HeaderValue::from_str(&format!("\"{name}\""))
                    .map_err(|_| WebDavError::InvalidInput)?,
            );
            let denied = self
                .request(
                    Method::PUT,
                    url.clone(),
                    headers,
                    b"changed".to_vec(),
                    cancel,
                )
                .await?;
            if denied.status() != StatusCode::PRECONDITION_FAILED {
                return Err(WebDavError::UnsupportedAtomicUpdate);
            }
            let response = self
                .request(
                    Method::GET,
                    url.clone(),
                    HeaderMap::new(),
                    Vec::new(),
                    cancel,
                )
                .await?;
            ensure_success(response.status())?;
            if response.content_length().is_some_and(|length| length != 5) {
                return Err(WebDavError::UnsupportedReadback);
            }
            let mut response = response;
            let mut body = Vec::with_capacity(5);
            loop {
                let mut rx = cancel.0.subscribe();
                if *rx.borrow() { return Err(WebDavError::Cancelled); }
                let chunk = tokio::select! { biased; _ = rx.changed() => return Err(WebDavError::Cancelled), r = tokio::time::timeout(self.timeout, response.chunk()) => r.map_err(|_| WebDavError::Timeout)?.map_err(|_| WebDavError::Transport)? };
                match chunk { Some(chunk) => { if body.len() + chunk.len() > 5 { return Err(WebDavError::UnsupportedReadback); } body.extend_from_slice(&chunk); }, None => break }
            }
            if body.as_slice() != b"probe" {
                return Err(WebDavError::UnsupportedReadback);
            }
            let head = self
                .request(
                    Method::HEAD,
                    url.clone(),
                    HeaderMap::new(),
                    Vec::new(),
                    cancel,
                )
                .await?;
            ensure_success(head.status())?;
            let etag = head
                .headers()
                .get(ETAG)
                .and_then(|v| v.to_str().ok())
                .ok_or(WebDavError::UnsupportedStrongEtag)?;
            if etag.starts_with("W/") || !etag.starts_with('"') || !etag.ends_with('"') {
                return Err(WebDavError::UnsupportedStrongEtag);
            }
            let mut match_headers = HeaderMap::new();
            match_headers.insert(
                IF_MATCH,
                HeaderValue::from_str(etag).map_err(|_| WebDavError::UnsupportedStrongEtag)?,
            );
            let (a, b) = tokio::join!(
                self.request(
                    Method::PUT,
                    url.clone(),
                    match_headers.clone(),
                    b"first".to_vec(),
                    cancel
                ),
                self.request(
                    Method::PUT,
                    url.clone(),
                    match_headers,
                    b"second".to_vec(),
                    cancel
                )
            );
            let (a, b) = (a?.status(), b?.status());
            if !((a.is_success() && b == StatusCode::PRECONDITION_FAILED)
                || (b.is_success() && a == StatusCode::PRECONDITION_FAILED))
            {
                return Err(WebDavError::UnsupportedAtomicUpdate);
            }
            Ok(Capabilities { supports_move })
        }).await.unwrap_or(Err(WebDavError::Timeout));
        // Attempt metadata is persisted and can be deserialized by callers;
        // never treat it as proof of remote-object ownership for deletion.
        result
    }

    /// Upload an immutable encrypted snapshot and atomically update its index.
    /// The caller must obtain a strong ETag for an existing index. Probe and
    /// publication share one deadline; cleanup and uncertain-result recovery
    /// each have a separate bounded timeout so they can run after that deadline.
    /// MOVE currently falls back to conditional immutable PUT.
    pub async fn publish_tracked(
        &self,
        id: &str,
        encrypted: &[u8],
        condition: PublishCondition,
        attempt: &PublicationAttempt,
        cancel: &Cancellation,
    ) -> Result<PublicationSuccess, PublicationFailure> {
        if attempt.operation_token.is_empty()
            || attempt.operation_token.len() > 128
            || !attempt
                .operation_token
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || attempt.target != self.base.as_str()
            || attempt.snapshot_id != id
            || attempt.digest != <[u8; 32]>::from(Sha256::digest(encrypted))
            || attempt.condition != condition
        {
            return Err(PublicationFailure {
                operation_token: attempt.operation_token.clone(),
                attempt_id: attempt.attempt_id.clone(),
                cleanup_pending: false,
                possible_objects: Vec::new(),
                error: WebDavError::InvalidInput,
            });
        }
        self.publish_attempt(id, encrypted, condition, attempt, cancel)
            .await
            .map(|mode| PublicationSuccess {
                mode,
                operation_token: attempt.operation_token.clone(),
                attempt_id: attempt.attempt_id.clone(),
                cleanup_pending: true,
                possible_objects: attempt.possible_objects(),
            })
            .map_err(|error| PublicationFailure {
                operation_token: attempt.operation_token.clone(),
                attempt_id: attempt.attempt_id.clone(),
                cleanup_pending: true,
                possible_objects: attempt.possible_objects(),
                error,
            })
    }

    async fn publish_attempt(
        &self,
        id: &str,
        encrypted: &[u8],
        condition: PublishCondition,
        attempt: &PublicationAttempt,
        cancel: &Cancellation,
    ) -> Result<PublishMode, WebDavError> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        if encrypted.len() > MAX_OBJECT {
            return Err(WebDavError::InvalidInput);
        }
        if id.eq_ignore_ascii_case("latest")
            || id.starts_with("tmp-")
            || id.starts_with("probe-")
            || id.starts_with("attempt-")
        {
            return Err(WebDavError::InvalidInput);
        }
        self.object(id)?;
        let _capabilities = self
            .probe_until(
                cancel,
                deadline,
                &attempt.probe_id,
                &attempt.receipt_probe_name(),
            )
            .await?;
        // The receipt is immutable and never treated as a cleanup candidate.
        // A lost create response is resolved only by reading the exact receipt;
        // missing/corrupt/unreadable receipts fail closed before publication.
        let receipt = OperationReceipt {
            version: 1,
            operation_token: attempt.operation_token.clone(),
            target: attempt.target.clone(),
            snapshot_id: id.into(),
            digest: attempt.digest,
            condition: condition.clone(),
        };
        let encoded_receipt =
            serde_json::to_vec(&receipt).map_err(|_| WebDavError::InvalidInput)?;
        if encoded_receipt.len() > MAX_INDEX {
            return Err(WebDavError::InvalidInput);
        }
        let receipt_url = self.object(&attempt.receipt_name())?;
        let lookup = tokio::time::timeout_at(
            deadline,
            self.request(
                Method::GET,
                receipt_url.clone(),
                HeaderMap::new(),
                Vec::new(),
                cancel,
            ),
        )
        .await
        .map_err(|_| WebDavError::OutcomeUnknown)?
        .map_err(|_| WebDavError::OutcomeUnknown)?;
        if lookup.status() == StatusCode::NOT_FOUND {
            let mut headers = HeaderMap::new();
            headers.insert(IF_NONE_MATCH, HeaderValue::from_static("*"));
            let created = tokio::time::timeout_at(
                deadline,
                self.request(
                    Method::PUT,
                    receipt_url.clone(),
                    headers,
                    encoded_receipt,
                    cancel,
                ),
            )
            .await;
            match created {
                Ok(Ok(response))
                    if response.status().is_success()
                        || response.status() == StatusCode::PRECONDITION_FAILED => {}
                _ => return Err(WebDavError::OutcomeUnknown),
            }
        } else {
            ensure_success(lookup.status()).map_err(|_| WebDavError::OutcomeUnknown)?;
        }
        let received =
            tokio::time::timeout_at(deadline, self.read_bounded(receipt_url, MAX_INDEX, cancel))
                .await
                .map_err(|_| WebDavError::OutcomeUnknown)?
                .map_err(|_| WebDavError::OutcomeUnknown)?;
        let existing_receipt: OperationReceipt =
            serde_json::from_slice(&received).map_err(|_| WebDavError::OutcomeUnknown)?;
        if existing_receipt != receipt {
            return Err(WebDavError::Conflict);
        }
        // MOVE Overwrite:F has not been verified against the destination. Use
        // the probed conditional-PUT path and explicitly report fallback.
        let index = self.object("latest")?;
        let current = tokio::time::timeout_at(
            deadline,
            self.request(
                Method::GET,
                index.clone(),
                HeaderMap::new(),
                Vec::new(),
                cancel,
            ),
        )
        .await
        .map_err(|_| WebDavError::Timeout)??;
        let mut previous = None;
        if current.status() != StatusCode::NOT_FOUND {
            ensure_success(current.status())?;
            drop(current);
            let bytes = tokio::time::timeout_at(
                deadline,
                self.read_bounded(index.clone(), MAX_INDEX, cancel),
            )
            .await
            .map_err(|_| WebDavError::Timeout)??;
            let existing: IndexRecord =
                serde_json::from_slice(&bytes).map_err(|_| WebDavError::Integrity)?;
            if !matches!(existing.version, 1 | 2)
                || self.published_object(&existing.object).is_err()
            {
                return Err(WebDavError::Integrity);
            }
            if existing.snapshot_id == id {
                if existing.operation_token != attempt.operation_token
                    || existing.condition != condition
                {
                    return Err(WebDavError::Conflict);
                }
                let body = tokio::time::timeout_at(
                    deadline,
                    self.read_bounded(self.published_object(&existing.object)?, MAX_OBJECT, cancel),
                )
                .await
                .map_err(|_| WebDavError::Timeout)??;
                if body.len() == encrypted.len()
                    && Sha256::digest(&body) == Sha256::digest(encrypted)
                {
                    return Ok(PublishMode::ImmutableFallback);
                }
                return Err(WebDavError::Conflict);
            }
            // Preserve the previous *published index*, not a directory listing:
            // only records reachable from the successful latest update count.
            let history_name = format!("history-{:x}", Sha256::digest(&bytes));
            let history_url = self.object(&history_name)?;
            let mut headers = HeaderMap::new();
            headers.insert(IF_NONE_MATCH, HeaderValue::from_static("*"));
            let response = self
                .request(
                    Method::PUT,
                    history_url.clone(),
                    headers,
                    bytes.clone(),
                    cancel,
                )
                .await?;
            if response.status() != StatusCode::PRECONDITION_FAILED {
                ensure_success(response.status())?;
            }
            if self.read_bounded(history_url, MAX_INDEX, cancel).await? != bytes {
                return Err(WebDavError::Integrity);
            }
            previous = Some(history_name);
        }
        let temp_name = attempt.attempt_id.clone();
        if !temp_name.starts_with("attempt-")
            || temp_name.len() != 40
            || !temp_name[8..].bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(WebDavError::InvalidInput);
        }
        let object_name = if let Some(directory) = &attempt.directory {
            if directory.len() != 14 || !directory.bytes().all(|b| b.is_ascii_digit()) {
                return Err(WebDavError::InvalidInput);
            }
            let dir = self
                .base
                .join(&format!("{directory}/"))
                .map_err(|_| WebDavError::InvalidInput)?;
            let response = tokio::time::timeout_at(
                deadline,
                self.request(
                    Method::from_bytes(b"MKCOL").unwrap(),
                    dir,
                    HeaderMap::new(),
                    Vec::new(),
                    cancel,
                ),
            )
            .await
            .map_err(|_| WebDavError::OutcomeUnknown)?
            .map_err(|_| WebDavError::OutcomeUnknown)?;
            if response.status() != StatusCode::CREATED {
                return Err(if response.status() == StatusCode::METHOD_NOT_ALLOWED {
                    WebDavError::OutcomeUnknown
                } else if response.status().is_success() {
                    WebDavError::Unsupported
                } else {
                    ensure_success(response.status()).unwrap_err()
                });
            }
            format!("{directory}/{temp_name}")
        } else {
            temp_name.clone()
        };
        let upload = self.published_object(&object_name)?;
        let mut create = HeaderMap::new();
        create.insert(IF_NONE_MATCH, HeaderValue::from_static("*"));
        let result = tokio::time::timeout_at(deadline, async {
            let upload_result = self.request(Method::PUT, upload.clone(), create, encrypted.to_vec(), cancel).await?;
            if upload_result.status() != StatusCode::PRECONDITION_FAILED { ensure_success(upload_result.status())?; }
            let mut response = self.request(Method::GET, upload.clone(), HeaderMap::new(), Vec::new(), cancel).await?;
            ensure_success(response.status())?;
            if response.content_length().is_some_and(|n| n > MAX_OBJECT as u64) { return Err(WebDavError::Integrity); }
            let mut hash = Sha256::new(); let mut size = 0usize;
            loop {
                let mut rx = cancel.0.subscribe(); if *rx.borrow() { return Err(WebDavError::Cancelled); }
                let chunk = tokio::select! { biased; _ = rx.changed() => return Err(WebDavError::Cancelled), r = tokio::time::timeout(self.timeout, response.chunk()) => r.map_err(|_| WebDavError::Timeout)?.map_err(|_| WebDavError::Transport)? };
                match chunk { Some(chunk) => { size = size.checked_add(chunk.len()).ok_or(WebDavError::Integrity)?; if size > MAX_OBJECT { return Err(WebDavError::Integrity); } hash.update(&chunk); }, None => break }
            }
            if size != encrypted.len() || hash.finalize().as_slice() != Sha256::digest(encrypted).as_slice() { return Err(WebDavError::Integrity); }
            let mut headers = HeaderMap::new();
            match condition { PublishCondition::Absent => { headers.insert(IF_NONE_MATCH, HeaderValue::from_static("*")); }, PublishCondition::Match(ref etag) => {
                if etag.starts_with("W/") || !etag.starts_with('"') || !etag.ends_with('"') { return Err(WebDavError::InvalidInput); }
                headers.insert(IF_MATCH, HeaderValue::from_str(etag).map_err(|_| WebDavError::InvalidInput)?);
            } }
            let body = serde_json::to_vec(&IndexRecord { version: 2, snapshot_id: id.into(), object: object_name.clone(), operation_token: attempt.operation_token.clone(), condition: condition.clone(), previous }).map_err(|_| WebDavError::InvalidInput)?;
            if body.len() > MAX_INDEX { return Err(WebDavError::InvalidInput); }
            ensure_success(self.request(Method::PUT, index, headers, body, cancel).await?.status())?;
            Ok(PublishMode::ImmutableFallback)
        }).await.unwrap_or(Err(WebDavError::Timeout));
        if matches!(
            result,
            Err(WebDavError::Conflict
                | WebDavError::Transport
                | WebDavError::Timeout
                | WebDavError::Server)
        ) {
            // A previous conditional write may have committed before its reply
            // was lost. Only report success if both immutable bytes and the
            // currently published index identify the exact same snapshot.
            let recovered = tokio::time::timeout(self.timeout, async {
                let current = self
                    .read_bounded(self.object("latest")?, MAX_INDEX, cancel)
                    .await?;
                let index: IndexRecord =
                    serde_json::from_slice(&current).map_err(|_| WebDavError::Integrity)?;
                if !matches!(index.version, 1 | 2)
                    || index.snapshot_id != id
                    || self.published_object(&index.object).is_err()
                    || index.operation_token != attempt.operation_token
                    || index.condition != condition
                {
                    return Ok(false);
                }
                let object = self
                    .read_bounded(self.published_object(&index.object)?, MAX_OBJECT, cancel)
                    .await?;
                if object.len() != encrypted.len()
                    || Sha256::digest(&object) != Sha256::digest(encrypted)
                {
                    return Ok(false);
                }
                Ok::<bool, WebDavError>(true)
            })
            .await;
            if matches!(recovered, Ok(Ok(true))) {
                return Ok(PublishMode::ImmutableFallback);
            }
        }
        if matches!(
            result,
            Err(WebDavError::Conflict
                | WebDavError::Transport
                | WebDavError::Timeout
                | WebDavError::Cancelled
                | WebDavError::Server)
        ) {
            return Err(WebDavError::OutcomeUnknown);
        }
        result
    }

    /// Read only the immutable object named by a valid published index.
    pub async fn download_latest(&self, cancel: &Cancellation) -> Result<Vec<u8>, WebDavError> {
        tokio::time::timeout(self.timeout, self.download_latest_inner(cancel))
            .await
            .map_err(|_| WebDavError::Timeout)?
    }

    async fn download_latest_inner(&self, cancel: &Cancellation) -> Result<Vec<u8>, WebDavError> {
        let bytes = self
            .read_bounded(self.object("latest")?, MAX_INDEX, cancel)
            .await?;
        let index: IndexRecord =
            serde_json::from_slice(&bytes).map_err(|_| WebDavError::Integrity)?;
        if !matches!(index.version, 1 | 2)
            || self.published_object(&index.object).is_err()
            || !valid_condition(&index.condition)
        {
            return Err(WebDavError::Integrity);
        }
        let url = self
            .published_object(&index.object)
            .map_err(|_| WebDavError::Integrity)?;
        self.read_bounded(url, MAX_OBJECT, cancel).await
    }

    /// List only versions reachable from the published latest pointer. Objects
    /// uploaded by interrupted attempts are deliberately excluded.
    async fn published_versions(
        &self,
        cancel: &Cancellation,
    ) -> Result<Vec<IndexRecord>, WebDavError> {
        let mut bytes = self
            .read_bounded(self.object("latest")?, MAX_INDEX, cancel)
            .await?;
        let mut records = Vec::new();
        let mut visited = std::collections::HashSet::new();
        loop {
            let index: IndexRecord =
                serde_json::from_slice(&bytes).map_err(|_| WebDavError::Integrity)?;
            if !matches!(index.version, 1 | 2)
                || self.published_object(&index.object).is_err()
                || !valid_condition(&index.condition)
                || index.snapshot_id.is_empty()
                || !visited.insert(index.snapshot_id.clone())
                || (index.version == 1 && index.previous.is_some())
            {
                return Err(WebDavError::Integrity);
            }
            let previous = index.previous.clone();
            records.push(index);
            let Some(name) = previous else { break };
            if name.len() != 72
                || !name.starts_with("history-")
                || !name[8..]
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            {
                return Err(WebDavError::Integrity);
            }
            bytes = self
                .read_bounded(self.object(&name)?, MAX_INDEX, cancel)
                .await?;
            if format!("history-{:x}", Sha256::digest(&bytes)) != name {
                return Err(WebDavError::Integrity);
            }
        }
        Ok(records)
    }

    pub async fn list_versions(
        &self,
        cancel: &Cancellation,
    ) -> Result<Vec<BackupVersion>, WebDavError> {
        Ok(self.list_versions_page(None, cancel).await?.versions)
    }

    pub async fn list_versions_page(
        &self,
        cursor: Option<&str>,
        cancel: &Cancellation,
    ) -> Result<BackupVersionPage, WebDavError> {
        let head = self
            .read_bounded(self.object("latest")?, MAX_INDEX, cancel)
            .await?;
        let head_digest = format!("{:x}", Sha256::digest(&head));
        let offset = match cursor {
            None => 0,
            Some(value) => {
                let (digest, raw) = value.split_once(':').ok_or(WebDavError::InvalidInput)?;
                if digest != head_digest {
                    return Err(WebDavError::Conflict);
                }
                raw.parse::<usize>()
                    .map_err(|_| WebDavError::InvalidInput)?
            }
        };
        let records = self.published_versions_from(head, cancel).await?;
        let page_size = 50usize;
        if offset > records.len() {
            return Err(WebDavError::InvalidInput);
        }
        let end = (offset + page_size).min(records.len());
        let versions = records[offset..end]
            .iter()
            .enumerate()
            .map(|(i, record)| BackupVersion {
                id: record.snapshot_id.clone(),
                timestamp: record.object.split_once('/').map(|(dir, _)| dir.to_owned()),
                latest: offset + i == 0,
            })
            .collect();
        let next_cursor = (end < records.len()).then(|| format!("{head_digest}:{end}"));
        Ok(BackupVersionPage {
            versions,
            next_cursor,
        })
    }

    async fn published_versions_from(
        &self,
        head: Vec<u8>,
        cancel: &Cancellation,
    ) -> Result<Vec<IndexRecord>, WebDavError> {
        let records = self.published_versions_bytes(head, cancel).await?;
        Ok(records)
    }

    async fn published_versions_bytes(
        &self,
        mut bytes: Vec<u8>,
        cancel: &Cancellation,
    ) -> Result<Vec<IndexRecord>, WebDavError> {
        let mut records = Vec::new();
        let mut visited = std::collections::HashSet::new();
        loop {
            let index: IndexRecord =
                serde_json::from_slice(&bytes).map_err(|_| WebDavError::Integrity)?;
            if !matches!(index.version, 1 | 2)
                || self.published_object(&index.object).is_err()
                || !valid_condition(&index.condition)
                || index.snapshot_id.is_empty()
                || !visited.insert(index.snapshot_id.clone())
                || (index.version == 1 && index.previous.is_some())
            {
                return Err(WebDavError::Integrity);
            }
            let previous = index.previous.clone();
            records.push(index);
            let Some(name) = previous else { break };
            if name.len() != 72
                || !name.starts_with("history-")
                || !name[8..]
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            {
                return Err(WebDavError::Integrity);
            }
            bytes = self
                .read_bounded(self.object(&name)?, MAX_INDEX, cancel)
                .await?;
            if format!("history-{:x}", Sha256::digest(&bytes)) != name {
                return Err(WebDavError::Integrity);
            };
        }
        Ok(records)
    }

    pub async fn download_version(
        &self,
        id: &str,
        cancel: &Cancellation,
    ) -> Result<Vec<u8>, WebDavError> {
        tokio::time::timeout(self.timeout, async {
            let records = self.published_versions(cancel).await?;
            let record = records
                .into_iter()
                .find(|r| r.snapshot_id == id)
                .ok_or(WebDavError::NotFound)?;
            self.read_bounded(self.published_object(&record.object)?, MAX_OBJECT, cancel)
                .await
        })
        .await
        .map_err(|_| WebDavError::Timeout)?
    }

    /// Return the strong ETag of the current index, or None if it does not exist.
    pub async fn index_etag(&self, cancel: &Cancellation) -> Result<Option<String>, WebDavError> {
        let response = self
            .request(
                Method::HEAD,
                self.object("latest")?,
                HeaderMap::new(),
                Vec::new(),
                cancel,
            )
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure_success(response.status())?;
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .ok_or(WebDavError::Unsupported)?;
        if !valid_condition(&PublishCondition::Match(etag.into())) {
            return Err(WebDavError::Unsupported);
        }
        Ok(Some(etag.into()))
    }
}

/// Accept a single unambiguous DAV response for the requested URL. Entity
/// references, DTDs, foreign namespaces, duplicate hrefs and malformed XML
/// fail closed; a 207 alone does not establish that the target is a collection.
fn is_collection_response(body: &[u8], target: &Url) -> bool {
    use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
    if body.len() > 65536 {
        return false;
    }
    let mut reader = NsReader::from_reader(body);
    reader.config_mut().expand_empty_elements = true;
    let mut path: Vec<String> = Vec::new();
    let mut href = String::new();
    let mut status = String::new();
    let mut response_count = 0usize;
    let mut propstat_count = 0usize;
    let mut is_collection = false;
    let mut valid_propstat = false;
    let mut href_count = 0usize;
    let mut status_count = 0usize;
    loop {
        let Ok((namespace, event)) = reader.read_resolved_event() else {
            return false;
        };
        match event {
            Event::Start(e) => {
                let dav = matches!(namespace,ResolveResult::Bound(ns) if ns.as_ref()=="DAV:");
                let local = e.local_name().as_ref().to_owned();
                let name = if dav { local } else { "#".into() };
                path.push(name);
                if path == ["multistatus", "response"] {
                    response_count += 1;
                }
                if path == ["multistatus", "response", "href"] {
                    href_count += 1;
                }
                if path == ["multistatus", "response", "status"] {
                    return false;
                }
                if path == ["multistatus", "response", "propstat"] {
                    propstat_count += 1;
                    status.clear();
                    status_count = 0;
                    is_collection = false;
                }
                if path == ["multistatus", "response", "propstat", "status"] {
                    status_count += 1;
                    if status_count > 1 {
                        return false;
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
                    is_collection = true;
                }
            }
            Event::Text(t) => {
                let s = t.as_ref();
                if s.contains('&') {
                    return false;
                }
                if path == ["multistatus", "response", "href"] {
                    href.push_str(s);
                }
                if path == ["multistatus", "response", "propstat", "status"] {
                    status.push_str(s);
                }
            }
            Event::End(_) => {
                if path == ["multistatus", "response", "propstat"]
                    && status_count == 1
                    && is_collection
                    && is_success_status_line(&status)
                {
                    valid_propstat = true;
                }
                if path.pop().is_none() {
                    return false;
                }
            }
            Event::Eof => break,
            Event::Decl(_) | Event::Comment(_) => {}
            _ => return false,
        }
    }
    if href.trim().is_empty() {
        return false;
    }
    let Ok(url) = target.join(href.trim()) else {
        return false;
    };
    response_count == 1
        && href_count == 1
        && propstat_count > 0
        && valid_propstat
        && path.is_empty()
        && url.origin() == target.origin()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path().trim_end_matches('/') == target.path().trim_end_matches('/')
        && url.query().is_none()
        && url.fragment().is_none()
}

fn is_success_status_line(status: &str) -> bool {
    let status = status.trim();
    let rest = status
        .strip_prefix("HTTP/1.1 200")
        .or_else(|| status.strip_prefix("HTTP/1.0 200"));
    let Some(rest) = rest else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    let Some(reason) = rest.strip_prefix(' ') else {
        return false;
    };
    reason
        .bytes()
        .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
}

#[cfg(test)]
mod collection_response_tests {
    use super::*;
    #[test]
    fn verifies_target_namespace_status_and_collection() {
        let url = Url::parse("https://example.org/root/folder/").unwrap();
        let document = |href: &str, ns: &str, status: &str, resource: &str| {
            format!("<d:multistatus xmlns:d=\"{ns}\"><d:response><d:href>{href}</d:href><d:propstat><d:prop><d:resourcetype>{resource}</d:resourcetype></d:prop><d:status>HTTP/1.1 {status} OK</d:status></d:propstat></d:response></d:multistatus>")
        };
        let ok = document("/root/folder/", "DAV:", "200", "<d:collection/>");
        assert!(is_collection_response(ok.as_bytes(), &url));
        let other_namespace =
            ok.replace("<d:collection/>", "<x:collection xmlns:x=\"urn:other\"/>");
        let response_status = ok.replace(
            "<d:href>",
            "<d:status>HTTP/1.1 404 Not Found</d:status><d:href>",
        );
        let encoded = document("/root/%66older/", "DAV:", "200", "<d:collection/>");
        let duplicate_status = ok.replace(
            "</d:status>",
            "</d:status><d:status>HTTP/1.1 404 Not Found</d:status>",
        );
        let duplicate_href = ok.replace("</d:href>", "</d:href><d:href>/root/folder/</d:href>");
        let duplicate_response = ok.replace(
            "</d:multistatus>",
            "<d:response><d:href>/root/folder/</d:href></d:response></d:multistatus>",
        );
        let invalid_prefix = ok.replace("HTTP/1.1 200 OK", "garbage 200");
        let valid_custom_reason = ok.replace("HTTP/1.1 200 OK", "HTTP/1.1 200 Custom success");
        assert!(is_collection_response(valid_custom_reason.as_bytes(), &url));
        let valid_numeric_reason = ok.replace("HTTP/1.1 200 OK", "HTTP/1.1 200 404");
        assert!(is_collection_response(
            valid_numeric_reason.as_bytes(),
            &url
        ));
        let invalid_status_code = ok.replace("HTTP/1.1 200 OK", "HTTP/1.1 2000 OK");
        for bad in [
            document("/root/other/", "DAV:", "200", "<d:collection/>"),
            document("/root/folder/", "urn:other", "200", "<d:collection/>"),
            document("/root/folder/", "DAV:", "404", "<d:collection/>"),
            document("/root/folder/", "DAV:", "200", ""),
            other_namespace,
            response_status,
            encoded,
            duplicate_status,
            duplicate_href,
            duplicate_response,
            invalid_prefix,
            invalid_status_code,
            document("", "DAV:", "200", "<d:collection/>"),
            " ".repeat(65537),
        ] {
            assert!(!is_collection_response(bad.as_bytes(), &url));
        }
        let alternate = ok.replace("d:", "z:").replace("xmlns:d", "xmlns:z");
        assert!(is_collection_response(alternate.as_bytes(), &url));
    }
}

fn ensure_success(status: StatusCode) -> Result<(), WebDavError> {
    match status {
        s if s.is_success() => Ok(()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(WebDavError::Authentication),
        StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => Err(WebDavError::Conflict),
        StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED => {
            Err(WebDavError::Unsupported)
        }
        s if s.is_server_error() || s == StatusCode::TOO_MANY_REQUESTS => Err(WebDavError::Server),
        _ => Err(WebDavError::Transport),
    }
}
