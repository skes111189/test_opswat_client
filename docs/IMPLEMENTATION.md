# 實作對照

| 要求 | 實作位置／行為 |
| --- | --- |
| Cargo workspace、原專案保留 | 根 `Cargo.toml`；所有新檔案位於 `test_opswat_client` |
| Iced GUI、三分頁、深藍標題、無最大化鈕 | `crates/client/src/main.rs`，900×760 邏輯尺寸、自訂標題列、原生 DPI |
| 檔案／資料夾／拖放、選取移除與清空確認 | Iced 訊息＋原生 rfd 選擇器＋背景 WalkDir，清單可捲動 |
| UUID 與非同步隔離 | 每列 UUID＋attempt，過期 attempt／不同工作回覆不更新該列 |
| 批次不可變快照與密碼清空 | `SettingsSnapshot` 與 `Secret`；等待密碼仍鎖定批次 |
| 批次統計與原檔標示 | `statistics()` 依 batch UUID；`ContentKind` 獨立欄位 |
| 規則／目的地保留 | `preserve()` 依名稱／key；只有成功載入的新清單可判定選項消失 |
| 每服務健康狀態 | `Health`，獨立 in-flight／時間／失敗門檻，設定重載帶 generation |
| CSV／活動篩選 | `activity.rs` 與活動分頁；BOM、CSV escaping、公式字首處理 |
| Core 批次／分段／密碼重掃／原檔補送 | `core-client` 與 GUI 狀態任務，Core 批次完成後關閉 |
| receiver 工作綁定／驗證 | `store.rs`、`RegisterJob`、token 驗證、Core SHA-256；保留歷次 Core ID |
| 清洗主動取檔／通知不可信 | worker 主動 GET converted；webhook 只接受 UUID 通知 |
| SQLite／同名版本／重啟 | durable publication intent＋UUID 檔名＋no-overwrite hard link＋雜湊核對 |
| 外部設定、TLS、磁碟限制 | `config/*.example.toml`、rustls、已掛載目錄、串流大小與可用空間檢查 |
| 模擬測試 | `crates/receiver/tests/integration.rs`；不需 Python，Rust Axum mock Core |
| 可操作的開發服務 | `scripts/mock_core.py`；僅提供人工 GUI 整合測試資料，不是產品 runtime |
| 原生 CI 與封裝 | `.github/workflows/build.yml` 與 `scripts/package.py` |

尚未驗證的項目與已知實作邊界：

- macOS ARM64 release 編譯、嚴格 Clippy、格式化與 18 個測試已通過。15 個需 TCP listener 的整合測試因執行環境權限失敗，不能視為通過；Windows／Linux 原生編譯與完整 TCP/TLS 整合仍待執行。
- 沒有取得可讀取的定案設計稿檔案，GUI 依交接計畫的文字規格實作，尚未做像素比對或人工 GUI 操作驗收。
- 真實 Core 對進度欄位、URL 編碼規則名稱、分段 cookie／版本限制與清洗留存行為需實機驗證；參照原專案附帶 OpenAPI 與既有行為實作。
- SMB／NAS 必須支援目前的原子 hard-link 發布；不支援時失敗，沒有以非原子發布替代。
- client 不自動重新提交上傳結果未知的 Core 工作；透過 metadata 與原 UUID 做人工復原。已登記 receiver 工作則可安全核對復原。
- GUI 清單使用 Iced Scrollable，尚未驗收萬筆規模效能；檔名超過 200 UTF-8 bytes 或跨平台不合法時會略過並記錄數量。
- `state_file` 不保存活動紀錄、密碼或尚未開始傳送的待選檔案。請於關閉前匯出需要的活動紀錄。
- 封裝未簽章、公證、建立安裝程式或推送正式發行。CI 的 workflow 與 artifacts 需在已連接 GitHub repository 執行後才有遠端結果。

開發參照：[Iced 0.13 原始碼](https://github.com/iced-rs/iced/tree/0.13.1)、[reqwest 0.12 文件](https://docs.rs/reqwest/0.12/reqwest/)、[Axum 文件](https://docs.rs/axum/0.8/axum/)、[GitHub runner 文件](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)。Core 介面依據相鄰原專案 `MetaDefender Core API.json`，未複製其中以外的服務金鑰。
