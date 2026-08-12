# gitview

本機端的 Git 視覺化桌面應用程式。與現有工具的差異在於呈現的重點：不只畫「已經發生的歷史」，而是畫「即將進來的變更」與「你接下來的操作會造成什麼結果」。

主要針對的情境是一個人同時管理多個 repository，並在自己的多台設備之間切換工作。

> 狀態：可執行。核心功能已完成並通過測試；介面繪製尚待在實體桌面環境確認。

## Overview

- 要解決的問題：多 repository 狀態難以掌握、遠端有更新不會主動知道、本機與遠端分岔後不知道怎麼處理才安全。
- 方法摘要：常駐系統列的本機應用程式，背景定期 fetch，在執行操作前先分析並預覽結果。不使用伺服器，不需要帳號，資料不離開本機。
- 目前結論：多 repo 總覽、背景 fetch、拉取前預覽與分岔分析皆已實作並通過測試。

詳細範圍與成功標準見 `docs/spec.md`；實作、演算法與驗證設計見 `docs/architecture.md`。

## Requirements

- Runtime / language：Rust 1.97 + Tauri 2。前端為純 HTML/CSS/JS，不使用框架或打包工具。
- Package manager / build tool：Cargo。前端無建置步驟。
- External services：無。透過 SSH 存取使用者既有的 git 遠端，認證委由系統的 SSH agent。

## Setup

需要 Rust 工具鏈（rustup）與下列系統套件：

```sh
sudo apt-get install -y build-essential pkg-config libssl-dev cmake \
    libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
    librsvg2-dev file wget nodejs npm
```

`build-essential` 是必要的：`git2` 需要編譯 libgit2 的 C 原始碼。
webkit2gtk 等套件供之後導入 Tauri 使用。

## Run

```sh
cargo build --release
```

**桌面應用程式**：

```sh
./target/release/gitview-app
```

啟動後常駐系統列。關閉視窗只會隱藏，從系統列選單可重新開啟或結束。

**命令列工具**（不需要圖形介面，適合遠端連線時使用）：

```sh
./target/release/gitview <repository 路徑> [--limit N]
```

兩者都只讀取 repository。唯一會寫入的操作是 fetch，它只更新遠端追蹤分支，
不會動到本機分支、工作目錄或未提交的內容。

## Verify

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

目前狀態：72 項測試通過；clippy 與 fmt 通過。整合測試會在系統暫存目錄
自建 repository，不接觸網路，也不接觸既有的任何 repository。

尚未驗證：桌面介面的實際繪製。開發環境是遠端桌面工作階段且無硬體加速，
WebKitGTK 在其中無法繪製任何內容，需要在實體桌面環境上確認。

## Usage

1. 按「加入 repository」選擇資料夾。可以加入多個。
2. 左側總覽顯示每個 repository 的狀態，需要處理的排在前面：
   分岔、有待拉取的內容、有未推送的 commit、有未提交的變更、乾淨。
3. 程式在背景定期 fetch。有新內容時更新畫面；若進來的變更會碰到你尚未提交的
   檔案，即使關閉通知也一定會提醒。
4. 點選任一 repository：
   - 「同步狀態」顯示即將進來的 commit、會被影響的檔案、兩側都改到的檔案，
     以及建議的處置方式。分岔時會同時畫出 rebase 與 merge 兩種結果的形狀。
   - 「歷史圖」顯示 commit 圖，分支為直線、同一線道同色、分支名稱標在線上。

## Project Structure

- `crates/core`：git 讀取、commit DAG、圖形佈局、狀態與分岔分析。純運算，
  不含介面相依；關閉 `git` feature 後完全沒有第三方相依。
- `crates/app`：Tauri 桌面應用程式。`service` 是資料層，`ui/` 是前端
  （純 HTML/CSS/JS，沒有打包工具）。
- `crates/cli`：命令列工具，用於沒有圖形介面的環境。
- `docs/spec.md`：需求、範圍與成功標準
- `docs/architecture.md`：系統設計、演算法與驗證設計
- `AGENTS.md`：專案規則與工作流程

## Configuration

- 設定檔：JSON，位於系統設定目錄下的 `dev.gitview.desktop/settings.json`
  （Linux 為 `~/.config/`）。內容只有受監控的路徑與檢查偏好。
- SSH 金鑰：由系統既有機制（ssh-agent、git credential helper）管理，
  本程式不儲存也不要求任何憑證。

不得把密碼、token、私鑰、個資或正式資料寫入 repository。

## Known Limitations

- 桌面介面的繪製尚未目視驗證，見上方 Verify。
- 尚未實作基本 Git 操作（提交、分支、stash、衝突解決等），目前只讀不寫。
  清單與分級見 `docs/spec.md`。
- 無法得知另一台設備上尚未推送的內容。這是不使用伺服器的必然結果，屬已知且已接受的取捨（見 `docs/architecture.md` 的設計決策）。
- 檔案層級的重疊偵測只能評估衝突風險，不能保證一定會或不會衝突。
- Windows 版本無法在 Linux 上交叉建置，需要實體 Windows 環境或 CI。
- 不支援 macOS。
- 不包含 GitHub / GitLab 等平台整合。
