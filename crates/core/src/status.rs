//! Repository 的狀態摘要，供多 repo 總覽使用。
//!
//! 只讀取，不修改。所有數值都必須能對應到使用者用 git 指令看得到的結果。

use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};
use git2::{Repository, RepositoryState, StatusOptions};

/// 一個 repository 需要使用者注意的程度。
///
/// 總覽會依此排序：需要動作的排在前面。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Attention {
    /// 沒有待處理事項。
    Clean,
    /// 有本機變更尚未提交。
    Uncommitted,
    /// 有已提交但尚未推送的內容。
    Unpushed,
    /// 遠端有新內容可以拉取。
    Incoming,
    /// 本機與遠端已分岔。
    Diverged,
    /// 有未完成的操作（合併、rebase 等），或無法取得狀態。
    NeedsAttention,
}

impl Attention {
    /// 給介面用的穩定識別字串。
    pub fn as_str(self) -> &'static str {
        match self {
            Attention::Clean => "clean",
            Attention::Uncommitted => "uncommitted",
            Attention::Unpushed => "unpushed",
            Attention::Incoming => "incoming",
            Attention::Diverged => "diverged",
            Attention::NeedsAttention => "attention",
        }
    }
}

/// 工作目錄的變更計數。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkingTree {
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
}

impl WorkingTree {
    pub fn total(&self) -> usize {
        self.staged + self.unstaged + self.untracked + self.conflicted
    }

    pub fn is_clean(&self) -> bool {
        self.total() == 0
    }
}

/// 單一 repository 的完整狀態。
#[derive(Debug, Clone)]
pub struct RepoStatus {
    pub path: String,
    pub name: String,
    pub branch: Option<String>,
    /// 追蹤中的遠端分支簡稱，例如 `origin/main`。
    pub upstream: Option<String>,
    /// 本機領先遠端的 commit 數。
    pub ahead: usize,
    /// 遠端領先本機的 commit 數。
    pub behind: usize,
    pub working_tree: WorkingTree,
    /// 未完成的操作名稱；沒有時為 `None`。
    pub operation: Option<String>,
    /// 最後一次成功 fetch 的時間，取自 `FETCH_HEAD` 的修改時間。
    pub last_fetch: Option<SystemTime>,
    pub attention: Attention,
}

impl RepoStatus {
    /// 是否已分岔：兩側都有對方沒有的 commit。
    pub fn is_diverged(&self) -> bool {
        self.ahead > 0 && self.behind > 0
    }
}

fn describe_state(state: RepositoryState) -> Option<String> {
    let label = match state {
        RepositoryState::Clean => return None,
        RepositoryState::Merge => "合併中",
        RepositoryState::Revert | RepositoryState::RevertSequence => "還原中",
        RepositoryState::CherryPick | RepositoryState::CherryPickSequence => "揀選中",
        RepositoryState::Bisect => "二分搜尋中",
        RepositoryState::Rebase
        | RepositoryState::RebaseInteractive
        | RepositoryState::RebaseMerge => "rebase 中",
        RepositoryState::ApplyMailbox | RepositoryState::ApplyMailboxOrRebase => "套用 patch 中",
    };
    Some(label.to_owned())
}

fn working_tree_counts(repo: &Repository) -> Result<WorkingTree> {
    if repo.is_bare() {
        return Ok(WorkingTree::default());
    }
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .include_ignored(false)
        .renames_head_to_index(true);

    let statuses = repo
        .statuses(Some(&mut options))
        .context("無法讀取工作目錄狀態")?;

    let mut counts = WorkingTree::default();
    for entry in statuses.iter() {
        let flags = entry.status();
        if flags.is_conflicted() {
            counts.conflicted += 1;
        } else if flags.is_wt_new() {
            counts.untracked += 1;
        } else {
            // 一個檔案可能同時有已暫存與未暫存的變更，兩邊都要計入。
            if flags.is_index_new()
                || flags.is_index_modified()
                || flags.is_index_deleted()
                || flags.is_index_renamed()
                || flags.is_index_typechange()
            {
                counts.staged += 1;
            }
            if flags.is_wt_modified()
                || flags.is_wt_deleted()
                || flags.is_wt_renamed()
                || flags.is_wt_typechange()
            {
                counts.unstaged += 1;
            }
        }
    }
    Ok(counts)
}

/// 讀取 `FETCH_HEAD` 的修改時間作為最後 fetch 時間。
///
/// 這是 git 自己會更新的檔案，因此即使使用者是在別處用 git 指令 fetch，
/// 這個時間也會正確反映。
fn last_fetch_time(repo: &Repository) -> Option<SystemTime> {
    let path = repo.path().join("FETCH_HEAD");
    std::fs::metadata(path).ok()?.modified().ok()
}

fn repo_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

fn classify(
    ahead: usize,
    behind: usize,
    working_tree: &WorkingTree,
    operation: &Option<String>,
) -> Attention {
    if operation.is_some() || working_tree.conflicted > 0 {
        return Attention::NeedsAttention;
    }
    if ahead > 0 && behind > 0 {
        return Attention::Diverged;
    }
    if behind > 0 {
        return Attention::Incoming;
    }
    if ahead > 0 {
        return Attention::Unpushed;
    }
    if !working_tree.is_clean() {
        return Attention::Uncommitted;
    }
    Attention::Clean
}

/// 讀出 repository 的完整狀態。
pub fn status(repo: &Repository) -> Result<RepoStatus> {
    let path = repo
        .workdir()
        .unwrap_or_else(|| repo.path())
        .to_string_lossy()
        .trim_end_matches('/')
        .to_owned();

    let head = repo.head().ok();
    let branch = head.as_ref().filter(|head| head.is_branch()).map(|head| {
        head.shorthand()
            .map(str::to_owned)
            .unwrap_or_else(|_| String::from_utf8_lossy(head.shorthand_bytes()).into_owned())
    });

    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;

    if let (Some(head_ref), Some(branch_name)) = (head.as_ref(), branch.as_deref()) {
        if let Ok(local) = repo.find_branch(branch_name, git2::BranchType::Local) {
            if let Ok(tracked) = local.upstream() {
                upstream = tracked.name().ok().flatten().map(str::to_owned);
                if let (Some(local_oid), Some(upstream_oid)) =
                    (head_ref.target(), tracked.get().target())
                {
                    // graph_ahead_behind 回傳 (本機獨有, 遠端獨有)。
                    if let Ok((local_only, remote_only)) =
                        repo.graph_ahead_behind(local_oid, upstream_oid)
                    {
                        ahead = local_only;
                        behind = remote_only;
                    }
                }
            }
        }
    }

    let working_tree = working_tree_counts(repo)?;
    let operation = describe_state(repo.state());
    let attention = classify(ahead, behind, &working_tree, &operation);

    Ok(RepoStatus {
        name: repo_name(&path),
        path,
        branch,
        upstream,
        ahead,
        behind,
        working_tree,
        operation,
        last_fetch: last_fetch_time(repo),
        attention,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_prioritises_unfinished_operations() {
        let dirty = WorkingTree {
            staged: 1,
            ..WorkingTree::default()
        };
        assert_eq!(
            classify(3, 2, &dirty, &Some("rebase 中".to_owned())),
            Attention::NeedsAttention
        );
    }

    #[test]
    fn divergence_outranks_single_sided_differences() {
        let clean = WorkingTree::default();
        assert_eq!(classify(1, 1, &clean, &None), Attention::Diverged);
        assert_eq!(classify(0, 1, &clean, &None), Attention::Incoming);
        assert_eq!(classify(1, 0, &clean, &None), Attention::Unpushed);
    }

    #[test]
    fn clean_repository_needs_no_attention() {
        assert_eq!(
            classify(0, 0, &WorkingTree::default(), &None),
            Attention::Clean
        );
    }

    #[test]
    fn uncommitted_changes_are_reported_when_synchronised() {
        let dirty = WorkingTree {
            untracked: 2,
            ..WorkingTree::default()
        };
        assert_eq!(classify(0, 0, &dirty, &None), Attention::Uncommitted);
    }

    #[test]
    fn conflicts_always_need_attention() {
        let conflicted = WorkingTree {
            conflicted: 1,
            ..WorkingTree::default()
        };
        assert_eq!(
            classify(0, 0, &conflicted, &None),
            Attention::NeedsAttention
        );
    }

    #[test]
    fn working_tree_totals_every_category() {
        let tree = WorkingTree {
            staged: 1,
            unstaged: 2,
            untracked: 3,
            conflicted: 4,
        };
        assert_eq!(tree.total(), 10);
        assert!(!tree.is_clean());
        assert!(WorkingTree::default().is_clean());
    }

    #[test]
    fn repository_name_comes_from_the_final_path_segment() {
        assert_eq!(repo_name("/srv/projects/gitview"), "gitview");
        assert_eq!(repo_name("gitview"), "gitview");
    }
}
