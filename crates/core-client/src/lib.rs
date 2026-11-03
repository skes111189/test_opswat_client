use futures_util::StreamExt;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use protocol::{Destination, Failure, JobStatus, RebindJob, RegisterJob};
use reqwest::{Client, Method, RequestBuilder, Response, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

pub type Result<T> = std::result::Result<T, Failure>;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub url: String,
    pub credential_env: String,
    pub ca_pem: Option<PathBuf>,
    #[serde(default)]
    pub allow_insecure_http: bool,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub core: Endpoint,
    pub receiver: Endpoint,
    #[serde(default = "default_state_file")]
    pub state_file: PathBuf,
    #[serde(default = "default_chunk")]
    pub chunk_bytes: u64,
    #[serde(default = "default_threshold")]
    pub multipart_threshold: u64,
}
fn default_chunk() -> u64 {
    8 * 1024 * 1024
}
fn default_state_file() -> PathBuf {
    "client-state.json".into()
}
fn default_threshold() -> u64 {
    64 * 1024 * 1024
}
impl ClientConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|_| Failure::new("載入設定", "無法讀取設定檔", None))?;
        let c: Self =
            toml::from_str(&raw).map_err(|_| Failure::new("載入設定", "設定檔格式不正確", None))?;
        if !(64 * 1024..=64 * 1024 * 1024).contains(&c.chunk_bytes) {
            return Err(Failure::new(
                "載入設定",
                "chunk_bytes 必須介於 64 KiB 與 64 MiB",
                None,
            ));
        }
        Ok(c)
    }
}

/// Credentials stay in sensitive headers. Error messages never include reqwest's URL/body.
#[derive(Clone)]
pub struct HttpApi {
    client: Client,
    base: String,
    auth: reqwest::header::HeaderMap,
}
impl HttpApi {
    pub fn new(endpoint: &Endpoint, bearer: bool) -> Result<Self> {
        let url =
            Url::parse(&endpoint.url).map_err(|_| Failure::new("設定", "URL 格式不正確", None))?;
        let loopback = matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        if !matches!(url.scheme(), "https" | "http")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || (url.scheme() == "http" && !loopback && !endpoint.allow_insecure_http)
        {
            return Err(Failure::new(
                "設定",
                "URL 需使用 HTTPS；遠端 HTTP 必須明確啟用",
                None,
            ));
        }
        let secret = std::env::var(&endpoint.credential_env)
            .map_err(|_| Failure::new("設定", "缺少憑證環境變數", None))?;
        if secret.trim().is_empty() {
            return Err(Failure::new("設定", "憑證不可空白", None));
        }
        let value = if bearer {
            format!("Bearer {secret}")
        } else {
            secret
        };
        let mut header = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| Failure::new("設定", "憑證格式不正確", None))?;
        header.set_sensitive(true);
        let mut auth = reqwest::header::HeaderMap::new();
        auth.insert(if bearer { "authorization" } else { "apikey" }, header);
        let mut builder = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(3600))
            .redirect(reqwest::redirect::Policy::none())
            .cookie_store(true);
        if let Some(path) = &endpoint.ca_pem {
            let bytes =
                std::fs::read(path).map_err(|_| Failure::new("設定", "無法讀取 CA 憑證", None))?;
            let cert = reqwest::Certificate::from_pem(&bytes)
                .map_err(|_| Failure::new("設定", "CA 憑證無效", None))?;
            builder = builder.add_root_certificate(cert);
        }
        let client = builder
            .build()
            .map_err(|_| Failure::new("設定", "無法初始化 TLS", None))?;
        Ok(Self {
            client,
            base: endpoint.url.trim_end_matches('/').into(),
            auth,
        })
    }
    pub fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base))
            .headers(self.auth.clone())
    }
    pub async fn send(request: RequestBuilder, step: &str) -> Result<Response> {
        let response = request
            .send()
            .await
            .map_err(|_| Failure::new(step, "連線失敗或逾時；請查詢原工作狀態", None))?;
        if !response.status().is_success() {
            return Err(Failure::new(
                step,
                "服務回覆失敗",
                Some(response.status().as_u16()),
            ));
        }
        Ok(response)
    }
    pub async fn json<T: DeserializeOwned>(request: RequestBuilder, step: &str) -> Result<T> {
        let response = Self::send(request, step).await?;
        Self::decode(response, step).await
    }
    async fn decode<T: DeserializeOwned>(response: Response, step: &str) -> Result<T> {
        let status = response.status().as_u16();
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Failure::new(step, "回覆讀取失敗", Some(status)))?;
            if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err(Failure::new(step, "JSON 回覆超過限制", Some(status)));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| Failure::new(step, "回覆格式不正確", Some(status)))
    }
}

#[derive(Clone)]
pub struct Core {
    pub http: HttpApi,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub name: String,
}
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ScanResult {
    #[serde(default)]
    pub process_info: ProcessInfo,
    #[serde(default)]
    pub scan_results: ScanInfo,
    #[serde(default)]
    pub file_info: FileInfo,
}
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProcessInfo {
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub progress_percentage: u8,
    #[serde(default)]
    pub post_processing: PostProcessing,
}
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PostProcessing {
    #[serde(default)]
    pub converted_destination: String,
    #[serde(default)]
    pub actions_ran: String,
    #[serde(default)]
    pub sanitization_details: CdrDetails,
}
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CdrDetails {
    #[serde(default)]
    pub sanitized_file_info: SanitizedInfo,
}
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SanitizedInfo {
    #[serde(default)]
    pub sha256: String,
}
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ScanInfo {
    #[serde(default)]
    pub progress_percentage: u8,
    pub scan_all_result_i: Option<u16>,
}
#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileInfo {
    #[serde(default)]
    pub sha256: String,
}
impl ScanResult {
    pub fn complete(&self) -> bool {
        self.process_info.progress_percentage == 100
            && self.scan_results.progress_percentage == 100
            && matches!(self.process_info.result.as_str(), "Allowed" | "Blocked")
    }
    pub fn needs_password(&self) -> bool {
        self.complete() && matches!(self.scan_results.scan_all_result_i, Some(12 | 15))
    }
    pub fn allowed(&self) -> bool {
        self.complete() && !self.needs_password() && self.process_info.result == "Allowed"
    }
    pub fn has_sanitization(&self) -> bool {
        let p = &self.process_info.post_processing;
        !p.converted_destination.is_empty()
            || p.actions_ran.split('|').any(|s| s.trim() == "Sanitized")
            || !p.sanitization_details.sanitized_file_info.sha256.is_empty()
    }
}
#[derive(Deserialize)]
struct DataId {
    data_id: String,
}
#[derive(Deserialize)]
struct CoreBatch {
    batch_id: String,
}
#[derive(Deserialize)]
struct MultipartStatus {
    status: String,
}

pub struct Upload<'a> {
    pub job_id: Option<protocol::JobId>,
    pub path: &'a Path,
    pub filename: &'a str,
    pub rule: &'a str,
    pub batch: Option<&'a str>,
    pub password: &'a str,
    pub chunk_bytes: u64,
    pub multipart_threshold: u64,
}
fn encoded(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}
impl Core {
    pub fn new(endpoint: &Endpoint) -> Result<Self> {
        Ok(Self {
            http: HttpApi::new(endpoint, false)?,
        })
    }
    pub async fn ready(&self) -> Result<()> {
        HttpApi::send(
            self.http
                .request(Method::GET, "/readyz")
                .timeout(Duration::from_secs(5)),
            "Core 健康檢查",
        )
        .await?;
        Ok(())
    }
    pub async fn rules(&self) -> Result<Vec<String>> {
        let rules: Vec<Rule> = HttpApi::json(
            self.http
                .request(Method::GET, "/file/rules")
                .timeout(Duration::from_secs(5)),
            "載入規則",
        )
        .await?;
        if rules.is_empty() || rules.iter().any(|r| r.name.trim().is_empty()) {
            return Err(Failure::new("載入規則", "規則清單無效", None));
        }
        Ok(rules.into_iter().map(|r| r.name).collect())
    }
    pub async fn create_batch(&self, rule: &str, name: &str) -> Result<String> {
        let r: CoreBatch = HttpApi::json(
            self.http
                .request(Method::POST, "/file/batch")
                .header("rule", encoded(rule))
                .header("user-data", encoded(name))
                .header("content-length", 0)
                .timeout(Duration::from_secs(30)),
            "建立 Core 批次",
        )
        .await?;
        if !protocol::valid_data_id(&r.batch_id) {
            return Err(Failure::new("建立 Core 批次", "批次 ID 無效", None));
        }
        Ok(r.batch_id)
    }
    pub async fn close_batch_when_complete(&self, id: &str) -> Result<()> {
        for _ in 0..21600 {
            let v: serde_json::Value = HttpApi::json(
                self.http
                    .request(Method::GET, &format!("/file/batch/{id}"))
                    .timeout(Duration::from_secs(30)),
                "查詢 Core 批次",
            )
            .await?;
            let b = &v["batch_files"];
            if let (Some(total), Some(done)) = (
                b["batch_count"].as_u64(),
                b["current_finished_files"].as_u64(),
            ) {
                if done >= total {
                    HttpApi::send(
                        self.http
                            .request(Method::POST, &format!("/file/batch/{id}/close"))
                            .header("content-length", 0)
                            .timeout(Duration::from_secs(30)),
                        "關閉 Core 批次",
                    )
                    .await?;
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(Failure::new(
            "關閉 Core 批次",
            "等待批次完成逾時；保留批次供管理者查詢",
            None,
        ))
    }
    pub async fn scan(&self, id: &str) -> Result<ScanResult> {
        if !protocol::valid_data_id(id) {
            return Err(Failure::new("查詢掃描", "無效 ID", None));
        }
        HttpApi::json(
            self.http
                .request(Method::GET, &format!("/file/{id}"))
                .timeout(Duration::from_secs(30)),
            "查詢掃描",
        )
        .await
    }
    fn upload_headers(&self, r: RequestBuilder, u: &Upload<'_>) -> RequestBuilder {
        let mut r = r
            .header("filename", encoded(u.filename))
            .header("rule", encoded(u.rule))
            .header("content-type", "application/octet-stream");
        if let Some(id) = u.job_id {
            r = r.header(
                "metadata",
                encoded(&serde_json::json!({"transfer_job_id": id}).to_string()),
            );
        }
        if let Some(id) = u.batch {
            r = r.header("batch", id);
        }
        if !u.password.is_empty() {
            let mut h = reqwest::header::HeaderValue::from_str(&encoded(u.password))
                .expect("encoded ASCII");
            h.set_sensitive(true);
            r = r.header("archivepwd", h);
        }
        r
    }
    pub async fn upload(&self, u: Upload<'_>) -> Result<String> {
        let size = tokio::fs::metadata(u.path)
            .await
            .map_err(|_| Failure::new("開啟檔案", "無法讀取檔案", None).safe())?
            .len();
        let id = if size >= u.multipart_threshold && size > 0 {
            let session =
                Self::upload_headers(self, self.http.request(Method::POST, "/file/multipart"), &u)
                    .header("total-length", size)
                    .header("content-length", 0)
                    .timeout(Duration::from_secs(30));
            let response = session
                .send()
                .await
                .map_err(|_| Failure::new("建立分段上傳", "連線失敗或逾時；請查詢原工作", None))?;
            let status = response.status().as_u16();
            let created = if response.status().is_success() {
                HttpApi::decode::<DataId>(response, "建立分段上傳").await
            } else if status == 400 {
                let error: serde_json::Value = HttpApi::decode(response, "建立分段上傳").await?;
                if error["err"].as_str() == Some("Multipart uploading unavailable") {
                    return self.upload_single(&u, size).await;
                }
                Err(Failure::new("建立分段上傳", "服務回覆失敗", Some(status)))
            } else {
                Err(Failure::new("建立分段上傳", "服務回覆失敗", Some(status)))
            };
            match created {
                Ok(d) => {
                    self.upload_parts(&u, size, &d.data_id).await?;
                    d.data_id
                }
                // Only unsupported endpoint is safe to fall back; never create a second uncertain submission.
                Err(e) if matches!(e.http_status, Some(404 | 405 | 501)) => {
                    self.upload_single(&u, size).await?
                }
                Err(e) => return Err(e),
            }
        } else {
            self.upload_single(&u, size).await?
        };
        if !protocol::valid_data_id(&id) {
            return Err(Failure::new("上傳", "Core 工作 ID 無效", None));
        }
        Ok(id)
    }
    async fn upload_single(&self, u: &Upload<'_>, size: u64) -> Result<String> {
        let file = tokio::fs::File::open(u.path)
            .await
            .map_err(|_| Failure::new("開啟檔案", "無法開啟檔案", None).safe())?;
        let body = reqwest::Body::wrap_stream(ReaderStream::new(file.take(size)));
        let d: DataId = HttpApi::json(
            self.upload_headers(self.http.request(Method::POST, "/file"), u)
                .header("content-length", size)
                .body(body),
            "上傳檔案",
        )
        .await?;
        if !protocol::valid_data_id(&d.data_id) {
            return Err(Failure::new("上傳檔案", "Core 工作 ID 無效", None));
        }
        Ok(d.data_id)
    }
    async fn upload_parts(&self, u: &Upload<'_>, size: u64, id: &str) -> Result<()> {
        if !protocol::valid_data_id(id) {
            return Err(Failure::new("分段上傳", "Core 工作 ID 無效", None));
        }
        if !(64 * 1024..=64 * 1024 * 1024).contains(&u.chunk_bytes) {
            return Err(Failure::new("分段上傳", "分段大小無效", None));
        }
        let path = format!("/file/multipart/{id}");
        let mut file = tokio::fs::File::open(u.path)
            .await
            .map_err(|_| Failure::new("分段上傳", "無法開啟檔案", None))?;
        let mut offset = 0;
        while offset < size {
            let len = (size - offset).min(u.chunk_bytes);
            let mut buffer = vec![0u8; len as usize];
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|_| Failure::new("分段上傳", "定位檔案失敗", None))?;
            file.read_exact(&mut buffer)
                .await
                .map_err(|_| Failure::new("分段上傳", "檔案讀取失敗", None))?;
            let mut last = None;
            for attempt in 0..3 {
                let request = self
                    .http
                    .request(Method::POST, &path)
                    .header("content-range", format!("{offset}-{len}/{size}"))
                    .header("content-length", len)
                    .header("content-type", "application/octet-stream")
                    .body(buffer.clone())
                    .timeout(Duration::from_secs(120));
                match HttpApi::send(request, "上傳分段").await {
                    Ok(_) => {
                        last = None;
                        break;
                    }
                    Err(e) => {
                        last = Some(e);
                        tokio::time::sleep(Duration::from_secs(attempt + 1)).await;
                    }
                }
            }
            if let Some(e) = last {
                return Err(e);
            }
            offset += len;
        }
        for _ in 0..60 {
            let s: MultipartStatus = HttpApi::json(
                self.http
                    .request(Method::GET, &path)
                    .timeout(Duration::from_secs(30)),
                "確認分段上傳",
            )
            .await?;
            match s.status.as_str() {
                "completed" => return Ok(()),
                "uploading" => tokio::time::sleep(Duration::from_secs(2)).await,
                _ => return Err(Failure::new("確認分段上傳", "分段工作已取消或逾時", None)),
            }
        }
        Err(Failure::new(
            "確認分段上傳",
            "等待組合逾時；請查詢原工作",
            None,
        ))
    }
}

#[derive(Clone)]
pub struct Receiver {
    pub http: HttpApi,
}
impl Receiver {
    pub fn new(endpoint: &Endpoint) -> Result<Self> {
        Ok(Self {
            http: HttpApi::new(endpoint, true)?,
        })
    }
    pub async fn destinations(&self) -> Result<Vec<Destination>> {
        let d: Vec<Destination> = HttpApi::json(
            self.http
                .request(Method::GET, "/destinations")
                .timeout(Duration::from_secs(5)),
            "receiver 健康檢查",
        )
        .await?;
        let mut seen = std::collections::HashSet::new();
        if d.is_empty()
            || d.iter()
                .any(|d| d.key.is_empty() || d.label.is_empty() || !seen.insert(&d.key))
        {
            return Err(Failure::new("receiver 健康檢查", "目的地清單無效", None));
        }
        Ok(d)
    }
    pub async fn register(&self, registration: &RegisterJob) -> Result<JobStatus> {
        HttpApi::json(
            self.http
                .request(Method::POST, "/jobs")
                .json(registration)
                .timeout(Duration::from_secs(30)),
            "登記工作",
        )
        .await
    }
    pub async fn status(&self, id: protocol::JobId) -> Result<JobStatus> {
        HttpApi::json(
            self.http
                .request(Method::GET, &format!("/jobs/{id}"))
                .timeout(Duration::from_secs(10)),
            "查詢投遞狀態",
        )
        .await
    }
    pub async fn rebind(&self, id: protocol::JobId, request: &RebindJob) -> Result<JobStatus> {
        HttpApi::json(
            self.http
                .request(Method::POST, &format!("/jobs/{id}/rebind"))
                .json(request)
                .timeout(Duration::from_secs(30)),
            "更新密碼掃描工作",
        )
        .await
    }
    pub async fn retry(&self, id: protocol::JobId) -> Result<JobStatus> {
        HttpApi::json(
            self.http
                .request(Method::POST, &format!("/jobs/{id}/retry"))
                .timeout(Duration::from_secs(30)),
            "恢復投遞工作",
        )
        .await
    }
    pub async fn original(&self, id: protocol::JobId, path: &Path) -> Result<JobStatus> {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|_| Failure::new("補送原檔", "無法開啟檔案", None))?;
        let len = file
            .metadata()
            .await
            .map_err(|_| Failure::new("補送原檔", "無法讀取檔案大小", None))?
            .len();
        HttpApi::json(
            self.http
                .request(Method::POST, &format!("/jobs/{id}/original"))
                .header("content-length", len)
                .header("content-type", "application/octet-stream")
                .body(reqwest::Body::wrap_stream(ReaderStream::new(
                    file.take(len),
                ))),
            "補送原檔",
        )
        .await
    }
}

pub async fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| Failure::new("計算 SHA-256", "無法開啟檔案", None).safe())?;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 65536];
    let mut size = 0u64;
    loop {
        let n = file
            .read(&mut bytes)
            .await
            .map_err(|_| Failure::new("計算 SHA-256", "讀取檔案失敗", None).safe())?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
        size += n as u64;
    }
    Ok((hex::encode(hash.finalize()), size))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn result_is_fail_closed() {
        let mut r = ScanResult::default();
        assert!(!r.allowed());
        r.process_info.result = "Allowed".into();
        r.scan_results.progress_percentage = 100;
        assert!(!r.allowed());
        r.process_info.progress_percentage = 100;
        assert!(r.allowed());
        r.scan_results.scan_all_result_i = Some(12);
        assert!(r.needs_password());
        assert!(!r.allowed());
    }
}
