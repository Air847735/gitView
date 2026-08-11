# Project Overview

- Project: `gitview`
- Purpose: 本機端的 Git 視覺化桌面應用程式，聚焦於「即將發生的變更」而非只呈現既有歷史。
- Project type: desktop
- Primary language / runtime: Rust（版本待確認）+ Tauri 2；前端框架待確認
- Requirements source: `docs/spec.md`
- Design source: `docs/architecture.md`

專案目前只有文件，尚無任何程式碼、建置設定或測試。下列架構與指令多數為規劃內容，實作開始後必須以實際程式碼與實際執行結果更新。

# Architecture Map

尚未建立任何原始碼檔案。以下為規劃中的結構，實作時若有調整以實際結構為準。

- Rust 後端：git 資料讀取、圖形佈局計算、背景 fetch 排程、分岔分析
- 前端（框架待確認）：多 repo 總覽、單一 repo 歷史圖、分岔預覽畫面
- Data / external boundary：本機檔案系統上的 git repository；透過 SSH 對遠端 git 主機執行 fetch。不使用自建後端服務，不儲存使用者帳號。
- Detailed design and verification：`docs/architecture.md`

# Commands and Verification

開發環境尚未安裝（Rust 工具鏈、Node.js、C 編譯器、Tauri 的 Linux 系統依賴皆缺）。以下指令在環境建立並確認可執行前，一律視為未驗證。

- Install / setup: 待確認（需先安裝 rustup、Node.js、build-essential 及 Tauri Linux 依賴）
- Run / develop: 待確認（預期為 `cargo tauri dev`）
- Format / lint: 待確認（預期為 `cargo fmt`、`cargo clippy`）
- Type / static check: 由 `cargo check` 涵蓋，待確認
- Unit / integration test: 待確認（預期為 `cargo test`）
- Build / package: 待確認（預期為 `cargo tauri build`）
- Benchmark（適用時）: 未定義

不得宣稱未實際執行的檢查已通過。無法在本機確認的外部服務、正式資料、部署、效能與安全結果，必須列為未驗證。

Windows 版本無法在本機（Linux）交叉建置，需要實體 Windows 環境或 CI。此限制未實測，屬 Tauri 官方已知條件。

# Required Rules

- 修改前確認需求、受影響模組、介面、資料、相容性、測試與輸出。
- 修改範圍限於需求與必要連帶調整，不做無關重構或格式大洗版。
- 遵循現有架構、命名、錯誤處理、logging、測試與套件管理方式。
- 不覆蓋或還原使用者既有且與本次無關的變更。
- 不提交密碼、token、私鑰、個資、正式資料或其他機密。
- 新增依賴前確認必要性、相容性、授權與維護風險。
- 改變公開 API、資料格式、schema、持久化資料或部署方式前，核對 `docs/spec.md`，並在 `docs/architecture.md` 說明相容性、migration 與 rollback（如適用）。
- 修正缺陷時，在可行範圍內新增能重現問題並防止回歸的測試。
- 完成前執行與風險相稱的檢查，清楚列出未執行項目及原因。
- 開發新系統時，驗證隨實作進度建立，不預先設計實驗、指標算法、樣本數或合格門檻；待功能可執行後，再依實際輸出決定評估方式。
- 缺少自動化測試或外部驗證不阻止保存範圍明確的 commit，但不得因此宣稱功能已完整驗證或可上線。
- 本專案的操作對象是使用者真實的 git repository。任何會改寫歷史或丟棄工作成果的操作（rebase、reset、force push、丟棄未提交變更）必須先確認，並提供可回復的路徑。
- 未經使用者明確要求，不得對使用者的任何 repository 執行寫入性 git 操作。

# Language and Tooling Notes

- Rust 原始碼遵循 `rustfmt` 預設格式與 `clippy` 建議；提交前執行兩者。
- 圖結構（commit DAG）不使用 `Rc<RefCell<>>` 互相參照，改用 arena 模式：所有節點存於連續容器，彼此以索引參照。此為效能與可維護性考量，見 `docs/architecture.md`。
- 尚未確認的第三方 crate（git 存取、前端框架、繪圖方式）在選定前不得寫入文件為既定事實。

# Documentation Maintenance

- 使用者確認的研究問題、目標、範圍、限制、輸入輸出與成功標準更新至 `docs/spec.md`。
- 穩定的模組責任、資料流、介面、資料模型、演算法、正確性、複雜度、測試與實驗設計更新至 `docs/architecture.md`。
- 重要設計選擇記錄在 `docs/architecture.md` 的「Design Decisions and Trade-offs」，不只留在聊天紀錄。
- 專案入口、安裝、執行、驗證指令或結果摘要改變時更新 `README.md`。
- 只有存在未完成工作且需要交接時才更新 `HANDOFF.md`。
- 每次維護文件時，替換所有可由程式碼、設定、測試、版本歷史或使用者指令確認的 placeholder。
- 若文件變得難以閱讀或某類內容需要獨立維護，再按實際需求拆分；不得預先建立沒有內容的文件。

# Read Documents on Demand

- 需求、成功標準或範圍改變：讀取並更新 `docs/spec.md`。
- 模組、資料流、公開介面、schema、演算法、複雜度、測試或實驗方法改變：讀取並更新 `docs/architecture.md`。
- 涉及既有設計理由或替代方案：查詢 `docs/architecture.md` 的設計決策與 Git 歷史。
- 接手未完成工作：讀取 `HANDOFF.md`，並完成下列稽核。

# Handoff Audit

接手工作時先不要修改檔案：

1. 讀取 `HANDOFF.md`；若涉及需求或成功標準，再讀取 `docs/spec.md`。
2. 檢查 `git status`、`git diff` 與最近 commits；不是 Git repository 時說明缺口。
3. 核對 handoff 與實際程式碼、設定、測試及驗證輸出。
4. 先回報矛盾、未完成項目、驗證狀態與預計修改範圍，再開始實作。

`HANDOFF.md` 是交接摘要；Git、程式碼、設定與實際測試結果優先。

# Git Commit Rules

本專案目錄目前不是 Git repository。未經使用者明確要求，不得執行 `git init`。

- 每完成一個可獨立理解、可獨立檢視且不破壞 repository 基本一致性的子步驟，就建立一個 commit。
- Commit 前檢查 `git status` 與 `git diff`，只納入本次子步驟的變更。
- 無法執行的檢查在 commit 的 `Verification` 記為 `not run` 並說明原因。
- Commit 是版本檢查點，不代表已通過完整驗收、部署或正式環境驗證。
- 不得 amend、rebase 或 force push 已存在的 commits，除非使用者明確要求。
- 不得自行執行 `git init`、建立遠端 repository 或 `git push`，除非使用者明確要求。

小型文件或格式修改可使用：

```text
type(scope): short summary
```

一般功能、修正、跨檔案或行為變更使用：

```text
type(scope): short summary

Why:
- modification reason

Changes:
- important behavior or structure change

Verification:
- checks actually executed
- checks not executed and reason

Risks:
- remaining risk; omit when none
```
