//! 前端可呼叫的指令。
//!
//! 這一層只負責參數轉換與錯誤格式，實際邏輯都在 [`crate::service`]。

use tauri::State;

use crate::dto::{DivergenceDto, GraphDto, OpOutcomeDto, RepoStatusDto};
use crate::service::{self, AppState, DEFAULT_GRAPH_LIMIT};
use crate::settings::Settings;

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.snapshot_settings()
}

#[tauri::command]
pub fn update_settings(
    state: State<'_, AppState>,
    fetch_interval_secs: Option<u64>,
    notify_incoming: Option<bool>,
    background_fetch: Option<bool>,
) -> Settings {
    state.mutate_settings(|settings| {
        if let Some(interval) = fetch_interval_secs {
            settings.fetch_interval_secs = interval;
        }
        if let Some(notify) = notify_incoming {
            settings.notify_incoming = notify;
        }
        if let Some(enabled) = background_fetch {
            settings.background_fetch = enabled;
        }
    })
}

#[tauri::command]
pub fn list_repos(state: State<'_, AppState>) -> Vec<RepoStatusDto> {
    service::all_statuses(&state)
}

#[tauri::command]
pub fn add_repo(state: State<'_, AppState>, path: String) -> Result<Vec<RepoStatusDto>, String> {
    service::add_repo(&state, &path)?;
    Ok(service::all_statuses(&state))
}

#[tauri::command]
pub fn remove_repo(state: State<'_, AppState>, path: String) -> Vec<RepoStatusDto> {
    service::remove_repo(&state, &path);
    service::all_statuses(&state)
}

#[tauri::command]
pub fn repo_graph(path: String, limit: Option<usize>) -> Result<GraphDto, String> {
    service::graph_of(&path, limit.unwrap_or(DEFAULT_GRAPH_LIMIT))
}

#[tauri::command]
pub fn repo_divergence(path: String) -> Result<DivergenceDto, String> {
    service::divergence_of(&path)
}

/// 立即對單一 repository 執行 fetch。
#[tauri::command]
pub fn fetch_repo(state: State<'_, AppState>, path: String) -> RepoStatusDto {
    service::fetch_repo(&state, &path)
}

/// 立即對所有受監控的 repository 執行 fetch。
#[tauri::command]
pub fn fetch_all(state: State<'_, AppState>) -> Vec<RepoStatusDto> {
    let settings = state.snapshot_settings();
    for path in &settings.repos {
        service::fetch_repo(&state, path);
    }
    service::all_statuses(&state)
}

/// 一次取回工作區狀態：變更、分支、stash、衝突、還原點。
#[tauri::command]
pub fn repo_workspace(path: String) -> Result<crate::dto::WorkspaceDto, String> {
    service::workspace_of(&path)
}

#[tauri::command]
pub fn op_fast_forward(path: String) -> Result<OpOutcomeDto, String> {
    service::fast_forward(&path)
}

#[tauri::command]
pub fn op_rebase(path: String) -> Result<OpOutcomeDto, String> {
    service::rebase(&path)
}

#[tauri::command]
pub fn op_merge(path: String) -> Result<OpOutcomeDto, String> {
    service::merge(&path)
}

#[tauri::command]
pub fn op_push(path: String, force: Option<bool>) -> Result<OpOutcomeDto, String> {
    service::push(&path, force.unwrap_or(false))
}

#[tauri::command]
pub fn op_abort(path: String) -> Result<OpOutcomeDto, String> {
    service::abort(&path)
}

#[tauri::command]
pub fn op_undo(path: String, reference: String) -> Result<OpOutcomeDto, String> {
    service::undo(&path, &reference)
}

#[tauri::command]
pub fn op_stage(path: String, paths: Vec<String>) -> Result<OpOutcomeDto, String> {
    service::stage(&path, paths)
}

#[tauri::command]
pub fn op_unstage(path: String, paths: Vec<String>) -> Result<OpOutcomeDto, String> {
    service::unstage(&path, paths)
}

#[tauri::command]
pub fn op_commit(
    path: String,
    message: String,
    amend: Option<bool>,
) -> Result<OpOutcomeDto, String> {
    service::commit(&path, message, amend.unwrap_or(false))
}

#[tauri::command]
pub fn op_discard(path: String, paths: Vec<String>) -> Result<OpOutcomeDto, String> {
    service::discard(&path, paths)
}

#[tauri::command]
pub fn op_checkout(path: String, name: String) -> Result<OpOutcomeDto, String> {
    service::checkout_branch(&path, name)
}

#[tauri::command]
pub fn op_create_branch(path: String, name: String) -> Result<OpOutcomeDto, String> {
    service::create_branch(&path, name)
}

#[tauri::command]
pub fn op_stash_save(path: String, message: Option<String>) -> Result<OpOutcomeDto, String> {
    service::stash_save(&path, message.unwrap_or_default())
}

#[tauri::command]
pub fn op_stash_pop(path: String, index: usize) -> Result<OpOutcomeDto, String> {
    service::stash_pop(&path, index)
}

#[tauri::command]
pub fn op_stash_drop(path: String, index: usize) -> Result<OpOutcomeDto, String> {
    service::stash_drop(&path, index)
}

#[tauri::command]
pub fn op_resolve_conflict(
    path: String,
    file: String,
    side: Option<String>,
    content: Option<String>,
) -> Result<OpOutcomeDto, String> {
    service::resolve_conflict(&path, file, side, content)
}

#[tauri::command]
pub fn op_continue(path: String) -> Result<OpOutcomeDto, String> {
    service::continue_operation(&path)
}

#[tauri::command]
pub fn op_skip_step(path: String) -> Result<OpOutcomeDto, String> {
    service::skip_step(&path)
}
