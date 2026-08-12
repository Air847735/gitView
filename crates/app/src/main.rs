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
