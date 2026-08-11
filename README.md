# gitview

本機端的 Git 視覺化桌面應用程式。與現有工具的差異在於呈現的重點：不只畫「已經發生的歷史」，而是畫「即將進來的變更」與「你接下來的操作會造成什麼結果」。

主要針對的情境是一個人同時管理多個 repository，並在自己的多台設備之間切換工作。

> 狀態：規劃中

## Overview

- 要解決的問題：多 repository 狀態難以掌握、遠端有更新不會主動知道、本機與遠端分岔後不知道怎麼處理才安全。
- 方法摘要：常駐系統列的本機應用程式，背景定期 fetch，在執行操作前先分析並預覽結果。不使用伺服器，不需要帳號，資料不離開本機。
- 目前結論：待確認。尚無實作。

詳細範圍與成功標準見 `docs/spec.md`；實作、演算法與驗證設計見 `docs/architecture.md`。

## Requirements

- Runtime / language：Rust（版本待確認）+ Tauri 2。前端框架待確認。
- Package manager / build tool：Cargo；前端建置工具待確認。
- External services：無。透過 SSH 存取使用者既有的 git 遠端，認證委由系統的 SSH agent。

## Setup

Rust 工具鏈已安裝（1.97.1）。其餘系統依賴尚缺，安裝需要管理者權限：

```sh
sudo apt-get install -y build-essential pkg-config libssl-dev cmake \
    libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
    librsvg2-dev file wget nodejs npm
```

`build-essential` 是必要的：`git2` 需要編譯 libgit2 的 C 原始碼。

## Run

```text
待確認（預期為 cargo tauri dev）
```

命令列工具 `gitview` 已實作但尚未建置，同樣受限於缺少 C 編譯器。

## Verify

```sh
cargo fmt --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

上列指令需要 C 編譯器。在尚未安裝的環境下，可單獨驗證不含 git 綁定的
純運算部分（`gitview-core` 的 `dag` 與 `layout`）：

```sh
LLD=$(ls ~/.rustup/toolchains/*/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld | head -1)
export RUSTFLAGS="-C linker-flavor=ld.lld -C linker=$LLD -C link-self-contained=yes -C target-feature=+crt-static"
cargo test -p gitview-core --no-default-features --target x86_64-unknown-linux-musl
```

目前狀態：純運算部分 16 項單元測試通過，clippy 與 fmt 通過。
需要 C 編譯器的部分（`repo` 模組與命令列工具）尚未建置或測試。

## Usage

尚無可執行的程式。規劃中的使用流程：

1. 加入要監控的 repository。
2. 程式在背景定期檢查遠端更新，有變化時通知。
3. 執行 pull 之前先檢視即將進來的變更與可能的衝突；分岔時檢視不同處置方式的結果後再決定。

## Project Structure

- `docs/spec.md`：需求、範圍與成功標準
- `docs/architecture.md`：系統設計、演算法與驗證設計
- `AGENTS.md`：專案規則與工作流程
- `HANDOFF.md`：未完成工作的交接摘要

尚無原始碼目錄。

## Configuration

- 設定檔格式與存放位置：待確認。
- SSH 金鑰：由系統既有機制（ssh-agent 等）管理，本程式不儲存金鑰或密語。

不得把密碼、token、私鑰、個資或正式資料寫入 repository。

## Known Limitations

- 尚無任何實作。
- 無法得知另一台設備上尚未推送的內容。這是不使用伺服器的必然結果，屬已知且已接受的取捨（見 `docs/architecture.md` 的設計決策）。
- 檔案層級的重疊偵測只能評估衝突風險，不能保證一定會或不會衝突。
- Windows 版本無法在 Linux 上交叉建置，需要實體 Windows 環境或 CI。
- 不支援 macOS。
- 不包含 GitHub / GitLab 等平台整合。
