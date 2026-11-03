use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use core_client::{Core, Endpoint, HttpApi, Receiver, Upload};
use protocol::{ContentKind, JobState, RebindJob, RegisterJob};
use receiver::{store::Prepared, Config, DestinationConfig, Service};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, Once,
    },
    time::Duration,
};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

fn hash(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
const ORIGINAL: &[u8] = b"original";
const CLEAN: &[u8] = b"cleaned";
const TOKEN: &str = "test-only-receiver-token-32-bytes-long";
#[derive(Default)]
struct Mock {
    converted_calls: AtomicUsize,
    requests: Mutex<Vec<(String, HeaderMap, Vec<u8>)>>,
}
async fn scan(Path(id): Path<String>) -> impl IntoResponse {
    if id == "timeout" {
        tokio::time::sleep(Duration::from_secs(31)).await;
    }
    let blocked = id == "blocked" || id == "password";
    let expected_cdr = matches!(id.as_str(), "retained" | "corrupt");
    Json(json!({
        "process_info": {"result": if blocked {"Blocked"} else {"Allowed"}, "progress_percentage": 100,
            "post_processing": {"actions_ran": if expected_cdr {"Sanitized"} else {""},
                "sanitization_details": {"sanitized_file_info": {"sha256": if id == "corrupt" {hash(b"other")} else {String::new()}}}}},
        "scan_results": {"progress_percentage": 100, "scan_all_result_i": if id == "password" {12} else {0}},
        "file_info": {"sha256": if id == "mismatch" {hash(b"changed")} else if id == "missinghash" {String::new()} else {hash(ORIGINAL)}}
    }))
}
async fn converted(State(m): State<Arc<Mock>>, Path(id): Path<String>) -> axum::response::Response {
    m.converted_calls.fetch_add(1, Ordering::SeqCst);
    match id.as_str() {
        "original" | "retained" | "mismatch" | "missinghash" => {
            StatusCode::NOT_FOUND.into_response()
        }
        "error" => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        _ => ([("content-type", "application/octet-stream")], CLEAN).into_response(),
    }
}
async fn upload(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Json<Value> {
    m.requests
        .lock()
        .unwrap()
        .push(("file".into(), headers, body.to_vec()));
    Json(json!({"data_id":"uploaded"}))
}
async fn session(State(m): State<Arc<Mock>>, headers: HeaderMap) -> Json<Value> {
    m.requests
        .lock()
        .unwrap()
        .push(("session".into(), headers, vec![]));
    Json(json!({"data_id":"multipart"}))
}
async fn part(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Json<Value> {
    m.requests
        .lock()
        .unwrap()
        .push(("part".into(), headers, body.to_vec()));
    Json(json!({"ranges_received":"ok"}))
}
async fn bind(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, task)
}
struct Fixture {
    config: Config,
    service: Arc<Service>,
    mock: Arc<Mock>,
    server: Option<tokio::task::JoinHandle<()>>,
    dir: TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(server) = &self.server {
            server.abort();
        }
    }
}
fn endpoint(url: String, env: &str) -> Endpoint {
    Endpoint {
        url,
        credential_env: env.into(),
        ca_pem: None,
        allow_insecure_http: false,
    }
}
impl Fixture {
    async fn new() -> Self {
        let mock = Arc::new(Mock::default());
        let app = Router::new()
            .route("/file/{id}", get(scan))
            .route("/file/converted/{id}", get(converted))
            .route("/file", post(upload))
            .route("/file/multipart", post(session))
            .route(
                "/file/multipart/{id}",
                post(part).get(|| async { Json(json!({"status":"completed"})) }),
            )
            .with_state(mock.clone());
        let (url, server) = bind(app).await;
        Self::configured(url, Some(server), mock)
    }
    fn local() -> Self {
        // These tests exercise the real store, disk publication and router without Core I/O.
        Self::configured(
            "https://core.example.invalid".into(),
            None,
            Arc::new(Mock::default()),
        )
    }
    fn configured(
        url: String,
        server: Option<tokio::task::JoinHandle<()>>,
        mock: Arc<Mock>,
    ) -> Self {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            std::env::set_var("OPSWAT_TEST_CORE", "test-only-core-key");
            std::env::set_var("OPSWAT_TEST_RECEIVER", TOKEN);
            std::env::set_var(
                "OPSWAT_TEST_WEBHOOK",
                "test-only-webhook-token-32-bytes-long",
            );
        });
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let config = Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            database: dir.path().join("jobs.sqlite"),
            core: endpoint(url, "OPSWAT_TEST_CORE"),
            token_env: "OPSWAT_TEST_RECEIVER".into(),
            webhook_token_env: "OPSWAT_TEST_WEBHOOK".into(),
            tls: None,
            max_file_bytes: 1048576,
            min_free_bytes: 0,
            scan_timeout_seconds: 60,
            destinations: BTreeMap::from([(
                "target".into(),
                DestinationConfig {
                    label: "正式目錄".into(),
                    path: target,
                },
            )]),
        };
        let service = Service::new(config.clone()).unwrap();
        Self {
            dir,
            config,
            service,
            mock,
            server,
        }
    }
    fn register(&self, data: &str) -> RegisterJob {
        let r = RegisterJob {
            id: Uuid::new_v4(),
            batch_id: Uuid::new_v4(),
            data_id: data.into(),
            destination: "target".into(),
            filename: "中文報告.txt".into(),
            sha256: hash(ORIGINAL),
        };
        self.service.store.register(r.clone()).unwrap();
        r
    }
    fn state(&self, r: &RegisterJob) -> protocol::JobStatus {
        self.service.store.get(r.id).unwrap().unwrap().status
    }
    fn target(&self) -> PathBuf {
        self.config.destinations["target"].path.clone()
    }
}

#[tokio::test]
async fn converted_bytes_come_from_core_and_repeated_ticks_are_idempotent() {
    let f = Fixture::new().await;
    let r = f.register("clean");
    f.service.tick().await.unwrap();
    let s = f.state(&r);
    assert_eq!(s.state, JobState::Delivered);
    assert_eq!(s.content, Some(ContentKind::Sanitized));
    assert_eq!(
        std::fs::read(f.target().join(s.delivered_name.unwrap())).unwrap(),
        CLEAN
    );
    for _ in 0..3 {
        f.service.tick().await.unwrap();
    }
    assert_eq!(f.mock.converted_calls.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 1);
}
#[tokio::test]
async fn original_requires_authoritative_allowed_hash() {
    let f = Fixture::new().await;
    let r = f.register("original");
    f.service.tick().await.unwrap();
    assert_eq!(f.state(&r).state, JobState::AwaitingOriginal);
    let s = f
        .service
        .receive_original(r.id, Body::from(ORIGINAL))
        .await
        .unwrap();
    assert_eq!(s.state, JobState::Delivered);
    assert_eq!(s.content, Some(ContentKind::Original));
    assert_eq!(
        std::fs::read(f.target().join(s.delivered_name.unwrap())).unwrap(),
        ORIGINAL
    );
    // A repeated body cannot overwrite the already verified final file.
    f.service
        .receive_original(r.id, Body::from("malicious webhook/body"))
        .await
        .unwrap();
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 1);
}
#[tokio::test]
async fn wrong_original_hash_is_never_published() {
    let f = Fixture::new().await;
    let r = f.register("original");
    f.service.tick().await.unwrap();
    assert!(f
        .service
        .receive_original(r.id, Body::from("wrong"))
        .await
        .is_err());
    assert_eq!(f.state(&r).state, JobState::Failed);
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 0);
}
#[tokio::test]
async fn blocked_missing_hash_and_changed_source_are_rejected() {
    let f = Fixture::new().await;
    let jobs = [
        f.register("blocked"),
        f.register("missinghash"),
        f.register("mismatch"),
    ];
    f.service.tick().await.unwrap();
    for r in jobs {
        assert_eq!(f.state(&r).state, JobState::Rejected);
    }
    assert_eq!(f.mock.converted_calls.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 0);
}
#[tokio::test]
async fn converted_retention_error_cannot_fall_back_to_original() {
    let f = Fixture::new().await;
    let retained = f.register("retained");
    let corrupt = f.register("corrupt");
    let error = f.register("error");
    f.service.tick().await.unwrap();
    for r in [retained, corrupt, error] {
        assert_eq!(f.state(&r).state, JobState::Failed);
        assert!(f
            .service
            .receive_original(r.id, Body::from(ORIGINAL))
            .await
            .is_err());
    }
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 0);
}
#[tokio::test]
async fn password_rebind_is_immutable_and_old_core_binding_is_retained() {
    let f = Fixture::new().await;
    let r = f.register("password");
    f.service.tick().await.unwrap();
    assert_eq!(f.state(&r).state, JobState::NeedsPassword);
    f.service
        .store
        .rebind(
            r.id,
            RebindJob {
                previous_data_id: "password".into(),
                data_id: "clean".into(),
            },
        )
        .unwrap();
    let mut stolen = r.clone();
    stolen.id = Uuid::new_v4();
    assert!(f.service.store.register(stolen).is_err());
    f.service.tick().await.unwrap();
    assert_eq!(f.state(&r).state, JobState::Delivered);
    assert!(f
        .service
        .store
        .rebind(
            r.id,
            RebindJob {
                previous_data_id: "clean".into(),
                data_id: "new".into()
            }
        )
        .is_err());
}
#[tokio::test]
async fn local_registration_is_idempotent_and_binding_cannot_change() {
    let f = Fixture::local();
    let r = f.register("clean");
    assert_eq!(f.service.store.register(r.clone()).unwrap().registration, r);
    let mut changed = r.clone();
    changed.destination = "other".into();
    assert!(f.service.store.register(changed).is_err());
    let mut duplicate = r;
    duplicate.id = Uuid::new_v4();
    assert!(f.service.store.register(duplicate).is_err());
}
#[tokio::test]
async fn same_filename_uses_unique_suffix_and_preserves_versions() {
    let f = Fixture::new().await;
    let a = f.register("clean1");
    let b = f.register("clean2");
    f.service.tick().await.unwrap();
    assert_eq!(f.state(&a).state, JobState::Delivered);
    assert_eq!(f.state(&b).state, JobState::Delivered);
    assert_ne!(f.state(&a).delivered_name, f.state(&b).delivered_name);
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 2);
}
#[tokio::test]
async fn restart_resumes_scanning_and_reconciles_publication_journal() {
    let mut f = Fixture::new().await;
    let r = f.register("clean");
    // Simulate process termination after publication and before marking Delivered.
    let name = format!("中文報告--{}.txt", r.id);
    std::fs::write(f.target().join(&name), CLEAN).unwrap();
    let mut record = f.service.store.get(r.id).unwrap().unwrap();
    record.prepared = Some(Prepared {
        sha256: hash(CLEAN),
        bytes: CLEAN.len() as u64,
        kind: ContentKind::Sanitized,
        name: name.clone(),
    });
    record.status.state = JobState::Delivering;
    f.service.store.save(&record).unwrap();
    let second = f.register("clean2");
    let old = std::mem::replace(
        &mut f.service,
        Service::new(Config {
            database: f.dir.path().join("placeholder.sqlite"),
            ..f.config.clone()
        })
        .unwrap(),
    );
    drop(old);
    f.service = Service::new(f.config.clone()).unwrap();
    f.service.tick().await.unwrap();
    assert_eq!(f.state(&r).state, JobState::Delivered);
    assert_eq!(f.state(&second).state, JobState::Delivered);
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 2);
    assert_eq!(f.mock.converted_calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn local_journal_survives_uncertain_publication_and_never_overwrites_conflicts() {
    let f = Fixture::local();
    let r = f.register("clean");
    let name = format!("中文報告--{}.txt", r.id);
    std::fs::write(f.target().join(&name), b"another version").unwrap();
    let mut record = f.service.store.get(r.id).unwrap().unwrap();
    record.prepared = Some(Prepared {
        sha256: hash(CLEAN),
        bytes: CLEAN.len() as u64,
        kind: ContentKind::Sanitized,
        name: name.clone(),
    });
    record.status.state = JobState::Delivering;
    f.service.store.save(&record).unwrap();
    f.service.tick().await.unwrap();
    let record = f.service.store.get(r.id).unwrap().unwrap();
    assert_eq!(record.status.state, JobState::Failed);
    assert!(record.prepared.is_some());
    assert!(!record.status.failure.unwrap().retry_safe);
    assert_eq!(
        std::fs::read(f.target().join(name)).unwrap(),
        b"another version"
    );
}
#[tokio::test]
async fn local_persisted_intent_can_publish_staged_bytes_without_downloading_again() {
    let f = Fixture::local();
    let r = f.register("clean");
    let name = format!("中文報告--{}.txt", r.id);
    std::fs::write(f.target().join(format!(".mdtransfer-{}.part", r.id)), CLEAN).unwrap();
    let mut record = f.service.store.get(r.id).unwrap().unwrap();
    record.prepared = Some(Prepared {
        sha256: hash(CLEAN),
        bytes: CLEAN.len() as u64,
        kind: ContentKind::Sanitized,
        name,
    });
    record.status.state = JobState::Delivering;
    f.service.store.save(&record).unwrap();
    f.service.tick().await.unwrap();
    assert_eq!(f.state(&r).state, JobState::Delivered);
    assert_eq!(f.mock.converted_calls.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 1);
}
#[tokio::test]
async fn disk_write_failure_and_limits_leave_no_formal_file() {
    let f = Fixture::new().await;
    let r = f.register("clean");
    std::fs::remove_dir(f.target()).unwrap();
    std::fs::write(f.target(), b"not a directory").unwrap();
    f.service.tick().await.unwrap();
    assert_eq!(f.state(&r).state, JobState::Failed);
    assert!(f.state(&r).delivered_name.is_none());
    assert_eq!(std::fs::read(f.target()).unwrap(), b"not a directory");
}
#[tokio::test]
async fn bounded_file_size_and_reserved_space_are_enforced() {
    let mut f = Fixture::new().await;
    // A second database permits constructing independently limited services without sharing locks.
    let config = Config {
        database: f.dir.path().join("limited.sqlite"),
        max_file_bytes: 2,
        ..f.config.clone()
    };
    f.service = Service::new(config).unwrap();
    let r = f.register("clean");
    f.service.tick().await.unwrap();
    assert_eq!(f.state(&r).state, JobState::Failed);
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 0);
    let config = Config {
        database: f.dir.path().join("full.sqlite"),
        min_free_bytes: u64::MAX,
        ..f.config.clone()
    };
    f.service = Service::new(config).unwrap();
    let r = f.register("clean");
    f.service.tick().await.unwrap();
    assert_eq!(f.state(&r).state, JobState::Failed);
}
#[tokio::test]
async fn timeout_is_reported_without_delivering() {
    let f = Fixture::new().await;
    let r = f.register("timeout");
    f.service.tick().await.unwrap();
    assert_eq!(f.state(&r).state, JobState::Failed);
    assert!(f.state(&r).failure.unwrap().message.contains("逾時"));
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 0);
}
#[tokio::test]
async fn authenticated_http_and_notification_never_publish_webhook_body() {
    let f = Fixture::new().await;
    let r = f.register("clean");
    let (url, server) = bind(receiver::router(f.service.clone())).await;
    let http = reqwest::Client::new();
    assert_eq!(
        http.get(format!("{url}/destinations"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let api = Receiver::new(&endpoint(url.clone(), "OPSWAT_TEST_RECEIVER")).unwrap();
    assert_eq!(api.destinations().await.unwrap()[0].key, "target");
    assert_eq!(api.register(&r).await.unwrap().registration, r);
    for _ in 0..3 {
        assert_eq!(
            http.post(format!("{url}/webhook"))
                .bearer_auth("test-only-webhook-token-32-bytes-long")
                .json(&json!({"id":r.id,"data":"untrusted contents"}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
    }
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 0);
    f.service.tick().await.unwrap();
    assert_eq!(api.status(r.id).await.unwrap().state, JobState::Delivered);
    assert_eq!(
        http.post(format!("{url}/jobs/{}/retry", r.id))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn rejected_http_retry_is_disallowed_and_resume_reuses_id() {
    let f = Fixture::new().await;
    let r = f.register("blocked");
    let failed = f.register("error");
    f.service.tick().await.unwrap();
    let (url, server) = bind(receiver::router(f.service.clone())).await;
    let api = Receiver::new(&endpoint(url, "OPSWAT_TEST_RECEIVER")).unwrap();
    assert_eq!(api.retry(r.id).await.unwrap_err().http_status, Some(409));
    assert_eq!(
        api.retry(failed.id).await.unwrap().registration.id,
        failed.id
    );
    assert_eq!(f.state(&failed).state, JobState::Scanning);
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn local_scan_deadline_is_persisted_and_database_has_single_owner() {
    let f = Fixture::local();
    let r = f.register("clean");
    assert!(Service::new(f.config.clone()).is_err());
    let mut record = f.service.store.get(r.id).unwrap().unwrap();
    record.created_at = Utc::now() - chrono::Duration::seconds(61);
    f.service.store.save(&record).unwrap();
    f.service.tick().await.unwrap();
    assert_eq!(f.state(&r).state, JobState::Failed);
    assert_eq!(f.mock.converted_calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn local_restart_reconciles_published_bytes_from_durable_intent() {
    let mut f = Fixture::local();
    let r = f.register("clean");
    let name = format!("中文報告--{}.txt", r.id);
    std::fs::write(f.target().join(&name), CLEAN).unwrap();
    let mut record = f.service.store.get(r.id).unwrap().unwrap();
    record.prepared = Some(Prepared {
        sha256: hash(CLEAN),
        bytes: CLEAN.len() as u64,
        kind: ContentKind::Sanitized,
        name,
    });
    record.status.state = JobState::Delivering;
    f.service.store.save(&record).unwrap();
    let old = std::mem::replace(
        &mut f.service,
        Service::new(Config {
            database: f.dir.path().join("placeholder.sqlite"),
            ..f.config.clone()
        })
        .unwrap(),
    );
    drop(old);
    f.service = Service::new(f.config.clone()).unwrap();
    f.service.tick().await.unwrap();
    assert_eq!(f.state(&r).state, JobState::Delivered);
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 1);
    assert_eq!(f.mock.converted_calls.load(Ordering::SeqCst), 0);
}

/// Runs real Axum middleware/handlers and SQLite without binding a socket.
/// This verifies HTTP semantics, not TCP/TLS transport or Core integration.
async fn local_request(
    f: &Fixture,
    method: &str,
    path: &str,
    token: Option<&str>,
    payload: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = axum::http::Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let body = match payload {
        Some(p) => {
            request = request.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&p).unwrap())
        }
        None => Body::empty(),
    };
    let response = receiver::router(f.service.clone())
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn local_http_auth_registration_and_webhook_use_real_handlers() {
    let f = Fixture::local();
    assert_eq!(
        local_request(&f, "GET", "/destinations", None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (code, destinations) = local_request(&f, "GET", "/destinations", Some(TOKEN), None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(destinations[0]["key"], "target");
    assert!(destinations[0].get("path").is_none());
    let r = RegisterJob {
        id: Uuid::new_v4(),
        batch_id: Uuid::new_v4(),
        data_id: "clean".into(),
        destination: "target".into(),
        filename: "中文.txt".into(),
        sha256: hash(ORIGINAL),
    };
    for _ in 0..2 {
        let (code, status) = local_request(
            &f,
            "POST",
            "/jobs",
            Some(TOKEN),
            Some(serde_json::to_value(&r).unwrap()),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(status["registration"]["id"], r.id.to_string());
    }
    let mut changed = r.clone();
    changed.filename = "other.txt".into();
    assert_eq!(
        local_request(
            &f,
            "POST",
            "/jobs",
            Some(TOKEN),
            Some(serde_json::to_value(changed).unwrap())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        local_request(
            &f,
            "POST",
            "/webhook",
            Some(TOKEN),
            Some(json!({"id":r.id}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    for _ in 0..3 {
        assert_eq!(
            local_request(
                &f,
                "POST",
                "/webhook",
                Some("test-only-webhook-token-32-bytes-long"),
                Some(json!({"id":r.id,"data":"untrusted contents"}))
            )
            .await
            .0,
            StatusCode::ACCEPTED
        );
    }
    let (code, status) =
        local_request(&f, "GET", &format!("/jobs/{}", r.id), Some(TOKEN), None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(status["state"], "scanning");
    assert_eq!(std::fs::read_dir(f.target()).unwrap().count(), 0);
    assert_eq!(f.service.store.all().unwrap().len(), 1);
}

#[tokio::test]
async fn local_http_retry_and_password_rebind_preserve_work_identity() {
    let f = Fixture::local();
    let r = f.register("password");
    let rebind_path = format!("/jobs/{}/rebind", r.id);
    let retry_path = format!("/jobs/{}/retry", r.id);
    let rebind = json!({"previous_data_id":"password","data_id":"unlocked"});
    assert_eq!(
        local_request(&f, "POST", &rebind_path, Some(TOKEN), Some(rebind.clone()))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut record = f.service.store.get(r.id).unwrap().unwrap();
    record.status.state = JobState::NeedsPassword;
    f.service.store.save(&record).unwrap();
    for _ in 0..2 {
        let (code, body) =
            local_request(&f, "POST", &rebind_path, Some(TOKEN), Some(rebind.clone())).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["registration"]["data_id"], "unlocked");
        assert_eq!(body["registration"]["destination"], r.destination);
        assert_eq!(body["registration"]["sha256"], r.sha256);
    }
    let mut duplicate = r.clone();
    duplicate.id = Uuid::new_v4();
    assert!(f.service.store.register(duplicate).is_err());
    record = f.service.store.get(r.id).unwrap().unwrap();
    record.status.state = JobState::Rejected;
    f.service.store.save(&record).unwrap();
    assert_eq!(
        local_request(&f, "POST", &retry_path, Some(TOKEN), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    record.status.state = JobState::Failed;
    f.service.store.save(&record).unwrap();
    let (code, body) = local_request(&f, "POST", &retry_path, Some(TOKEN), None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["state"], "scanning");
    assert_eq!(body["registration"]["id"], r.id.to_string());
    assert_eq!(body["registration"]["filename"], r.filename);
    assert_eq!(f.service.store.all().unwrap().len(), 1);
}
#[tokio::test]
async fn streaming_and_multipart_use_exact_lengths_ranges_and_encoded_passwords() {
    let f = Fixture::new().await;
    let path = f.dir.path().join("input.txt");
    let bytes = vec![b'x'; 150_000];
    std::fs::write(&path, &bytes).unwrap();
    let core = Core::new(&f.config.core).unwrap();
    let upload = |threshold| Upload {
        job_id: None,
        path: &path,
        filename: "中文 檔案.txt",
        rule: "rule",
        batch: None,
        password: "密碼",
        chunk_bytes: 65536,
        multipart_threshold: threshold,
    };
    assert_eq!(core.upload(upload(u64::MAX)).await.unwrap(), "uploaded");
    assert_eq!(core.upload(upload(1)).await.unwrap(), "multipart");
    let requests = f.mock.requests.lock().unwrap();
    assert_eq!(requests[0].2, bytes);
    assert_eq!(requests[0].1["content-length"], "150000");
    assert!(requests[0].1.get("transfer-encoding").is_none());
    assert!(requests[0].1["archivepwd"]
        .to_str()
        .unwrap()
        .starts_with('%'));
    let parts: Vec<_> = requests.iter().filter(|r| r.0 == "part").collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].1["content-range"], "0-65536/150000");
    assert_eq!(parts[2].1["content-range"], "131072-18928/150000");
    assert_eq!(
        parts
            .iter()
            .flat_map(|p| p.2.iter().copied())
            .collect::<Vec<_>>(),
        bytes
    );
}
#[tokio::test]
async fn failures_do_not_expose_keys_passwords_or_upstream_body() {
    let f = Fixture::new().await;
    let request = f
        .service
        .core
        .http
        .request(reqwest::Method::GET, "/not-found");
    let err = HttpApi::send(request, "測試步驟").await.unwrap_err();
    let serialized = serde_json::to_string(&err).unwrap();
    assert!(serialized.contains("404"));
    assert!(!serialized.contains("test-only-core-key"));
    assert!(!serialized.contains("127.0.0.1"));
}
