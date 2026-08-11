//! 從實際的 git repository 讀出 commit DAG 與基本狀態。
//!
//! 所有對 git 的存取集中在本模組，其餘部分只面對 [`CommitGraph`]，
//! 以保留日後更換底層實作（例如改用 gitoxide）的空間。
//!
//! 本模組只讀取，不寫入。任何會改動使用者 repository 的操作不放在這裡。

use anyhow::{Context, Result};
use git2::{Repository, Sort};

use crate::dag::{CommitGraph, CommitGraphBuilder};

/// Repository 的概況，供總覽畫面使用。
#[derive(Debug, Clone)]
pub struct RepoSummary {
    pub path: String,
    /// 目前的分支名稱；detached HEAD 或空 repository 時為 `None`。
    pub branch: Option<String>,
    pub commit_count: usize,
    pub merge_count: usize,
    /// 工作目錄中有異動的檔案數（含未追蹤檔案）。
    pub dirty_files: usize,
    /// repository 是否處於未完成的操作中（合併、rebase 等）。
    pub operation_in_progress: bool,
}

/// 讀出 repository 中所有 ref 可達的 commit。
///
/// 走訪順序不影響結果：佈局階段會自行排序，此處只負責把資料讀進來。
pub fn load_graph(repo: &Repository) -> Result<CommitGraph> {
    let mut walk = repo.revwalk().context("無法建立 revwalk")?;
    walk.push_glob("refs/*")
        .context("無法將 refs 加入走訪範圍")?;
    // HEAD 可能指向未被任何 ref 涵蓋的位置（detached HEAD）。
    if repo.head().is_ok() {
        walk.push_head().context("無法將 HEAD 加入走訪範圍")?;
    }
    walk.set_sorting(Sort::TOPOLOGICAL)
        .context("無法設定走訪順序")?;

    let mut builder = CommitGraphBuilder::new();
    for oid in walk {
        let oid = oid.context("走訪 commit 時發生錯誤")?;
        let commit = match repo.find_commit(oid) {
            Ok(commit) => commit,
            // 過濾過的 clone 可能參照到本機沒有的物件；略過而非中止。
            Err(_) => continue,
        };
        let parents = commit.parent_ids().map(|id| id.to_string()).collect();
        // 作者名稱與訊息可能不是 UTF-8：長期存在的 repository 一定會有這種資料。
        // git2 的這些存取子在非 UTF-8 時回傳 Err，改由位元組版本以有損轉換取代，
        // 確保讀取不會因為個別 commit 的編碼而中止。
        let signature = commit.author();
        let author = match signature.name() {
            Ok(name) => name.to_owned(),
            Err(_) => String::from_utf8_lossy(signature.name_bytes()).into_owned(),
        };
        let summary = match commit.summary() {
            Ok(Some(text)) => text.to_owned(),
            // 沒有訊息的 commit 是合法的。
            Ok(None) => String::new(),
            Err(_) => {
                String::from_utf8_lossy(commit.summary_bytes().unwrap_or_default()).into_owned()
            }
        };

        builder.push(
            oid.to_string(),
            parents,
            commit.time().seconds(),
            author,
            summary,
        );
    }

    Ok(builder.build())
}

/// 讀出 repository 的概況。
pub fn summarize(repo: &Repository, graph: &CommitGraph) -> Result<RepoSummary> {
    let path = repo
        .workdir()
        .unwrap_or_else(|| repo.path())
        .to_string_lossy()
        .into_owned();

    let branch = match repo.head() {
        Ok(head) if head.is_branch() => Some(match head.shorthand() {
            Ok(name) => name.to_owned(),
            Err(_) => String::from_utf8_lossy(head.shorthand_bytes()).into_owned(),
        }),
        // detached HEAD、空 repository，或 HEAD 讀取失敗。
        _ => None,
    };

    // 裸 repository 沒有工作目錄，狀態查詢會失敗；此時視為沒有異動檔案。
    let dirty_files = if repo.is_bare() {
        0
    } else {
        let mut options = git2::StatusOptions::new();
        options.include_untracked(true).include_ignored(false);
        repo.statuses(Some(&mut options))
            .context("無法讀取工作目錄狀態")?
            .len()
    };

    let operation_in_progress = repo.state() != git2::RepositoryState::Clean;

    Ok(RepoSummary {
        path,
        branch,
        commit_count: graph.len(),
        merge_count: graph.merge_count(),
        dirty_files,
        operation_in_progress,
    })
}
