use chrono::{DateTime, Utc};
use fs2::FileExt;
use protocol::{ContentKind, Failure, JobId, JobState, JobStatus, RebindJob, RegisterJob};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    path::Path,
    sync::Mutex,
};

pub type Result<T> = std::result::Result<T, Failure>;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prepared {
    pub sha256: String,
    pub bytes: u64,
    pub kind: ContentKind,
    pub name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    pub prepared: Option<Prepared>,
}
pub struct Store {
    db: Mutex<Connection>,
    _lock: File,
}
fn db_error() -> Failure {
    Failure::new("SQLite", "工作資料庫讀寫失敗", None)
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let lock_path = path.with_extension("receiver.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|_| db_error())?;
        lock.try_lock_exclusive()
            .map_err(|_| Failure::new("啟動", "另一個 receiver 正在使用資料庫", None))?;
        let db = Connection::open(path).map_err(|_| db_error())?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY, data_id TEXT UNIQUE NOT NULL, record TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS bindings(data_id TEXT PRIMARY KEY, job_id TEXT NOT NULL);")
            .map_err(|_| db_error())?;
        Ok(Self {
            db: Mutex::new(db),
            _lock: lock,
        })
    }
    pub fn get(&self, id: JobId) -> Result<Option<Record>> {
        let db = self.db.lock().map_err(|_| db_error())?;
        let raw: Option<String> = db
            .query_row(
                "SELECT record FROM jobs WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| db_error())?;
        raw.map(|s| serde_json::from_str(&s).map_err(|_| db_error()))
            .transpose()
    }
    pub fn all(&self) -> Result<Vec<Record>> {
        let db = self.db.lock().map_err(|_| db_error())?;
        let mut statement = db
            .prepare("SELECT record FROM jobs ORDER BY rowid")
            .map_err(|_| db_error())?;
        let rows = statement
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|_| db_error())?;
        rows.map(|row| serde_json::from_str(&row.map_err(|_| db_error())?).map_err(|_| db_error()))
            .collect()
    }
    pub fn save(&self, record: &Record) -> Result<()> {
        let raw = serde_json::to_string(record).map_err(|_| db_error())?;
        let db = self.db.lock().map_err(|_| db_error())?;
        let changed = db
            .execute(
                "UPDATE jobs SET data_id=?2, record=?3 WHERE id=?1",
                params![
                    record.status.registration.id.to_string(),
                    record.status.registration.data_id,
                    raw
                ],
            )
            .map_err(|_| db_error())?;
        if changed != 1 {
            return Err(db_error());
        }
        Ok(())
    }
    pub fn register(&self, request: RegisterJob) -> Result<JobStatus> {
        if let Some(existing) = self.get(request.id)? {
            if existing.status.registration == request {
                return Ok(existing.status);
            }
            return Err(Failure::new(
                "登記工作",
                "工作 ID 已綁定其他內容",
                Some(409),
            ));
        }
        let record = Record {
            status: JobStatus::new(request),
            created_at: Utc::now(),
            prepared: None,
        };
        let raw = serde_json::to_string(&record).map_err(|_| db_error())?;
        let mut db = self.db.lock().map_err(|_| db_error())?;
        let tx = db.transaction().map_err(|_| db_error())?;
        tx.execute(
            "INSERT INTO bindings(data_id,job_id) VALUES(?1,?2)",
            params![
                record.status.registration.data_id,
                record.status.registration.id.to_string()
            ],
        )
        .map_err(|_| Failure::new("登記工作", "Core ID 已綁定工作", Some(409)))?;
        tx.execute(
            "INSERT INTO jobs(id,data_id,record) VALUES(?1,?2,?3)",
            params![
                record.status.registration.id.to_string(),
                record.status.registration.data_id,
                raw
            ],
        )
        .map_err(|_| db_error())?;
        tx.commit().map_err(|_| db_error())?;
        Ok(record.status)
    }
    pub fn rebind(&self, id: JobId, request: RebindJob) -> Result<JobStatus> {
        let mut record = self
            .get(id)?
            .ok_or_else(|| Failure::new("更新工作", "工作不存在", Some(404)))?;
        if record.status.registration.data_id == request.data_id {
            return Ok(record.status);
        }
        if record.status.state != JobState::NeedsPassword
            || record.status.registration.data_id != request.previous_data_id
            || !protocol::valid_data_id(&request.data_id)
        {
            return Err(Failure::new(
                "更新工作",
                "僅等待密碼的工作可更新 Core ID",
                Some(409),
            ));
        }
        record.status.registration.data_id = request.data_id;
        record.status.state = JobState::Scanning;
        record.status.failure = None;
        record.status.updated_at = Utc::now();
        record.created_at = Utc::now();
        let raw = serde_json::to_string(&record).map_err(|_| db_error())?;
        let mut db = self.db.lock().map_err(|_| db_error())?;
        let tx = db.transaction().map_err(|_| db_error())?;
        // Retain old bindings: stale password attempts must never be delivered as another job.
        tx.execute(
            "INSERT INTO bindings(data_id,job_id) VALUES(?1,?2)",
            params![record.status.registration.data_id, id.to_string()],
        )
        .map_err(|_| Failure::new("更新工作", "Core ID 已綁定工作", Some(409)))?;
        tx.execute(
            "UPDATE jobs SET data_id=?2,record=?3 WHERE id=?1",
            params![id.to_string(), record.status.registration.data_id, raw],
        )
        .map_err(|_| db_error())?;
        tx.commit().map_err(|_| db_error())?;
        Ok(record.status)
    }
}
