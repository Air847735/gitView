//! 分岔分析：在執行任何操作之前，算出會發生什麼、安不安全。
//!
//! 這是本專案的主要差異點。現有工具在分岔時只提供 merge / rebase 兩個
//! 按鈕，使用者無從得知按下去的後果；此模組先把兩側的變更範圍算出來，
//! 判斷是否可能衝突，並據此給出建議。
//!
//! 只讀取，不修改任何內容。

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use git2::{Commit, Diff, DiffOptions, Oid, Repository, StatusOptions};

/// 供介面顯示的單一 commit 資訊。
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub oid: String,
    pub short_oid: String,
    pub summary: String,
    pub author: String,
    pub timestamp: i64,
}

/// 檔案層級的衝突風險評估。
///
/// 檔案沒有重疊時可以確定不會衝突；有重疊時只能說「可能」，
/// 因為 git 是以區塊為單位合併，同檔案的不同區塊未必衝突。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictRisk {
    /// 兩側沒有改到同一個檔案。
    None,
    /// 兩側改到同一個檔案，可能衝突。
    Possible,
}

/// 對使用者的建議處置方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recommendation {
    /// 沒有設定追蹤的遠端分支。
    NoUpstream,
    /// 與遠端一致。
    UpToDate,
    /// 只有本機領先，推送即可。
    Push,
    /// 只有遠端領先，可以直接快轉，不會產生新的 commit。
    FastForward,
    /// 已分岔，建議 rebase。
    Rebase(ConflictRisk),
    /// 已分岔，但工作目錄有未提交且會被影響的變更，應先處理。
    ResolveWorkingTreeFirst,
}

impl Recommendation {
    /// 給介面用的穩定識別字串。
    pub fn as_str(&self) -> &'static str {
        match self {
            Recommendation::NoUpstream => "no-upstream",
            Recommendation::UpToDate => "up-to-date",
            Recommendation::Push => "push",
            Recommendation::FastForward => "fast-forward",
            Recommendation::Rebase(_) => "rebase",
            Recommendation::ResolveWorkingTreeFirst => "resolve-working-tree",
        }
    }

    /// 一句話的建議。
    pub fn headline(&self) -> &'static str {
        match self {
            Recommendation::NoUpstream => "沒有追蹤的遠端分支",
            Recommendation::UpToDate => "已與遠端一致",
            Recommendation::Push => "推送即可",
            Recommendation::FastForward => "可以直接快轉",
            Recommendation::Rebase(ConflictRisk::None) => "建議 rebase，不會衝突",
            Recommendation::Rebase(ConflictRisk::Possible) => "建議 rebase，但可能需要解衝突",
            Recommendation::ResolveWorkingTreeFirst => "請先處理未提交的變更",
        }
    }
}

/// 分岔分析的完整結果。
#[derive(Debug, Clone)]
pub struct Divergence {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    /// 本機獨有的 commit，由新到舊。
    pub ahead: Vec<CommitInfo>,
    /// 遠端獨有的 commit，由新到舊。
    pub behind: Vec<CommitInfo>,
    /// 本機獨有的 commit 改動到的檔案。
    pub local_files: Vec<String>,
    /// 即將進來的變更改動到的檔案。
    pub incoming_files: Vec<String>,
    /// 兩側都改到的檔案。
    pub overlapping_files: Vec<String>,
    /// 即將進來的變更會碰到、但本機尚未提交的檔案。
    pub uncommitted_overlap: Vec<String>,
    pub recommendation: Recommendation,
}

impl Divergence {
    pub fn is_diverged(&self) -> bool {
        !self.ahead.is_empty() && !self.behind.is_empty()
    }

    /// 檔案層級的衝突風險。
    pub fn risk(&self) -> ConflictRisk {
        if self.overlapping_files.is_empty() {
            ConflictRisk::None
        } else {
            ConflictRisk::Possible
        }
    }

    /// rebase 之後主線上的 commit 數量。
    ///
    /// 本機的 commit 會被重新套用到遠端內容之後，歷史維持單線。
    pub fn commits_after_rebase(&self) -> usize {
        self.behind.len() + self.ahead.len()
    }

    /// merge 之後主線上的 commit 數量，含新產生的合併節點。
    pub fn commits_after_merge(&self) -> usize {
        self.behind.len() + self.ahead.len() + 1
    }
}

fn commit_info(commit: &Commit<'_>) -> CommitInfo {
    let signature = commit.author();
    let author = match signature.name() {
        Ok(name) => name.to_owned(),
        Err(_) => String::from_utf8_lossy(signature.name_bytes()).into_owned(),
    };
    let summary = match commit.summary() {
        Ok(Some(text)) => text.to_owned(),
        Ok(None) => String::new(),
        Err(_) => String::from_utf8_lossy(commit.summary_bytes().unwrap_or_default()).into_owned(),
    };
    let oid = commit.id().to_string();
    CommitInfo {
        short_oid: oid.chars().take(8).collect(),
        oid,
        summary,
        author,
        timestamp: commit.time().seconds(),
    }
}

/// 列出 `from` 可達、但 `exclude` 不可達的 commit，由新到舊。
fn commits_between(repo: &Repository, from: Oid, exclude: Oid) -> Result<Vec<CommitInfo>> {
    let mut walk = repo.revwalk().context("無法建立 revwalk")?;
    walk.push(from).context("無法設定走訪起點")?;
    walk.hide(exclude).context("無法設定走訪排除點")?;

    let mut commits = Vec::new();
    for oid in walk {
        let oid = oid.context("走訪 commit 時發生錯誤")?;
        if let Ok(commit) = repo.find_commit(oid) {
            commits.push(commit_info(&commit));
        }
    }
    Ok(commits)
}

fn diff_paths(diff: &Diff<'_>, into: &mut BTreeSet<String>) {
    for delta in diff.deltas() {
        // 改名會同時有新舊路徑，兩者都要納入比對。
        for file in [delta.old_file(), delta.new_file()] {
            if let Some(path) = file.path() {
                into.insert(path.to_string_lossy().into_owned());
            }
        }
    }
}

/// 一組 commit 總共改動到的檔案。
///
/// 合併節點與其第一個父節點比較，這樣得到的是該合併「帶進主線」的變更。
fn changed_files(repo: &Repository, commits: &[CommitInfo]) -> Result<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    let mut options = DiffOptions::new();

    for info in commits {
        let oid = Oid::from_str(&info.oid).context("commit 識別碼格式錯誤")?;
        let commit = match repo.find_commit(oid) {
            Ok(commit) => commit,
            Err(_) => continue,
        };
        let tree = commit.tree().context("無法讀取 commit 的 tree")?;
        let parent_tree = match commit.parent(0) {
            Ok(parent) => Some(parent.tree().context("無法讀取父節點的 tree")?),
            // 根節點沒有父節點，與空樹比較。
            Err(_) => None,
        };
        let diff = repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options))
            .context("無法比較 tree")?;
        diff_paths(&diff, &mut paths);
    }
    Ok(paths)
}

/// 工作目錄中已被修改、但尚未提交的檔案路徑。
fn uncommitted_paths(repo: &Repository) -> Result<BTreeSet<String>> {
    if repo.is_bare() {
        return Ok(BTreeSet::new());
    }
    let mut options = StatusOptions::new();
    options.include_untracked(true).include_ignored(false);
    let statuses = repo
        .statuses(Some(&mut options))
        .context("無法讀取工作目錄狀態")?;

    let mut paths = BTreeSet::new();
    for entry in statuses.iter() {
        // 路徑不是 UTF-8 時以有損轉換保留，總比整個略過好。
        match entry.path() {
            Ok(path) => {
                paths.insert(path.to_owned());
            }
            Err(_) => {
                paths.insert(String::from_utf8_lossy(entry.path_bytes()).into_owned());
            }
        }
    }
    Ok(paths)
}

fn recommend(
    ahead: usize,
    behind: usize,
    has_upstream: bool,
    risk: ConflictRisk,
    uncommitted_overlap: bool,
) -> Recommendation {
    if !has_upstream {
        return Recommendation::NoUpstream;
    }
    // 未提交的變更會被即將進來的內容影響時，任何處置都可能導致工作遺失。
    if behind > 0 && uncommitted_overlap {
        return Recommendation::ResolveWorkingTreeFirst;
    }
    match (ahead, behind) {
        (0, 0) => Recommendation::UpToDate,
        (_, 0) => Recommendation::Push,
        (0, _) => Recommendation::FastForward,
        // 已分岔。本機的 commit 尚未被任何人取得，重新套用到遠端之後
        // 歷史維持單線，因此建議 rebase 而非 merge。
        _ => Recommendation::Rebase(risk),
    }
}

/// 分析目前分支與其追蹤的遠端分支之間的差異。
pub fn analyse(repo: &Repository) -> Result<Divergence> {
    let head = repo.head().ok();
    let branch = head.as_ref().filter(|head| head.is_branch()).map(|head| {
        head.shorthand()
            .map(str::to_owned)
            .unwrap_or_else(|_| String::from_utf8_lossy(head.shorthand_bytes()).into_owned())
    });

    let mut upstream_name = None;
    let mut ahead = Vec::new();
    let mut behind = Vec::new();

    if let (Some(head_ref), Some(branch_name)) = (head.as_ref(), branch.as_deref()) {
        if let Ok(local) = repo.find_branch(branch_name, git2::BranchType::Local) {
            if let Ok(tracked) = local.upstream() {
                upstream_name = tracked.name().ok().flatten().map(str::to_owned);
                if let (Some(local_oid), Some(upstream_oid)) =
                    (head_ref.target(), tracked.get().target())
                {
                    ahead = commits_between(repo, local_oid, upstream_oid)?;
                    behind = commits_between(repo, upstream_oid, local_oid)?;
                }
            }
        }
    }

    let local_files = changed_files(repo, &ahead)?;
    let incoming_files = changed_files(repo, &behind)?;
    let overlapping: Vec<String> = local_files.intersection(&incoming_files).cloned().collect();

    let dirty = uncommitted_paths(repo)?;
    let uncommitted_overlap: Vec<String> = incoming_files.intersection(&dirty).cloned().collect();

    let risk = if overlapping.is_empty() {
        ConflictRisk::None
    } else {
        ConflictRisk::Possible
    };
    let recommendation = recommend(
        ahead.len(),
        behind.len(),
        upstream_name.is_some(),
        risk,
        !uncommitted_overlap.is_empty(),
    );

    Ok(Divergence {
        branch,
        upstream: upstream_name,
        ahead,
        behind,
        local_files: local_files.into_iter().collect(),
        incoming_files: incoming_files.into_iter().collect(),
        overlapping_files: overlapping,
        uncommitted_overlap,
        recommendation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_upstream_is_reported_before_anything_else() {
        assert_eq!(
            recommend(3, 2, false, ConflictRisk::Possible, true),
            Recommendation::NoUpstream
        );
    }

    #[test]
    fn uncommitted_overlap_blocks_the_operation() {
        assert_eq!(
            recommend(1, 1, true, ConflictRisk::None, true),
            Recommendation::ResolveWorkingTreeFirst
        );
        // 沒有東西要進來時，未提交的變更不構成阻礙。
        assert_eq!(
            recommend(1, 0, true, ConflictRisk::None, true),
            Recommendation::Push
        );
    }

    #[test]
    fn single_sided_differences_have_simple_answers() {
        assert_eq!(
            recommend(0, 0, true, ConflictRisk::None, false),
            Recommendation::UpToDate
        );
        assert_eq!(
            recommend(2, 0, true, ConflictRisk::None, false),
            Recommendation::Push
        );
        assert_eq!(
            recommend(0, 2, true, ConflictRisk::None, false),
            Recommendation::FastForward
        );
    }

    #[test]
    fn divergence_recommends_rebase_and_carries_the_risk() {
        assert_eq!(
            recommend(1, 1, true, ConflictRisk::None, false),
            Recommendation::Rebase(ConflictRisk::None)
        );
        assert_eq!(
            recommend(1, 1, true, ConflictRisk::Possible, false),
            Recommendation::Rebase(ConflictRisk::Possible)
        );
    }

    #[test]
    fn headlines_distinguish_the_two_risk_levels() {
        assert_ne!(
            Recommendation::Rebase(ConflictRisk::None).headline(),
            Recommendation::Rebase(ConflictRisk::Possible).headline()
        );
    }

    #[test]
    fn resulting_commit_counts_account_for_the_merge_node() {
        let divergence = Divergence {
            branch: None,
            upstream: None,
            ahead: vec![sample("a"), sample("b")],
            behind: vec![sample("c")],
            local_files: Vec::new(),
            incoming_files: Vec::new(),
            overlapping_files: Vec::new(),
            uncommitted_overlap: Vec::new(),
            recommendation: Recommendation::UpToDate,
        };
        assert!(divergence.is_diverged());
        assert_eq!(divergence.commits_after_rebase(), 3);
        assert_eq!(divergence.commits_after_merge(), 4);
        assert_eq!(divergence.risk(), ConflictRisk::None);
    }

    fn sample(oid: &str) -> CommitInfo {
        CommitInfo {
            oid: oid.to_owned(),
            short_oid: oid.to_owned(),
            summary: String::new(),
            author: String::new(),
            timestamp: 0,
        }
    }
}
