//! Commit DAG 的記憶體表示。
//!
//! 採 arena 模式：所有節點存放於單一連續容器，節點之間以索引（而非參照）
//! 互相指涉。理由見 `docs/architecture.md` 的設計決策。

use std::collections::HashMap;

/// 節點在 [`CommitGraph`] 內的位置。
///
/// 索引只在產生它的那個 graph 內有效，跨 graph 使用會取到錯誤的節點。
pub type NodeIndex = usize;

/// 單一 commit 的中繼資料。
///
/// `parents` 已解析為索引，且只包含存在於同一個 graph 內的父節點；
/// 淺層或過濾過的 clone 可能使部分父節點不存在，那些會在建構時被略過。
#[derive(Debug, Clone)]
pub struct Commit {
    pub oid: String,
    pub parents: Vec<NodeIndex>,
    /// Committer date，Unix 秒。用於排序，不用於顯示。
    pub timestamp: i64,
    pub author: String,
    pub summary: String,
}

impl Commit {
    /// 是否為合併節點（兩個以上的父節點）。
    pub fn is_merge(&self) -> bool {
        self.parents.len() >= 2
    }

    /// 是否為根節點（在本 graph 內沒有父節點）。
    pub fn is_root(&self) -> bool {
        self.parents.is_empty()
    }
}

/// 一組 commit 及其父子關係。
///
/// 建構完成後不可變。以 [`CommitGraphBuilder`] 產生。
#[derive(Debug, Default)]
pub struct CommitGraph {
    commits: Vec<Commit>,
    children: Vec<Vec<NodeIndex>>,
    by_oid: HashMap<String, NodeIndex>,
}

impl CommitGraph {
    pub fn len(&self) -> usize {
        self.commits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    pub fn commit(&self, index: NodeIndex) -> &Commit {
        &self.commits[index]
    }

    pub fn parents(&self, index: NodeIndex) -> &[NodeIndex] {
        &self.commits[index].parents
    }

    pub fn children(&self, index: NodeIndex) -> &[NodeIndex] {
        &self.children[index]
    }

    pub fn index_of(&self, oid: &str) -> Option<NodeIndex> {
        self.by_oid.get(oid).copied()
    }

    /// 依索引順序走訪全部節點。
    pub fn indices(&self) -> impl Iterator<Item = NodeIndex> {
        0..self.commits.len()
    }

    /// 合併節點的數量。
    pub fn merge_count(&self) -> usize {
        self.commits.iter().filter(|c| c.is_merge()).count()
    }
}

/// 尚未解析父節點的 commit 輸入。
struct RawCommit {
    oid: String,
    parent_oids: Vec<String>,
    timestamp: i64,
    author: String,
    summary: String,
}

/// 分兩階段建立 [`CommitGraph`]。
///
/// 需要兩階段是因為 commit 可能在其父節點之前被加入，
/// 父節點的索引要等全部加入後才能確定。
#[derive(Default)]
pub struct CommitGraphBuilder {
    entries: Vec<RawCommit>,
    by_oid: HashMap<String, NodeIndex>,
}

impl CommitGraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            by_oid: HashMap::with_capacity(capacity),
        }
    }

    /// 加入一個 commit。重複的 oid 會被忽略，保留先加入的那筆。
    pub fn push(
        &mut self,
        oid: impl Into<String>,
        parent_oids: Vec<String>,
        timestamp: i64,
        author: impl Into<String>,
        summary: impl Into<String>,
    ) {
        let oid = oid.into();
        if self.by_oid.contains_key(&oid) {
            return;
        }
        self.by_oid.insert(oid.clone(), self.entries.len());
        self.entries.push(RawCommit {
            oid,
            parent_oids,
            timestamp,
            author: author.into(),
            summary: summary.into(),
        });
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 解析父節點索引並產生不可變的 graph。
    pub fn build(self) -> CommitGraph {
        let by_oid = self.by_oid;
        let mut commits = Vec::with_capacity(self.entries.len());

        for entry in self.entries {
            // 不在集合內的父節點會被略過：淺層或過濾過的 clone 會出現這種情形。
            let parents = entry
                .parent_oids
                .iter()
                .filter_map(|oid| by_oid.get(oid).copied())
                .collect();
            commits.push(Commit {
                oid: entry.oid,
                parents,
                timestamp: entry.timestamp,
                author: entry.author,
                summary: entry.summary,
            });
        }

        let mut children = vec![Vec::new(); commits.len()];
        for (index, commit) in commits.iter().enumerate() {
            for &parent in &commit.parents {
                children[parent].push(index);
            }
        }

        CommitGraph {
            commits,
            children,
            by_oid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::graph_from;

    #[test]
    fn resolves_parents_to_indices() {
        let graph = graph_from(&[("b", &["a"]), ("a", &[])]);
        let b = graph.index_of("b").expect("b 應存在");
        let a = graph.index_of("a").expect("a 應存在");
        assert_eq!(graph.parents(b), &[a]);
        assert!(graph.parents(a).is_empty());
    }

    #[test]
    fn records_children_in_both_directions() {
        let graph = graph_from(&[("m", &["x", "y"]), ("x", &["r"]), ("y", &["r"]), ("r", &[])]);
        let m = graph.index_of("m").unwrap();
        let x = graph.index_of("x").unwrap();
        let r = graph.index_of("r").unwrap();
        assert_eq!(graph.children(x), &[m]);
        assert_eq!(graph.children(r).len(), 2);
        assert!(graph.commit(m).is_merge());
        assert!(graph.commit(r).is_root());
    }

    #[test]
    fn skips_parents_outside_the_graph() {
        // 淺層 clone：b 的父節點不在集合內。
        let graph = graph_from(&[("b", &["missing"])]);
        let b = graph.index_of("b").unwrap();
        assert!(graph.parents(b).is_empty());
        assert!(graph.commit(b).is_root());
    }

    #[test]
    fn ignores_duplicate_oids() {
        let mut builder = CommitGraphBuilder::new();
        builder.push("a", Vec::new(), 10, "dev", "first");
        builder.push("a", Vec::new(), 20, "dev", "second");
        let graph = builder.build();
        assert_eq!(graph.len(), 1);
        assert_eq!(graph.commit(0).summary, "first");
    }

    #[test]
    fn empty_graph_is_usable() {
        let graph = CommitGraphBuilder::new().build();
        assert!(graph.is_empty());
        assert_eq!(graph.len(), 0);
        assert_eq!(graph.indices().count(), 0);
    }
}
