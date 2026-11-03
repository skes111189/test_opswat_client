use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

pub type JobId = Uuid;
pub type BatchId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Pending,
    Uploading,
    Scanning,
    NeedsPassword,
    AwaitingOriginal,
    Delivering,
    Delivered,
    Rejected,
    Failed,
}
impl JobState {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Delivered | Self::Rejected | Self::Failed)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "待傳送",
            Self::Uploading => "上傳中",
            Self::Scanning => "掃描中",
            Self::NeedsPassword => "需要密碼",
            Self::AwaitingOriginal => "補送原檔",
            Self::Delivering => "投遞中",
            Self::Delivered => "已送達",
            Self::Rejected => "已拒絕",
            Self::Failed => "失敗",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Sanitized,
    Original,
}

/// Only controlled messages belong here; never persist upstream response bodies/URLs/headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{step}: {message} (HTTP {http_status:?})")]
pub struct Failure {
    pub step: String,
    pub http_status: Option<u16>,
    pub message: String,
    /// False means reconcile the existing job; never upload a new copy automatically.
    pub retry_safe: bool,
}
impl Failure {
    pub fn new(step: &str, message: &str, http_status: Option<u16>) -> Self {
        Self {
            step: step.into(),
            message: message.into(),
            http_status,
            retry_safe: false,
        }
    }
    pub fn safe(mut self) -> Self {
        self.retry_safe = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Destination {
    pub key: String,
    pub label: String,
}
impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterJob {
    pub id: JobId,
    pub batch_id: BatchId,
    pub data_id: String,
    pub destination: String,
    pub filename: String,
    pub sha256: String,
}
impl RegisterJob {
    pub fn validate(&self) -> bool {
        valid_data_id(&self.data_id)
            && valid_filename(&self.filename)
            && self.filename.len() <= 200
            && !self.destination.is_empty()
            && valid_sha256(&self.sha256)
    }
}
pub fn valid_data_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
pub fn valid_filename(s: &str) -> bool {
    if s.is_empty()
        || s.len() > 240
        || s.ends_with(['.', ' '])
        || s.chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return false;
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    !matches!(stem.as_str(), "." | ".." | "CON" | "PRN" | "AUX" | "NUL")
        && !(stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebindJob {
    pub previous_data_id: String,
    pub data_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStatus {
    pub registration: RegisterJob,
    pub state: JobState,
    pub content: Option<ContentKind>,
    pub failure: Option<Failure>,
    pub delivered_name: Option<String>,
    pub updated_at: DateTime<Utc>,
}
impl JobStatus {
    pub fn new(registration: RegisterJob) -> Self {
        Self {
            registration,
            state: JobState::Scanning,
            content: None,
            failure: None,
            delivered_name: None,
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthColor {
    Unknown,
    Healthy,
    Unavailable,
}
#[derive(Debug, Clone)]
pub struct Health {
    pub color: HealthColor,
    pub failures: u8,
    pub checked_at: Option<DateTime<Utc>>,
    pub in_flight: bool,
}
impl Default for Health {
    fn default() -> Self {
        Self {
            color: HealthColor::Unknown,
            failures: 0,
            checked_at: None,
            in_flight: false,
        }
    }
}
impl Health {
    /// Emit an event only on confirmed failure or recovery.
    pub fn finish(&mut self, ok: bool) -> Option<&'static str> {
        let before = self.color;
        self.checked_at = Some(Utc::now());
        self.in_flight = false;
        if ok {
            self.failures = 0;
            self.color = HealthColor::Healthy;
        } else {
            self.failures = self.failures.saturating_add(1);
            if self.failures >= 2 {
                self.color = HealthColor::Unavailable;
            }
        }
        match (before, self.color) {
            (HealthColor::Unavailable, HealthColor::Healthy) => Some("服務已恢復"),
            (x, HealthColor::Unavailable) if x != HealthColor::Unavailable => {
                Some("服務連續兩次檢查失敗")
            }
            _ => None,
        }
    }
    pub fn usable(&self) -> bool {
        self.color == HealthColor::Healthy && self.failures == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Level {
    Info,
    Warning,
    Error,
}
impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    pub at: DateTime<Utc>,
    pub job: Option<JobId>,
    pub filename: String,
    pub event: String,
    pub level: Level,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn health_threshold_and_recovery() {
        let mut h = Health::default();
        assert_eq!(h.color, HealthColor::Unknown);
        assert!(h.finish(false).is_none());
        assert_eq!(h.color, HealthColor::Unknown);
        assert!(h.finish(false).is_some());
        assert_eq!(h.color, HealthColor::Unavailable);
        assert!(h.finish(false).is_none());
        assert!(h.finish(true).is_some());
        assert!(h.usable());
        assert!(h.finish(false).is_none());
        assert!(!h.usable());
        assert_eq!(h.color, HealthColor::Healthy);
    }
    #[test]
    fn filename_and_identifiers() {
        for s in [
            "../a", "a/b", "a\\b", "NUL.txt", "CON", "x.", "LPT1.txt", "",
        ] {
            assert!(!valid_filename(s), "{s}");
        }
        assert!(valid_filename("中文報告.txt"));
        assert!(!valid_data_id("../../file"));
        assert!(valid_data_id("aabb0011-cc"));
        assert!(!JobState::NeedsPassword.terminal());
    }
}
