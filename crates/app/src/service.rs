//! 應用程式狀態與各項操作的實作。
//!
//! Tauri 指令與背景排程都呼叫這裡，避免同一段邏輯寫兩次。
//!
//! `git2::Repository` 不可跨執行緒共用，因此每次操作都重新開啟；
//! 對本機檔案而言成本很低，換來的是不需要處理共享狀態的正確性問題。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use git2::Repository;
use gitview_core::{
    conflict, diff, divergence, fetch, lay_out, ops, repo as core_repo, search, status, workspace,
};

use crate::dto::{
    BlameLineDto, BranchDto, CommitDetailDto, ConflictFileDto, DivergenceDto, FetchStateDto,
    FileChangeDto, FileDiffDto, GraphDto, OpOutcomeDto, RepoStatusDto, SafetyPointDto,
    SearchHitDto, StashDto, WorkspaceDto,
};
use crate::settings::{self, Settings};

/// 單一 repository 歷史圖預設載入的列數。
///
/// 介面一次看得到的遠少於此；設上限是為了避免超大型 repository
/// 在開啟時就付出完整佈局的成本。
pub const DEFAULT_GRAPH_LIMIT: usize = 500;

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub settings_path: PathBuf,
    /// 每個 repository 上一次 fetch 的結果。
    pub fetch_states: Mutex<HashMap<String, FetchStateDto>>,
}

impl AppState {
    pub fn new(settings_path: PathBuf) -> Self {
        let settings = settings::load(&settings_path);
        Self {
            settings: Mutex::new(settings),
            settings_path,
            fetch_states: Mutex::new(HashMap::new()),
        }
    }

    pub fn snapshot_settings(&self) -> Settings {
        self.settings.lock().expect("設定鎖已中毒").clone()
    }

    /// 修改設定並立即寫回檔案。
    pub fn mutate_settings<F>(&self, change: F) -> Settings
    where
        F: FnOnce(&mut Settings),
    {
        let mut guard = self.settings.lock().expect("設定鎖已中毒");
        change(&mut guard);
        guard.normalise();
        if let Err(error) = settings::save(&self.settings_path, &guard) {
            // 寫入失敗不應讓操作中斷，但必須留下痕跡。
            eprintln!("無法寫入設定檔 {}：{error}", self.settings_path.display());
        }
        guard.clone()
    }

    fn fetch_state(&self, path: &str) -> Option<FetchStateDto> {
        self.fetch_states
            .lock()
            .expect("fetch 狀態鎖已中毒")
            .get(path)
            .cloned()
    }

    fn record_fetch_state(&self, path: &str, state: FetchStateDto) {
        self.fetch_states
            .lock()
            .expect("fetch 狀態鎖已中毒")
            .insert(path.to_owned(), state);
    }
}

fn now_millis() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_millis() as u64)
}

fn open(path: &str) -> Result<Repository, String> {
    Repository::discover(path).map_err(|error| format!("無法開啟 repository：{}", error.message()))
}

/// 讀取單一 repository 的狀態，失敗時回傳可顯示的佔位項目。
pub fn status_of(state: &AppState, path: &str) -> RepoStatusDto {
    match open(path) {
        Ok(repository) => match status::status(&repository) {
            Ok(status) => RepoStatusDto::from_status(&status, state.fetch_state(path)),
            Err(error) => RepoStatusDto::unreadable(path, format!("{error:#}")),
        },
        Err(error) => RepoStatusDto::unreadable(path, error),
    }
}

/// 所有受監控 repository 的狀態，需要處理的排在前面。
pub fn all_statuses(state: &AppState) -> Vec<RepoStatusDto> {
    let settings = state.snapshot_settings();
    let mut statuses: Vec<RepoStatusDto> = settings
        .repos
        .iter()
        .map(|path| status_of(state, path))
        .collect();

    // 依需要注意的程度排序，同級再依名稱，讓順序穩定。
    statuses.sort_by(|left, right| {
        attention_rank(&right.attention)
            .cmp(&attention_rank(&left.attention))
            .then_with(|| left.name.cmp(&right.name))
    });
    statuses
}

/// 數字越大越需要注意。
fn attention_rank(attention: &str) -> u8 {
    match attention {
        "attention" => 5,
        "diverged" => 4,
        "incoming" => 3,
        "unpushed" => 2,
        "uncommitted" => 1,
        _ => 0,
    }
}

/// 加入一個 repository。路徑必須確實是 git repository。
pub fn add_repo(state: &AppState, path: &str) -> Result<String, String> {
    let repository = open(path)?;
    // 統一存工作目錄的路徑，避免同一個 repository 以不同寫法重複加入。
    let canonical = repository
        .workdir()
        .unwrap_or_else(|| repository.path())
        .to_string_lossy()
        .trim_end_matches('/')
        .to_owned();

    let mut added = false;
    state.mutate_settings(|settings| {
        added = settings.add_repo(canonical.clone());
    });
    if added {
        Ok(canonical)
    } else {
        Err("這個 repository 已經在清單中".to_owned())
    }
}

pub fn remove_repo(state: &AppState, path: &str) {
    state.mutate_settings(|settings| {
        settings.remove_repo(path);
    });
    state
        .fetch_states
        .lock()
        .expect("fetch 狀態鎖已中毒")
        .remove(path);
}

/// 讀出歷史圖。
pub fn graph_of(path: &str, limit: usize) -> Result<GraphDto, String> {
    let repository = open(path)?;
    let graph = core_repo::load_graph(&repository).map_err(|error| format!("{error:#}"))?;
    let labels = core_repo::ref_labels(&repository).map_err(|error| format!("{error:#}"))?;
    let layout = lay_out(&graph).map_err(|error| format!("無法計算佈局：{error}"))?;
    Ok(GraphDto::from_layout(&graph, &layout, &labels, limit))
}

/// 分岔分析。
pub fn divergence_of(path: &str) -> Result<DivergenceDto, String> {
    let repository = open(path)?;
    let divergence = divergence::analyse(&repository).map_err(|error| format!("{error:#}"))?;
    Ok(DivergenceDto::from(&divergence))
}

/// 對單一 repository 執行 fetch。
///
/// Fetch 只更新遠端追蹤分支，不會動到本機分支或工作目錄。
pub fn fetch_repo(state: &AppState, path: &str) -> RepoStatusDto {
    let outcome = (|| -> Result<Option<usize>, String> {
        let repository = open(path)?;
        let Some(remote) = fetch::default_remote(&repository) else {
            return Err("這個 repository 沒有設定遠端".to_owned());
        };
        match fetch::fetch(&repository, &remote) {
            Ok(report) => Ok(Some(report.received_objects)),
            Err(failure) => Err(failure.headline()),
        }
    })();

    let fetch_state = match &outcome {
        Ok(Some(received)) => FetchStateDto {
            ok: true,
            message: if *received > 0 {
                format!("已取得 {received} 個新物件")
            } else {
                "已是最新".to_owned()
            },
            kind: None,
            at_millis: now_millis(),
        },
        Ok(None) => FetchStateDto {
            ok: true,
            message: "已是最新".to_owned(),
            kind: None,
            at_millis: now_millis(),
        },
        Err(message) => FetchStateDto {
            ok: false,
            message: message.clone(),
            kind: Some("error".to_owned()),
            at_millis: now_millis(),
        },
    };

    state.record_fetch_state(path, fetch_state);
    status_of(state, path)
}

/// 還原點的保留數量。超過的會在每次建立新還原點後自動清除。
///
/// 還原點會讓被丟棄的 commit 無法被回收，不設上限會無限累積。
pub const KEEP_UNDO_POINTS: usize = 20;

fn open_mut(path: &str) -> Result<Repository, String> {
    open(path)
}

/// 一次取回單一 repository 的工作區狀態。
pub fn workspace_of(path: &str) -> Result<WorkspaceDto, String> {
    let mut repository = open_mut(path)?;

    let changes = workspace::changes(&repository)
        .map_err(|error| format!("{error:#}"))?
        .iter()
        .map(FileChangeDto::from)
        .collect();
    let branches = workspace::branches(&repository)
        .map_err(|error| format!("{error:#}"))?
        .iter()
        .map(BranchDto::from)
        .collect();
    let conflicts = conflict::conflicts(&repository)
        .map_err(|error| format!("{error:#}"))?
        .iter()
        .map(ConflictFileDto::from)
        .collect();
    let undo_points = ops::list_safety_points(&repository)
        .map_err(|error| format!("{error:#}"))?
        .iter()
        .map(SafetyPointDto::from)
        .collect();
    let operation = status::status(&repository)
        .map_err(|error| format!("{error:#}"))?
        .operation;
    let stashes = workspace::stashes(&mut repository)
        .map_err(|error| format!("{error:#}"))?
        .into_iter()
        .map(|entry| StashDto {
            index: entry.index,
            message: entry.message,
        })
        .collect();

    Ok(WorkspaceDto {
        changes,
        branches,
        stashes,
        conflicts,
        operation,
        undo_points,
        side_labels: conflict::side_labels(&repository).into(),
    })
}

/// 執行一個會改動 repository 的操作。
///
/// 所有這類操作都經由這裡，統一在成功後清理過舊的還原點。
fn run_op<F>(path: &str, action: F) -> Result<OpOutcomeDto, String>
where
    F: FnOnce(&mut Repository) -> anyhow::Result<gitview_core::ops::OpOutcome>,
{
    let mut repository = open_mut(path)?;
    let outcome = action(&mut repository).map_err(|error| format!("{error:#}"))?;
    let _ = ops::prune_safety_points(&repository, KEEP_UNDO_POINTS);
    Ok(OpOutcomeDto::from(outcome))
}

pub fn fast_forward(path: &str) -> Result<OpOutcomeDto, String> {
    run_op(path, |repo| ops::fast_forward(repo))
}

pub fn rebase(path: &str) -> Result<OpOutcomeDto, String> {
    run_op(path, |repo| ops::rebase_onto_upstream(repo))
}

pub fn merge(path: &str) -> Result<OpOutcomeDto, String> {
    run_op(path, |repo| ops::merge_upstream(repo))
}

pub fn push(path: &str, force: bool) -> Result<OpOutcomeDto, String> {
    run_op(path, |repo| ops::push(repo, force))
}

pub fn abort(path: &str) -> Result<OpOutcomeDto, String> {
    run_op(path, |repo| ops::abort_operation(repo))
}

pub fn undo(path: &str, reference: &str) -> Result<OpOutcomeDto, String> {
    let reference = reference.to_owned();
    run_op(path, move |repo| ops::undo_to(repo, &reference))
}

pub fn stage(path: &str, paths: Vec<String>) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| workspace::stage(repo, &paths))
}

pub fn unstage(path: &str, paths: Vec<String>) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| workspace::unstage(repo, &paths))
}

pub fn commit(path: &str, message: String, amend: bool) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| workspace::commit(repo, &message, amend))
}

pub fn discard(path: &str, paths: Vec<String>) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| workspace::discard(repo, &paths))
}

pub fn checkout_branch(path: &str, name: String) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| workspace::checkout_branch(repo, &name))
}

pub fn create_branch(path: &str, name: String) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| workspace::create_branch(repo, &name))
}

pub fn stash_save(path: &str, message: String) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| workspace::stash_save(repo, &message))
}

pub fn stash_pop(path: &str, index: usize) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| workspace::stash_pop(repo, index))
}

pub fn stash_drop(path: &str, index: usize) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| workspace::stash_drop(repo, index))
}

pub fn resolve_conflict(
    path: &str,
    file: String,
    side: Option<String>,
    content: Option<String>,
) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| match (side.as_deref(), content) {
        (Some("ours"), _) => conflict::resolve_using(repo, &file, conflict::Side::Ours),
        (Some("theirs"), _) => conflict::resolve_using(repo, &file, conflict::Side::Theirs),
        (_, Some(text)) => conflict::resolve_with_content(repo, &file, &text),
        _ => anyhow::bail!("必須指定採用哪一方，或提供合併後的內容"),
    })
}

pub fn continue_operation(path: &str) -> Result<OpOutcomeDto, String> {
    run_op(path, |repo| conflict::continue_operation(repo))
}

pub fn skip_step(path: &str) -> Result<OpOutcomeDto, String> {
    run_op(path, |repo| conflict::skip_current_step(repo))
}

/// 工作目錄或索引的差異。未暫存的差異會標出與即將進來的變更相撞的區段。
pub fn diff_of(path: &str, source: &str) -> Result<Vec<FileDiffDto>, String> {
    let repository = open(path)?;
    let kind = match source {
        "staged" => diff::DiffSource::Staged,
        _ => diff::DiffSource::Unstaged,
    };
    let mut diffs =
        diff::workspace_diff(&repository, kind).map_err(|error| format!("{error:#}"))?;

    if kind == diff::DiffSource::Unstaged {
        // 撞擊標記只對尚未提交的變更有意義：那是使用者還能調整的部分。
        if let Ok(incoming) = diff::incoming_line_ranges(&repository) {
            diff::mark_incoming_collisions(&mut diffs, &incoming);
        }
    }
    Ok(diffs.iter().map(FileDiffDto::from).collect())
}

/// 單一 commit 的詳細內容與它改動的檔案。
pub fn commit_detail_of(path: &str, oid: &str) -> Result<CommitDetailDto, String> {
    let repository = open(path)?;
    let id = git2::Oid::from_str(oid).map_err(|_| "commit 識別碼格式錯誤".to_owned())?;
    let commit = repository
        .find_commit(id)
        .map_err(|_| "找不到該 commit".to_owned())?;

    let signature = commit.author();
    let author = match signature.name() {
        Ok(name) => name.to_owned(),
        Err(_) => String::from_utf8_lossy(signature.name_bytes()).into_owned(),
    };
    let email = match signature.email() {
        Ok(value) => value.to_owned(),
        Err(_) => String::from_utf8_lossy(signature.email_bytes()).into_owned(),
    };
    let summary = match commit.summary() {
        Ok(Some(text)) => text.to_owned(),
        _ => String::new(),
    };
    let body = match commit.body() {
        Ok(Some(text)) => text.to_owned(),
        _ => String::new(),
    };

    let files = diff::commit_diff(&repository, oid)
        .map_err(|error| format!("{error:#}"))?
        .iter()
        .map(FileDiffDto::from)
        .collect();

    Ok(CommitDetailDto {
        short_oid: oid.chars().take(8).collect(),
        oid: oid.to_owned(),
        summary,
        body,
        author,
        email,
        timestamp: commit.time().seconds(),
        parents: commit.parent_ids().map(|id| id.to_string()).collect(),
        files,
    })
}

/// 觸及某個檔案的 commit 清單。
pub fn file_history_of(path: &str, file: &str, limit: usize) -> Result<Vec<String>, String> {
    let repository = open(path)?;
    diff::file_history(&repository, file, limit).map_err(|error| format!("{error:#}"))
}

fn to_line_refs(selection: Vec<(usize, usize)>) -> Vec<workspace::LineRef> {
    selection
        .into_iter()
        .map(|(hunk, line)| workspace::LineRef { hunk, line })
        .collect()
}

pub fn stage_selection(
    path: &str,
    file: String,
    selection: Vec<(usize, usize)>,
) -> Result<OpOutcomeDto, String> {
    let refs = to_line_refs(selection);
    run_op(path, move |repo| {
        workspace::stage_selection(repo, &file, &refs)
    })
}

pub fn unstage_selection(
    path: &str,
    file: String,
    selection: Vec<(usize, usize)>,
) -> Result<OpOutcomeDto, String> {
    let refs = to_line_refs(selection);
    run_op(path, move |repo| {
        workspace::unstage_selection(repo, &file, &refs)
    })
}

/// 搜尋歷史。內容比對成本高，因此由呼叫端明確開啟。
pub fn search_in(
    path: &str,
    needle: String,
    include_content: bool,
) -> Result<Vec<SearchHitDto>, String> {
    let repository = open(path)?;
    let scope = search::SearchScope {
        content: include_content,
        ..search::SearchScope::default()
    };
    // 掃描上限避免在大型 repository 上無限期執行；目標規模遠低於此。
    search::search(&repository, &needle, scope, 100, 5_000)
        .map_err(|error| format!("{error:#}"))
        .map(|hits| hits.iter().map(SearchHitDto::from).collect())
}

/// 逐行標出檔案每一行的來源。
pub fn blame_of(path: &str, file: &str) -> Result<Vec<BlameLineDto>, String> {
    let repository = open(path)?;
    search::blame(&repository, file, 5_000)
        .map_err(|error| format!("{error:#}"))
        .map(|lines| lines.iter().map(BlameLineDto::from).collect())
}

pub fn delete_branch(path: &str, name: String) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| workspace::delete_branch(repo, &name))
}

pub fn rename_branch(path: &str, from: String, to: String) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| workspace::rename_branch(repo, &from, &to))
}

pub fn set_upstream(
    path: &str,
    name: String,
    upstream: Option<String>,
) -> Result<OpOutcomeDto, String> {
    run_op(path, move |repo| {
        workspace::set_upstream(repo, &name, upstream.as_deref())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_ordering_puts_problems_first() {
        assert!(attention_rank("attention") > attention_rank("diverged"));
        assert!(attention_rank("diverged") > attention_rank("incoming"));
        assert!(attention_rank("incoming") > attention_rank("unpushed"));
        assert!(attention_rank("unpushed") > attention_rank("uncommitted"));
        assert!(attention_rank("uncommitted") > attention_rank("clean"));
    }

    #[test]
    fn unknown_attention_sorts_last() {
        assert_eq!(attention_rank("something-else"), 0);
    }
}
