//! gitview 桌面應用程式。
//!
//! 常駐系統列，背景定期檢查遠端更新。所有資料留在本機，
//! 不使用任何後端服務，也不儲存憑證。

// Windows 下不要另外開一個主控台視窗。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use gitview_app::{commands, service, settings, watcher};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};

use service::AppState;

fn main() {
    tauri::Builder::default()
        // 只允許一個實例。關閉視窗只是隱藏、程序仍在背景執行，
        // 若不擋住重複啟動，使用者每次點圖示都會多開一個常駐程序。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let config_dir = app
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let state = AppState::new(settings::settings_path(&config_dir));
            app.manage(state);

            build_tray(app.handle())?;
            watcher::spawn(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            // 關閉視窗時只隱藏，程式繼續在系統列背景執行。
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::update_settings,
            commands::list_repos,
            commands::add_repo,
            commands::remove_repo,
            commands::repo_graph,
            commands::repo_divergence,
            commands::fetch_repo,
            commands::fetch_all,
            commands::repo_workspace,
            commands::op_fast_forward,
            commands::op_rebase,
            commands::op_merge,
            commands::op_push,
            commands::op_abort,
            commands::op_undo,
            commands::op_stage,
            commands::op_unstage,
            commands::op_commit,
            commands::op_discard,
            commands::op_checkout,
            commands::op_create_branch,
            commands::op_stash_save,
            commands::op_stash_pop,
            commands::op_stash_drop,
            commands::op_resolve_conflict,
            commands::op_continue,
            commands::op_skip_step,
            commands::repo_diff,
            commands::commit_detail,
            commands::file_history,
            commands::op_stage_selection,
            commands::op_unstage_selection,
            commands::ui_probe,
            commands::repo_search,
            commands::repo_blame,
            commands::op_delete_branch,
            commands::op_rename_branch,
            commands::op_set_upstream,
        ])
        .run(tauri::generate_context!())
        .expect("gitview 啟動失敗");
}

/// 建立系統列圖示與選單。
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "開啟 gitview", true, None::<&str>)?;
    let check = MenuItem::with_id(app, "check", "立即檢查全部", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "結束", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &check, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(
            app.default_window_icon()
                .cloned()
                .ok_or_else(|| tauri::Error::AssetNotFound("找不到預設視窗圖示".to_owned()))?,
        )
        .tooltip("gitview")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "check" => {
                let handle = app.clone();
                // fetch 會等待網路，不能擋住選單的回呼。
                std::thread::spawn(move || {
                    let state = handle.state::<AppState>();
                    let settings = state.snapshot_settings();
                    for path in &settings.repos {
                        service::fetch_repo(&state, path);
                    }
                    use tauri::Emitter;
                    let _ =
                        handle.emit(watcher::REPOS_UPDATED_EVENT, service::all_statuses(&state));
                });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}
