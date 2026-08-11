//! 把 commit DAG 攤平成可繪製的座標。
//!
//! 純運算，不依賴任何介面。輸入節點集合與父子關係，輸出每個節點的
//! 列（row）與線道（lane）。歷史檢視與分岔預覽共用這份輸出。

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fmt;

use crate::dag::{CommitGraph, NodeIndex};

/// 佈局失敗的原因。
///
/// 使用具型別的錯誤而非 `anyhow`，讓本模組不帶任何相依，
/// 呼叫端也能針對個別情況處理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// 圖中含有環，或父子關係不一致，無法排出完整順序。
    Cyclic { ordered: usize, total: usize },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutError::Cyclic { ordered, total } => write!(
                formatter,
                "commit 圖含有環或資料不一致：排入 {ordered} 個節點，實際有 {total} 個"
            ),
        }
    }
}

impl std::error::Error for LayoutError {}

/// 本模組的結果型別。
pub type Result<T> = std::result::Result<T, LayoutError>;

/// 一條從子節點（上方）連到父節點（下方）的線。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub child_row: usize,
    pub child_lane: usize,
    pub parent_row: usize,
    pub parent_lane: usize,
}

impl Edge {
    /// 是否需要橫向跨越線道。不跨線道的邊是單純的垂直線。
    pub fn spans_lanes(&self) -> bool {
        self.child_lane != self.parent_lane
    }
}

/// 佈局結果。
#[derive(Debug, Default)]
pub struct Layout {
    /// 由上而下的節點順序；索引即為列號。
    pub order: Vec<NodeIndex>,
    /// 節點索引 → 列號。未出現在 `order` 中的節點為 [`Layout::UNPLACED`]。
    pub row_of: Vec<usize>,
    /// 節點索引 → 線道編號。未出現者為 [`Layout::UNPLACED`]。
    pub lane_of: Vec<usize>,
    pub edges: Vec<Edge>,
    /// 同時使用中的線道數量最大值，即繪製所需的寬度。
    pub lane_count: usize,
}

impl Layout {
    /// 表示該節點未被放置。
    pub const UNPLACED: usize = usize::MAX;

    pub fn rows(&self) -> usize {
        self.order.len()
    }

    pub fn is_placed(&self, node: NodeIndex) -> bool {
        self.row_of[node] != Self::UNPLACED
    }

    /// 需要橫向跨越線道的邊數量。線道跨越越多，圖越難讀。
    pub fn crossing_edges(&self) -> usize {
        self.edges.iter().filter(|edge| edge.spans_lanes()).count()
    }
}

/// 將節點由新到舊排序，同時保證父節點永遠排在子節點之後。
///
/// 只有當某節點的所有子節點都已排入後，它才成為候選；在候選中取
/// committer date 最新者，時間相同時取 oid 較小者。這兩條規則使輸出
/// 只取決於圖本身，與讀取順序無關 —— 否則畫面會在每次重新整理時跳動。
pub fn topo_time_order(graph: &CommitGraph) -> Result<Vec<NodeIndex>> {
    let total = graph.len();
    let mut pending_children = vec![0usize; total];
    for node in graph.indices() {
        for &parent in graph.parents(node) {
            pending_children[parent] += 1;
        }
    }

    // 最大堆：時間大者優先；時間相同時 Reverse(oid) 大者優先，即 oid 小者優先。
    // 堆中借用 graph 內的 oid，因此不抽成閉包 —— 閉包參數的生命週期會被推斷為
    // 'static，使借用逃逸出函式。
    let mut heap: BinaryHeap<(i64, Reverse<&str>, NodeIndex)> = BinaryHeap::new();
    for node in graph.indices() {
        if pending_children[node] == 0 {
            let commit = graph.commit(node);
            heap.push((commit.timestamp, Reverse(commit.oid.as_str()), node));
        }
    }

    let mut order = Vec::with_capacity(total);
    while let Some((_, _, node)) = heap.pop() {
        order.push(node);
        for &parent in graph.parents(node) {
            pending_children[parent] -= 1;
            if pending_children[parent] == 0 {
                let commit = graph.commit(parent);
                heap.push((commit.timestamp, Reverse(commit.oid.as_str()), parent));
            }
        }
    }

    if order.len() != total {
        return Err(LayoutError::Cyclic {
            ordered: order.len(),
            total,
        });
    }
    Ok(order)
}

/// 取得 `node` 該用的線道：優先取最左邊已為它保留的線道，
/// 其餘重複保留的線道一併釋放；沒有保留時取最左邊的空位。
fn claim_lane(active: &mut Vec<Option<NodeIndex>>, node: NodeIndex) -> usize {
    let mut claimed: Option<usize> = None;
    for (lane, slot) in active.iter_mut().enumerate() {
        if *slot == Some(node) {
            match claimed {
                None => claimed = Some(lane),
                Some(_) => *slot = None,
            }
        }
    }
    if let Some(lane) = claimed {
        return lane;
    }
    if let Some(lane) = active.iter().position(|slot| slot.is_none()) {
        return lane;
    }
    active.push(None);
    active.len() - 1
}

/// 依既定順序配置線道。
///
/// 由上而下逐列處理，維護目前使用中的線道，每條線道記錄它預期會接到的
/// 父節點。第一個父節點沿用子節點的線道 —— 這是分支能呈現為直線的關鍵；
/// 其餘父節點各自取得新的線道。線道釋放後可被後續分支重用。
pub fn assign_lanes(graph: &CommitGraph, order: &[NodeIndex]) -> Layout {
    let total = graph.len();
    let mut active: Vec<Option<NodeIndex>> = Vec::new();
    let mut row_of = vec![Layout::UNPLACED; total];
    let mut lane_of = vec![Layout::UNPLACED; total];
    let mut lane_count = 0usize;

    for (row, &node) in order.iter().enumerate() {
        let lane = claim_lane(&mut active, node);
        active[lane] = None;
        row_of[node] = row;
        lane_of[node] = lane;
        // 節點本身就佔用寬度：線道可能在同一列結束並被回收，
        // 只看使用中的線道數會低估繪製寬度。
        lane_count = lane_count.max(lane + 1);

        for (position, &parent) in graph.parents(node).iter().enumerate() {
            if row_of[parent] != Layout::UNPLACED {
                // 已經畫過的節點不會再被往下連；重複的父節點也走這條路徑。
                continue;
            }
            if position == 0 {
                active[lane] = Some(parent);
            } else if !active.contains(&Some(parent)) {
                let extra = claim_lane(&mut active, parent);
                active[extra] = Some(parent);
            }
        }

        while active.last().is_some_and(|slot| slot.is_none()) {
            active.pop();
        }
        lane_count = lane_count.max(active.len());
    }

    let mut edges = Vec::new();
    for &node in order {
        for &parent in graph.parents(node) {
            if row_of[parent] == Layout::UNPLACED {
                continue;
            }
            edges.push(Edge {
                child_row: row_of[node],
                child_lane: lane_of[node],
                parent_row: row_of[parent],
                parent_lane: lane_of[parent],
            });
        }
    }

    Layout {
        order: order.to_vec(),
        row_of,
        lane_of,
        edges,
        lane_count,
    }
}

/// 排序後配置線道。這是本模組對外的主要進入點。
pub fn lay_out(graph: &CommitGraph) -> Result<Layout> {
    let order = topo_time_order(graph)?;
    Ok(assign_lanes(graph, &order))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{graph_from, graph_with_times, oids_in_order};

    #[test]
    fn parents_never_precede_children() {
        let graph = graph_from(&[
            ("m", &["a", "f"]),
            ("a", &["b"]),
            ("f", &["b"]),
            ("b", &["c"]),
            ("c", &[]),
        ]);
        let layout = lay_out(&graph).unwrap();
        for edge in &layout.edges {
            assert!(edge.child_row < edge.parent_row, "邊必須由上往下：{edge:?}");
        }
    }

    #[test]
    fn order_does_not_depend_on_insertion_order() {
        // 時間戳記綁在 oid 上，因此兩份輸入描述的是同一張圖，只有插入順序不同。
        let forward = graph_with_times(&[
            ("m", &["a", "f"], 500),
            ("a", &["b"], 400),
            ("f", &["b"], 300),
            ("b", &["c"], 200),
            ("c", &[], 100),
        ]);
        let reversed = graph_with_times(&[
            ("c", &[], 100),
            ("b", &["c"], 200),
            ("f", &["b"], 300),
            ("a", &["b"], 400),
            ("m", &["a", "f"], 500),
        ]);

        let first = lay_out(&forward).unwrap();
        let second = lay_out(&reversed).unwrap();
        assert_eq!(
            oids_in_order(&forward, &first),
            oids_in_order(&reversed, &second)
        );
    }

    #[test]
    fn equal_timestamps_break_ties_by_oid() {
        let mut builder = crate::dag::CommitGraphBuilder::new();
        builder.push("x", vec!["z".to_string()], 500, "dev", "x");
        builder.push("y", vec!["z".to_string()], 500, "dev", "y");
        builder.push("z", Vec::new(), 500, "dev", "z");
        let graph = builder.build();
        let layout = lay_out(&graph).unwrap();
        // 時間相同時取 oid 較小者，因此 x 在 y 之前。
        assert_eq!(oids_in_order(&graph, &layout), vec!["x", "y", "z"]);
    }

    #[test]
    fn linear_history_uses_one_lane() {
        let graph = graph_from(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]);
        let layout = lay_out(&graph).unwrap();
        assert_eq!(layout.lane_count, 1);
        assert_eq!(layout.crossing_edges(), 0);
        assert!(layout.lane_of.iter().all(|lane| *lane == 0));
    }

    #[test]
    fn first_parent_keeps_the_lane() {
        let graph = graph_from(&[("m", &["a", "f"]), ("a", &["b"]), ("f", &["b"]), ("b", &[])]);
        let layout = lay_out(&graph).unwrap();
        let m = graph.index_of("m").unwrap();
        let a = graph.index_of("a").unwrap();
        let f = graph.index_of("f").unwrap();
        assert_eq!(
            layout.lane_of[m], layout.lane_of[a],
            "第一個父節點應沿用線道"
        );
        assert_ne!(layout.lane_of[m], layout.lane_of[f], "其餘父節點應另配線道");
    }

    #[test]
    fn lanes_are_reused_after_release() {
        // 兩條互不重疊的側分支應共用同一條線道。
        let graph = graph_from(&[
            ("e", &["d", "s2"]),
            ("d", &["c"]),
            ("s2", &["c"]),
            ("c", &["b", "s1"]),
            ("b", &["a"]),
            ("s1", &["a"]),
            ("a", &[]),
        ]);
        let layout = lay_out(&graph).unwrap();
        assert_eq!(layout.lane_count, 2, "側分支未重疊時只需兩條線道");
    }

    #[test]
    fn every_node_is_placed_exactly_once() {
        let graph = graph_from(&[("m", &["a", "f"]), ("a", &["b"]), ("f", &["b"]), ("b", &[])]);
        let layout = lay_out(&graph).unwrap();
        assert_eq!(layout.rows(), graph.len());
        let mut rows: Vec<usize> = layout.row_of.clone();
        rows.sort_unstable();
        assert_eq!(rows, (0..graph.len()).collect::<Vec<_>>());
    }

    #[test]
    fn multiple_roots_are_supported() {
        // 兩段沒有共同祖先的歷史。
        let graph = graph_from(&[("a", &["b"]), ("b", &[]), ("x", &["y"]), ("y", &[])]);
        let layout = lay_out(&graph).unwrap();
        assert_eq!(layout.rows(), 4);
        assert!(graph.indices().all(|node| layout.is_placed(node)));
    }

    #[test]
    fn empty_graph_produces_empty_layout() {
        let graph = crate::dag::CommitGraphBuilder::new().build();
        let layout = lay_out(&graph).unwrap();
        assert_eq!(layout.rows(), 0);
        assert_eq!(layout.lane_count, 0);
        assert!(layout.edges.is_empty());
    }

    #[test]
    fn single_commit_is_placed() {
        let graph = graph_from(&[("only", &[])]);
        let layout = lay_out(&graph).unwrap();
        assert_eq!(layout.rows(), 1);
        assert_eq!(layout.lane_count, 1);
        assert!(layout.edges.is_empty());
    }

    #[test]
    fn cycles_are_reported_rather_than_hanging() {
        let mut builder = crate::dag::CommitGraphBuilder::new();
        builder.push("a", vec!["b".to_string()], 10, "dev", "a");
        builder.push("b", vec!["a".to_string()], 20, "dev", "b");
        let graph = builder.build();
        assert!(topo_time_order(&graph).is_err());
    }
}
