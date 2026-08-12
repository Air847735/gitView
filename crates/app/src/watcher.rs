//! 背景排程：定期 fetch，並在有新內容時通知。
//!
//! 通知的觸發條件刻意分兩級。全部都通知很快就會變成雜訊，
//! 使用者關掉通知之後這個功能就等於不存在：
//!
//! - 遠端有新內容 → 更新畫面即可，除非使用者選擇要通知
//! - 進來的變更會碰到本機未提交的檔案 → 一定通知，這是真的會出事的情況

use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::service::{self, AppState};

/// 狀態更新時送給前端的事件名稱。
pub const REPOS_UPDATED_EVENT: &str = "repos-updated";

/// 背景執行緒每次醒來之間最多睡這麼久。
///
/// 設定改變時不必等完整的 fetch 間隔就能生效。
const TICK: Duration = Duration::from_secs(15);

/// 啟動背景排程。
pub fn spawn(app: AppHandle) {
    thread::spawn(move || {
        // 記錄上次看到的落後數量，用來判斷「是不是新出現的」。
        let mut previous_behind: HashMap<String, usize> = HashMap::new();
        let mut elapsed = Duration::ZERO;

        loop {
            thread::sleep(TICK);
            elapsed += TICK;

            let state = app.state::<AppState>();
            let settings = state.snapshot_settings();

            if !settings.background_fetch || settings.repos.is_empty() {
                elapsed = Duration::ZERO;
                continue;
            }
            if elapsed < Duration::from_secs(settings.fetch_interval_secs) {
                continue;
            }
            elapsed = Duration::ZERO;

            for path in &settings.repos {
                service::fetch_repo(&state, path);
            }

            let statuses = service::all_statuses(&state);
            notify_if_needed(
                &app,
                &statuses,
                &mut previous_behind,
                settings.notify_incoming,
            );

            // 前端可能沒開著，送不出去不是錯誤。
            let _ = app.emit(REPOS_UPDATED_EVENT, &statuses);
        }
    });
}

/// 依照上一段說明的兩級規則決定要不要發通知。
fn notify_if_needed(
    app: &AppHandle,
    statuses: &[crate::dto::RepoStatusDto],
    previous_behind: &mut HashMap<String, usize>,
    notify_incoming: bool,
) {
    for status in statuses {
        let before = previous_behind
            .insert(status.path.clone(), status.behind)
            .unwrap_or(0);

        // 只在落後數量增加時通知，否則每次檢查都會重複提醒同一件事。
        if status.behind == 0 || status.behind <= before {
            continue;
        }

        let arrived = status.behind - before;
        let title = format!("{}：有 {arrived} 個新 commit", status.name);
        let body = if status.working_tree.total > 0 {
            format!(
                "分支 {}。本機有 {} 個未提交的變更，拉取前建議先檢視。",
                status.branch.as_deref().unwrap_or("(未知)"),
                status.working_tree.total
            )
        } else {
            format!("分支 {}", status.branch.as_deref().unwrap_or("(未知)"))
        };

        let worth_interrupting = notify_incoming || status.working_tree.total > 0;
        if !worth_interrupting {
            continue;
        }

        let _ = app.notification().builder().title(title).body(body).show();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{RepoStatusDto, WorkingTreeDto};

    fn status(path: &str, behind: usize, dirty: usize) -> RepoStatusDto {
        RepoStatusDto {
            path: path.to_owned(),
            name: path.to_owned(),
            branch: Some("main".to_owned()),
            upstream: Some("origin/main".to_owned()),
            ahead: 0,
            behind,
            working_tree: WorkingTreeDto {
                staged: 0,
                unstaged: dirty,
                untracked: 0,
                conflicted: 0,
                total: dirty,
            },
            operation: None,
            attention: "incoming".to_owned(),
            last_fetch_millis: None,
            error: None,
            fetch_state: None,
        }
    }

    /// 通知的判斷邏輯與 Tauri 無關，抽出來單獨驗證。
    fn should_notify(before: usize, now: usize, dirty: usize, notify_incoming: bool) -> bool {
        if now == 0 || now <= before {
            return false;
        }
        notify_incoming || dirty > 0
    }

    #[test]
    fn repeated_checks_do_not_repeat_the_same_notification() {
        assert!(should_notify(0, 3, 0, true));
        // 同樣落後 3 個，第二次檢查不應再通知。
        assert!(!should_notify(3, 3, 0, true));
    }

    #[test]
    fn further_commits_trigger_a_new_notification() {
        assert!(should_notify(3, 5, 0, true));
    }

    #[test]
    fn nothing_to_pull_means_no_notification() {
        assert!(!should_notify(0, 0, 0, true));
    }

    #[test]
    fn dirty_working_tree_notifies_even_when_switched_off() {
        // 使用者關掉了一般通知，但本機有未提交的變更時仍要提醒。
        assert!(!should_notify(0, 2, 0, false));
        assert!(should_notify(0, 2, 1, false));
    }

    #[test]
    fn previous_values_are_recorded_for_every_repository() {
        let mut previous = HashMap::new();
        let statuses = vec![status("/a", 2, 0), status("/b", 0, 0)];
        for entry in &statuses {
            previous.insert(entry.path.clone(), entry.behind);
        }
        assert_eq!(previous.get("/a"), Some(&2));
        assert_eq!(previous.get("/b"), Some(&0));
    }
}
