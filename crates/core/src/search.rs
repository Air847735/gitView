//! 搜尋 commit 與 blame。
//!
//! 兩者都是「從歷史裡找東西」，共用走訪與過濾的骨架，因此放在同一個模組。
//! 只讀取，不修改。

use anyhow::{Context, Result};
use git2::{BlameOptions, DiffOptions, Oid, Repository, Sort};

/// 搜尋的比對範圍。至少要開啟一項。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchScope {
    pub message: bool,
    pub author: bool,
    pub path: bool,
    /// 比對變更的內容。逐個 commit 比對差異，成本遠高於其他項目。
    pub content: bool,
}

impl Default for SearchScope {
    fn default() -> Self {
        Self {
            message: true,
            author: true,
            path: true,
            content: false,
        }
    }
}

impl SearchScope {
    pub fn any(&self) -> bool {
        self.message || self.author || self.path || self.content
    }
}

/// 一筆搜尋結果。
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub oid: String,
    pub short_oid: String,
    pub summary: String,
    pub author: String,
    pub timestamp: i64,
    /// 命中的位置：`message`、`author`、`path`、`content`。
    pub matched: Vec<&'static str>,
    /// 命中的檔案路徑，最多數筆，供介面顯示。
    pub paths: Vec<String>,
}

fn commit_text(commit: &git2::Commit<'_>) -> (String, String) {
    let signature = commit.author();
    let author = match signature.name() {
        Ok(name) => name.to_owned(),
        Err(_) => String::from_utf8_lossy(signature.name_bytes()).into_owned(),
    };
    let message = String::from_utf8_lossy(commit.message_bytes()).into_owned();
    (author, message)
}

/// 這個 commit 是否讓 `needle` 的出現次數改變。
///
/// 這是 `git log -S` 的語意：找的是「引入或移除這段文字」的 commit，
/// 而不是「內容裡曾經出現過」的 commit。後者幾乎每個 commit 都會命中。
fn changes_occurrences(repo: &Repository, commit: &git2::Commit<'_>, needle: &str) -> bool {
    let Ok(tree) = commit.tree() else {
        return false;
    };
    let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
    let mut options = DiffOptions::new();
    options.context_lines(0);
    let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options))
    else {
        return false;
    };

    let mut delta = 0i64;
    let _ = diff.foreach(
        &mut |_, _| true,
        None,
        None,
        Some(&mut |_, _, line| {
            let text = String::from_utf8_lossy(line.content());
            if text.contains(needle) {
                match line.origin() {
                    '+' => delta += 1,
                    '-' => delta -= 1,
                    _ => {}
                }
            }
            true
        }),
    );
    delta != 0
}

fn touched_paths(repo: &Repository, commit: &git2::Commit<'_>, limit: usize) -> Vec<String> {
    let Ok(tree) = commit.tree() else {
        return Vec::new();
    };
    let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
    let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else {
        return Vec::new();
    };
    diff.deltas()
        .filter_map(|delta| {
            delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|path| path.to_string_lossy().into_owned())
        })
        .take(limit)
        .collect()
}

/// 搜尋歷史。
///
/// `scan_limit` 是最多走訪幾個 commit，避免在大型 repository 上無限期執行；
/// `limit` 是最多回傳幾筆結果。
pub fn search(
    repo: &Repository,
    needle: &str,
    scope: SearchScope,
    limit: usize,
    scan_limit: usize,
) -> Result<Vec<SearchHit>> {
    let needle = needle.trim();
    if needle.is_empty() || !scope.any() {
        return Ok(Vec::new());
    }
    let lowered = needle.to_lowercase();

    let mut walk = repo.revwalk().context("無法建立 revwalk")?;
    walk.push_glob("refs/heads/*")
        .context("無法將分支加入走訪範圍")?;
    if repo.head().is_ok() {
        let _ = walk.push_head();
    }
    walk.set_sorting(Sort::TIME).context("無法設定走訪順序")?;

    let mut hits = Vec::new();

    for (scanned, oid) in walk.enumerate() {
        if hits.len() >= limit || scanned >= scan_limit {
            break;
        }
        let Ok(oid) = oid else { continue };
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };

        let (author, message) = commit_text(&commit);
        let mut matched = Vec::new();
        let mut paths = Vec::new();

        if scope.message && message.to_lowercase().contains(&lowered) {
            matched.push("message");
        }
        if scope.author && author.to_lowercase().contains(&lowered) {
            matched.push("author");
        }
        if scope.path {
            let touched = touched_paths(repo, &commit, 32);
            let hit: Vec<String> = touched
                .into_iter()
                .filter(|path| path.to_lowercase().contains(&lowered))
                .collect();
            if !hit.is_empty() {
                matched.push("path");
                paths = hit.into_iter().take(5).collect();
            }
        }
        if scope.content && changes_occurrences(repo, &commit, needle) {
            matched.push("content");
        }

        if matched.is_empty() {
            continue;
        }
        let oid_text = oid.to_string();
        hits.push(SearchHit {
            short_oid: oid_text.chars().take(8).collect(),
            oid: oid_text,
            summary: message.lines().next().unwrap_or_default().to_owned(),
            author,
            timestamp: commit.time().seconds(),
            matched,
            paths,
        });
    }
    Ok(hits)
}

/// blame 的一行。
#[derive(Debug, Clone)]
pub struct BlameLine {
    pub line_number: usize,
    pub content: String,
    pub oid: String,
    pub short_oid: String,
    pub author: String,
    pub summary: String,
    pub timestamp: i64,
    /// 與上一行是否屬於同一個 commit。介面可據此只在區塊開頭顯示來源。
    pub same_as_previous: bool,
}

/// 逐行標出每一行最後是由哪個 commit 改動的。
///
/// 開啟跨檔案改名追蹤：檔案被改名時仍能追到原本的來源，
/// 否則改名之後整個檔案都會顯示成同一個 commit。
pub fn blame(repo: &Repository, path: &str, limit: usize) -> Result<Vec<BlameLine>> {
    let mut options = BlameOptions::new();
    options
        .track_copies_same_file(true)
        .track_copies_same_commit_moves(true);

    let blame = repo
        .blame_file(std::path::Path::new(path), Some(&mut options))
        .with_context(|| format!("無法對 {path} 執行 blame"))?;

    let workdir = repo.workdir().context("裸 repository 沒有工作目錄")?;
    let content = std::fs::read(workdir.join(path)).with_context(|| format!("無法讀取 {path}"))?;
    let text = String::from_utf8_lossy(&content);

    let mut lines = Vec::new();
    let mut previous: Option<Oid> = None;

    for (index, line) in text.lines().enumerate().take(limit) {
        let number = index + 1;
        let Some(hunk) = blame.get_line(number) else {
            continue;
        };
        let oid = hunk.final_commit_id();
        let (author, summary, timestamp) = match repo.find_commit(oid) {
            Ok(commit) => {
                let (author, message) = commit_text(&commit);
                (
                    author,
                    message.lines().next().unwrap_or_default().to_owned(),
                    commit.time().seconds(),
                )
            }
            Err(_) => (String::new(), String::new(), 0),
        };
        let oid_text = oid.to_string();
        lines.push(BlameLine {
            line_number: number,
            content: line.to_owned(),
            short_oid: oid_text.chars().take(8).collect(),
            oid: oid_text,
            author,
            summary,
            timestamp,
            same_as_previous: previous == Some(oid),
        });
        previous = Some(oid);
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_scope_matches_nothing() {
        let scope = SearchScope {
            message: false,
            author: false,
            path: false,
            content: false,
        };
        assert!(!scope.any());
    }

    #[test]
    fn the_default_scope_excludes_content() {
        // 內容比對要逐個 commit 算差異，成本高，預設不開。
        let scope = SearchScope::default();
        assert!(scope.message && scope.author && scope.path);
        assert!(!scope.content);
        assert!(scope.any());
    }
}
