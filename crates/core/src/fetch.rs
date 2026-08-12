//! 從遠端取得更新。
//!
//! Fetch 只更新遠端追蹤分支（`refs/remotes/*`），不會動到使用者的本機分支、
//! 工作目錄或未提交的內容，因此是背景可以自動執行的操作。
//! 任何會改動本機工作成果的操作（pull、merge、rebase）不放在這裡。
//!
//! 認證委由系統既有的機制：SSH 走 ssh-agent 或預設路徑的金鑰，
//! HTTPS 走 git 的 credential helper。本模組不儲存也不要求任何憑證。

use std::time::SystemTime;

use git2::{AutotagOption, Cred, CredentialType, ErrorClass, ErrorCode, FetchOptions, Repository};

/// Fetch 成功後的結果。
#[derive(Debug, Clone)]
pub struct FetchReport {
    pub remote: String,
    /// 收到的物件數量；為 0 表示本來就是最新的。
    pub received_objects: usize,
    pub at: SystemTime,
}

impl FetchReport {
    pub fn brought_anything(&self) -> bool {
        self.received_objects > 0
    }
}

/// Fetch 失敗的原因。
///
/// 分類的目的是讓介面能給出可行動的訊息 —— 認證失敗與網路不通
/// 需要使用者做的事完全不同。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchFailure {
    /// 沒有設定該遠端。
    NoRemote(String),
    /// 認證失敗。最常見的原因是 SSH 金鑰未解鎖或未加入 agent。
    Authentication(String),
    /// 無法連線到遠端主機。
    Network(String),
    Other(String),
}

impl FetchFailure {
    /// 給介面用的穩定識別字串。
    pub fn as_str(&self) -> &'static str {
        match self {
            FetchFailure::NoRemote(_) => "no-remote",
            FetchFailure::Authentication(_) => "auth",
            FetchFailure::Network(_) => "network",
            FetchFailure::Other(_) => "other",
        }
    }

    /// 一句話說明狀況，以及使用者可以做什麼。
    pub fn headline(&self) -> String {
        match self {
            FetchFailure::NoRemote(name) => format!("找不到遠端「{name}」"),
            FetchFailure::Authentication(_) => {
                "認證失敗，請確認 SSH 金鑰已加入 ssh-agent 並已解鎖".to_owned()
            }
            FetchFailure::Network(_) => "無法連線到遠端主機".to_owned(),
            FetchFailure::Other(message) => message.clone(),
        }
    }

    /// 底層的原始訊息，供診斷使用。
    pub fn detail(&self) -> &str {
        match self {
            FetchFailure::NoRemote(detail)
            | FetchFailure::Authentication(detail)
            | FetchFailure::Network(detail)
            | FetchFailure::Other(detail) => detail,
        }
    }
}

impl std::fmt::Display for FetchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.headline())
    }
}

impl std::error::Error for FetchFailure {}

fn classify(error: &git2::Error) -> FetchFailure {
    let detail = error.message().to_owned();
    match (error.class(), error.code()) {
        (_, ErrorCode::Auth) => FetchFailure::Authentication(detail),
        (ErrorClass::Ssh, _) => FetchFailure::Authentication(detail),
        (ErrorClass::Net, _) | (ErrorClass::Http, _) => FetchFailure::Network(detail),
        (_, ErrorCode::Certificate) => FetchFailure::Network(detail),
        _ => FetchFailure::Other(detail),
    }
}

/// 依遠端允許的認證方式，交出系統既有的憑證。
///
/// 每種方式只嘗試一次：libgit2 會重複呼叫此函式直到成功或放棄，
/// 若同一種方式無限重試會造成迴圈。
pub(crate) fn make_credentials_callback(
) -> impl FnMut(&str, Option<&str>, CredentialType) -> Result<Cred, git2::Error> {
    let mut tried_agent = false;
    let mut tried_helper = false;

    move |url: &str, username_from_url: Option<&str>, allowed: CredentialType| {
        let username = username_from_url.unwrap_or("git");

        if allowed.contains(CredentialType::SSH_KEY) && !tried_agent {
            tried_agent = true;
            return Cred::ssh_key_from_agent(username);
        }
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) && !tried_helper {
            tried_helper = true;
            let config = git2::Config::open_default()?;
            return Cred::credential_helper(&config, url, username_from_url);
        }
        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(username);
        }
        Err(git2::Error::from_str(
            "沒有可用的認證方式；SSH 請確認金鑰已加入 ssh-agent",
        ))
    }
}

/// 對指定的遠端執行 fetch。
///
/// 這會更新遠端追蹤分支，不會改變本機分支或工作目錄。
pub fn fetch(repo: &Repository, remote_name: &str) -> Result<FetchReport, FetchFailure> {
    let mut remote = repo
        .find_remote(remote_name)
        .map_err(|_| FetchFailure::NoRemote(remote_name.to_owned()))?;

    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(make_credentials_callback());

    let mut options = FetchOptions::new();
    options
        .remote_callbacks(callbacks)
        // 不自動下載標籤以外的額外內容，背景執行要盡量輕。
        .download_tags(AutotagOption::Auto)
        .prune(git2::FetchPrune::On);

    // 傳入空的 refspec 表示採用遠端本身設定的預設 refspec。
    let refspecs: [&str; 0] = [];
    remote
        .fetch(&refspecs, Some(&mut options), None)
        .map_err(|error| classify(&error))?;

    let stats = remote.stats();
    Ok(FetchReport {
        remote: remote_name.to_owned(),
        received_objects: stats.received_objects(),
        at: SystemTime::now(),
    })
}

/// 取得預設遠端的名稱。
///
/// 優先使用目前分支追蹤的遠端；沒有時退回 `origin`，再沒有就取第一個。
pub fn default_remote(repo: &Repository) -> Option<String> {
    if let Ok(head) = repo.head() {
        if let Ok(branch_name) = head.shorthand() {
            if let Ok(buffer) = repo.branch_upstream_remote(&format!("refs/heads/{branch_name}")) {
                if let Ok(name) = buffer.as_str() {
                    return Some(name.to_owned());
                }
            }
        }
    }
    let remotes = repo.remotes().ok()?;
    let names: Vec<&str> = remotes
        .iter()
        .filter_map(|name| name.ok().flatten())
        .collect();
    if names.contains(&"origin") {
        return Some("origin".to_owned());
    }
    names.first().map(|name| (*name).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_errors_are_distinguished_from_network_errors() {
        let auth = git2::Error::new(ErrorCode::Auth, ErrorClass::Http, "denied");
        assert!(matches!(classify(&auth), FetchFailure::Authentication(_)));

        let network = git2::Error::new(ErrorCode::GenericError, ErrorClass::Net, "unreachable");
        assert!(matches!(classify(&network), FetchFailure::Network(_)));
    }

    #[test]
    fn ssh_failures_are_treated_as_authentication_problems() {
        let ssh = git2::Error::new(ErrorCode::GenericError, ErrorClass::Ssh, "no key");
        assert!(matches!(classify(&ssh), FetchFailure::Authentication(_)));
    }

    #[test]
    fn unknown_failures_keep_their_message() {
        let other = git2::Error::new(ErrorCode::GenericError, ErrorClass::Repository, "broken");
        let failure = classify(&other);
        assert_eq!(failure.as_str(), "other");
        assert_eq!(failure.detail(), "broken");
    }

    #[test]
    fn authentication_headline_tells_the_user_what_to_do() {
        let failure = FetchFailure::Authentication("denied".to_owned());
        assert!(failure.headline().contains("ssh-agent"));
        // 原始訊息仍保留供診斷。
        assert_eq!(failure.detail(), "denied");
    }

    #[test]
    fn a_report_knows_whether_anything_arrived() {
        let empty = FetchReport {
            remote: "origin".to_owned(),
            received_objects: 0,
            at: SystemTime::now(),
        };
        assert!(!empty.brought_anything());
    }
}
