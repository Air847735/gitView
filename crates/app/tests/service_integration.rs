//! 對 UI 資料層的整合測試。
//!
//! 這一層是介面實際消費的資料。本機的 WebKitGTK 在無硬體加速的遠端桌面
//! 工作階段中無法繪製，畫面本身無法在此驗證；因此改為驗證送進畫面的資料
//! 一定正確 —— 剩下未驗證的只有繪製本身。
//!
//! 測試全部在暫存目錄自建 repository，不接觸網路，也不接觸使用者既有的
//! 任何 repository。

use std::fs;
use std::path::{Path, PathBuf};

use git2::{Repository, Signature};
use gitview_app::service::{self, AppState};
use gitview_app::settings;

struct Playground {
    root: PathBuf,
}

impl Playground {
    fn new(label: &str) -> Self {
        let unique = format!(
            "gitview-svc-{label}-{}-{}",
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

    fn state(&self) -> AppState {
        AppState::new(settings::settings_path(&self.root))
    }
}

impl Drop for Playground {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
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
    let signature = Signature::now("Test", "test@example.com").expect("無法建立簽章");
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

fn seed_repo(playground: &Playground, name: &str) -> PathBuf {
    let path = playground.path(name);
    let repository = Repository::init(&path).expect("無法初始化");
    commit_file(&repository, "a.txt", "one\n", "first");
    commit_file(&repository, "a.txt", "two\n", "second");
    path
}

#[test]
fn adding_a_repository_persists_it_to_settings() {
    let playground = Playground::new("add");
    let repo_path = seed_repo(&playground, "demo");
    let state = playground.state();

    let stored = service::add_repo(&state, repo_path.to_str().unwrap()).expect("加入失敗");
    assert!(stored.ends_with("demo"));

    // 重新讀取設定檔，確認真的寫入而不只是留在記憶體。
    let reloaded = settings::load(&settings::settings_path(&playground.root));
    assert_eq!(reloaded.repos.len(), 1);
    assert!(reloaded.repos[0].ends_with("demo"));
}

#[test]
fn adding_the_same_repository_twice_is_rejected() {
    let playground = Playground::new("dup");
    let repo_path = seed_repo(&playground, "demo");
    let state = playground.state();

    service::add_repo(&state, repo_path.to_str().unwrap()).expect("第一次應成功");
    let second = service::add_repo(&state, repo_path.to_str().unwrap());
    assert!(second.is_err(), "重複加入應被拒絕");
}

#[test]
fn adding_a_non_repository_fails_with_a_message() {
    let playground = Playground::new("notrepo");
    let plain = playground.path("plain");
    fs::create_dir_all(&plain).expect("無法建立目錄");
    let state = playground.state();

    let result = service::add_repo(&state, plain.to_str().unwrap());
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("無法開啟"));
}

#[test]
fn status_list_reports_working_tree_changes() {
    let playground = Playground::new("status");
    let repo_path = seed_repo(&playground, "demo");
    fs::write(repo_path.join("untracked.txt"), "new\n").expect("無法寫入");

    let state = playground.state();
    service::add_repo(&state, repo_path.to_str().unwrap()).expect("加入失敗");

    let statuses = service::all_statuses(&state);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].working_tree.untracked, 1);
    assert_eq!(statuses[0].working_tree.total, 1);
    assert_eq!(statuses[0].attention, "uncommitted");
    assert!(statuses[0].error.is_none());
}

#[test]
fn missing_repositories_still_appear_with_an_explanation() {
    let playground = Playground::new("missing");
    let repo_path = seed_repo(&playground, "demo");
    let state = playground.state();
    service::add_repo(&state, repo_path.to_str().unwrap()).expect("加入失敗");

    // 使用者可能在應用程式之外把目錄刪掉或移走。
    fs::remove_dir_all(&repo_path).expect("無法刪除");

    let statuses = service::all_statuses(&state);
    assert_eq!(statuses.len(), 1, "路徑消失時仍應列出，才能讓使用者移除它");
    assert!(statuses[0].error.is_some());
    assert_eq!(statuses[0].attention, "attention");
}

#[test]
fn repositories_needing_attention_sort_first() {
    let playground = Playground::new("sort");
    let clean = seed_repo(&playground, "aaa-clean");
    let dirty = seed_repo(&playground, "zzz-dirty");
    fs::write(dirty.join("untracked.txt"), "new\n").expect("無法寫入");

    let state = playground.state();
    service::add_repo(&state, clean.to_str().unwrap()).expect("加入失敗");
    service::add_repo(&state, dirty.to_str().unwrap()).expect("加入失敗");

    let statuses = service::all_statuses(&state);
    assert_eq!(statuses.len(), 2);
    // 名稱排序會讓 aaa-clean 在前，但它沒有待辦事項，應排在後面。
    assert_eq!(statuses[0].name, "zzz-dirty");
    assert_eq!(statuses[1].name, "aaa-clean");
}

#[test]
fn graph_data_matches_the_repository() {
    let playground = Playground::new("graph");
    let repo_path = seed_repo(&playground, "demo");

    let graph = service::graph_of(repo_path.to_str().unwrap(), 100).expect("無法讀取圖");
    assert_eq!(graph.total_commits, 2);
    assert_eq!(graph.commits.len(), 2);
    assert!(!graph.truncated);
    assert_eq!(graph.lane_count, 1, "線性歷史只需要一條線道");

    // 列號必須連續，前端是靠它計算 y 座標的。
    assert_eq!(graph.commits[0].row, 0);
    assert_eq!(graph.commits[1].row, 1);
    // 目前分支的 ref 應該標在最新的 commit 上。
    assert!(
        !graph.commits[0].refs.is_empty(),
        "最新的 commit 應帶有分支標籤"
    );
}

#[test]
fn graph_limit_truncates_and_reports_it() {
    let playground = Playground::new("limit");
    let repo_path = seed_repo(&playground, "demo");

    let graph = service::graph_of(repo_path.to_str().unwrap(), 1).expect("無法讀取圖");
    assert_eq!(graph.commits.len(), 1);
    assert!(graph.truncated);
    assert_eq!(graph.total_commits, 2);
    // 被截掉的那一端的邊不能留下，否則前端會畫到畫面外。
    assert!(graph.edges.is_empty());
}

#[test]
fn divergence_data_is_available_for_a_solo_repository() {
    let playground = Playground::new("divergence");
    let repo_path = seed_repo(&playground, "demo");

    let divergence = service::divergence_of(repo_path.to_str().unwrap()).expect("分析失敗");
    assert_eq!(divergence.recommendation, "no-upstream");
    assert!(!divergence.recommendation_headline.is_empty());
    assert!(!divergence.is_diverged);
}

#[test]
fn removing_a_repository_clears_it_from_the_list() {
    let playground = Playground::new("remove");
    let repo_path = seed_repo(&playground, "demo");
    let state = playground.state();
    let stored = service::add_repo(&state, repo_path.to_str().unwrap()).expect("加入失敗");

    service::remove_repo(&state, &stored);
    assert!(service::all_statuses(&state).is_empty());

    let reloaded = settings::load(&settings::settings_path(&playground.root));
    assert!(reloaded.repos.is_empty(), "移除必須寫回設定檔");
}

#[test]
fn removing_never_touches_the_repository_itself() {
    let playground = Playground::new("safe-remove");
    let repo_path = seed_repo(&playground, "demo");
    let state = playground.state();
    let stored = service::add_repo(&state, repo_path.to_str().unwrap()).expect("加入失敗");

    service::remove_repo(&state, &stored);

    // 從清單移除只影響設定，使用者的檔案必須完好。
    assert!(repo_path.join(".git").exists(), "repository 不得被刪除");
    assert!(repo_path.join("a.txt").exists(), "工作目錄檔案不得被刪除");
}
