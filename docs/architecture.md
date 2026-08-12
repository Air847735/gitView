# Architecture and Design

本文件回答「如何實作與驗證」，保存目前採用的系統設計、演算法、測試方法與重要取捨。需求與成功標準以 `spec.md` 為準。

**目前狀態：核心功能與基本 Git 操作皆已實作並通過測試**，包含分岔的實際處置與衝突解決。尚未實作的項目見 `spec.md` 的基本功能清單與本文件的 Known Gaps。

## Overview

- System / approach：單一本機桌面應用程式，常駐系統列。Rust 負責 git 資料讀取、圖形佈局計算、背景排程與分岔分析；前端負責介面呈現。不含伺服器元件，不含使用者帳號。
- Primary language / runtime：Rust 1.97 + Tauri 2。前端為純 HTML/CSS/JS，無框架與打包工具。
- Data / external boundary：
  - 讀取：本機檔案系統上的 Git repository（object database、refs、工作目錄狀態）。
  - 網路：透過 SSH 對遠端 git 主機執行 fetch。認證委由系統既有的 SSH agent，應用程式不持有金鑰。
  - 寫入：使用者設定檔（JSON，位於系統設定目錄）；fetch 對遠端追蹤分支的更新；
    以及經使用者確認後對 repository 執行的操作（提交、暫存、分支、stash、
    快轉、rebase、merge、推送、衝突解決）。會改寫歷史或移動分支的操作一律
    先建立還原點。

## Repository Map

- `crates/core/src/dag.rs`：commit DAG 的 arena 表示與兩階段建構。
- `crates/core/src/layout.rs`：時間與拓撲的混合排序、線道配置。零相依。
- `crates/core/src/repo.rs`：以 git2 讀取 commit 與 ref。只讀不寫。
- `crates/core/src/status.rs`：狀態彙整與需要注意的程度分級。
- `crates/core/src/divergence.rs`：分岔分析與建議。
- `crates/core/src/fetch.rs`：fetch 與認證，錯誤分類為認證／網路／其他。
- `crates/core/src/ops.rs`：會改動 repository 的同步操作（快轉、rebase、merge、
  推送、中止），以及還原點的建立、列出、還原與清理。
- `crates/core/src/workspace.rs`：暫存、提交、分支、stash、丟棄。
- `crates/core/src/conflict.rs`：衝突的檢視與解決，以及解決後的繼續與略過。
- `crates/core/src/diff.rs`：結構化差異、行內字元標示、空白變更判定、
  以及與即將進來的遠端變更的撞擊偵測。
- `crates/app/src/service.rs`：應用程式狀態與各項操作，指令與背景排程共用。
- `crates/app/src/commands.rs`：前端可呼叫的指令。
- `crates/app/src/watcher.rs`：背景排程與通知規則。
- `crates/app/src/dto.rs`：傳給前端的資料形狀。
- `crates/app/ui/`：前端，純 HTML/CSS/JS。
- `crates/cli/src/main.rs`：命令列工具。
- 測試：各模組內的 `#[cfg(test)]`，以及 `crates/*/tests/` 下的整合測試。

## Components and Responsibilities

- **Git 存取層**：開啟 repository、讀取 commit 與 refs、讀取工作目錄狀態、執行 fetch。所有對 git 的存取集中於此，其餘元件不直接接觸底層函式庫，以保留日後更換實作的空間。
- **狀態彙整**：對每個受監控的 repository 計算總覽所需狀態（領先／落後 commit 數、未提交變更數、目前分支、是否處於未完成操作中）。
- **圖形佈局**：輸入 commit 集合與父子關係，輸出每個 commit 的列位置與線道（lane）編號。純運算，不依賴介面。
- **背景排程**：定期對受監控的 repository 執行 fetch，處理失敗與重試，並將狀態變化通知介面。
- **分岔分析**：計算本機與遠端的差異範圍、各自變更的檔案集合、以及與本機未提交變更的重疊。
- **介面層**：Rust 與前端之間的資料傳遞。

## Interfaces and Data Flow

1. 使用者以資料夾選擇器加入 repository；路徑經 `Repository::discover` 驗證並正規化為工作目錄路徑後存入設定。
2. 背景排程定期對各 repository 執行 fetch。
3. Fetch 完成後重新計算狀態；若出現需要注意的變化，發出通知。
4. 使用者點選單一 repository 時，讀取 commit 歷史並執行圖形佈局，於前端繪出。
5. 使用者要求 pull 或處理分岔時，先執行分岔分析並呈現預覽；使用者確認後才執行實際 git 操作。

- Interface：桌面圖形介面 + 系統通知。無對外 API。
- Data model / state：核心資料為 commit DAG（arena 容器加索引）、各 repository 的狀態快照、使用者設定。應用程式層另存每個 repository 上次 fetch 的結果。

## Algorithm Design

本專案有兩個需要設計的演算法。兩者皆已實作並有測試涵蓋。

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

兩項關鍵性質已由測試鎖定：所有邊方向一致（`parents_never_precede_children`）；相同輸入產生相同輸出，與插入順序無關（`order_does_not_depend_on_insertion_order`、`equal_timestamps_break_ties_by_oid`）。整體正確性尚未形式化論證。

#### Complexity and Practical Limits

- Time / Space：排序為 O(V+E log V)，線道配置為 O(V × 線道數)。實測 85,224 個節點於 1.13 秒內完成，峰值記憶體 233MB。
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

- Unit：圖形佈局的排序與線道分配以合成 DAG 測試，不需真實 repository；分岔建議、狀態分級、fetch 錯誤分類同樣為純函式測試。
- Integration：於暫存目錄自建 repository（含以本機裸 repository 模擬遠端），驗證讀取、狀態、分岔分析與應用程式資料層。不連網。
- Static checks：`cargo fmt`、`cargo clippy --all-targets -D warnings`。
- 介面繪製：無自動化方式，需人工在實體桌面環境確認。

### Commands

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

### Data and Environment

- Dataset / fixture：單元測試使用合成 DAG；整合測試於執行時自建暫存 repository。效能量測使用 git 專案本身的 blobless clone。
- Environment：開發環境為 Ubuntu 24.04.4 LTS x86_64，31GB RAM，12 核心。目標平台為 Windows 與 Linux。
- Baseline：Sourcetree。版本與比較方法尚未定義。
- Metrics：讀取與佈局耗時、峰值記憶體、線道數、跨線道邊數。
- Reproducibility：佈局為決定性函式，相同 repository 每次輸出相同；效能數據為單次量測，未取多次平均。

### Critical Cases

- [x] 正常案例：具有分支與合併的 repository 能正確讀取並佈局。
- [x] 邊界案例：空 repository、單一 commit、多根節點、重複 oid、缺漏父節點、環。
- [x] 與已知答案比對：以 git2 建立已知結構後驗證領先／落後數與檔案集合。
- [x] 錯誤案例：路徑消失、非 repository、無追蹤分支、fetch 錯誤分類。
- [ ] 未完成的 rebase 狀態：狀態分級已有單元測試，但未對真實的 rebase 中途狀態測試。
- [ ] SSH 認證失敗與遠端無法連線的實地驗證（錯誤分類已有單元測試）。

### Verification Status

環境：Ubuntu 24.04.4 x86_64、Rust 1.97.1、gcc 13.3.0、31GB RAM、12 核心。

- `cargo test --workspace`：`passed`，93 項
  - core 單元測試：排序決定性、線道配置、空圖、單一 commit、多根節點、
    重複 oid、缺漏父節點、環的偵測、狀態分級、分岔建議、fetch 錯誤分類、
    工作區狀態標籤。
  - core 整合測試（讀取）10 項：自建暫存 repository 的讀取、分支與合併、
    工作目錄計數，以及以本機裸 repository 模擬遠端的完整分岔情境。
  - core 整合測試（操作）16 項：快轉成功與被拒絕的兩種前置條件、rebase 產生
    線性歷史且可還原、merge 產生合併節點、還原點的列出與清理、還原拒絕
    命名空間外的 ref、**實際製造衝突後的完整流程**（停在衝突、列出雙方內容、
    整檔擇一、以編輯內容解決、繼續、中止）、暫存與提交與取消暫存、
    空訊息被拒、分支建立與切換、stash 往返、丟棄變更。
  - app 單元測試：設定正規化與往返、通知的重複抑制、排序規則。
  - app 整合測試 11 項：加入與移除 repository、設定持久化、路徑消失的處理、
    排序、圖形資料與截斷、分岔資料。
- `cargo clippy --workspace --all-targets`（`-D warnings`）：`passed`
- `cargo fmt --check`：`passed`
- `cargo build --release`：`passed`
- 命令列工具對真實 repository 執行：`passed` —— 見下方實測。
- 桌面應用程式啟動：`passed` —— 視窗可開啟、程序穩定執行。
- 前端語法檢查（`node --check`）：`passed`。
- 操作類指令的介面互動：`not run` —— 受限於下方的繪製問題，按鈕未經人工點擊
  驗證。其後端邏輯已由 16 項操作整合測試涵蓋。
- **桌面介面的繪製：`not run`** —— 見下方「介面繪製的診斷結果」。已確認為
  環境限制，非程式缺陷。需在實體桌面環境確認。
- 背景 fetch 對真實遠端執行：`not run` —— 本機無 SSH 私鑰，且不應在未經
  要求下對使用者的 repository 連線。fetch 的錯誤分類已有單元測試涵蓋。
- 與 Sourcetree 的效能比較：`not run` —— 方法尚未定義。
- 安裝檔封裝（`cargo tauri build`）：`not run`。

不得把未實際執行的檢查記為通過。

### 介面繪製的診斷結果

在開發環境（透過 RDP 的遠端桌面工作階段，無硬體加速）中，應用程式視窗會
開啟且程序穩定，但內容區域始終是空白的。診斷過程與結論如下，記錄於此是為了
避免日後有人重複同一段排查。

**結論：DOM 完全正確，失敗的是繪製。** 由前端回報的實際版面幾何為：

```
視窗 1280×820 · body 1280×820 · 工具列 1280×54 · 版面 1280×659 · 清單 320×659
背景 rgb(242,244,245) · visibility: visible · display: flex · body 有 4 個子元素
```

所有元素都有正確的尺寸、樣式已套用、腳本已執行，只是像素沒有被合成到螢幕。

**排除掉的可能原因**（每一項都實測過）：

- 前端資源未嵌入 —— 以 `asset_resolver` 確認三個檔案都在，大小正確。
- 外部 `<link>` / `<script>` 載入失敗 —— 外部與內嵌兩種方式的版面幾何完全相同。
- CSP 阻擋 —— 停用 CSP 後行為不變。
- 版面高度依賴 `100vh` 而塌陷 —— 幾何數值證明高度正確。
- `display: grid` 不支援 —— 改為 flex 後行為不變。
- WebKit 合成模式 —— `WEBKIT_DISABLE_COMPOSITING_MODE`、
  `WEBKIT_DISABLE_DMABUF_RENDERER`、`LIBGL_ALWAYS_SOFTWARE` 各種組合皆無效。

**唯一會顯示的情況**是頁面極為簡單時（例如只有純色背景與一行文字），
顯示複雜度似乎存在一個門檻。這也解釋了排查過程中「逐段加入 CSS 會在某個
長度後變成空白」的現象 —— 那是繪製複雜度的門檻，不是某一條規則有問題。

因此本專案未因此修改任何程式碼。介面需在具備硬體加速的實體桌面環境
（或 Windows）上驗證。

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

## Design Decisions and Trade-offs

### 差異以結構化資料輸出，而非 patch 文字

- Status：accepted
- Context：介面需要並排呈現、逐行選取以做部分暫存、以及標示行內的字元差異。
  這三件事都無法從 patch 文字可靠地還原。
- Decision：輸出結構化的 hunk 與行，每一行帶有角色、新舊行號、行內變動範圍
  與空白判定。
- Consequences：介面實作直接，部分暫存可以精確到單行。代價是資料量比文字大，
  但差異一次只看一個檔案，規模可控。

### 行內差異用前後綴消去，而非完整 LCS

- Status：accepted
- Context：標出「這一行的哪幾個字改了」需要行內比對。完整的 LCS 成本較高。
- Decision：以字詞為單位切分後，消去共同前綴與後綴，中間即為變動處。
  只在刪除行與新增行數量相同時配對；數量不同代表整段增刪，逐行標示反而誤導。
- Consequences：對「改了一個識別字或數值」這類最常見的情況，結果與 LCS 相同
  但便宜得多。大幅重排的行會退化成整行標示，這在可接受範圍內。

### 在未提交的差異上標示與即將進來的變更相撞的區段

- Status：accepted
- Context：本工具的主張是「在事情發生前就告訴你」。分岔分析已能指出哪些
  檔案兩側都改過，但檔案層級太粗 —— 同一個大檔案的不同區域互不相干。
- Decision：比較 HEAD 與遠端追蹤分支，取得即將變動的行號範圍，
  與工作目錄差異的每個 hunk 比對，重疊者標記。
- Consequences：使用者在編輯當下就知道哪幾段有風險，可以及早調整。
  兩份差異的基準都是 HEAD，索引與 HEAD 不同時會有誤差，
  因此呈現為風險提示而非保證，介面用語需反映這一點。

### 以還原點取代 reflog 檢視作為復原機制

- Status：accepted
- Context：任何會改寫歷史或移動分支的操作都可能讓使用者失去工作成果。
  git 本身有 reflog，但要求使用者理解它、找到正確的項目、再自行 reset。
- Decision：在每次危險操作前，於 `refs/gitview/undo/` 建立一個指向當時 HEAD
  的 ref，並在介面提供一鍵還原。保留最近 20 個，超過的在下次操作後自動清除。
- Alternatives：呈現 reflog 讓使用者自行選擇；完全依賴 git 既有機制。
- Consequences：使用者不需要理解 reflog 就能反悔。放在 `refs/gitview/` 而非
  `refs/heads/`，因此不會出現在分支清單，也不會被推送。代價是被丟棄的 commit
  在還原點存在期間不會被回收，所以必須設保留上限。

### 前置條件不符時拒絕，而非自動處理

- Status：accepted
- Context：工作目錄有未提交的變更時執行 rebase 或 merge 會失敗或造成混亂。
  常見做法是自動暫存、操作完再放回。
- Decision：直接拒絕並說明原因，同時在介面上提供一顆「先暫存起來」的按鈕，
  由使用者明確決定。
- Alternatives：自動暫存並自動還原。
- Consequences：多一次點擊，但少一個會出錯的隱藏環節 —— 自動還原若失敗，
  使用者的變更會停在一個他不知情的 stash 裡。這類「幫使用者做決定」的便利
  在出錯時的代價，高於它節省的一次點擊。

### 衝突解決做在應用程式內，但不做逐區塊挑選

- Status：accepted
- Context：rebase 與 merge 撞到衝突是常態。若把使用者丟回終端機，
  「不必離開這個工具」的價值就消失了。但完整的三方合併編輯器（逐區塊挑選、
  行內差異標示）工作量極大。
- Decision：提供衝突解決畫面，內容為：雙方版本並列顯示、整檔採用其中一方、
  以及直接編輯含衝突標記的合併結果。逐區塊挑選由使用者在編輯區完成。
- Alternatives：完整的三方合併編輯器；只在確定不會衝突時才允許操作。
- Consequences：涵蓋實際會遇到的情況且工作量可控。逐區塊挑選的效率不如
  專用編輯器，日後可在同一個畫面上疊加而不需改動核心介面。

### 介面層的四個取捨

**前端不使用框架與打包工具。** 這個介面是一份儀表板加一張圖，框架帶來的
狀態管理與元件化在此規模下沒有回報，卻要換來 npm 相依樹、打包設定與版本
升級的長期負擔。純 HTML/CSS/JS 讓建置只剩 `cargo build` 一步。

**歷史圖以 canvas 畫線、以 HTML 排文字。** 全部畫在 canvas 上會讓中文字型
與選取行為都要自己處理；全部用 DOM 則在數千個節點時會拖慢捲動。分工之後
兩邊都用在它擅長的地方。

**狀態同時以文字與顏色表示。** 只用顏色編碼狀態的話，色覺障礙者無法分辨；
合併節點畫成空心圓也是同樣的理由 —— 形狀本身要能區分。

**核心層不依賴 serde。** DTO 定義在應用程式層，介面需要什麼欄位不會反過來
牽動核心的資料模型。代價是多一層轉換程式碼，換得的是兩邊可以各自演進。

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

- **桌面介面的繪製與互動未經人工驗證。** 開發環境無法繪製 WebKitGTK 內容，
  已確認為環境限制而非程式缺陷。所有按鈕的後端邏輯有整合測試涵蓋，
  但沒有人實際點過。需在實體桌面環境確認。
- 逐區塊挑選衝突內容尚未提供，目前以編輯合併結果代替。
- 分支的刪除、重新命名與設定追蹤分支尚未實作。
- 互動式 rebase（調整順序、squash 等）尚未實作。
- 背景 fetch 未對真實遠端驗證（本機無 SSH 私鑰）。
- Windows 建置途徑未確認，需實體 Windows 環境或 CI。
- 與 Sourcetree 的效能比較方法未定義。
- 大型 repository 的線道數問題（見實測結果），目標規模不受影響。
