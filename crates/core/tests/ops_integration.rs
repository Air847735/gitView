//! 會改動 repository 的操作的整合測試。
//!
//! 這些操作會寫入使用者的工作成果，是專案裡風險最高的部分，因此測試涵蓋
//! 成功路徑、被拒絕的前置條件、以及還原是否真的有效。
//!
//! 全部在暫存目錄自建 repository，以本機裸 repository 模擬遠端，不連網。

#![cfg(feature = "git")]

use std::fs;
use std::path::{Path, PathBuf};

use git2::{Repository, Signature};
use gitview_core::conflict::{self, Side};
use gitview_core::{ops, workspace};

struct Playground {
    root: PathBuf,
}

impl Playground {
    fn new(label: &str) -> Self {
        let unique = format!(
            "gitview-ops-{label}-{}-{}",
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

fn write_commit(repo: &Repository, name: &str, content: &str, message: &str) -> git2::Oid {
    let workdir = repo.workdir().expect("需要工作目錄").to_path_buf();
    fs::write(workdir.join(name), content).expect("無法寫入");
    let mut index = repo.index().expect("無法取得索引");
    index.add_path(Path::new(name)).expect("無法加入索引");
    index.write().expect("無法寫入索引");
    let tree_id = index.write_tree().expect("無法寫出 tree");
    let tree = repo.find_tree(tree_id).expect("找不到 tree");
    let parents: Vec<git2::Commit> = match repo.head() {
        Ok(head) => vec![head.peel_to_commit().expect("HEAD 不是 commit")],
        Err(_) => Vec::new(),
    };
    let refs: Vec<&git2::Commit> = parents.iter().collect();
    let sig = signature();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &refs)
        .expect("無法建立 commit")
}

fn push_branch(repo: &Repository) {
    let branch = repo
        .head()
        .expect("需要 HEAD")
        .shorthand()
        .expect("分支名稱需為 UTF-8")
        .to_owned();
    let mut remote = repo.find_remote("origin").expect("找不到 origin");
    remote
        .push(&[format!("refs/heads/{branch}:refs/heads/{branch}")], None)
        .expect("推送失敗");
}

fn fetch(repo: &Repository) {
    let mut remote = repo.find_remote("origin").expect("找不到 origin");
    remote
        .fetch(&[] as &[&str], None, None)
        .expect("fetch 失敗");
}

/// 建立「遠端 + 本機」，遠端領先本機。`local_change` 非 None 時本機也有獨有的 commit。
fn scenario(
    playground: &Playground,
    remote_file: &str,
    remote_content: &str,
    local_change: Option<(&str, &str)>,
) -> Repository {
    let origin = playground.path("origin.git");
    Repository::init_bare(&origin).expect("無法建立裸 repository");
    let url = format!("file://{}", origin.display());

    let seed = Repository::clone(&url, playground.path("seed")).expect("無法 clone");
    write_commit(&seed, "shared.txt", "base\n", "base");
    push_branch(&seed);

    let other = Repository::clone(&url, playground.path("other")).expect("無法 clone");
    write_commit(&other, remote_file, remote_content, "remote work");
    push_branch(&other);

    if let Some((name, content)) = local_change {
        write_commit(&seed, name, content, "local work");
    }
    fetch(&seed);
    seed
}

#[test]
fn fast_forward_advances_the_branch_and_records_an_undo_point() {
    let playground = Playground::new("ff");
    let repo = scenario(&playground, "remote.txt", "remote\n", None);

    let before = repo.head().unwrap().peel_to_commit().unwrap().id();
    let outcome = ops::fast_forward(&repo).expect("快轉失敗");
    let after = repo.head().unwrap().peel_to_commit().unwrap().id();

    assert_ne!(before, after, "分支應該前進");
    assert!(repo.workdir().unwrap().join("remote.txt").exists());
    let point = outcome.undo.expect("應該建立還原點");
    assert_eq!(point.oid, before.to_string());
}

#[test]
fn fast_forward_refuses_when_the_branch_has_diverged() {
    let playground = Playground::new("ff-diverged");
    let repo = scenario(
        &playground,
        "remote.txt",
        "remote\n",
        Some(("local.txt", "local\n")),
    );

    let error = ops::fast_forward(&repo).expect_err("分岔時不應允許快轉");
    assert!(format!("{error}").contains("無法快轉"));
}

#[test]
fn fast_forward_refuses_with_uncommitted_changes() {
    let playground = Playground::new("ff-dirty");
    let repo = scenario(&playground, "remote.txt", "remote\n", None);
    fs::write(repo.workdir().unwrap().join("shared.txt"), "edited\n").expect("無法寫入");

    let error = ops::fast_forward(&repo).expect_err("有未提交變更時不應執行");
    assert!(format!("{error}").contains("未提交"));
}

#[test]
fn rebase_produces_linear_history_and_can_be_undone() {
    let playground = Playground::new("rebase");
    let repo = scenario(
        &playground,
        "remote.txt",
        "remote\n",
        Some(("local.txt", "local\n")),
    );
    let before = repo.head().unwrap().peel_to_commit().unwrap().id();

    let outcome = ops::rebase_onto_upstream(&repo).expect("rebase 失敗");
    assert!(outcome.message.contains("重新套用"));

    // rebase 之後本機應領先遠端 1 個，且不再落後。
    let head = repo.head().unwrap().target().unwrap();
    let upstream = repo
        .find_branch("master", git2::BranchType::Local)
        .or_else(|_| repo.find_branch("main", git2::BranchType::Local))
        .unwrap()
        .upstream()
        .unwrap()
        .get()
        .target()
        .unwrap();
    let (ahead, behind) = repo.graph_ahead_behind(head, upstream).unwrap();
    assert_eq!((ahead, behind), (1, 0), "rebase 後應只領先，不落後");

    // 還原回操作前。
    let point = outcome.undo.expect("應該建立還原點");
    ops::undo_to(&repo, &point.reference).expect("還原失敗");
    assert_eq!(
        repo.head().unwrap().peel_to_commit().unwrap().id(),
        before,
        "還原後應回到操作前的位置"
    );
}

#[test]
fn rebase_refuses_with_uncommitted_changes() {
    let playground = Playground::new("rebase-dirty");
    let repo = scenario(
        &playground,
        "remote.txt",
        "remote\n",
        Some(("local.txt", "local\n")),
    );
    fs::write(repo.workdir().unwrap().join("shared.txt"), "edited\n").expect("無法寫入");

    let error = ops::rebase_onto_upstream(&repo).expect_err("有未提交變更時不應執行");
    assert!(format!("{error}").contains("未提交"));
}

#[test]
fn merge_creates_a_merge_commit() {
    let playground = Playground::new("merge");
    let repo = scenario(
        &playground,
        "remote.txt",
        "remote\n",
        Some(("local.txt", "local\n")),
    );

    ops::merge_upstream(&repo).expect("合併失敗");
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.parent_count(), 2, "應該產生一個有兩個父節點的合併");
}

#[test]
fn safety_points_are_listed_and_pruned() {
    let playground = Playground::new("points");
    let repo = scenario(&playground, "remote.txt", "remote\n", None);

    for index in 0..5 {
        ops::create_safety_point(&repo, &format!("test{index}")).expect("無法建立還原點");
    }
    let points = ops::list_safety_points(&repo).expect("無法列出");
    assert!(points.len() >= 5);

    let removed = ops::prune_safety_points(&repo, 2).expect("無法清理");
    assert!(removed >= 3);
    assert_eq!(ops::list_safety_points(&repo).unwrap().len(), 2);
}

#[test]
fn undo_rejects_references_outside_its_own_namespace() {
    let playground = Playground::new("undo-guard");
    let repo = scenario(&playground, "remote.txt", "remote\n", None);

    let error = ops::undo_to(&repo, "refs/heads/master").expect_err("不應接受任意 ref");
    assert!(format!("{error}").contains("只能還原"));
}

#[test]
fn conflicting_rebase_stops_and_can_be_resolved_then_continued() {
    let playground = Playground::new("conflict");
    // 兩側都改 shared.txt，必然衝突。
    let repo = scenario(
        &playground,
        "shared.txt",
        "remote version\n",
        Some(("shared.txt", "local version\n")),
    );

    let outcome = ops::rebase_onto_upstream(&repo).expect("rebase 應停在衝突而非失敗");
    assert!(outcome.message.contains("衝突"), "應回報遇到衝突");

    // 衝突檔案應被列出，且雙方內容都拿得到。
    let files = conflict::conflicts(&repo).expect("無法列出衝突");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "shared.txt");
    assert!(files[0].ours.exists && files[0].theirs.exists);
    assert!(files[0].merged.is_some(), "工作目錄應有含衝突標記的內容");
    assert!(!conflict::all_resolved(&repo).unwrap());

    // 採用其中一方後應標記為已解決。
    conflict::resolve_using(&repo, "shared.txt", Side::Theirs).expect("解決失敗");
    assert!(conflict::all_resolved(&repo).unwrap());

    let done = conflict::continue_operation(&repo).expect("繼續失敗");
    assert!(done.message.contains("完成"));
    assert_eq!(repo.state(), git2::RepositoryState::Clean);
}

#[test]
fn a_conflicted_operation_can_be_aborted() {
    let playground = Playground::new("abort");
    let repo = scenario(
        &playground,
        "shared.txt",
        "remote version\n",
        Some(("shared.txt", "local version\n")),
    );
    let before = repo.head().unwrap().peel_to_commit().unwrap().id();

    ops::rebase_onto_upstream(&repo).expect("應停在衝突");
    assert_ne!(repo.state(), git2::RepositoryState::Clean);

    ops::abort_operation(&repo).expect("中止失敗");
    assert_eq!(repo.state(), git2::RepositoryState::Clean);
    assert_eq!(
        repo.head().unwrap().peel_to_commit().unwrap().id(),
        before,
        "中止後應回到操作前"
    );
}

#[test]
fn resolving_with_edited_content_is_accepted() {
    let playground = Playground::new("resolve-edit");
    let repo = scenario(
        &playground,
        "shared.txt",
        "remote version\n",
        Some(("shared.txt", "local version\n")),
    );
    ops::rebase_onto_upstream(&repo).expect("應停在衝突");

    conflict::resolve_with_content(&repo, "shared.txt", "手動合併的結果\n").expect("解決失敗");
    assert!(conflict::all_resolved(&repo).unwrap());
    conflict::continue_operation(&repo).expect("繼續失敗");

    let content = fs::read_to_string(repo.workdir().unwrap().join("shared.txt")).unwrap();
    assert_eq!(content, "手動合併的結果\n");
}

#[test]
fn staging_committing_and_unstaging_work() {
    let playground = Playground::new("commit");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\n", "first");

    fs::write(path.join("b.txt"), "two\n").expect("無法寫入");
    let changes = workspace::changes(&repo).expect("無法讀取變更");
    assert_eq!(changes.len(), 1);
    assert!(changes[0].is_untracked);

    workspace::stage(&repo, &["b.txt".to_owned()]).expect("暫存失敗");
    let staged = workspace::changes(&repo).expect("無法讀取變更");
    assert_eq!(staged[0].staged, "new");

    workspace::unstage(&repo, &["b.txt".to_owned()]).expect("取消暫存失敗");
    let unstaged = workspace::changes(&repo).expect("無法讀取變更");
    assert_eq!(unstaged[0].staged, "none");
    assert!(
        path.join("b.txt").exists(),
        "取消暫存不得刪除工作目錄的檔案"
    );

    workspace::stage(&repo, &[]).expect("暫存全部失敗");
    workspace::commit(&repo, "第二個 commit", false).expect("提交失敗");
    assert_eq!(
        repo.head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .summary()
            .unwrap()
            .unwrap(),
        "第二個 commit"
    );
    assert!(workspace::changes(&repo).unwrap().is_empty());
}

#[test]
fn committing_without_a_message_is_rejected() {
    let playground = Playground::new("empty-msg");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\n", "first");
    fs::write(path.join("b.txt"), "two\n").unwrap();
    workspace::stage(&repo, &[]).unwrap();

    let error = workspace::commit(&repo, "   ", false).expect_err("空訊息應被拒絕");
    assert!(format!("{error}").contains("不能是空的"));
}

#[test]
fn branches_can_be_created_listed_and_switched() {
    let playground = Playground::new("branch");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\n", "first");
    let original = repo.head().unwrap().shorthand().unwrap().to_owned();

    workspace::create_branch(&repo, "feature/x").expect("建立分支失敗");
    assert_eq!(repo.head().unwrap().shorthand().unwrap(), "feature/x");

    let list = workspace::branches(&repo).expect("無法列出分支");
    assert!(list.iter().any(|b| b.name == "feature/x" && b.is_head));
    assert!(list.iter().any(|b| b.name == original));

    workspace::checkout_branch(&repo, &original).expect("切換失敗");
    assert_eq!(repo.head().unwrap().shorthand().unwrap(), original);
}

#[test]
fn stash_round_trip_preserves_changes() {
    let playground = Playground::new("stash");
    let path = playground.path("solo");
    let mut repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\n", "first");

    fs::write(path.join("a.txt"), "edited\n").expect("無法寫入");
    workspace::stash_save(&mut repo, "測試暫存").expect("暫存失敗");
    assert_eq!(
        fs::read_to_string(path.join("a.txt")).unwrap(),
        "one\n",
        "暫存後工作目錄應回到乾淨狀態"
    );
    assert_eq!(workspace::stashes(&mut repo).unwrap().len(), 1);

    workspace::stash_pop(&mut repo, 0).expect("取出失敗");
    assert_eq!(
        fs::read_to_string(path.join("a.txt")).unwrap(),
        "edited\n",
        "取出後應恢復原本的編輯"
    );
    assert!(workspace::stashes(&mut repo).unwrap().is_empty());
}

#[test]
fn discard_reverts_a_file_and_refuses_an_empty_list() {
    let playground = Playground::new("discard");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\n", "first");
    fs::write(path.join("a.txt"), "edited\n").expect("無法寫入");

    let error = workspace::discard(&repo, &[]).expect_err("不應允許一次丟棄全部");
    assert!(format!("{error}").contains("必須指定"));

    workspace::discard(&repo, &["a.txt".to_owned()]).expect("丟棄失敗");
    assert_eq!(fs::read_to_string(path.join("a.txt")).unwrap(), "one\n");
}
