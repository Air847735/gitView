//! 傳給前端的資料結構。
//!
//! 核心運算層刻意不依賴 serde；這裡負責把領域型別轉換成介面需要的形狀。
//! 分成兩層的好處是介面要什麼欄位不會反過來影響核心的設計。

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use gitview_core::divergence::{CommitInfo, Divergence};
use gitview_core::status::RepoStatus;
use gitview_core::{CommitGraph, Layout};
use serde::Serialize;

/// 線道顏色的數量。實際色碼由前端決定，這裡只給穩定的索引。
pub const LANE_COLOURS: usize = 8;

fn to_millis(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_millis() as u64)
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkingTreeDto {
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
    pub total: usize,
}

/// 上一次背景 fetch 的結果。
#[derive(Debug, Clone, Serialize)]
pub struct FetchStateDto {
    /// 成功時為 `true`。
    pub ok: bool,
    /// 給使用者看的一句話。
    pub message: String,
    /// 失敗類別的穩定識別字串；成功時為 `null`。
    pub kind: Option<String>,
    pub at_millis: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoStatusDto {
    pub path: String,
    pub name: String,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub working_tree: WorkingTreeDto,
    pub operation: Option<String>,
    pub attention: String,
    pub last_fetch_millis: Option<u64>,
    /// 讀取狀態時發生的錯誤；正常時為 `null`。
    pub error: Option<String>,
    pub fetch_state: Option<FetchStateDto>,
}

impl RepoStatusDto {
    pub fn from_status(status: &RepoStatus, fetch_state: Option<FetchStateDto>) -> Self {
        Self {
            path: status.path.clone(),
            name: status.name.clone(),
            branch: status.branch.clone(),
            upstream: status.upstream.clone(),
            ahead: status.ahead,
            behind: status.behind,
            working_tree: WorkingTreeDto {
                staged: status.working_tree.staged,
                unstaged: status.working_tree.unstaged,
                untracked: status.working_tree.untracked,
                conflicted: status.working_tree.conflicted,
                total: status.working_tree.total(),
            },
            operation: status.operation.clone(),
            attention: status.attention.as_str().to_owned(),
            last_fetch_millis: status.last_fetch.and_then(to_millis),
            error: None,
            fetch_state,
        }
    }

    /// 無法讀取時的佔位項目，讓總覽仍能顯示這個 repository 並說明原因。
    pub fn unreadable(path: &str, error: String) -> Self {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_owned());
        Self {
            path: path.to_owned(),
            name,
            branch: None,
            upstream: None,
            ahead: 0,
            behind: 0,
            working_tree: WorkingTreeDto {
                staged: 0,
                unstaged: 0,
                untracked: 0,
                conflicted: 0,
                total: 0,
            },
            operation: None,
            attention: "attention".to_owned(),
            last_fetch_millis: None,
            error: Some(error),
            fetch_state: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitDto {
    pub oid: String,
    pub short_oid: String,
    pub summary: String,
    pub author: String,
    pub timestamp: i64,
    pub is_merge: bool,
    pub row: usize,
    pub lane: usize,
    /// 線道顏色索引，範圍 `0..LANE_COLOURS`。
    pub colour: usize,
    /// 指向此 commit 的 ref 名稱。
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EdgeDto {
    pub child_row: usize,
    pub child_lane: usize,
    pub parent_row: usize,
    pub parent_lane: usize,
    pub colour: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphDto {
    pub commits: Vec<CommitDto>,
    pub edges: Vec<EdgeDto>,
    pub lane_count: usize,
    /// repository 中的 commit 總數，可能大於 `commits` 的長度。
    pub total_commits: usize,
    pub truncated: bool,
}

impl GraphDto {
    /// 取佈局結果的前 `limit` 列轉為介面資料。
    ///
    /// 邊只保留兩端都在範圍內的部分，避免前端畫到看不見的座標。
    pub fn from_layout(
        graph: &CommitGraph,
        layout: &Layout,
        labels: &HashMap<String, Vec<String>>,
        limit: usize,
    ) -> Self {
        let shown = limit.min(layout.rows());
        let mut commits = Vec::with_capacity(shown);

        for (row, &node) in layout.order.iter().take(shown).enumerate() {
            let commit = graph.commit(node);
            let lane = layout.lane_of[node];
            commits.push(CommitDto {
                short_oid: commit.oid.chars().take(8).collect(),
                oid: commit.oid.clone(),
                summary: commit.summary.clone(),
                author: commit.author.clone(),
                timestamp: commit.timestamp,
                is_merge: commit.is_merge(),
                row,
                lane,
                colour: lane % LANE_COLOURS,
                refs: labels.get(&commit.oid).cloned().unwrap_or_default(),
            });
        }

        let edges = layout
            .edges
            .iter()
            .filter(|edge| edge.child_row < shown && edge.parent_row < shown)
            .map(|edge| EdgeDto {
                child_row: edge.child_row,
                child_lane: edge.child_lane,
                parent_row: edge.parent_row,
                parent_lane: edge.parent_lane,
                // 線的顏色跟著它所屬的線道，分支才會整條同色。
                colour: edge.parent_lane % LANE_COLOURS,
            })
            .collect();

        Self {
            commits,
            edges,
            lane_count: layout.lane_count,
            total_commits: layout.rows(),
            truncated: layout.rows() > shown,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitSummaryDto {
    pub oid: String,
    pub short_oid: String,
    pub summary: String,
    pub author: String,
    pub timestamp: i64,
}

impl From<&CommitInfo> for CommitSummaryDto {
    fn from(info: &CommitInfo) -> Self {
        Self {
            oid: info.oid.clone(),
            short_oid: info.short_oid.clone(),
            summary: info.summary.clone(),
            author: info.author.clone(),
            timestamp: info.timestamp,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DivergenceDto {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: Vec<CommitSummaryDto>,
    pub behind: Vec<CommitSummaryDto>,
    pub local_files: Vec<String>,
    pub incoming_files: Vec<String>,
    pub overlapping_files: Vec<String>,
    pub uncommitted_overlap: Vec<String>,
    pub is_diverged: bool,
    /// 衝突風險：`none` 或 `possible`。
    pub risk: String,
    /// 建議的穩定識別字串。
    pub recommendation: String,
    pub recommendation_headline: String,
    /// rebase 之後主線上的 commit 數。
    pub commits_after_rebase: usize,
    /// merge 之後主線上的 commit 數，含新的合併節點。
    pub commits_after_merge: usize,
}

impl From<&Divergence> for DivergenceDto {
    fn from(divergence: &Divergence) -> Self {
        Self {
            branch: divergence.branch.clone(),
            upstream: divergence.upstream.clone(),
            ahead: divergence
                .ahead
                .iter()
                .map(CommitSummaryDto::from)
                .collect(),
            behind: divergence
                .behind
                .iter()
                .map(CommitSummaryDto::from)
                .collect(),
            local_files: divergence.local_files.clone(),
            incoming_files: divergence.incoming_files.clone(),
            overlapping_files: divergence.overlapping_files.clone(),
            uncommitted_overlap: divergence.uncommitted_overlap.clone(),
            is_diverged: divergence.is_diverged(),
            risk: match divergence.risk() {
                gitview_core::ConflictRisk::None => "none".to_owned(),
                gitview_core::ConflictRisk::Possible => "possible".to_owned(),
            },
            recommendation: divergence.recommendation.as_str().to_owned(),
            recommendation_headline: divergence.recommendation.headline().to_owned(),
            commits_after_rebase: divergence.commits_after_rebase(),
            commits_after_merge: divergence.commits_after_merge(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitview_core::{lay_out, CommitGraphBuilder};

    #[test]
    fn graph_truncation_drops_edges_that_leave_the_window() {
        let mut builder = CommitGraphBuilder::new();
        builder.push("c", vec!["b".to_owned()], 300, "dev", "third");
        builder.push("b", vec!["a".to_owned()], 200, "dev", "second");
        builder.push("a", Vec::new(), 100, "dev", "first");
        let graph = builder.build();
        let layout = lay_out(&graph).unwrap();

        let dto = GraphDto::from_layout(&graph, &layout, &HashMap::new(), 2);
        assert_eq!(dto.commits.len(), 2);
        assert!(dto.truncated);
        assert_eq!(dto.total_commits, 3);
        // c→b 兩端都在範圍內；b→a 的 a 已被截掉。
        assert_eq!(dto.edges.len(), 1);
    }

    #[test]
    fn labels_are_attached_to_the_matching_commit() {
        let mut builder = CommitGraphBuilder::new();
        builder.push("aaa", Vec::new(), 100, "dev", "only");
        let graph = builder.build();
        let layout = lay_out(&graph).unwrap();

        let mut labels = HashMap::new();
        labels.insert("aaa".to_owned(), vec!["main".to_owned()]);

        let dto = GraphDto::from_layout(&graph, &layout, &labels, 10);
        assert_eq!(dto.commits[0].refs, vec!["main".to_owned()]);
        assert!(!dto.truncated);
    }

    #[test]
    fn colours_stay_within_the_palette() {
        let mut builder = CommitGraphBuilder::new();
        for index in 0..30 {
            builder.push(format!("c{index}"), Vec::new(), index as i64, "dev", "x");
        }
        let graph = builder.build();
        let layout = lay_out(&graph).unwrap();
        let dto = GraphDto::from_layout(&graph, &layout, &HashMap::new(), 100);
        assert!(dto
            .commits
            .iter()
            .all(|commit| commit.colour < LANE_COLOURS));
    }

    #[test]
    fn unreadable_repositories_still_produce_an_entry() {
        let dto = RepoStatusDto::unreadable("/srv/projects/missing", "不存在".to_owned());
        assert_eq!(dto.name, "missing");
        assert_eq!(dto.attention, "attention");
        assert_eq!(dto.error.as_deref(), Some("不存在"));
    }
}

/// 操作結果。
#[derive(Debug, Clone, Serialize)]
pub struct OpOutcomeDto {
    pub message: String,
    /// 可還原時為還原點的 ref 名稱。
    pub undo_ref: Option<String>,
}

impl From<gitview_core::ops::OpOutcome> for OpOutcomeDto {
    fn from(outcome: gitview_core::ops::OpOutcome) -> Self {
        Self {
            message: outcome.message,
            undo_ref: outcome.undo.map(|point| point.reference),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SafetyPointDto {
    pub reference: String,
    pub oid: String,
    pub operation: String,
    pub created_unix: u64,
}

impl From<&gitview_core::ops::SafetyPoint> for SafetyPointDto {
    fn from(point: &gitview_core::ops::SafetyPoint) -> Self {
        Self {
            reference: point.reference.clone(),
            oid: point.oid.clone(),
            operation: point.operation.clone(),
            created_unix: point.created_unix,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FileChangeDto {
    pub path: String,
    pub staged: String,
    pub unstaged: String,
    pub is_untracked: bool,
    pub is_conflicted: bool,
}

impl From<&gitview_core::workspace::FileChange> for FileChangeDto {
    fn from(change: &gitview_core::workspace::FileChange) -> Self {
        Self {
            path: change.path.clone(),
            staged: change.staged.to_owned(),
            unstaged: change.unstaged.to_owned(),
            is_untracked: change.is_untracked,
            is_conflicted: change.is_conflicted,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BranchDto {
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    pub upstream: Option<String>,
}

impl From<&gitview_core::workspace::BranchInfo> for BranchDto {
    fn from(branch: &gitview_core::workspace::BranchInfo) -> Self {
        Self {
            name: branch.name.clone(),
            is_head: branch.is_head,
            is_remote: branch.is_remote,
            upstream: branch.upstream.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StashDto {
    pub index: usize,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictSideDto {
    pub text: Option<String>,
    pub exists: bool,
}

impl From<&gitview_core::conflict::ConflictSide> for ConflictSideDto {
    fn from(side: &gitview_core::conflict::ConflictSide) -> Self {
        Self {
            text: side.text.clone(),
            exists: side.exists,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictFileDto {
    pub path: String,
    pub base: ConflictSideDto,
    pub ours: ConflictSideDto,
    pub theirs: ConflictSideDto,
    pub merged: Option<String>,
    pub is_binary: bool,
}

impl From<&gitview_core::conflict::ConflictFile> for ConflictFileDto {
    fn from(file: &gitview_core::conflict::ConflictFile) -> Self {
        Self {
            path: file.path.clone(),
            base: ConflictSideDto::from(&file.base),
            ours: ConflictSideDto::from(&file.ours),
            theirs: ConflictSideDto::from(&file.theirs),
            merged: file.merged.clone(),
            is_binary: file.is_binary,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SideLabelsDto {
    pub ours: String,
    pub theirs: String,
    pub note: String,
}

impl From<gitview_core::conflict::SideLabels> for SideLabelsDto {
    fn from(labels: gitview_core::conflict::SideLabels) -> Self {
        Self {
            ours: labels.ours,
            theirs: labels.theirs,
            note: labels.note,
        }
    }
}

/// 單一 repository 的工作區狀態，一次取回介面需要的全部資料。
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceDto {
    pub changes: Vec<FileChangeDto>,
    pub branches: Vec<BranchDto>,
    pub stashes: Vec<StashDto>,
    pub conflicts: Vec<ConflictFileDto>,
    /// 進行中的操作名稱，例如 `rebase 中`；沒有時為 `null`。
    pub operation: Option<String>,
    pub undo_points: Vec<SafetyPointDto>,
    /// 衝突兩側在目前操作下的實際意義。
    pub side_labels: SideLabelsDto,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpanDto {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffLineDto {
    pub kind: String,
    pub content: String,
    pub old_lineno: Option<u32>,
    pub new_lineno: Option<u32>,
    pub spans: Vec<SpanDto>,
    pub whitespace_only: bool,
    /// 配對的另一行在同一個 hunk 中的索引；選取時必須一起處理。
    pub pair: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HunkDto {
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<DiffLineDto>,
    pub whitespace_only: bool,
    /// 這一段會與即將進來的遠端變更相撞。
    pub collides_with_incoming: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDiffDto {
    pub path: String,
    pub old_path: Option<String>,
    pub hunks: Vec<HunkDto>,
    pub is_binary: bool,
    pub added: usize,
    pub removed: usize,
}

impl From<&gitview_core::diff::FileDiff> for FileDiffDto {
    fn from(file: &gitview_core::diff::FileDiff) -> Self {
        Self {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            is_binary: file.is_binary,
            added: file.added,
            removed: file.removed,
            hunks: file
                .hunks
                .iter()
                .map(|hunk| HunkDto {
                    header: hunk.header.clone(),
                    old_start: hunk.old_start,
                    new_start: hunk.new_start,
                    whitespace_only: hunk.whitespace_only(),
                    collides_with_incoming: hunk.collides_with_incoming,
                    lines: hunk
                        .lines
                        .iter()
                        .map(|line| DiffLineDto {
                            kind: line.kind.as_str().to_owned(),
                            content: line.content.clone(),
                            old_lineno: line.old_lineno,
                            new_lineno: line.new_lineno,
                            spans: line
                                .spans
                                .iter()
                                .map(|span| SpanDto {
                                    start: span.start,
                                    end: span.end,
                                })
                                .collect(),
                            whitespace_only: line.whitespace_only,
                            pair: line.pair,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

/// 圖上點選某個 commit 之後顯示的內容。
#[derive(Debug, Clone, Serialize)]
pub struct CommitDetailDto {
    pub oid: String,
    pub short_oid: String,
    pub summary: String,
    pub body: String,
    pub author: String,
    pub email: String,
    pub timestamp: i64,
    pub parents: Vec<String>,
    pub files: Vec<FileDiffDto>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHitDto {
    pub oid: String,
    pub short_oid: String,
    pub summary: String,
    pub author: String,
    pub timestamp: i64,
    pub matched: Vec<String>,
    pub paths: Vec<String>,
}

impl From<&gitview_core::search::SearchHit> for SearchHitDto {
    fn from(hit: &gitview_core::search::SearchHit) -> Self {
        Self {
            oid: hit.oid.clone(),
            short_oid: hit.short_oid.clone(),
            summary: hit.summary.clone(),
            author: hit.author.clone(),
            timestamp: hit.timestamp,
            matched: hit.matched.iter().map(|m| (*m).to_owned()).collect(),
            paths: hit.paths.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BlameLineDto {
    pub line_number: usize,
    pub content: String,
    pub oid: String,
    pub short_oid: String,
    pub author: String,
    pub summary: String,
    pub timestamp: i64,
    pub same_as_previous: bool,
}

impl From<&gitview_core::search::BlameLine> for BlameLineDto {
    fn from(line: &gitview_core::search::BlameLine) -> Self {
        Self {
            line_number: line.line_number,
            content: line.content.clone(),
            oid: line.oid.clone(),
            short_oid: line.short_oid.clone(),
            author: line.author.clone(),
            summary: line.summary.clone(),
            timestamp: line.timestamp,
            same_as_previous: line.same_as_previous,
        }
    }
}
