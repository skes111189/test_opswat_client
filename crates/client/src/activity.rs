use protocol::{Activity, Failure};
use std::path::Path;
pub fn export(path: &Path, events: &[Activity]) -> Result<(), Failure> {
    let mut data = vec![0xEF, 0xBB, 0xBF];
    {
        let mut csv = csv::WriterBuilder::new()
            .terminator(csv::Terminator::CRLF)
            .from_writer(&mut data);
        csv.write_record(["時間", "工作ID", "檔案", "事件", "層級"])
            .map_err(|_| error())?;
        for e in events {
            // Prevent spreadsheet formula execution in exported filenames/events.
            fn cell(s: &str) -> String {
                if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
                    format!("'{s}")
                } else {
                    s.into()
                }
            }
            csv.write_record([
                e.at.to_rfc3339(),
                e.job.map(|i| i.to_string()).unwrap_or_default(),
                cell(&e.filename),
                cell(&e.event),
                e.level.to_string(),
            ])
            .map_err(|_| error())?;
        }
        csv.flush().map_err(|_| error())?;
    }
    std::fs::write(path, data).map_err(|_| error())
}
fn error() -> Failure {
    Failure::new("匯出 CSV", "無法寫入活動紀錄", None).safe()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn csv_bom_chinese_quotes_and_formula() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("log.csv");
        let event = Activity {
            at: chrono::Utc::now(),
            job: None,
            filename: "=cmd,中文.txt".into(),
            event: "含\"引號\"\n換行".into(),
            level: protocol::Level::Warning,
        };
        export(&p, &[event]).unwrap();
        let b = std::fs::read(p).unwrap();
        assert!(b.starts_with(&[239, 187, 191]));
        let mut reader = csv::Reader::from_reader(&b[3..]);
        let r = reader.records().next().unwrap().unwrap();
        assert_eq!(&r[2], "'=cmd,中文.txt");
        assert_eq!(&r[3], "含\"引號\"\n換行");
    }
}
