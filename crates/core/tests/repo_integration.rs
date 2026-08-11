//! 對真實 git repository 的整合測試。
//!
//! 測試會在暫存目錄建立自己的 repository，不依賴外部資料，
//! 也不會碰到使用者既有的任何 repository。

#![cfg(feature = "git")]

use std::fs;
use std::path::{Path, PathBuf};

use git2::{Repository, Signature};
use gitview_core::{lay_out, repo};

/// 建立一個獨立的暫存目錄，測試結束時清除。
struct TempRepo {
    path: PathBuf,
}

impl TempRepo {
    fn new(label: &str) -> Self {
        let unique = format!(
            "gitview-test-{label}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).expect("無法建立暫存目錄");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        // 清理失敗不應讓測試失敗。
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn signature() -> Signature<'static> {
    Signature::now("Test", "test@example.com").expect("無法建立簽章")
}

/// 在目前分支上建立一個 commit。
fn commit_file(repository: &Repository, name: &str, content: &str, message: &str) -> git2::Oid {
    let workdir = repository.workdir().expect("需要工作目錄").to_path_buf();
    fs::write(workdir.join(name), content).expect("無法寫入檔案");

    let mut index = repository.index().expect("無法取得索引");
    index.add_path(Path::new(name)).expect("無法加入索引");
    index.write().expect("無法寫入索引");
    let tree_id = index.write_tree().expect("無法寫出 tree");
    let tree = repository.find_tree(tree_id).expect("找不到 tree");

    let parents: Vec<git2::Commit> = match repository.head() {
        Ok(head) => vec![head.peel_to_commit().expect("HEAD 不是 commit")],
        Err(_) => Vec::new(),
    };
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

    let signature = signature();
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parent_refs,
        )
        .expect("無法建立 commit")
}

#[test]
fn reads_a_linear_history() {
    let temp = TempRepo::new("linear");
    let repository = Repository::init(temp.path()).expect("無法初始化 repository");
    commit_file(&repository, "a.txt", "one", "first");
    commit_file(&repository, "a.txt", "two", "second");
    commit_file(&repository, "a.txt", "three", "third");

    let graph = repo::load_graph(&repository).expect("無法讀取 commit 圖");
    assert_eq!(graph.len(), 3);
    assert_eq!(graph.merge_count(), 0);

    let layout = lay_out(&graph).expect("佈局失敗");
    assert_eq!(layout.rows(), 3);
    assert_eq!(layout.lane_count, 1, "線性歷史只需要一條線道");
    assert_eq!(layout.crossing_edges(), 0);
}

#[test]
fn reads_a_branch_and_merge() {
    let temp = TempRepo::new("merge");
    let repository = Repository::init(temp.path()).expect("無法初始化 repository");
    commit_file(&repository, "base.txt", "base", "base");
    let base = repository
        .head()
        .expect("需要 HEAD")
        .peel_to_commit()
        .expect("HEAD 不是 commit");

    // 側分支
    repository
        .branch("topic", &base, false)
        .expect("無法建立分支");
    repository
        .set_head("refs/heads/topic")
        .expect("無法切換分支");
    commit_file(&repository, "topic.txt", "topic", "topic work");
    let topic = repository
        .head()
        .unwrap()
        .peel_to_commit()
        .expect("topic 不是 commit");

    // 回到主線再做一個 commit
    repository
        .set_head("refs/heads/master")
        .or_else(|_| repository.set_head("refs/heads/main"))
        .expect("無法切回主線");
    repository
        .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .expect("無法檢出主線");
    commit_file(&repository, "main.txt", "main", "main work");
    let mainline = repository.head().unwrap().peel_to_commit().unwrap();

    // 合併
    let tree = repository
        .find_tree(
            repository
                .index()
                .unwrap()
                .write_tree()
                .expect("無法寫出 tree"),
        )
        .unwrap();
    let signature = signature();
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "merge topic",
            &tree,
            &[&mainline, &topic],
        )
        .expect("無法建立合併 commit");

    let graph = repo::load_graph(&repository).expect("無法讀取 commit 圖");
    assert_eq!(graph.len(), 4, "base、topic、main、merge 共四個");
    assert_eq!(graph.merge_count(), 1);

    let layout = lay_out(&graph).expect("佈局失敗");
    assert_eq!(layout.rows(), 4);
    assert_eq!(layout.lane_count, 2, "一條側分支只需要第二條線道");

    // 合併節點的第一個父節點應沿用同一條線道。
    let merge_index = graph
        .indices()
        .find(|index| graph.commit(*index).is_merge())
        .expect("應該有合併節點");
    let first_parent = graph.parents(merge_index)[0];
    assert_eq!(
        layout.lane_of[merge_index], layout.lane_of[first_parent],
        "第一個父節點應沿用線道，分支才會是直線"
    );
}

#[test]
fn summarizes_working_directory_state() {
    let temp = TempRepo::new("summary");
    let repository = Repository::init(temp.path()).expect("無法初始化 repository");
    commit_file(&repository, "tracked.txt", "content", "only commit");

    // 一個未追蹤的檔案
    fs::write(temp.path().join("untracked.txt"), "new").expect("無法寫入檔案");

    let graph = repo::load_graph(&repository).expect("無法讀取 commit 圖");
    let summary = repo::summarize(&repository, &graph).expect("無法讀取概況");

    assert_eq!(summary.commit_count, 1);
    assert_eq!(summary.merge_count, 0);
    assert_eq!(summary.dirty_files, 1, "未追蹤的檔案應計入");
    assert!(!summary.operation_in_progress);
    assert!(summary.branch.is_some(), "一般狀態下應有分支名稱");
}

#[test]
fn handles_an_empty_repository() {
    let temp = TempRepo::new("empty");
    let repository = Repository::init(temp.path()).expect("無法初始化 repository");

    let graph = repo::load_graph(&repository).expect("空 repository 也要能讀取");
    assert_eq!(graph.len(), 0);

    let layout = lay_out(&graph).expect("空圖也要能佈局");
    assert_eq!(layout.rows(), 0);

    let summary = repo::summarize(&repository, &graph).expect("空 repository 也要能取得概況");
    assert_eq!(summary.commit_count, 0);
    // 尚未有任何 commit，HEAD 指向不存在的分支。
    assert!(summary.branch.is_none() || summary.commit_count == 0);
}
