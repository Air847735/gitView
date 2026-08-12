//! 前端可呼叫的指令。
//!
//! 這一層只負責參數轉換與錯誤格式，實際邏輯都在 [`crate::service`]。

use tauri::State;

use crate::dto::{DivergenceDto, GraphDto, RepoStatusDto};
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
