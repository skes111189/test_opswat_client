use chrono::{Local, Utc};
use protocol::{
    Activity, BatchId, ContentKind, Destination, Failure, JobId, JobState, Level, RegisterJob,
};
use std::{collections::HashSet, path::PathBuf};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct Secret(pub Zeroizing<String>);
impl Default for Secret {
    fn default() -> Self {
        Self(Zeroizing::new(String::new()))
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}
impl Secret {
    pub fn clear(&mut self) {
        self.replace(String::new());
    }
    pub fn replace(&mut self, value: String) {
        self.0 = Zeroizing::new(value);
    }
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SettingsSnapshot {
    pub batch_id: BatchId,
    pub name: String,
    pub rule: String,
    pub destination: Destination,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Item {
    pub id: JobId,
    pub attempt: u64,
    pub path: PathBuf,
    pub filename: String,
    pub bytes: u64,
    pub selected: bool,
    pub state: JobState,
    pub content: Option<ContentKind>,
    pub failure: Option<Failure>,
    pub settings: Option<SettingsSnapshot>,
    pub registration: Option<RegisterJob>,
    pub rebind: Option<protocol::RebindJob>,
    #[serde(skip)]
    pub busy: bool,
}
impl Item {
    pub fn new(path: PathBuf, bytes: u64) -> Option<Self> {
        let filename = path.file_name()?.to_str()?.to_string();
        Some(Self {
            id: Uuid::new_v4(),
            attempt: 0,
            path,
            filename,
            bytes,
            selected: false,
            state: JobState::Pending,
            content: None,
            failure: None,
            settings: None,
            registration: None,
            rebind: None,
            busy: false,
        })
    }
    pub fn label(&self) -> &'static str {
        if self.state == JobState::Delivered && self.content == Some(ContentKind::Original) {
            "⚠ 原始檔已送達"
        } else {
            self.state.label()
        }
    }
    pub fn retry_enabled(&self, services: bool) -> bool {
        services && self.state == JobState::Failed && !self.busy
    }
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Statistics {
    pub total: usize,
    pub delivered: usize,
    pub original: usize,
    pub rejected: usize,
    pub failed: usize,
    pub unfinished: usize,
}
pub fn statistics(items: &[Item], batch: Option<BatchId>) -> Statistics {
    let mut s = Statistics::default();
    let Some(batch) = batch else {
        return s;
    };
    for i in items
        .iter()
        .filter(|i| i.settings.as_ref().is_some_and(|r| r.batch_id == batch))
    {
        s.total += 1;
        if i.busy && i.state.terminal() {
            s.unfinished += 1;
            continue;
        }
        match i.state {
            JobState::Delivered => {
                s.delivered += 1;
                if i.content == Some(ContentKind::Original) {
                    s.original += 1;
                }
            }
            JobState::Rejected => s.rejected += 1,
            JobState::Failed => s.failed += 1,
            _ => s.unfinished += 1,
        }
    }
    s
}
pub fn preserve<T: Clone>(
    selection: &mut Option<T>,
    options: &[T],
    key: impl Fn(&T) -> &str,
) -> bool {
    if let Some(old) = selection {
        if let Some(new) = options.iter().find(|v| key(v) == key(old)) {
            *old = new.clone();
            return false;
        }
        *selection = None;
        return true;
    }
    false
}
pub fn batch_name(input: &str, first_folder: Option<&str>) -> String {
    if !input.trim().is_empty() {
        return input.trim().into();
    }
    format!(
        "{} {}",
        first_folder.unwrap_or("檔案傳送"),
        Local::now().format("%Y%m%d-%H%M%S")
    )
}
pub fn apply_failure(item: &mut Item, e: Failure) {
    item.state = JobState::Failed;
    item.failure = Some(e);
    item.busy = false;
}
pub fn event(item: Option<&Item>, text: String, level: Level) -> Activity {
    Activity {
        at: Utc::now(),
        job: item.map(|i| i.id),
        filename: item.map(|i| i.filename.clone()).unwrap_or_default(),
        event: text,
        level,
    }
}
pub fn unique_paths(items: &[Item]) -> HashSet<PathBuf> {
    items.iter().map(|i| i.path.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings(id: BatchId) -> SettingsSnapshot {
        SettingsSnapshot {
            batch_id: id,
            name: "batch".into(),
            rule: "rule".into(),
            destination: Destination {
                key: "d".into(),
                label: "D".into(),
            },
        }
    }
    #[test]
    fn stable_identity_and_batch_statistics() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut items: Vec<Item> = (0..4)
            .map(|n| Item::new(format!("{n}.txt").into(), 10).unwrap())
            .collect();
        let id = items[1].id;
        for i in &mut items {
            i.settings = Some(settings(a));
            i.state = JobState::Delivered;
        }
        items[0].settings = Some(settings(b));
        items[1].content = Some(ContentKind::Original);
        items[2].state = JobState::Rejected;
        items[3].state = JobState::NeedsPassword;
        items.swap(0, 1);
        assert_eq!(
            items.iter().find(|i| i.id == id).unwrap().content,
            Some(ContentKind::Original)
        );
        let s = statistics(&items, Some(a));
        assert_eq!(
            s,
            Statistics {
                total: 3,
                delivered: 1,
                original: 1,
                rejected: 1,
                failed: 0,
                unfinished: 1
            }
        );
        assert_eq!(statistics(&items, None).total, 0);
        assert!(!items
            .iter()
            .find(|i| i.state == JobState::Rejected)
            .unwrap()
            .retry_enabled(true));
        assert!(items[0].label().contains("原始檔"));
    }
    #[test]
    fn selection_preserved_by_key_and_never_defaults() {
        let mut s = Some(Destination {
            key: "a".into(),
            label: "old".into(),
        });
        let opts = vec![Destination {
            key: "a".into(),
            label: "new".into(),
        }];
        assert!(!preserve(&mut s, &opts, |v| &v.key));
        assert_eq!(s.as_ref().unwrap().label, "new");
        assert!(preserve(&mut s, &[], |v| &v.key));
        assert!(s.is_none());
        assert!(!preserve(&mut s, &opts, |v| &v.key));
        assert!(s.is_none());
    }
    #[test]
    fn snapshot_and_password_lifetime() {
        let mut name = "original".to_string();
        let snapshot = SettingsSnapshot {
            name: name.clone(),
            ..settings(Uuid::new_v4())
        };
        let mut password = Secret::default();
        password.replace("sensitive".into());
        name.clear();
        password.clear();
        assert_eq!(snapshot.name, "original");
        assert!(password.0.is_empty());
        assert!(!format!("{password:?}").contains("sensitive"));
        assert!(batch_name("", Some("資料夾")).starts_with("資料夾 "));
        assert!(batch_name("", None).starts_with("檔案傳送 "));
    }
}
