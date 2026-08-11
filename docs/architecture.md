# Architecture and Design

本文件回答「如何實作與驗證」，保存目前採用的系統設計、演算法、測試方法與重要取捨。需求與成功標準以 `spec.md` 為準。

**目前狀態：尚無任何實作。** 以下為設計意圖，未經程式碼或實測驗證。實作開始後須依實際結果修正本文件。

## Overview

- System / approach：單一本機桌面應用程式，常駐系統列。Rust 負責 git 資料讀取、圖形佈局計算、背景排程與分岔分析；前端負責介面呈現。不含伺服器元件，不含使用者帳號。
- Primary language / runtime：Rust（版本待確認）+ Tauri 2。前端框架待確認。
- Data / external boundary：
  - 讀取：本機檔案系統上的 Git repository（object database、refs、工作目錄狀態）。
  - 網路：透過 SSH 對遠端 git 主機執行 fetch。認證委由系統既有的 SSH agent，應用程式不持有金鑰。
  - 寫入：使用者設定檔（格式與位置待確認）；以及經使用者確認後對 repository 執行的 git 操作。

## Repository Map

尚未建立任何原始碼檔案。規劃結構如下，實作時以實際結構為準。

- Rust 端：git 存取層、圖形佈局、背景排程、分岔分析、與前端的介面層
- 前端：多 repo 總覽、單一 repo 歷史圖、變更預覽、分岔預覽
- 測試位置：待確認

## Components and Responsibilities

以下為規劃中的責任切分，尚未實作。

- **Git 存取層**：開啟 repository、讀取 commit 與 refs、讀取工作目錄狀態、執行 fetch。所有對 git 的存取集中於此，其餘元件不直接接觸底層函式庫，以保留日後更換實作的空間。
- **狀態彙整**：對每個受監控的 repository 計算總覽所需狀態（領先／落後 commit 數、未提交變更數、目前分支、是否處於未完成操作中）。
- **圖形佈局**：輸入 commit 集合與父子關係，輸出每個 commit 的列位置與線道（lane）編號。純運算，不依賴介面。
- **背景排程**：定期對受監控的 repository 執行 fetch，處理失敗與重試，並將狀態變化通知介面。
- **分岔分析**：計算本機與遠端的差異範圍、各自變更的檔案集合、以及與本機未提交變更的重疊。
- **介面層**：Rust 與前端之間的資料傳遞。

## Interfaces and Data Flow

1. 使用者加入 repository（方式待確認）。應用程式讀取其狀態並顯示於總覽。
2. 背景排程定期對各 repository 執行 fetch。
3. Fetch 完成後重新計算狀態；若出現需要注意的變化，發出通知。
4. 使用者點選單一 repository 時，讀取 commit 歷史並執行圖形佈局，於前端繪出。
5. 使用者要求 pull 或處理分岔時，先執行分岔分析並呈現預覽；使用者確認後才執行實際 git 操作。

- Interface：桌面圖形介面 + 系統通知。無對外 API。
- Data model / state：核心資料為 commit DAG（節點與父子關係）、各 repository 的狀態快照、使用者設定。具體結構待實作時確定。

## Algorithm Design

本專案有兩個需要設計的演算法。兩者皆尚未實作。

### A. Commit 圖形佈局

#### Problem Definition

- Input：commit 集合，每個 commit 具有識別碼、父節點清單、時間戳記。
- Output：每個 commit 的列位置（row）與線道編號（lane），以及線段的連接關係。
- Objective：使分支走向易於辨識。具體要求：每列一個 commit；所有邊指向同一方向；同一分支盡量維持在同一條垂直線上。

#### Assumptions and Invariants

- Commit 圖為有向無環圖。
- 父節點必須排在子節點之後（顯示上為下方）。
- 相同的輸入必須產生相同的輸出，與讀取順序無關；否則畫面會在重新整理時跳動。

#### Approach

1. 排序：結合時間與拓撲的混合排序。僅在所有子節點都已排入後才排入某節點，在符合此條件的候選中取時間最新者。此規則使輸出與輸入順序無關。
2. 線道分配：由上而下逐列處理，維護目前使用中的線道。第一個父節點沿用子節點的線道（這是分支呈直線的關鍵），其餘父節點另外配置線道。
3. 線道釋放後可被後續分支重用。

#### Correctness

尚未論證。實作時應以測試鎖定兩項性質：所有邊方向一致；相同輸入產生相同輸出。

#### Complexity and Practical Limits

- Time / Space：待確認，需依實作後量測。
- Practical limit：目標使用情境為個人與小型團隊專案（數百至數千個 commit）。超大型 repository 不在本階段目標內。

#### Edge Cases

- 空 repository、僅有單一 commit、無 commit 的新建 repository。
- 多個根節點（無共同祖先的歷史）。
- 孤立分支、未被任何 ref 指到的 commit。
- 處於 rebase / merge 未完成狀態的 repository。

### B. 分岔分析

#### Problem Definition

- Input：本機分支與其對應的遠端追蹤分支；本機工作目錄的未提交變更。
- Output：兩側各自的 commit 數量與內容；兩側變更的檔案集合；重疊的檔案清單；與未提交變更重疊的檔案清單。
- Objective：在執行任何操作之前，判斷該操作是否可能產生衝突，並據此給出建議。

#### Approach

1. 計算兩側的差異範圍（本機獨有的 commit、遠端獨有的 commit）。
2. 分別取得兩側變更觸及的檔案集合。
3. 取交集。交集為空表示檔案層級無重疊，衝突風險低。
4. 另外比對即將進入的變更與本機未提交變更的檔案交集。
5. 依情境給出建議：當本機獨有的 commit 尚未被任何他人取得時（單人多設備的典型情況），建議 rebase。

#### Correctness

檔案層級無重疊可作為「衝突風險低」的指標，但不等於保證無衝突（同檔案不同區塊仍可能因上下文變動而衝突，反之亦然）。因此結果應呈現為風險評估而非保證。此界線必須在介面用語上表達清楚。

#### Edge Cases

- 無遠端追蹤分支。
- 遠端分支已被刪除。
- 兩側皆無獨有 commit（未分岔）。
- 僅單側有 commit（單純落後或單純領先，非分岔）。
- 重新命名的檔案（可能造成檔案集合比對失準）。

## Verification and Experiments

### Strategy

尚未建立。規劃方向：

- Unit：圖形佈局的排序與線道分配（可用合成的 DAG 測試，不需真實 repository）；分岔分析的集合運算。
- Integration / system：對真實 repository 執行，與 git 指令的輸出比對。
- Static checks：`cargo fmt`、`cargo clippy`。
- Experiment / benchmark：與 Sourcetree 的效能比較。基準 repository、指標與方法皆待定義。

### Commands

待確認。開發環境尚未建立。

### Data and Environment

- Dataset / fixture：待確認。單元測試預期使用合成 DAG；整合測試需要具有分支與合併的真實 repository。
- Environment：開發環境為 Ubuntu 24.04.4 LTS x86_64，31GB RAM，12 核心。目標平台為 Windows 與 Linux。
- Baseline：Sourcetree（版本待確認）。
- Metrics：待定義。
- Reproducibility：待確認。

### Critical Cases

- [ ] 正常案例：具有多個分支與合併的 repository 能正確繪出。
- [ ] 邊界案例：空 repository、單一 commit、多根節點、未完成的 rebase 狀態。
- [ ] 與已知答案比對：狀態計算結果與 git 指令輸出一致。
- [ ] 錯誤案例：SSH 認證失敗、遠端無法連線、路徑消失。

### Verification Status

環境：Ubuntu 24.04.4 x86_64、Rust 1.97.1、gcc 13.3.0、31GB RAM、12 核心。

- `cargo test --workspace`：`passed`，20 項
  - 單元測試 16 項：排序決定性、線道配置、空圖、單一 commit、多根節點、
    重複 oid、缺漏父節點、環的偵測。
  - 整合測試 4 項：對自建暫存 repository 讀取線性歷史、分支與合併、
    工作目錄狀態、空 repository。
- `cargo clippy --workspace --all-targets`（`-D warnings`）：`passed`
- `cargo fmt --check`：`passed`
- 命令列工具對真實 repository 執行：`passed` —— 見下方實測。
- 與 Sourcetree 的效能比較：`not run` —— 方法尚未定義。
- 桌面應用程式（Tauri）：`not run` —— 尚未開始。

不得把未實際執行的檢查記為通過。

### 實測結果

對 git 專案本身（85,224 commits、21,445 merges、1,006 refs，blobless clone）：

| 項目 | 數值 |
|---|---|
| 讀取 + 佈局總耗時 | 1.13 秒 |
| 峰值記憶體 | 233 MB |
| 產生的線道數 | 282 |
| 需要跨線道的邊 | 34,857 |

**效能結論**：在遠超目標使用規模的資料上，單次完整佈局約 1 秒。目標情境
（個人與小型團隊專案，數百至數千個 commit）不會構成效能問題。

**可讀性結論**：同一份資料產生 282 條線道。git 的整合分支連續合併上百個
topic 分支，每個合併的第二個父節點都會開一條線道並持續佔用，導致單列寬度
超過一千個字元。這說明**單純的 commit 層級線道配置在大型 repository 上不可讀**，
與演算法是否正確無關。目標使用規模不會觸及此問題，但若日後要支援大型
repository，需要額外的機制（例如只佈局可見範圍、或將側分支收合），
屆時應先量測再設計。命令列工具目前以繪製上限 12 條線道處理，超出的以計數表示。

不得把未實際執行的檢查記為通過。

## Design Decisions and Trade-offs

### 採用 Rust + Tauri

- Status：accepted
- Context：應用程式需常駐系統列、同時背景監控多個 repository、並繪製可能達數千節點的圖形。使用者主要語言為 Python，無 Rust 經驗，但明確表示希望採用效果最好的方案。
- Decision：Rust + Tauri 2。
- Alternatives：
  - Python + PySide6 + pygit2：使用者已熟悉，開發速度最快，效能對本專案規模足夠；代價為閒置記憶體較高（約 100–150MB 對比 20–40MB）、啟動較慢、Windows 打包較繁瑣。
  - C# + Avalonia、Go + Wails：使用者同樣不熟悉，學習成本與 Rust 相近但效能上限較低。
  - Electron / TypeScript：開發最快，但常駐記憶體占用正是本專案要避免的問題。
- Consequences：取得最佳的資源占用與效能上限。代價是使用者需同時學習 Rust 與前端開發，前期開發速度慢，專案中途放棄的風險升高。已知的緩解方式為先以無介面的命令列程式建立 git 讀取與圖形佈局，確認可行後再引入 Tauri 與前端。

### 不建立後端服務

- Status：accepted
- Context：使用者希望在自己的多台設備之間得知變更狀況。若要偵測「另一台設備上尚未推送的內容」，Git 本身無法提供，需自建同步服務。
- Decision：不建立後端，不設帳號系統，所有資料留在本機。
- Alternatives：自建後端同步各設備的 repository 元資料；或借用使用者既有的同步通道（私有 repository、雲端資料夾）。
- Consequences：使用者明確表示不願維護伺服器，此決定符合其意願，同時避免隱私疑慮與營運成本。代價是無法得知另一台設備上未推送的內容。取捨方式為改以兩點補償：在總覽上明確提示「本機有未推送的變更」以減少分岔發生；以及在分岔已發生時提供良好的處置流程。

### Commit 圖使用 arena 模式

- Status：accepted
- Context：Rust 的所有權模型使得以指標互相參照的圖結構難以實作，`Rc<RefCell<>>` 雖可行但會顯著增加複雜度，且對 Rust 新手是常見的挫折來源。
- Decision：所有 commit 節點存放於單一連續容器，節點之間以索引（而非參照）互相指涉。
- Alternatives：`Rc<RefCell<>>` 互相參照；使用現成的圖函式庫。
- Consequences：避開借用檢查器的主要衝突點，且記憶體連續有利於快取效能。代價是索引失效不會被型別系統攔截，需要以測試與封裝來保障。

### 效能取決於架構而非語言

- Status：accepted
- Context：使用者反映 Sourcetree 會卡頓。Sourcetree 的效能問題主要源於每次操作都啟動 git 子行程並解析文字輸出，以及一次載入完整歷史。
- Decision：直接讀取 git 的物件資料庫，不以啟動子行程解析文字輸出作為主要資料來源；狀態採增量更新；介面只繪製可見範圍。
- Consequences：此三項是效能目標能否達成的關鍵，優先於語言選擇。若架構錯誤，採用 Rust 亦無法達成目標。

## Known Gaps

- 全部實作。專案目前只有文件。
- 開發環境未安裝：Rust 工具鏈、Node.js、C 編譯器、Tauri 的 Linux 系統依賴皆缺。
- Windows 建置途徑未確認（需實體 Windows 環境或 CI）。
- 效能目標未量化，與 Sourcetree 的比較方法未定義。
- 「基本功能都要有」的具體清單未定義，見 `spec.md` 的 Open Questions。
