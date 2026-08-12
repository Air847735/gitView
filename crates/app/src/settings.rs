//! 使用者設定的讀寫。
//!
//! 設定只存在本機，不含任何憑證：認證一律委由系統的 ssh-agent 與
//! git credential helper 處理。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 背景檢查的預設間隔。
pub const DEFAULT_FETCH_INTERVAL_SECS: u64 = 900;

/// 允許的最短間隔，避免對遠端造成不必要的負擔。
pub const MIN_FETCH_INTERVAL_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 受監控的 repository 路徑。
    pub repos: Vec<String>,
    /// 背景 fetch 的間隔秒數。
    pub fetch_interval_secs: u64,
    /// 是否在背景 fetch 帶回新內容時發出系統通知。
    pub notify_incoming: bool,
    /// 是否啟用背景 fetch。
    pub background_fetch: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            repos: Vec::new(),
            fetch_interval_secs: DEFAULT_FETCH_INTERVAL_SECS,
            notify_incoming: true,
            background_fetch: true,
        }
    }
}

impl Settings {
    /// 修正超出合理範圍的值。
    pub fn normalise(&mut self) {
        if self.fetch_interval_secs < MIN_FETCH_INTERVAL_SECS {
            self.fetch_interval_secs = MIN_FETCH_INTERVAL_SECS;
        }
        self.repos.sort();
        self.repos.dedup();
    }

    pub fn add_repo(&mut self, path: String) -> bool {
        if self.repos.contains(&path) {
            return false;
        }
        self.repos.push(path);
        self.repos.sort();
        true
    }

    pub fn remove_repo(&mut self, path: &str) -> bool {
        let before = self.repos.len();
        self.repos.retain(|existing| existing != path);
        self.repos.len() != before
    }
}

/// 設定檔位置。
pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

/// 讀取設定。檔案不存在或損壞時回傳預設值，不讓應用程式無法啟動。
pub fn load(path: &Path) -> Settings {
    let mut settings = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Settings>(&text).ok())
        .unwrap_or_default();
    settings.normalise();
    settings
}

/// 寫回設定。
pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(settings)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_below_the_minimum_is_raised() {
        let mut settings = Settings {
            fetch_interval_secs: 5,
            ..Settings::default()
        };
        settings.normalise();
        assert_eq!(settings.fetch_interval_secs, MIN_FETCH_INTERVAL_SECS);
    }

    #[test]
    fn duplicate_repositories_are_removed() {
        let mut settings = Settings {
            repos: vec!["/b".into(), "/a".into(), "/a".into()],
            ..Settings::default()
        };
        settings.normalise();
        assert_eq!(settings.repos, vec!["/a".to_owned(), "/b".to_owned()]);
    }

    #[test]
    fn adding_an_existing_repository_reports_no_change() {
        let mut settings = Settings::default();
        assert!(settings.add_repo("/a".into()));
        assert!(!settings.add_repo("/a".into()));
        assert_eq!(settings.repos.len(), 1);
    }

    #[test]
    fn removing_reports_whether_anything_changed() {
        let mut settings = Settings::default();
        settings.add_repo("/a".into());
        assert!(settings.remove_repo("/a"));
        assert!(!settings.remove_repo("/a"));
    }

    #[test]
    fn a_missing_file_yields_defaults() {
        let settings = load(Path::new("/nonexistent/gitview/settings.json"));
        assert!(settings.repos.is_empty());
        assert_eq!(settings.fetch_interval_secs, DEFAULT_FETCH_INTERVAL_SECS);
    }

    #[test]
    fn settings_round_trip_through_a_file() {
        let dir = std::env::temp_dir().join(format!("gitview-settings-{}", std::process::id()));
        let path = settings_path(&dir);
        let mut original = Settings::default();
        original.add_repo("/srv/projects/gitview".into());
        original.notify_incoming = false;

        save(&path, &original).expect("無法寫入設定");
        let loaded = load(&path);
        assert_eq!(loaded.repos, original.repos);
        assert!(!loaded.notify_incoming);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
