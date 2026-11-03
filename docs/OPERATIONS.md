# 設定與操作

## 正式服務

先用 Core 使用者／金鑰確認可呼叫 `/readyz`、`/file/rules`、`/file`、批次與 `/file/converted/{data_id}`。分段上傳需 Core 5.11+；遇到明確不支援或 `Multipart uploading unavailable` 時才回到固定 Content-Length 的一般串流上傳。單一 POST 結果未知時不自動重送。Load Balancer 須設定 sticky session；HTTP client 會保留服務端 cookie。

將兩份 TOML 的 Core URL 指向同一個 Core 邏輯服務，client 與 receiver 的 Core 憑證需有對應工作查詢與清洗檔下載權限。receiver token 用於 client HTTP 驗證，webhook token 另存，不與 Core API key 混用。憑證值透過服務管理器或程序環境注入；範例不含實際金鑰。

遠端 client／receiver 使用 HTTPS。receiver 原生支援 PEM 憑證與私鑰，`certificate_pem` 使用含中繼憑證的 chain；`ca_pem` 可指定組織自簽 CA，保留主機名稱與信任鏈驗證。TLS 沒有略過驗證選項。Core 如需沿用內網 HTTP，必須明確設定該 endpoint 的 `allow_insecure_http = true`，並由管理者決定網路隔離方式；正式 receiver 非 loopback 監聽一律需 TLS。

資料庫放在 receiver 主機的本機持久磁碟，不能放在 SMB 目錄。預先建立資料庫父目錄。SQLite 採 WAL＋FULL synchronous，同一資料庫透過 OS 鎖只允許一個 receiver 實例。停止服務後可一起備份資料庫與相關檔案；在線備份應使用 SQLite backup 工具／API，不單獨複製使用中的 `.sqlite` 主檔。

## 分享目錄與磁碟

Windows TOML 使用 literal string，例如 `path = '\\fileserver\Transfer\Incoming'`。服務執行身分需具備 SMB 存取與建立檔案／hard link 的權限；不要依賴互動式使用者的磁碟代號。macOS 用 `/Volumes/TransferShare/Incoming`，Linux 用 `/mnt/transfer/Incoming` 等**已掛載的現有目錄**。服務不管理 SMB 密碼或掛載程序。

目的地目錄應限制為受管理的寫入區域。啟動時只確認路徑存在且為目錄；健康檢查不會寫探測檔。`max_file_bytes` 限制單檔接收大小，`min_free_bytes` 保留目的地可用空間，串流寫入過程會重新確認剩餘空間。權限、掛載中斷、配額或空間查詢失敗會記為具步驟的工作失敗。

每個工作寫入目的地中的 `.mdtransfer-{UUID}.part`。完整串流、雜湊、檔案同步成功後，將最終檔名、雜湊、大小與內容類型保存 SQLite，才以同一檔案系統的 **hard link 原子建立且禁止覆寫** 發布。成功後刪除暫存名；正式名為 `原始主檔名--UUID.副檔名`，不同工作不共用路徑。

這個發布策略要求目的地支援 hard links、穩定空間查詢及相應檔案同步操作。支援程度取決於 SMB 伺服器與儲存設備；不支援時會明確失敗，不會悄悄改用不安全的覆寫或直接串流到正式檔名。這是本次尚需真實 Windows SMB／NAS 驗證的限制。Windows／遠端檔案系統斷電後的持久性也需依實際設備驗收。

## 故障與復原

receiver worker 最多同時處理四個工作，逐工作鎖定；各工作串流不會阻擋其他工作的登記與狀態查詢。重啟後自動處理 `scanning`／`delivering`；等待密碼、等待原檔與失敗工作保留。`/retry` 只恢復既有工作，不新增掃描。

| 狀況 | 處理 |
| --- | --- |
| 已保存投遞意圖，正式檔不存在且暫存檔雜湊正確 | 發布同一個 UUID 路徑 |
| 正式檔已存在，與投遞意圖雜湊及大小相同 | 補記已送達，不另寫一份 |
| 正式檔存在但內容不同，或無法判斷存在狀態 | 失敗並保留投遞意圖；管理者查核，不覆寫 |
| Core 暫時不可用、磁碟錯誤、掃描超過期限 | 保存失敗步驟；修復後恢復同一工作 |
| 原檔 SHA-256 不符 | 不發布；修復來源後核對既有工作再處理 |
| 缺少已生成的清洗留存檔 | 不降級原檔；需 Core 管理者處理 |

client 的 `state_file` 保存工作 ID、來源路徑、非機密批次快照、登記與換綁請求，以及 endpoint／環境變數**名稱**；不保存密碼或憑證值。採同目錄暫存檔同步後原子置換。此檔包含使用者檔案路徑，應放在使用者可寫且受保護的位置。紀錄無法讀寫時停止開始新批次，保留既有檔案供管理者復原。

重開 client 後，已登記工作先核對 receiver 狀態；尚未登記但已取得 Core ID 的工作可冪等登記。Core 已收檔卻在回覆前斷線，或 client 在保存 Core ID 前退出，屬**上傳結果未知**，不會自動建立重複掃描。Core 請求的 URL 編碼 `metadata` 會帶 `transfer_job_id`，管理者可據此查詢 Core，再依原 UUID／SHA-256 登記或換綁。Core 本身沒有這套 client 的冪等鍵，不能保證此階段自動復原。

密碼僅存在記憶體，使用 Zeroizing 容器，背景任務完成後釋放；批次全部結束時清空名稱與密碼。若使用者暫緩提供密碼，該工作保持未完成。關閉 client 會停止本機上傳／追蹤與原檔補送，但 receiver 已登記的可處理工作持續執行。重開後重新輸入所需密碼。

## 日常檢查與發行

活動紀錄的錯誤顯示失敗步驟與可用的 HTTP 狀態，不保存上游本文或憑證。CSV 為 BOM UTF-8＋CRLF，保留中文與換行／引號；可能被試算表視為公式的內容加單引號處理。畫面活動紀錄為目前執行期間資料，receiver 的工作／復原資訊則持久保存。

使用封裝內的 `start-client`／`start-receiver` 啟動可固定工作目錄。先複製範例設定，不要將 token 放入啟動腳本。macOS `.app` 使用外層封裝目錄的 `config`，不可只搬走 `.app`。Windows 以服務帳號啟動 receiver 前，確認該身分可存取 UNC。程式沒有自行安裝系統服務；可交給既有 Windows 排程／服務管理器、launchd 或 systemd 管理。

本次沒有使用真實 Core、正式分享目錄或實際憑證發送檔案，沒有推送 GitHub、執行遠端 workflow 或發布版本。確認 [驗收表](VERIFICATION.md) 後再安排實際部署。
