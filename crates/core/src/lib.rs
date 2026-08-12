//! gitview 的核心運算：讀取 git 資料、建立 commit DAG、計算圖形佈局。
//!
//! 本 crate 不含任何介面相依，所有輸出皆為純資料，
//! 以便命令列工具與桌面應用程式共用同一份實作。

pub mod dag;
pub mod layout;

#[cfg(feature = "git")]
pub mod divergence;
#[cfg(feature = "git")]
pub mod fetch;
#[cfg(feature = "git")]
pub mod repo;
#[cfg(feature = "git")]
pub mod status;

pub use dag::{Commit, CommitGraph, CommitGraphBuilder, NodeIndex};
pub use layout::{assign_lanes, lay_out, topo_time_order, Edge, Layout, LayoutError};

#[cfg(feature = "git")]
pub use divergence::{analyse, ConflictRisk, Divergence, Recommendation};
#[cfg(feature = "git")]
pub use fetch::{default_remote, fetch, FetchFailure, FetchReport};
#[cfg(feature = "git")]
pub use repo::{load_graph, ref_labels, summarize, RepoSummary};
#[cfg(feature = "git")]
pub use status::{status, Attention, RepoStatus, WorkingTree};

#[cfg(test)]
pub(crate) mod testutil {
    use crate::dag::{CommitGraph, CommitGraphBuilder};
    use crate::layout::Layout;

    /// 依 `(oid, parents)` 建立測試用 graph，時間戳記依列出順序由新到舊遞減。
    ///
    /// 適合大多數測試；若測試本身要驗證「輸出與插入順序無關」，
    /// 請改用 [`graph_with_times`]，否則時間戳記會隨插入順序一起改變。
    pub(crate) fn graph_from(spec: &[(&str, &[&str])]) -> CommitGraph {
        let mut builder = CommitGraphBuilder::with_capacity(spec.len());
        for (position, (oid, parents)) in spec.iter().enumerate() {
            builder.push(
                *oid,
                parents.iter().map(|parent| (*parent).to_string()).collect(),
                1_000_000 - position as i64 * 3_600,
                "dev",
                format!("work on {oid}"),
            );
        }
        builder.build()
    }

    /// 依 `(oid, parents, timestamp)` 建立 graph，時間戳記由呼叫端指定。
    pub(crate) fn graph_with_times(spec: &[(&str, &[&str], i64)]) -> CommitGraph {
        let mut builder = CommitGraphBuilder::with_capacity(spec.len());
        for (oid, parents, timestamp) in spec {
            builder.push(
                *oid,
                parents.iter().map(|parent| (*parent).to_string()).collect(),
                *timestamp,
                "dev",
                format!("work on {oid}"),
            );
        }
        builder.build()
    }

    /// 取出佈局結果的 oid 序列，用於比較兩份佈局是否一致。
    pub(crate) fn oids_in_order(graph: &CommitGraph, layout: &Layout) -> Vec<String> {
        layout
            .order
            .iter()
            .map(|node| graph.commit(*node).oid.clone())
            .collect()
    }
}
