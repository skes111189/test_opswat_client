# MetaDefender 安全傳檔工具

Rust 跨平台新版，與原本的 C client／Go receiver 分開。正式執行程式為 Rust `client`（Iced）與 `receiver`（Tokio／Axum＋SQLite），不依賴 Python。Python 僅供原始碼檢查、開發用 Core 模擬與封裝。

**先開畫面或部署，請閱讀 [開始使用與部署](docs/QUICKSTART.md)。** macOS 套件最外層也有 `開始使用.md`、`開啟用戶端.command` 與 `啟動接收服務.command`。雙擊 `MetaDefender.app` 可先開 GUI；正式連線需設定實際服務與憑證。

| crate | 責任 |
| --- | --- |
| `client` | 三分頁桌面介面、檔案與資料夾加入、拖放、批次快照、服務健康、活動紀錄、CSV、本機工作復原 |
| `core-client` | Core API、固定 Content-Length 串流上傳、分段上傳、批次、掃描解析、receiver HTTP client |
| `protocol` | UUID 工作 ID、狀態、內容類型、事件、驗證與錯誤型別 |
| `receiver` | 驗證請求、SQLite 工作登記、Core 主動取檔、原檔 SHA-256、原子發布、重啟復原 |

## 建置與執行

需要 stable Rust、原生 C 編譯器（SQLite／TLS 相依套件）與各平台桌面環境。Linux 需 Fontconfig、XKB、Wayland／X11 開發套件，執行時需中文系統字型與可用的檔案選擇 portal。CI 的安裝清單見 `.github/workflows/build.yml`。

```sh
cargo fetch --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo build --workspace --release --locked
```

已使用 Cargo 1.99.1／rustc 1.99.1 完成 macOS ARM64 release 編譯，產生 client 與 receiver。嚴格 Clippy 與格式化檢查通過；11 個單元測試及 7 個不需監聽埠的整合測試通過。其餘 15 個 TCP 整合測試因執行環境禁止建立本機監聽埠，尚未完成驗證；真實 Core／SMB 與 GUI 人工驗收也尚未完成。`Cargo.lock` 已保存，CI 使用鎖定的相依版本。詳細狀態見 [驗證紀錄](docs/VERIFICATION.md)。

macOS ARM64 執行檔位於 `target/aarch64-apple-darwin/release/client` 與 `receiver`，可攜式封裝位於 `dist/metadefender-0.1.0-aarch64-apple-darwin.zip`。啟動前仍需依下文設定服務位址與憑證。

複製 `config/client.example.toml`、`config/receiver.example.toml` 為同目錄的 `client.toml`、`receiver.toml`，設定 URL、CA、TLS、目的地與限制。建立 `state` 目錄與已掛載的目的地目錄。相對路徑以**程式工作目錄**解析；啟動時傳入設定檔路徑。

```sh
cargo run -p receiver -- config/receiver.toml
cargo run -p client -- config/client.toml
```

兩個程式的環境需各自提供 `MD_CORE_API_KEY`，client／receiver 需相同的 `MD_RECEIVER_TOKEN`，receiver 另需 `MD_WEBHOOK_TOKEN`。receiver 與 webhook token 至少 32 bytes，建議由密碼管理工具產生不同的隨機值。設定檔只存環境變數名稱，沒有複製原專案金鑰。服務管理器與 GUI 必須能繼承這些環境變數；macOS Finder 啟動不會自動繼承終端機的環境。

## 本機整合操作

這個流程可在安裝 Rust 後測試完整桌面／receiver 呼叫鏈；模擬 Core 不提供真實安全掃描。

```sh
mkdir -p state delivery
cp config/client.local.example.toml config/client.toml
cp config/receiver.local.example.toml config/receiver.toml
export MD_CORE_API_KEY='local-development-core-key'
export MD_RECEIVER_TOKEN='local-development-receiver-token-32bytes'
export MD_WEBHOOK_TOKEN='local-development-webhook-token-32bytes'
python3 scripts/mock_core.py
```

在另兩個終端機設定相同的環境變數，分別執行 receiver 與 client。Windows 使用 PowerShell 的 `New-Item -ItemType Directory`、`Copy-Item` 與 `$env:MD_CORE_API_KEY = '…'` 等對應指令。

首次綠燈後，手動選擇「Mock workflow」與「本機驗證目錄」，加入檔案並開始傳送。模擬服務依**檔名**決定結果：

| 檔名包含 | 模擬結果 |
| --- | --- |
| `blocked` | Core 拒絕；禁止重送 |
| `password` | 等待密碼；輸入 `demo` 後重新掃描 |
| `original` | 沒有清洗輸出；核對 Core 與接收內容的 SHA-256 後原檔送達 |
| 其他 | 模擬清洗內容：`MOCK-CLEANED` 前綴與原內容 |

`delivery` 中正式檔案包含 UUID 後綴，同名檔案可保留多個版本。實際 Core 以產品的工作流程判定與輸出為準。

## 使用行為

視窗約 900 邏輯像素寬，自訂深藍標題列，提供最小化、關閉與三個上方分頁。內容淺色，沒有側欄與最大化鈕。背景工作透過訊息及 UUID 更新，傳送中仍可切換分頁；檔案集合、規則、目的地、批次名稱與批次密碼鎖定。

空白批次名稱取第一個加入的資料夾名稱與時間；僅檔案則為「檔案傳送＋時間」。批次採不可變快照；完成後清空批次名稱與密碼。等待密碼仍是未完成工作，可點「需要密碼」後補入，不會被當作已結束。只有失敗項目提供右鍵重試，而且服務必須可用；重試會先核對 receiver 的既有工作。已拒絕項目不能重送。

底部統計只涵蓋目前或上一批；「原始檔」是「已送達」的子集合，另以內容型別保存並用青色與警示符號標示。進度條執行中為藍色、結束為灰色。活動紀錄可依時間、檔案、事件、層級篩選，匯出目前篩選內容的 UTF-8 BOM CSV。

服務各自每 30 秒檢查、5 秒逾時，不重疊。初始灰燈，連續兩次失敗紅燈，一次成功綠燈；首次失敗即暫停新傳送，保留前次燈號直到確認。Core 綠燈僅表示 `/readyz` 最近健康，receiver 綠燈僅表示 `/destinations` 回覆有效。規則清單失敗會獨立提示並停用新批次；重新載入只依規則名稱／目的地代號保留選擇，沒有自動選第一項。

## 交付與部署

- [設定、TLS、SMB、復原與部署](docs/OPERATIONS.md)
- [receiver HTTP 介面與狀態](docs/API.md)
- [自動測試範圍與人工驗收表](docs/VERIFICATION.md)
- [實作對照與限制](docs/IMPLEMENTATION.md)

GitHub Actions 使用 Windows x64、macOS ARM64、Linux x64 原生 runner，執行檢查、測試、release 編譯及封裝，附上 client、receiver、設定範例、文件、校驗碼與 lockfile。這裡只提交 workflow，沒有執行遠端 workflow 或發布產品。封裝腳本也可在原生建置後執行：

```sh
cargo build --workspace --release --target aarch64-apple-darwin
python3 scripts/package.py --target aarch64-apple-darwin
```

封裝是可攜式 ZIP／tar.gz，macOS 另含 `.app`。尚未提供安裝程式、程式碼簽章、公證或正式發行。
