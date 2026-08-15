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

#[test]
fn diff_reports_hunks_and_intra_line_changes() {
    use gitview_core::diff::{self, DiffSource, LineKind};

    let playground = Playground::new("diff");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\ntwo\nthree\n", "first");
    fs::write(path.join("a.txt"), "one\nTWO\nthree\n").expect("無法寫入");

    let diffs = diff::workspace_diff(&repo, DiffSource::Unstaged).expect("無法計算差異");
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].path, "a.txt");
    assert_eq!(diffs[0].added, 1);
    assert_eq!(diffs[0].removed, 1);

    let lines = &diffs[0].hunks[0].lines;
    let removed = lines.iter().find(|l| l.kind == LineKind::Removed).unwrap();
    let added = lines.iter().find(|l| l.kind == LineKind::Added).unwrap();
    assert_eq!(removed.content, "two");
    assert_eq!(added.content, "TWO");
    assert!(!removed.spans.is_empty(), "應標出行內變動處");
}

#[test]
fn staging_a_single_line_leaves_the_rest_unstaged() {
    use gitview_core::diff::{self, DiffSource, LineKind};
    use gitview_core::workspace::LineRef;

    let playground = Playground::new("partial");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\ntwo\nthree\nfour\nfive\n", "first");
    // 同時改第一行與最後一行，之後只暫存其中一處。
    fs::write(path.join("a.txt"), "ONE\ntwo\nthree\nfour\nFIVE\n").expect("無法寫入");

    let diffs = diff::workspace_diff(&repo, DiffSource::Unstaged).expect("無法計算差異");
    let file = &diffs[0];

    // 找出「ONE」那一行與它對應的刪除行。
    let mut selection = Vec::new();
    for (hunk_index, hunk) in file.hunks.iter().enumerate() {
        for (line_index, line) in hunk.lines.iter().enumerate() {
            let touches_first = line.content == "ONE" || line.content == "one";
            if touches_first && line.kind != LineKind::Context {
                selection.push(LineRef {
                    hunk: hunk_index,
                    line: line_index,
                });
            }
        }
    }
    assert_eq!(selection.len(), 2, "應選到一個刪除行與一個新增行");
    workspace::stage_selection(&repo, "a.txt", &selection).expect("部分暫存失敗");

    // 索引中應只有第一行被改；第五行仍是原樣。
    let staged = diff::workspace_diff(&repo, DiffSource::Staged).expect("無法計算已暫存差異");
    let staged_added: Vec<&str> = staged[0]
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == LineKind::Added)
        .map(|l| l.content.as_str())
        .collect();
    assert_eq!(staged_added, vec!["ONE"], "只有選取的那一行應進入索引");

    // 工作目錄不應被動到。
    assert_eq!(
        fs::read_to_string(path.join("a.txt")).unwrap(),
        "ONE\ntwo\nthree\nfour\nFIVE\n"
    );

    // 剩下的變更仍在未暫存區。
    let remaining = diff::workspace_diff(&repo, DiffSource::Unstaged).expect("無法計算差異");
    let remaining_added: Vec<&str> = remaining[0]
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == LineKind::Added)
        .map(|l| l.content.as_str())
        .collect();
    assert_eq!(remaining_added, vec!["FIVE"]);
}

#[test]
fn unstaging_a_single_line_returns_it_to_unstaged() {
    use gitview_core::diff::{self, DiffSource, LineKind};
    use gitview_core::workspace::LineRef;

    let playground = Playground::new("partial-unstage");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\ntwo\nthree\nfour\nfive\n", "first");
    fs::write(path.join("a.txt"), "ONE\ntwo\nthree\nfour\nFIVE\n").expect("無法寫入");
    workspace::stage(&repo, &["a.txt".to_owned()]).expect("暫存失敗");

    let staged = diff::workspace_diff(&repo, DiffSource::Staged).expect("無法計算差異");
    let mut selection = Vec::new();
    for (hunk_index, hunk) in staged[0].hunks.iter().enumerate() {
        for (line_index, line) in hunk.lines.iter().enumerate() {
            if (line.content == "FIVE" || line.content == "five") && line.kind != LineKind::Context
            {
                selection.push(LineRef {
                    hunk: hunk_index,
                    line: line_index,
                });
            }
        }
    }
    assert_eq!(selection.len(), 2);
    workspace::unstage_selection(&repo, "a.txt", &selection).expect("部分取消暫存失敗");

    let still_staged = diff::workspace_diff(&repo, DiffSource::Staged).expect("無法計算差異");
    let added: Vec<&str> = still_staged[0]
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == LineKind::Added)
        .map(|l| l.content.as_str())
        .collect();
    assert_eq!(added, vec!["ONE"], "只有未被取消的那一行應留在索引");
}

#[test]
fn commit_diff_and_file_history_read_real_history() {
    use gitview_core::diff;

    let playground = Playground::new("history");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\n", "first");
    write_commit(&repo, "b.txt", "other\n", "unrelated");
    let target = write_commit(&repo, "a.txt", "one\ntwo\n", "extend a");

    let files = diff::commit_diff(&repo, &target.to_string()).expect("無法讀取 commit 差異");
    assert_eq!(files.len(), 1, "該 commit 只動到一個檔案");
    assert_eq!(files[0].path, "a.txt");
    assert_eq!(files[0].added, 1);

    let history = diff::file_history(&repo, "a.txt", 10).expect("無法讀取檔案歷史");
    assert_eq!(history.len(), 2, "a.txt 被兩個 commit 動過");
    assert_eq!(history[0], target.to_string(), "最新的排最前");
}

#[test]
fn incoming_collisions_are_marked_on_the_working_diff() {
    use gitview_core::diff::{self, DiffSource};

    let playground = Playground::new("collide");
    // 遠端改了 shared.txt。
    let repo = scenario(&playground, "shared.txt", "base\nremote change\n", None);
    // 本機也在同一個檔案有未提交的變更。
    fs::write(
        repo.workdir().unwrap().join("shared.txt"),
        "base\nlocal edit\n",
    )
    .expect("無法寫入");

    let mut diffs = diff::workspace_diff(&repo, DiffSource::Unstaged).expect("無法計算差異");
    let incoming = diff::incoming_line_ranges(&repo).expect("無法計算即將進來的範圍");
    diff::mark_incoming_collisions(&mut diffs, &incoming);

    let file = diffs
        .iter()
        .find(|f| f.path == "shared.txt")
        .expect("應有差異");
    assert!(
        file.hunks.iter().any(|hunk| hunk.collides_with_incoming),
        "本機改的位置與即將進來的變更重疊，應被標記"
    );
}

#[test]
fn paired_lines_are_linked_so_the_interface_can_select_them_together() {
    use gitview_core::diff::{self, DiffSource, LineKind};

    let playground = Playground::new("pairing");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\ntwo\nthree\n", "first");
    fs::write(path.join("a.txt"), "one\nTWO\nthree\n").expect("無法寫入");

    let diffs = diff::workspace_diff(&repo, DiffSource::Unstaged).expect("無法計算差異");
    let hunk = &diffs[0].hunks[0];

    let removed = hunk
        .lines
        .iter()
        .position(|line| line.kind == LineKind::Removed)
        .expect("應有刪除行");
    let added = hunk
        .lines
        .iter()
        .position(|line| line.kind == LineKind::Added)
        .expect("應有新增行");

    // 兩者互相指涉，介面才能一起選取。只選其中一邊會讓索引同時留下
    // 舊內容與新內容，產生無意義的結果。
    assert_eq!(hunk.lines[removed].pair, Some(added));
    assert_eq!(hunk.lines[added].pair, Some(removed));
}

#[test]
fn staging_only_one_side_of_a_pair_produces_both_lines() {
    use gitview_core::diff::{self, DiffSource, LineKind};
    use gitview_core::workspace::LineRef;

    let playground = Playground::new("half-pair");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\ntwo\nthree\n", "first");
    fs::write(path.join("a.txt"), "one\nTWO\nthree\n").expect("無法寫入");

    let diffs = diff::workspace_diff(&repo, DiffSource::Unstaged).expect("無法計算差異");
    let added = diffs[0].hunks[0]
        .lines
        .iter()
        .position(|line| line.kind == LineKind::Added)
        .unwrap();

    // 只暫存新增行、不暫存對應的刪除行。這是使用者實際踩到的情況：
    // 結果會同時保留舊行與新行。此測試把這個行為釘住，
    // 說明為什麼介面必須成對選取。
    workspace::stage_selection(
        &repo,
        "a.txt",
        &[LineRef {
            hunk: 0,
            line: added,
        }],
    )
    .expect("暫存失敗");

    let staged = diff::workspace_diff(&repo, DiffSource::Staged).expect("無法計算差異");
    let added_lines: Vec<&str> = staged[0].hunks[0]
        .lines
        .iter()
        .filter(|line| line.kind == LineKind::Added)
        .map(|line| line.content.as_str())
        .collect();
    assert_eq!(
        added_lines,
        vec!["TWO"],
        "索引會同時含有 two 與 TWO —— 這正是只選一邊的後果"
    );
}

#[test]
fn resolving_by_taking_theirs_skips_the_now_empty_commit() {
    let playground = Playground::new("empty-step");
    // 兩側改同一行 → rebase 必定衝突。
    let repo = scenario(
        &playground,
        "shared.txt",
        "remote version\n",
        Some(("shared.txt", "local version\n")),
    );

    ops::rebase_onto_upstream(&repo).expect("應停在衝突");

    // rebase 時 ours 是被接上去的基底（遠端側）。採用它之後，
    // 本機這個 commit 的內容完全被涵蓋，套用後不會有任何改變
    // —— 這是使用者實際踩到的情況。
    conflict::resolve_using(&repo, "shared.txt", Side::Ours).expect("解決失敗");

    let outcome = conflict::continue_operation(&repo).expect("應能完成而非報錯");
    assert!(
        outcome.message.contains("略過"),
        "應說明有步驟因為變成空的而被略過，實際訊息：{}",
        outcome.message
    );
    assert_eq!(
        repo.state(),
        git2::RepositoryState::Clean,
        "rebase 應已結束"
    );

    // 內容應為對方的版本。
    let content = fs::read_to_string(repo.workdir().unwrap().join("shared.txt")).unwrap();
    assert_eq!(content, "remote version\n");
}

#[test]
fn during_rebase_ours_is_the_upstream_side() {
    let playground = Playground::new("side-meaning");
    let repo = scenario(
        &playground,
        "shared.txt",
        "remote version\n",
        Some(("shared.txt", "local version\n")),
    );
    ops::rebase_onto_upstream(&repo).expect("應停在衝突");

    let files = conflict::conflicts(&repo).expect("無法列出衝突");
    let file = &files[0];

    // git 在 rebase 時的慣例與直覺相反：
    // ours 是「被接上去的那一端」（遠端），theirs 才是「正在重放的你的 commit」。
    assert_eq!(
        file.ours.text.as_deref(),
        Some("remote version\n"),
        "rebase 時 ours 應為遠端的內容"
    );
    assert_eq!(
        file.theirs.text.as_deref(),
        Some("local version\n"),
        "rebase 時 theirs 應為本機的內容"
    );
}

#[test]
fn branch_can_be_renamed_and_deleted_with_recovery() {
    let playground = Playground::new("branch-ops");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\n", "first");
    let main = repo.head().unwrap().shorthand().unwrap().to_owned();

    workspace::create_branch(&repo, "feature/old").expect("建立失敗");
    write_commit(&repo, "b.txt", "work\n", "分支上的工作");
    let tip = repo.head().unwrap().target().unwrap();
    workspace::checkout_branch(&repo, &main).expect("切換失敗");

    // 改名
    workspace::rename_branch(&repo, "feature/old", "feature/new").expect("改名失敗");
    let names: Vec<String> = workspace::branches(&repo)
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert!(names.contains(&"feature/new".to_owned()));
    assert!(!names.contains(&"feature/old".to_owned()));

    // 刪除：未合併，訊息要說明可還原
    let outcome = workspace::delete_branch(&repo, "feature/new").expect("刪除失敗");
    assert!(outcome.message.contains("尚未合併"), "訊息應提醒內容未合併");
    let point = outcome.undo.expect("刪除分支必須留下還原點");
    assert_eq!(point.oid, tip.to_string(), "還原點應指向分支原本的位置");

    // 還原點讓 commit 仍可取回
    assert!(repo.find_commit(tip).is_ok(), "還原點在，commit 就不會消失");
}

#[test]
fn the_current_branch_cannot_be_deleted() {
    let playground = Playground::new("branch-guard");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "one\n", "first");
    let current = repo.head().unwrap().shorthand().unwrap().to_owned();

    let error = workspace::delete_branch(&repo, &current).expect_err("不應允許");
    assert!(format!("{error}").contains("目前所在的分支"));
}

#[test]
fn upstream_can_be_set_and_cleared() {
    let playground = Playground::new("upstream");
    let repo = scenario(&playground, "remote.txt", "remote\n", None);
    let branch = repo.head().unwrap().shorthand().unwrap().to_owned();

    workspace::set_upstream(&repo, &branch, None).expect("取消追蹤失敗");
    assert!(
        repo.find_branch(&branch, git2::BranchType::Local)
            .unwrap()
            .upstream()
            .is_err(),
        "取消後不應有追蹤對象"
    );

    let remote_branch = format!("origin/{branch}");
    workspace::set_upstream(&repo, &branch, Some(&remote_branch)).expect("設定追蹤失敗");
    assert!(repo
        .find_branch(&branch, git2::BranchType::Local)
        .unwrap()
        .upstream()
        .is_ok());
}

#[test]
fn setting_a_nonexistent_upstream_is_rejected() {
    let playground = Playground::new("upstream-guard");
    let repo = scenario(&playground, "remote.txt", "remote\n", None);
    let branch = repo.head().unwrap().shorthand().unwrap().to_owned();

    let error = workspace::set_upstream(&repo, &branch, Some("origin/nope"))
        .expect_err("不存在的遠端分支應被拒絕");
    assert!(format!("{error}").contains("找不到遠端分支"));
}

#[test]
fn search_finds_commits_by_message_author_and_path() {
    use gitview_core::search::{self, SearchScope};

    let playground = Playground::new("search");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "alpha.txt", "one\n", "加入 alpha 功能");
    write_commit(&repo, "beta.txt", "two\n", "修正 beta 的問題");

    let by_message = search::search(&repo, "alpha", SearchScope::default(), 20, 100).unwrap();
    assert_eq!(by_message.len(), 1);
    assert!(by_message[0].matched.contains(&"message"));

    // 路徑命中：搜尋 beta 應同時命中訊息與檔名。
    let by_path = search::search(&repo, "beta.txt", SearchScope::default(), 20, 100).unwrap();
    assert_eq!(by_path.len(), 1);
    assert!(by_path[0].matched.contains(&"path"));

    let by_author = search::search(&repo, "Test", SearchScope::default(), 20, 100).unwrap();
    assert_eq!(by_author.len(), 2, "兩個 commit 的作者都是 Test");
}

#[test]
fn content_search_finds_the_commit_that_introduced_the_text() {
    use gitview_core::search::{self, SearchScope};

    let playground = Playground::new("pickaxe");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "hello\n", "第一版");
    write_commit(&repo, "a.txt", "hello\nMAGIC_TOKEN\n", "引入標記");
    write_commit(&repo, "a.txt", "hello\nMAGIC_TOKEN\nmore\n", "無關的變更");

    let scope = SearchScope {
        message: false,
        author: false,
        path: false,
        content: true,
    };
    let hits = search::search(&repo, "MAGIC_TOKEN", scope, 20, 100).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "只有引入該文字的 commit 應命中，而非每個含有它的"
    );
    assert_eq!(hits[0].summary, "引入標記");
}

#[test]
fn blame_attributes_each_line_to_its_last_change() {
    use gitview_core::search;

    let playground = Playground::new("blame");
    let path = playground.path("solo");
    let repo = Repository::init(&path).expect("無法初始化");
    write_commit(&repo, "a.txt", "line one\nline two\n", "建立檔案");
    let first = repo.head().unwrap().target().unwrap();
    write_commit(&repo, "a.txt", "line one\nCHANGED\n", "改第二行");
    let second = repo.head().unwrap().target().unwrap();

    let lines = search::blame(&repo, "a.txt", 100).expect("blame 失敗");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].content, "line one");
    assert_eq!(lines[0].oid, first.to_string(), "第一行仍屬於原本的 commit");
    assert_eq!(lines[1].content, "CHANGED");
    assert_eq!(lines[1].oid, second.to_string(), "第二行屬於後來的 commit");
    assert!(!lines[1].same_as_previous, "兩行來自不同 commit");
}
