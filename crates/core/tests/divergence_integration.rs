//! 分岔分析的整合測試。
//!
//! 測試會自建一個「遠端」裸 repository 與兩個工作副本，全部位於暫存目錄，
//! 不接觸網路，也不接觸使用者既有的任何 repository。

#![cfg(feature = "git")]

use std::fs;
use std::path::{Path, PathBuf};

use git2::{Repository, Signature};
use gitview_core::divergence::{self, ConflictRisk, Recommendation};
use gitview_core::status::{self, Attention};

struct Playground {
    root: PathBuf,
}

impl Playground {
    fn new(label: &str) -> Self {
        let unique = format!(
            "gitview-div-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        );
        let root = std::env::temp_dir().join(unique);
        fs::create_dir_all(&root).expect("無法建立暫存目錄");
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for Playground {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn signature() -> Signature<'static> {
    Signature::now("Test", "test@example.com").expect("無法建立簽章")
}

fn commit_file(repository: &Repository, name: &str, content: &str, message: &str) {
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
        .expect("無法建立 commit");
}

fn current_branch(repository: &Repository) -> String {
    repository
        .head()
        .expect("需要 HEAD")
        .shorthand()
        .expect("分支名稱需為 UTF-8")
        .to_owned()
}

fn push_current_branch(repository: &Repository) {
    let branch = current_branch(repository);
    let mut remote = repository.find_remote("origin").expect("找不到 origin");
    remote
        .push(&[format!("refs/heads/{branch}:refs/heads/{branch}")], None)
        .expect("推送失敗");
}

/// 建立「遠端 + 兩個工作副本」，並在兩邊各做一個 commit 造成分岔。
///
/// `local_file` 與 `remote_file` 相同時，兩側會改到同一個檔案。
fn diverged_clone(playground: &Playground, local_file: &str, remote_file: &str) -> Repository {
    let origin_path = playground.path("origin.git");
    Repository::init_bare(&origin_path).expect("無法建立裸 repository");
    let origin_url = format!("file://{}", origin_path.display());

    // 第一個工作副本：建立基礎 commit 並推送。
    let seed_path = playground.path("seed");
    let seed = Repository::clone(&origin_url, &seed_path).expect("無法 clone");
    commit_file(&seed, "shared.txt", "base\n", "base");
    push_current_branch(&seed);

    // 第二個工作副本：模擬另一台機器推了新的內容。
    let other_path = playground.path("other");
    let other = Repository::clone(&origin_url, &other_path).expect("無法 clone");
    commit_file(
        &other,
        remote_file,
        "from the other machine\n",
        "remote work",
    );
    push_current_branch(&other);

    // 回到第一個副本做一個本機獨有的 commit，然後 fetch 取得遠端進度。
    commit_file(&seed, local_file, "local work\n", "local work");
    {
        // remote 借用 seed，必須在回傳前釋放。
        let mut remote = seed.find_remote("origin").expect("找不到 origin");
        remote
            .fetch(&[] as &[&str], None, None)
            .expect("fetch 失敗");
    }

    seed
}

#[test]
fn detects_divergence_and_recommends_rebase() {
    let playground = Playground::new("clean");
    let repository = diverged_clone(&playground, "local.txt", "remote.txt");

    let result = divergence::analyse(&repository).expect("分析失敗");

    assert!(result.is_diverged(), "兩側各有一個 commit，應判定為分岔");
    assert_eq!(result.ahead.len(), 1);
    assert_eq!(result.behind.len(), 1);
    assert!(result.upstream.is_some(), "clone 出來的分支應有追蹤對象");

    // 兩側改的是不同檔案，因此可以確定不會衝突。
    assert!(result.overlapping_files.is_empty());
    assert_eq!(result.risk(), ConflictRisk::None);
    assert_eq!(
        result.recommendation,
        Recommendation::Rebase(ConflictRisk::None)
    );

    assert!(result.local_files.contains(&"local.txt".to_owned()));
    assert!(result.incoming_files.contains(&"remote.txt".to_owned()));
}

#[test]
fn overlapping_files_raise_the_risk() {
    let playground = Playground::new("overlap");
    // 兩邊都改 shared.txt。
    let repository = diverged_clone(&playground, "shared.txt", "shared.txt");

    let result = divergence::analyse(&repository).expect("分析失敗");

    assert!(result.is_diverged());
    assert_eq!(
        result.overlapping_files,
        vec!["shared.txt".to_owned()],
        "兩側都改到的檔案應被列出"
    );
    assert_eq!(result.risk(), ConflictRisk::Possible);
    assert_eq!(
        result.recommendation,
        Recommendation::Rebase(ConflictRisk::Possible)
    );
}

#[test]
fn uncommitted_changes_to_incoming_files_block_the_operation() {
    let playground = Playground::new("dirty");
    let repository = diverged_clone(&playground, "local.txt", "remote.txt");

    // 即將進來的變更會碰到 remote.txt，而本機正好也改了它但還沒提交。
    let workdir = repository.workdir().expect("需要工作目錄");
    fs::write(workdir.join("remote.txt"), "uncommitted edit\n").expect("無法寫入檔案");

    let result = divergence::analyse(&repository).expect("分析失敗");

    assert_eq!(
        result.uncommitted_overlap,
        vec!["remote.txt".to_owned()],
        "未提交且會被影響的檔案應被列出"
    );
    assert_eq!(
        result.recommendation,
        Recommendation::ResolveWorkingTreeFirst
    );
}

#[test]
fn a_repository_without_upstream_is_reported_as_such() {
    let playground = Playground::new("no-upstream");
    let path = playground.path("solo");
    let repository = Repository::init(&path).expect("無法初始化");
    commit_file(&repository, "a.txt", "one\n", "first");

    let result = divergence::analyse(&repository).expect("分析失敗");
    assert_eq!(result.recommendation, Recommendation::NoUpstream);
    assert!(result.ahead.is_empty());
    assert!(result.behind.is_empty());
}

#[test]
fn status_reports_divergence_and_counts() {
    let playground = Playground::new("status");
    let repository = diverged_clone(&playground, "local.txt", "remote.txt");

    let state = status::status(&repository).expect("無法取得狀態");
    assert_eq!(state.ahead, 1);
    assert_eq!(state.behind, 1);
    assert!(state.is_diverged());
    assert_eq!(state.attention, Attention::Diverged);
    assert!(state.upstream.is_some());
    assert!(state.working_tree.is_clean(), "剛提交完應該是乾淨的");
    assert!(state.last_fetch.is_some(), "fetch 過應該有時間戳記");
}

#[test]
fn status_counts_untracked_and_staged_separately() {
    let playground = Playground::new("worktree");
    let path = playground.path("solo");
    let repository = Repository::init(&path).expect("無法初始化");
    commit_file(&repository, "tracked.txt", "one\n", "first");

    fs::write(path.join("untracked.txt"), "new\n").expect("無法寫入");
    fs::write(path.join("tracked.txt"), "changed\n").expect("無法寫入");

    let mut index = repository.index().expect("無法取得索引");
    index
        .add_path(Path::new("tracked.txt"))
        .expect("無法加入索引");
    index.write().expect("無法寫入索引");

    let state = status::status(&repository).expect("無法取得狀態");
    assert_eq!(state.working_tree.untracked, 1);
    assert_eq!(state.working_tree.staged, 1);
    assert_eq!(state.attention, Attention::Uncommitted);
}
