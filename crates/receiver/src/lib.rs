pub mod store;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path as AxumPath, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use core_client::{sha256_file, Core, Endpoint};
use futures_util::{Stream, StreamExt};
use protocol::{
    ContentKind, Destination, Failure, JobId, JobState, JobStatus, RebindJob, RegisterJob,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use store::{Prepared, Record, Store};
use subtle::ConstantTimeEq;
use tokio::io::AsyncWriteExt;

type Result<T> = std::result::Result<T, Failure>;
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationConfig {
    pub label: String,
    pub path: PathBuf,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    pub certificate_pem: PathBuf,
    pub private_key_pem: PathBuf,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: std::net::SocketAddr,
    pub database: PathBuf,
    pub token_env: String,
    pub webhook_token_env: String,
    pub core: Endpoint,
    pub tls: Option<TlsConfig>,
    pub max_file_bytes: u64,
    pub min_free_bytes: u64,
    pub scan_timeout_seconds: u64,
    pub destinations: BTreeMap<String, DestinationConfig>,
}
pub struct Service {
    pub config: Config,
    pub core: Core,
    pub store: Store,
    /// Per-job serialization; unrelated uploads must not wait for a large Core download.
    locks: Mutex<HashMap<JobId, Arc<tokio::sync::Mutex<()>>>>,
    tick_gate: tokio::sync::Mutex<()>,
    token_digest: [u8; 32],
    webhook_digest: [u8; 32],
}
impl Service {
    pub fn new(mut config: Config) -> Result<Arc<Self>> {
        if config.destinations.is_empty()
            || config.max_file_bytes == 0
            || config.scan_timeout_seconds == 0
        {
            return Err(Failure::new("設定", "目的地與檔案限制不可為空", None));
        }
        if !config.listen.ip().is_loopback() && config.tls.is_none() {
            return Err(Failure::new("設定", "非 loopback 監聽必須設定 TLS", None));
        }
        for (key, dest) in &mut config.destinations {
            if key.is_empty() || dest.label.is_empty() {
                return Err(Failure::new("設定", "目的地格式錯誤", None));
            }
            dest.path = dest
                .path
                .canonicalize()
                .map_err(|_| Failure::new("設定", "目的地必須為已掛載的現有目錄", None))?;
            if !dest.path.is_dir() {
                return Err(Failure::new("設定", "目的地不是目錄", None));
            }
        }
        fn digest_env(name: &str) -> Result<[u8; 32]> {
            let token = std::env::var(name)
                .map_err(|_| Failure::new("設定", "缺少驗證憑證環境變數", None))?;
            if token.len() < 32 {
                return Err(Failure::new(
                    "設定",
                    "receiver 與 webhook 憑證至少需 32 bytes",
                    None,
                ));
            }
            Ok(Sha256::digest(token.as_bytes()).into())
        }
        let token_digest = digest_env(&config.token_env)?;
        let webhook_digest = digest_env(&config.webhook_token_env)?;
        let core = Core::new(&config.core)?;
        let store = Store::open(&config.database)?;
        Ok(Arc::new(Self {
            config,
            core,
            store,
            locks: Mutex::new(HashMap::new()),
            tick_gate: tokio::sync::Mutex::new(()),
            token_digest,
            webhook_digest,
        }))
    }
    fn job_lock(&self, id: JobId) -> Result<Arc<tokio::sync::Mutex<()>>> {
        let mut locks = self
            .locks
            .lock()
            .map_err(|_| Failure::new("工作鎖定", "工作鎖定失敗", None))?;
        Ok(locks
            .entry(id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone())
    }
    fn record(&self, id: JobId) -> Result<Record> {
        self.store
            .get(id)?
            .ok_or_else(|| Failure::new("查詢工作", "工作不存在", Some(404)))
    }
    fn destination<'a>(&'a self, r: &Record) -> Result<&'a Path> {
        self.config
            .destinations
            .get(&r.status.registration.destination)
            .map(|d| d.path.as_path())
            .ok_or_else(|| Failure::new("目的地", "目的地已移除，需由管理者復原設定", None))
    }
    fn stage_path(&self, r: &Record) -> Result<PathBuf> {
        Ok(self
            .destination(r)?
            .join(format!(".mdtransfer-{}.part", r.status.registration.id)))
    }
    fn final_name(&self, r: &Record) -> String {
        let p = Path::new(&r.status.registration.filename);
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
        let suffix = p
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| format!(".{s}"))
            .unwrap_or_default();
        format!("{stem}--{}{suffix}", r.status.registration.id)
    }
    fn fail(&self, mut r: Record, error: Failure) -> Result<()> {
        r.status.state = JobState::Failed;
        r.status.failure = Some(error);
        r.status.updated_at = Utc::now();
        self.store.save(&r)
    }
    pub async fn tick(&self) -> Result<()> {
        let _guard = self.tick_gate.lock().await;
        let records = self.store.all()?;
        let work = futures_util::stream::iter(
            records
                .into_iter()
                .filter(|r| matches!(r.status.state, JobState::Scanning | JobState::Delivering)),
        )
        .map(|r| async move {
            let _guard = self.job_lock(r.status.registration.id)?.lock_owned().await;
            let current = self.record(r.status.registration.id)?;
            if matches!(
                current.status.state,
                JobState::Scanning | JobState::Delivering
            ) {
                if let Err(e) = self.process(current).await {
                    // process may already have durably written a publication intent.
                    self.fail(self.record(r.status.registration.id)?, e)?;
                }
            }
            Ok::<(), Failure>(())
        })
        .buffer_unordered(4);
        futures_util::pin_mut!(work);
        while let Some(result) = work.next().await {
            result?;
        }
        Ok(())
    }
    pub async fn run(self: Arc<Self>) {
        let mut timer = tokio::time::interval(Duration::from_secs(2));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(e) = self.tick().await {
                eprintln!("receiver: {e}");
            }
        }
    }
    async fn process(&self, mut r: Record) -> Result<()> {
        if r.prepared.is_some() {
            return self.publish(&mut r).await;
        }
        if Utc::now()
            .signed_duration_since(r.created_at)
            .num_seconds()
            .max(0) as u64
            > self.config.scan_timeout_seconds
        {
            return Err(Failure::new("等待掃描", "超過掃描等待期限；可恢復原工作", None).safe());
        }
        let scan = self
            .core
            .scan(&r.status.registration.data_id)
            .await
            .map_err(|e| e.safe())?;
        if !scan.complete() {
            return Ok(());
        }
        if !protocol::valid_sha256(&scan.file_info.sha256)
            || !scan
                .file_info
                .sha256
                .eq_ignore_ascii_case(&r.status.registration.sha256)
        {
            r.status.state = JobState::Rejected;
            r.status.failure = Some(Failure::new(
                "核對來源",
                "Core 原始檔 SHA-256 與登記不符或缺少雜湊",
                None,
            ));
        } else if scan.needs_password() {
            r.status.state = JobState::NeedsPassword;
        } else if !scan.allowed() {
            r.status.state = JobState::Rejected;
            r.status.failure = Some(Failure::new("Core 判定", "Core 已拒絕此檔案", None));
        } else {
            let response = self
                .core
                .http
                .request(
                    reqwest::Method::GET,
                    &format!("/file/converted/{}", r.status.registration.data_id),
                )
                .send()
                .await
                .map_err(|_| Failure::new("取得清洗檔", "Core 連線失敗或逾時", None).safe())?;
            if response.status() == reqwest::StatusCode::NOT_FOUND && !scan.has_sanitization() {
                r.status.state = JobState::AwaitingOriginal;
            } else if !response.status().is_success() {
                return Err(Failure::new(
                    "取得清洗檔",
                    "無法取得清洗輸出；保留原工作",
                    Some(response.status().as_u16()),
                )
                .safe());
            } else {
                if response
                    .content_length()
                    .is_some_and(|n| n > self.config.max_file_bytes)
                {
                    return Err(Failure::new("取得清洗檔", "檔案超過設定限制", None).safe());
                }
                let expected = &scan
                    .process_info
                    .post_processing
                    .sanitization_details
                    .sanitized_file_info
                    .sha256;
                let expected = if expected.is_empty() {
                    None
                } else {
                    Some(expected.as_str())
                };
                let prepared = self
                    .stage(
                        &r,
                        response.bytes_stream(),
                        ContentKind::Sanitized,
                        expected,
                    )
                    .await?;
                return self.commit_and_publish(&mut r, prepared).await;
            }
        }
        r.status.updated_at = Utc::now();
        self.store.save(&r)
    }
    async fn stage<S, E>(
        &self,
        r: &Record,
        stream: S,
        kind: ContentKind,
        expected: Option<&str>,
    ) -> Result<Prepared>
    where
        S: Stream<Item = std::result::Result<axum::body::Bytes, E>> + Send,
        E: Send,
    {
        let path = self.stage_path(r)?;
        let dir = self.destination(r)?;
        let free = fs2::available_space(dir)
            .map_err(|_| Failure::new("磁碟檢查", "無法取得目的地可用空間", None).safe())?;
        if free <= self.config.min_free_bytes {
            return Err(Failure::new("磁碟檢查", "目的地剩餘空間不足", None).safe());
        }
        // Only the process holding the DB lock can own this UUID stage path.
        match tokio::fs::remove_file(&path).await {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(Failure::new("建立暫存檔", "無法清理舊暫存檔", None).safe()),
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
            .map_err(|_| Failure::new("建立暫存檔", "目的地無法寫入", None).safe())?;
        let result = async {
            let mut bytes = 0u64;
            let mut hash = Sha256::new();
            futures_util::pin_mut!(stream);
            while let Some(chunk) = stream.next().await {
                let chunk = chunk
                    .map_err(|_| Failure::new("接收檔案", "檔案串流中斷或逾時", None).safe())?;
                bytes = bytes
                    .checked_add(chunk.len() as u64)
                    .ok_or_else(|| Failure::new("接收檔案", "大小溢位", None))?;
                let free_now = fs2::available_space(dir)
                    .map_err(|_| Failure::new("磁碟檢查", "磁碟無法存取", None).safe())?;
                if bytes > self.config.max_file_bytes
                    || free_now
                        < self
                            .config
                            .min_free_bytes
                            .saturating_add(chunk.len() as u64)
                {
                    return Err(Failure::new("接收檔案", "超過檔案或剩餘磁碟空間限制", None).safe());
                }
                file.write_all(&chunk)
                    .await
                    .map_err(|_| Failure::new("寫入暫存檔", "目的地寫入失敗", None).safe())?;
                hash.update(&chunk);
            }
            file.sync_all()
                .await
                .map_err(|_| Failure::new("同步暫存檔", "目的地同步失敗", None).safe())?;
            let sha256 = hex::encode(hash.finalize());
            if expected.is_some_and(|s| !s.eq_ignore_ascii_case(&sha256)) {
                return Err(Failure::new("核對檔案", "SHA-256 不符；未發布檔案", None).safe());
            }
            Ok(Prepared {
                sha256,
                bytes,
                kind,
                name: self.final_name(r),
            })
        }
        .await;
        drop(file);
        if result.is_err() {
            let _ = tokio::fs::remove_file(path).await;
        }
        result
    }
    async fn commit_and_publish(&self, r: &mut Record, prepared: Prepared) -> Result<()> {
        r.prepared = Some(prepared);
        r.status.state = JobState::Delivering;
        r.status.updated_at = Utc::now();
        // The journal MUST be durable before any publication can happen.
        self.store.save(r)?;
        self.publish(r).await
    }
    async fn publish(&self, r: &mut Record) -> Result<()> {
        let p = r
            .prepared
            .as_ref()
            .ok_or_else(|| Failure::new("復原投遞", "缺少投遞意圖", None))?;
        if !protocol::valid_filename(&p.name) || !protocol::valid_sha256(&p.sha256) {
            return Err(Failure::new("復原投遞", "投遞意圖無效", None));
        }
        let dest = self.destination(r)?.join(&p.name);
        let stage = self.stage_path(r)?;
        match tokio::fs::symlink_metadata(&dest).await {
            Ok(meta) => {
                if !meta.is_file() || meta.file_type().is_symlink() {
                    return Err(Failure::new("復原投遞", "正式路徑已被其他內容占用", None));
                }
                let (hash, size) = sha256_file(&dest).await?;
                if hash != p.sha256 || size != p.bytes {
                    return Err(Failure::new(
                        "復原投遞",
                        "正式檔案與投遞意圖不符；需要人工查核",
                        None,
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let (hash, size) = sha256_file(&stage).await?;
                if hash != p.sha256 || size != p.bytes {
                    return Err(Failure::new("復原投遞", "暫存檔與投遞意圖不符", None));
                }
                // Atomic, no overwrite, same destination filesystem. SMB must support hard links.
                tokio::fs::hard_link(&stage, &dest).await.map_err(|_| {
                    Failure::new("發布檔案", "原子發布失敗；恢復原工作以核對結果", None)
                })?;
            }
            Err(_) => return Err(Failure::new("復原投遞", "無法確認正式檔案是否存在", None)),
        }
        // Persist publication only after syncing directory metadata where the OS supports it.
        #[cfg(unix)]
        {
            let dir = std::fs::File::open(self.destination(r)?)
                .map_err(|_| Failure::new("同步投遞", "無法開啟目的地目錄", None))?;
            dir.sync_all()
                .map_err(|_| Failure::new("同步投遞", "目的地目錄同步失敗", None))?;
        }
        r.status.state = JobState::Delivered;
        r.status.content = Some(p.kind);
        r.status.delivered_name = Some(p.name.clone());
        r.status.failure = None;
        r.status.updated_at = Utc::now();
        self.store.save(r)?;
        let _ = tokio::fs::remove_file(stage).await;
        Ok(())
    }
    pub async fn receive_original(&self, id: JobId, body: Body) -> Result<JobStatus> {
        let _guard = self.job_lock(id)?.lock_owned().await;
        let mut r = self.record(id)?;
        if r.status.state == JobState::Delivered {
            return Ok(r.status);
        }
        if r.status.state != JobState::AwaitingOriginal {
            return Err(Failure::new("補送原檔", "工作未要求原始檔", Some(409)));
        }
        let scan = self.core.scan(&r.status.registration.data_id).await?;
        if !scan.allowed()
            || scan.has_sanitization()
            || !scan
                .file_info
                .sha256
                .eq_ignore_ascii_case(&r.status.registration.sha256)
        {
            return Err(Failure::new(
                "補送原檔",
                "Core 未允許原始檔或雜湊不符",
                Some(403),
            ));
        }
        // Do not accept degraded original delivery when a converted artifact appears after polling.
        let head = self
            .core
            .http
            .request(
                reqwest::Method::HEAD,
                &format!("/file/converted/{}", r.status.registration.data_id),
            )
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| Failure::new("補送原檔", "無法確認清洗輸出狀態", None))?;
        if head.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(Failure::new(
                "補送原檔",
                "清洗輸出狀態已改變；需恢復查詢",
                Some(head.status().as_u16()),
            ));
        }
        let stream = body.into_data_stream();
        let result = tokio::time::timeout(
            Duration::from_secs(3600),
            self.stage(
                &r,
                stream,
                ContentKind::Original,
                Some(&r.status.registration.sha256),
            ),
        )
        .await
        .map_err(|_| Failure::new("補送原檔", "接收檔案逾時", None).safe())?;
        match result {
            Ok(p) => {
                if let Err(e) = self.commit_and_publish(&mut r, p).await {
                    // Reload the journal; never discard prepared state after an uncertain publication.
                    self.fail(self.record(id)?, e.clone())?;
                    return Err(e);
                }
            }
            Err(e) => {
                self.fail(r, e.clone())?;
                return Err(e);
            }
        }
        Ok(r.status)
    }
}

#[derive(Debug)]
struct ApiError(Failure);
impl From<Failure> for ApiError {
    fn from(e: Failure) -> Self {
        Self(e)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.0.http_status.unwrap_or(500))
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(self.0),
        )
            .into_response()
    }
}
async fn authorize(State(service): State<Arc<Service>>, request: Request, next: Next) -> Response {
    let is_webhook = request.uri().path() == "/webhook";
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    let expected = if is_webhook {
        &service.webhook_digest
    } else {
        &service.token_digest
    };
    if digest.ct_eq(expected).unwrap_u8() != 1 {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}
pub fn router(service: Arc<Service>) -> Router {
    let limit = usize::try_from(service.config.max_file_bytes).unwrap_or(usize::MAX);
    Router::new()
        .route("/destinations", get(destinations))
        .route("/jobs", post(register).layer(DefaultBodyLimit::max(16384)))
        .route("/jobs/{id}", get(status))
        .route(
            "/jobs/{id}/rebind",
            post(rebind).layer(DefaultBodyLimit::max(16384)),
        )
        .route("/jobs/{id}/retry", post(retry))
        .route(
            "/jobs/{id}/original",
            post(original).layer(DefaultBodyLimit::max(limit)),
        )
        .route("/webhook", post(webhook).layer(DefaultBodyLimit::max(1024)))
        .layer(middleware::from_fn_with_state(service.clone(), authorize))
        .with_state(service)
}
async fn destinations(State(s): State<Arc<Service>>) -> Json<Vec<Destination>> {
    Json(
        s.config
            .destinations
            .iter()
            .map(|(key, d)| Destination {
                key: key.clone(),
                label: d.label.clone(),
            })
            .collect(),
    )
}
async fn register(
    State(s): State<Arc<Service>>,
    Json(r): Json<RegisterJob>,
) -> std::result::Result<Json<JobStatus>, ApiError> {
    if !r.validate() || !s.config.destinations.contains_key(&r.destination) {
        return Err(Failure::new("登記工作", "工作內容或目的地無效", Some(400)).into());
    }
    let _guard = s.job_lock(r.id)?.lock_owned().await;
    Ok(Json(s.store.register(r)?))
}
async fn status(
    State(s): State<Arc<Service>>,
    AxumPath(id): AxumPath<JobId>,
) -> std::result::Result<Json<JobStatus>, ApiError> {
    Ok(Json(s.record(id)?.status))
}
async fn rebind(
    State(s): State<Arc<Service>>,
    AxumPath(id): AxumPath<JobId>,
    Json(r): Json<RebindJob>,
) -> std::result::Result<Json<JobStatus>, ApiError> {
    let _guard = s.job_lock(id)?.lock_owned().await;
    Ok(Json(s.store.rebind(id, r)?))
}
async fn retry(
    State(s): State<Arc<Service>>,
    AxumPath(id): AxumPath<JobId>,
) -> std::result::Result<Json<JobStatus>, ApiError> {
    let _guard = s.job_lock(id)?.lock_owned().await;
    let mut r = s.record(id)?;
    if r.status.state == JobState::Failed {
        r.status.state = if r.prepared.is_some() {
            JobState::Delivering
        } else {
            JobState::Scanning
        };
        r.created_at = Utc::now();
        r.status.failure = None;
        r.status.updated_at = Utc::now();
        s.store.save(&r)?;
    } else if r.status.state == JobState::Rejected {
        return Err(Failure::new("恢復工作", "已拒絕工作不可重送", Some(409)).into());
    }
    Ok(Json(r.status))
}
async fn original(
    State(s): State<Arc<Service>>,
    AxumPath(id): AxumPath<JobId>,
    body: Body,
) -> std::result::Result<Json<JobStatus>, ApiError> {
    Ok(Json(s.receive_original(id, body).await?))
}
#[derive(Deserialize)]
struct Notification {
    id: JobId,
}
async fn webhook(
    State(s): State<Arc<Service>>,
    Json(n): Json<Notification>,
) -> std::result::Result<StatusCode, ApiError> {
    s.record(n.id)?;
    // Notification only. The periodic worker obtains all authoritative bytes and verdicts from Core.
    Ok(StatusCode::ACCEPTED)
}
