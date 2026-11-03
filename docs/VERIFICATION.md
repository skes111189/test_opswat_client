# 驗證紀錄與驗收表

紀錄日期：2026-11-03。新專案位於 `/Users/cunyuan/Documents/project/test_opswat_client`。本文件區分已執行檢查與尚未執行的產品測試。

## 已執行

| 檢查 | 結果與證據範圍 |
| --- | --- |
| workspace／設定結構 | 通過：5 個 Cargo manifest、4 份設定範例、workspace 成員與來源進入點、範例憑證環境變數欄位 |
| Rust 工具鏈 | 已確認：`cargo 1.99.1`、`rustc 1.99.1`；工具位於 `/Users/cunyuan/.cargo/bin` |
| Rust 原生格式化 | 通過：已執行 `cargo fmt --all`，後續 `cargo fmt --all -- --check` exit 0 |
| Cargo workspace metadata | 通過：`cargo metadata --no-deps --offline --format-version 1` exit 0，四個 workspace 成員可辨識；不解析或驗證外部依賴 |
| Cargo 鎖定依賴與編譯 | 使用使用者下載的套件快取與已保存的 `Cargo.lock`，四個 crate 及所有測試目標成功離線編譯 |
| Clippy | 通過：`cargo clippy --workspace --all-targets --offline --locked -- -D warnings` exit 0；修正密碼容器 helper 未使用的警告 |
| Rust 單元測試 | 通過：`cargo test --workspace --lib --bins --offline --locked`，client 8、core-client 1、protocol 2，共 11 個通過；包含 Finder 啟動時套件設定位置的回歸測試 |
| 無監聽埠整合測試 | 通過：`cargo test -p receiver --test integration local_ --offline --locked`，7 個通過、15 個 TCP 案例由名稱篩選排除；使用實際 SQLite、磁碟與 Axum middleware／handler，沒有 TCP/TLS 或 Core 連線 |
| 原生 release 編譯 | 通過：`cargo build --workspace --release --offline --locked --target aarch64-apple-darwin` exit 0；`file` 確認 client／receiver 均為 Mach-O arm64 |
| macOS ARM64 實際封裝 | 使用上述實際 release binary 產生 `dist/metadefender-0.1.0-aarch64-apple-darwin.zip`，含 `.app`、兩個 binary、範例設定、文件、Cargo.lock 與 SHA256；校驗 archive 的 binary 與建置檔案相同。尚未簽章、公證或人工啟動驗收 |
| macOS 啟動入口 | bundle 已改為直接執行 Mach-O client，並處理 Finder 的工作目錄；新增最外層 `.command` 與 `開始使用.md`。此環境的 `open` 在修改前後仍回覆 `kLSNoExecutableErr`，註冊 bundle 也遭 Launch Services 拒絕；檔案存在、執行權限與 plist 格式檢查通過。直接啟動腳本可建立程序，但出現 macOS XPC／輸入法服務錯誤，未確認畫面正常顯示；已停止該測試程序 |
| Rust 語法 | 通過：10 個 `.rs` 由 tree-sitter-rust 解析，無 ERROR 或 missing node；**不等於 Rust 編譯或型別檢查** |
| workflow YAML | 通過：YAML 可解析，含 Windows x64、macOS ARM64、Linux x64 三組原生 target |
| 封裝腳本邏輯 | 先前通過：三種 archive 使用暫存目錄內明確標示的合成內容測試；此項不代表 Windows／Linux 原生編譯成功 |
| 開發用 mock Core handler | 通過：在記憶體 HTTP buffer 執行 11 組檢查，包含 ready、rules、清洗、拒絕、原檔、密碼、密碼重送、分段重試／範圍、批次關閉、HEAD、驗證；**未使用 TCP，也未測試 Rust receiver** |
| 原始碼中的開發腳本語法 | 通過：Python AST 解析 |

可重跑的結構檢查：

```sh
python3 scripts/check_source.py
# 若環境已安裝 tree-sitter 與 tree-sitter-rust：
python3 scripts/check_source.py --rust-syntax
```

本次使用既有 graphify Python 環境中的 parser 進行語法檢查，沒有重建原專案、安裝新套件或修改原專案。

## 未完成的測試與環境限制

| 項目 | 原因／狀態 |
| --- | --- |
| 完整 `cargo test --workspace --all-targets --offline --locked` | 未全數通過；15 個 TCP 案例在建立 `127.0.0.1:0` listener 時被拒絕：`Os { code: 1, kind: PermissionDenied, message: "Operation not permitted" }`。未進入各案例的 Core 呼叫與斷言，沒有略過或標為成功 |
| Core 模擬 HTTP 取檔／上傳／逾時 | 上述 TCP 案例未能驗證；須在允許監聽埠的環境執行完整測試 |
| GitHub Actions | 已交付 workflow，沒有已執行的遠端工作結果 |
| 真實 Core | 沒有向正式服務發送檔案或使用原專案金鑰 |
| Windows SMB／NAS、其他原生平台 | 沒有實機測試結果 |
| GUI、中文輸入、DPI、定案稿比對 | 已有 macOS binary，但沒有人工操作與定案稿比對結果 |

`Cargo.lock` 已產生並保存在專案根目錄。第一次完整測試為 10 個單元測試通過、19 個案例因 listener 權限失敗；將其中 4 個本來不需要網路的案例解除 mock server 相依，另補 3 個無 socket 案例及 1 個 Finder 啟動回歸測試後，目前共有 33 個測試，其中 18 個通過、15 個受上述權限阻擋。沒有把程序內 handler 測試等同 TCP、TLS 或真實 Core 整合成功。

## Rust 測試範圍

共 33 個 Rust 測試（11 個單元測試、22 個整合測試）。整合測試含 7 個不使用 listener 的 `local_` 案例，以及 15 個 TCP mock Core 案例；不依賴 Python 或正式服務。

| 測試範圍 | 位置 |
| --- | --- |
| 批次統計隔離、UUID 不受排序影響、原始檔子集合／標示、拒絕禁止重送 | `client/src/model.rs` |
| 選項依 key 保留、項目消失清除、不自動選第一項 | `client/src/model.rs` |
| 批次快照與密碼容器、名稱產生 | `client/src/model.rs` |
| 過期非同步結果、切換分頁、執行中鎖定、等待密碼、結束清密碼 | `client/src/main.rs` |
| 本機工作紀錄原子置換、UUID、busy 重設、不寫密碼 | `client/src/journal.rs` |
| Finder bundle 找到相鄰設定目錄，CLI 保留原工作目錄 | `client/src/startup.rs` |
| 中文 BOM CSV、引號／換行、公式字首 | `client/src/activity.rs` |
| 健康灰燈、兩次失敗紅燈、一次成功恢復、防重複事件；檔名與識別碼驗證 | `protocol/src/lib.rs` |
| 掃描欄位不完整時不允許、加密結果辨識 | `core-client/src/lib.rs` |
| 清洗由 Core 取得、重複 tick 冪等、原檔 Core 判定／SHA-256、錯誤雜湊 | `receiver/tests/integration.rs` |
| 拒絕、來源變更、缺少雜湊、清洗留存移除、清洗雜湊錯誤、上游錯誤不降級 | 同上 |
| 密碼換綁不可改目的地／內容、舊 Core binding 保留、重複登記與衝突 | 同上 |
| 同名唯一後綴、重啟續掃、已發布未記錄復原、暫存意圖發布、不覆寫衝突、保留意圖 | 同上 |
| 寫入失敗、單檔限制、磁碟保留量、掃描期限、30 秒 Core 逾時 | 同上 |
| HTTP token、webhook 不寫本文、拒絕禁止 retry、既有 UUID retry、單一資料庫實例 | 同上 |
| 串流 Content-Length、分段範圍、密碼 URL 編碼、錯誤不洩漏憑證 | 同上 |

執行：

```sh
cargo fetch --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo build --workspace --release --locked
```

Core 逾時整合測試刻意等待約 30 秒，其餘測試主要為本機 HTTP 與暫存目錄操作。檔案系統測試僅能證明測試主機行為，不能代替真實 SMB 保證。

在禁止監聽埠的環境，可重跑已通過的範圍：

```sh
cargo test --workspace --lib --bins --offline --locked
cargo test -p receiver --test integration local_ --offline --locked
```

`local_` 案例驗證冪等登記、單一資料庫實例、期限、暫存發布、正式檔衝突、重啟復原，以及 Axum 的 token／登記／通知／密碼換綁／拒絕禁止重試規則。其 Core endpoint 設為 `.invalid`，不呼叫 Core，不能代替清洗或原檔掃描驗證。

## 人工驗收

- [x] 保存 Cargo.lock，macOS ARM64 release build、嚴格 Clippy 通過。
- [x] 11 個單元測試與 7 個無 listener 整合測試通過。
- [ ] 在允許監聽埠的環境跑完全部 33 個 Rust 測試。
- [x] 確認 Rust 工具鏈可用，完成 rustfmt 與 workspace metadata 檢查。
- [ ] 真實 Core：規則清單、中文檔名、Core 批次查詢／關閉、分段與 cookie、密碼重送。
- [ ] 真實 Core：清洗／原檔／拒絕、CDR 保留期限、缺少 SHA-256、原檔來源變更。
- [ ] 真實 receiver：TLS chain／hostname、無效 token、目的地被移除與規則更名。
- [ ] 重複通知、上傳與查詢斷線、發布前／後強制停止 receiver、重開 client。
- [ ] Windows UNC 與實際 NAS：hard link、權限、空間／配額、掛載中斷、原子發布與同名版本。
- [ ] 中文 IME、混合 Unicode、200-byte 邊界、長路徑與不支援檔名提示。
- [ ] 大量清單（例如一萬筆）、拖放檔案／多資料夾、捲動、選取移除與清空確認。
- [ ] 工作中修改被鎖定、切換三分頁正常、需要密碼不被結束、關閉確認。
- [ ] 多批連續執行與失敗工作重試，底部只顯示目前／上一批，原始檔數不重複加總。
- [ ] Windows 100%／125%／150%／200% DPI、不同解析度與多螢幕切換；macOS Retina／Linux 桌面。
- [ ] 比對定案稿；目前僅依文字規格實作。
- [ ] UTF-8 BOM CSV 在目標試算表中文、篩選與欄位格式正確，無密碼／金鑰。
- [ ] 三平台 CI 與 artifact 可啟動，正式版本再完成簽章／公證與部署流程。
