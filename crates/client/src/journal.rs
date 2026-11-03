use crate::model::Item;
use core_client::ClientConfig;
use protocol::{BatchId, Failure};
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path};

#[derive(Serialize, Deserialize)]
pub struct Entry {
    pub item: Item,
    pub config: ClientConfig,
}
#[derive(Serialize, Deserialize)]
pub struct Journal {
    pub version: u32,
    pub batch: Option<BatchId>,
    pub core_batch: Option<(BatchId, String)>,
    pub entries: Vec<Entry>,
}
fn error() -> Failure {
    Failure::new(
        "本機工作紀錄",
        "無法讀寫本機復原紀錄；請確認設定路徑與權限",
        None,
    )
}
pub fn load(path: &Path) -> Result<Option<Journal>, Failure> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(m) if m.len() <= 64 * 1024 * 1024 => (),
        _ => return Err(error()),
    }
    let raw = std::fs::read(path).map_err(|_| error())?;
    let journal: Journal = serde_json::from_slice(&raw).map_err(|_| error())?;
    if journal.version != 1 {
        return Err(error());
    }
    Ok(Some(journal))
}
pub fn save(path: &Path, journal: &Journal) -> Result<(), Failure> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|_| error())?;
    let bytes = serde_json::to_vec(journal).map_err(|_| error())?;
    tmp.write_all(&bytes).map_err(|_| error())?;
    tmp.as_file().sync_all().map_err(|_| error())?;
    tmp.persist(path).map_err(|_| error())?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|_| error())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_replacement_retains_ids_without_persisting_busy_or_passwords() {
        let config: ClientConfig =
            toml::from_str(include_str!("../../../config/client.local.example.toml")).unwrap();
        let mut item = Item::new("中文.txt".into(), 2).unwrap();
        item.busy = true;
        item.state = protocol::JobState::Uploading;
        let id = item.id;
        let mut journal = Journal {
            version: 1,
            batch: None,
            core_batch: None,
            entries: vec![Entry { item, config }],
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.json");
        save(&path, &journal).unwrap();
        journal.entries[0].item.state = protocol::JobState::Scanning;
        save(&path, &journal).unwrap();
        let restored = load(&path).unwrap().unwrap();
        assert_eq!(restored.entries[0].item.id, id);
        assert_eq!(restored.entries[0].item.state, protocol::JobState::Scanning);
        assert!(!restored.entries[0].item.busy);
        let raw = std::fs::read_to_string(path).unwrap();
        assert!(!raw.contains("password"));
        assert!(!raw.contains("authorization"));
    }
}
