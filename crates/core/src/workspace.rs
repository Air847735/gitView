//! 工作區操作：暫存、提交、分支、stash、丟棄。
//!
//! 與 [`crate::ops`] 同樣會寫入使用者的工作成果，遵循相同的規則：
//! 前置條件不符就拒絕，會消滅內容的操作交由呼叫端先取得確認。

use anyhow::{bail, Context, Result};
use git2::build::CheckoutBuilder;
use git2::{IndexAddOption, Repository, ResetType, Signature, StatusOptions};

use crate::ops::OpOutcome;

/// 單一檔案在工作區中的狀態。
#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    /// 已暫存的變更類型：`new`、`modified`、`deleted`、`renamed`、`none`。
    pub staged: &'static str,
    /// 未暫存的變更類型，取值同上。
    pub unstaged: &'static str,
    pub is_untracked: bool,
    pub is_conflicted: bool,
}

/// 分支資訊。
#[derive(Debug, Clone)]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    pub upstream: Option<String>,
}

/// 一筆 stash。
#[derive(Debug, Clone)]
pub struct StashEntry {
    pub index: usize,
    pub message: String,
}

fn staged_kind(flags: git2::Status) -> &'static str {
    if flags.is_index_new() {
        "new"
    } else if flags.is_index_modified() {
        "modified"
    } else if flags.is_index_deleted() {
        "deleted"
    } else if flags.is_index_renamed() || flags.is_index_typechange() {
        "renamed"
    } else {
        "none"
    }
}

fn unstaged_kind(flags: git2::Status) -> &'static str {
    if flags.is_wt_new() {
        "new"
    } else if flags.is_wt_modified() {
        "modified"
    } else if flags.is_wt_deleted() {
        "deleted"
    } else if flags.is_wt_renamed() || flags.is_wt_typechange() {
        "renamed"
    } else {
        "none"
    }
}

/// 列出工作區中所有有變更的檔案。
pub fn changes(repo: &Repository) -> Result<Vec<FileChange>> {
    if repo.is_bare() {
        return Ok(Vec::new());
    }
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .include_ignored(false)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);

    let statuses = repo
        .statuses(Some(&mut options))
        .context("無法讀取工作目錄狀態")?;

    let mut changes = Vec::new();
    for entry in statuses.iter() {
        let flags = entry.status();
        let path = match entry.path() {
            Ok(path) => path.to_owned(),
            Err(_) => String::from_utf8_lossy(entry.path_bytes()).into_owned(),
        };
        changes.push(FileChange {
            path,
            staged: staged_kind(flags),
            unstaged: unstaged_kind(flags),
            is_untracked: flags.is_wt_new(),
            is_conflicted: flags.is_conflicted(),
        });
    }
    changes.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(changes)
}

/// 把指定的檔案加入暫存區。空清單代表全部。
pub fn stage(repo: &Repository, paths: &[String]) -> Result<OpOutcome> {
    let mut index = repo.index().context("無法取得索引")?;
    if paths.is_empty() {
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .context("無法加入全部變更")?;
    } else {
        for path in paths {
            let full = repo
                .workdir()
                .context("裸 repository 沒有工作目錄")?
                .join(path);
            if full.exists() {
                index
                    .add_path(std::path::Path::new(path))
                    .with_context(|| format!("無法加入 {path}"))?;
            } else {
                // 檔案已被刪除，暫存的是「刪除」這個動作。
                index
                    .remove_path(std::path::Path::new(path))
                    .with_context(|| format!("無法暫存 {path} 的刪除"))?;
            }
        }
    }
    index.write().context("無法寫入索引")?;
    Ok(OpOutcome {
        message: if paths.is_empty() {
            "已暫存全部變更".to_owned()
        } else {
            format!("已暫存 {} 個檔案", paths.len())
        },
        undo: None,
    })
}

/// 把指定的檔案移出暫存區。空清單代表全部。
///
/// 只影響暫存區，工作目錄中的內容不變，因此不會遺失任何編輯。
pub fn unstage(repo: &Repository, paths: &[String]) -> Result<OpOutcome> {
    let head = match repo.head().ok().and_then(|head| head.peel_to_commit().ok()) {
        Some(commit) => commit,
        None => {
            // 還沒有任何 commit：清空索引即可。
            let mut index = repo.index().context("無法取得索引")?;
            index.clear().context("無法清空索引")?;
            index.write().context("無法寫入索引")?;
            return Ok(OpOutcome {
                message: "已取消暫存".to_owned(),
                undo: None,
            });
        }
    };

    if paths.is_empty() {
        repo.reset(head.as_object(), ResetType::Mixed, None)
            .context("無法取消暫存")?;
    } else {
        let refs: Vec<&std::path::Path> = paths.iter().map(std::path::Path::new).collect();
        repo.reset_default(Some(head.as_object()), refs.iter())
            .context("無法取消暫存")?;
    }
    Ok(OpOutcome {
        message: "已取消暫存".to_owned(),
        undo: None,
    })
}

/// 建立 commit。
///
/// `amend` 為真時改寫前一個 commit，這會產生新的 commit 識別碼，
/// 因此呼叫端須先確認該 commit 尚未推送。
pub fn commit(repo: &Repository, message: &str, amend: bool) -> Result<OpOutcome> {
    if message.trim().is_empty() {
        bail!("提交訊息不能是空的");
    }
    let signature: Signature = repo
        .signature()
        .context("無法取得提交身分，請確認 git 的 user.name 與 user.email 已設定")?;

    let mut index = repo.index().context("無法取得索引")?;
    if index.has_conflicts() {
        bail!("還有未解決的衝突，無法提交");
    }
    let tree_id = index.write_tree().context("無法寫出 tree")?;
    let tree = repo.find_tree(tree_id).context("找不到 tree")?;

    if amend {
        let head = repo
            .head()
            .context("無法讀取 HEAD")?
            .peel_to_commit()
            .context("HEAD 不指向 commit")?;
        let point = crate::ops::create_safety_point(repo, "amend")?;
        head.amend(Some("HEAD"), None, None, None, Some(message), Some(&tree))
            .context("無法修改前一個 commit")?;
        return Ok(OpOutcome {
            message: "已修改前一個 commit".to_owned(),
            undo: Some(point),
        });
    }

    let parents: Vec<git2::Commit> = match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
        Some(parent) => vec![parent],
        None => Vec::new(),
    };
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

    let oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parent_refs,
        )
        .context("無法建立 commit")?;

    Ok(OpOutcome {
        message: format!("已建立 commit {}", &oid.to_string()[..8]),
        undo: None,
    })
}

/// 丟棄指定檔案在工作目錄中的變更。
///
/// **這會永久消滅尚未提交的編輯，git 無法還原。** 呼叫端必須先取得確認。
pub fn discard(repo: &Repository, paths: &[String]) -> Result<OpOutcome> {
    if paths.is_empty() {
        bail!("必須指定要丟棄的檔案，不提供一次丟棄全部");
    }
    let mut checkout = CheckoutBuilder::new();
    checkout.force();
    for path in paths {
        checkout.path(path);
    }
    repo.checkout_head(Some(&mut checkout))
        .context("無法還原檔案內容")?;

    Ok(OpOutcome {
        message: format!("已丟棄 {} 個檔案的變更", paths.len()),
        undo: None,
    })
}

/// 列出本機與遠端分支。
pub fn branches(repo: &Repository) -> Result<Vec<BranchInfo>> {
    let mut result = Vec::new();
    for kind in [git2::BranchType::Local, git2::BranchType::Remote] {
        let iter = repo.branches(Some(kind)).context("無法列出分支")?;
        for entry in iter.flatten() {
            let (branch, _) = (entry.0, entry.1);
            let Ok(Some(name)) = branch.name() else {
                continue;
            };
            result.push(BranchInfo {
                name: name.to_owned(),
                is_head: branch.is_head(),
                is_remote: kind == git2::BranchType::Remote,
                upstream: branch
                    .upstream()
                    .ok()
                    .and_then(|up| up.name().ok().flatten().map(str::to_owned)),
            });
        }
    }
    result.sort_by(|left, right| {
        left.is_remote
            .cmp(&right.is_remote)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(result)
}

/// 切換到指定分支。
pub fn checkout_branch(repo: &Repository, name: &str) -> Result<OpOutcome> {
    let branch = repo
        .find_branch(name, git2::BranchType::Local)
        .with_context(|| format!("找不到本機分支 {name}"))?;
    let reference = branch.get();
    let tree = reference.peel_to_tree().context("分支不指向有效的內容")?;

    // safe 模式在會覆寫未提交變更時會失敗，這正是我們要的行為。
    repo.checkout_tree(tree.as_object(), Some(CheckoutBuilder::new().safe()))
        .context("切換分支會覆蓋未提交的變更，請先提交或暫存")?;
    repo.set_head(reference.name().context("分支名稱不是有效的 UTF-8")?)
        .context("無法切換 HEAD")?;

    Ok(OpOutcome {
        message: format!("已切換到 {name}"),
        undo: None,
    })
}

/// 從目前的 HEAD 建立新分支，並切換過去。
pub fn create_branch(repo: &Repository, name: &str) -> Result<OpOutcome> {
    if name.trim().is_empty() {
        bail!("分支名稱不能是空的");
    }
    let head = repo
        .head()
        .context("無法讀取 HEAD")?
        .peel_to_commit()
        .context("HEAD 不指向 commit")?;
    repo.branch(name, &head, false)
        .with_context(|| format!("無法建立分支 {name}（可能已存在）"))?;
    checkout_branch(repo, name)?;

    Ok(OpOutcome {
        message: format!("已建立並切換到 {name}"),
        undo: None,
    })
}

/// 列出所有 stash。
pub fn stashes(repo: &mut Repository) -> Result<Vec<StashEntry>> {
    let mut entries = Vec::new();
    repo.stash_foreach(|index, message, _| {
        entries.push(StashEntry {
            index,
            message: message.to_owned(),
        });
        true
    })
    .context("無法列出 stash")?;
    Ok(entries)
}

/// 把目前的工作目錄變更暫存起來。
pub fn stash_save(repo: &mut Repository, message: &str) -> Result<OpOutcome> {
    let signature = repo
        .signature()
        .context("無法取得身分，請確認 git 的 user.name 與 user.email 已設定")?;
    let label = if message.trim().is_empty() {
        "gitview 暫存"
    } else {
        message
    };
    let oid = repo
        .stash_save(&signature, label, Some(git2::StashFlags::INCLUDE_UNTRACKED))
        .context("沒有可暫存的變更，或暫存失敗")?;

    Ok(OpOutcome {
        message: format!("已暫存為 {}", &oid.to_string()[..8]),
        undo: None,
    })
}

/// 取出指定的 stash 並從清單移除。
pub fn stash_pop(repo: &mut Repository, index: usize) -> Result<OpOutcome> {
    repo.stash_pop(index, None)
        .with_context(|| format!("無法取出第 {index} 筆暫存（可能與目前的內容衝突）"))?;
    Ok(OpOutcome {
        message: "已取出暫存的變更".to_owned(),
        undo: None,
    })
}

/// 刪除指定的 stash。
///
/// **內容會永久消失。** 呼叫端必須先取得確認。
pub fn stash_drop(repo: &mut Repository, index: usize) -> Result<OpOutcome> {
    repo.stash_drop(index)
        .with_context(|| format!("無法刪除第 {index} 筆暫存"))?;
    Ok(OpOutcome {
        message: "已刪除暫存".to_owned(),
        undo: None,
    })
}

/// 指向差異中的一行：第幾個 hunk 的第幾行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRef {
    pub hunk: usize,
    pub line: usize,
}

/// 檔案內容與它原本有沒有結尾換行。
struct Content {
    lines: Vec<String>,
    trailing_newline: bool,
}

impl Content {
    fn parse(text: &str) -> Self {
        Self {
            lines: text.lines().map(str::to_owned).collect(),
            trailing_newline: text.ends_with('\n'),
        }
    }

    fn render(lines: Vec<String>, trailing_newline: bool) -> String {
        let mut text = lines.join("\n");
        if trailing_newline && !text.is_empty() {
            text.push('\n');
        }
        text
    }
}

/// 依選取的行，把變更套用到舊版本上，產生新的檔案內容。
///
/// 這是部分暫存的核心：未選取的刪除行保留、未選取的新增行捨棄，
/// 選取的則相反。純運算，與 git 無關，因此可以獨立測試。
fn apply_selection(old: &Content, hunks: &[crate::diff::Hunk], selected: &[LineRef]) -> String {
    use crate::diff::LineKind;

    let is_selected = |hunk: usize, line: usize| {
        selected
            .iter()
            .any(|item| item.hunk == hunk && item.line == line)
    };

    let mut result: Vec<String> = Vec::new();
    let mut cursor = 0usize;

    for (hunk_index, hunk) in hunks.iter().enumerate() {
        // old_lines 為 0 代表純插入，old_start 是插入點之前的行數。
        let hunk_start = if hunk.old_lines == 0 {
            hunk.old_start as usize
        } else {
            (hunk.old_start as usize).saturating_sub(1)
        };
        while cursor < hunk_start && cursor < old.lines.len() {
            result.push(old.lines[cursor].clone());
            cursor += 1;
        }

        for (line_index, line) in hunk.lines.iter().enumerate() {
            match line.kind {
                LineKind::Context => {
                    if cursor < old.lines.len() {
                        result.push(old.lines[cursor].clone());
                        cursor += 1;
                    }
                }
                LineKind::Removed => {
                    if is_selected(hunk_index, line_index) {
                        // 選取了這個刪除：不寫出，等於套用刪除。
                        cursor += 1;
                    } else if cursor < old.lines.len() {
                        result.push(old.lines[cursor].clone());
                        cursor += 1;
                    }
                }
                LineKind::Added => {
                    if is_selected(hunk_index, line_index) {
                        result.push(line.content.clone());
                    }
                }
            }
        }
    }
    while cursor < old.lines.len() {
        result.push(old.lines[cursor].clone());
        cursor += 1;
    }

    Content::render(result, old.trailing_newline)
}

/// 讀出索引中某個檔案的內容；不存在時視為空。
fn index_content(repo: &Repository, path: &str) -> Result<String> {
    let index = repo.index().context("無法取得索引")?;
    match index.get_path(std::path::Path::new(path), 0) {
        Some(entry) => {
            let blob = repo.find_blob(entry.id).context("找不到索引中的內容")?;
            Ok(String::from_utf8_lossy(blob.content()).into_owned())
        }
        None => Ok(String::new()),
    }
}

/// 讀出 HEAD 中某個檔案的內容；不存在時視為空。
fn head_content(repo: &Repository, path: &str) -> Result<String> {
    let Ok(head) = repo.head() else {
        return Ok(String::new());
    };
    let Ok(tree) = head.peel_to_tree() else {
        return Ok(String::new());
    };
    match tree.get_path(std::path::Path::new(path)) {
        Ok(entry) => {
            let blob = repo.find_blob(entry.id()).context("找不到 HEAD 中的內容")?;
            Ok(String::from_utf8_lossy(blob.content()).into_owned())
        }
        Err(_) => Ok(String::new()),
    }
}

/// 把內容寫入索引中的指定路徑。
fn write_index_entry(repo: &Repository, path: &str, content: &str) -> Result<()> {
    let oid = repo.blob(content.as_bytes()).context("無法寫入內容")?;
    let mut index = repo.index().context("無法取得索引")?;

    let mut entry = match index.get_path(std::path::Path::new(path), 0) {
        Some(existing) => existing,
        None => git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            // 一般檔案的權限；新加入索引的檔案沿用這個預設。
            mode: 0o100644,
            uid: 0,
            gid: 0,
            file_size: 0,
            id: oid,
            flags: 0,
            flags_extended: 0,
            path: path.as_bytes().to_vec(),
        },
    };
    entry.id = oid;
    entry.file_size = content.len() as u32;

    index.add(&entry).context("無法更新索引")?;
    index.write().context("無法寫入索引")?;
    Ok(())
}

/// 只暫存選取的那些行。
///
/// 這是與整檔暫存不同的路徑：把選取的變更套用到索引原本的內容上，
/// 寫成新的內容放回索引，工作目錄完全不動。
pub fn stage_selection(repo: &Repository, path: &str, selected: &[LineRef]) -> Result<OpOutcome> {
    if selected.is_empty() {
        bail!("沒有選取任何一行");
    }
    let diffs = crate::diff::workspace_diff(repo, crate::diff::DiffSource::Unstaged)?;
    let file = diffs
        .iter()
        .find(|file| file.path == path)
        .with_context(|| format!("{path} 沒有未暫存的變更"))?;
    if file.is_binary {
        bail!("{path} 是二進位檔案，只能整檔暫存");
    }

    let old = Content::parse(&index_content(repo, path)?);
    let staged = apply_selection(&old, &file.hunks, selected);
    write_index_entry(repo, path, &staged)?;

    Ok(OpOutcome {
        message: format!("已暫存 {path} 的 {} 行", selected.len()),
        undo: None,
    })
}

/// 只取消暫存選取的那些行。
pub fn unstage_selection(repo: &Repository, path: &str, selected: &[LineRef]) -> Result<OpOutcome> {
    if selected.is_empty() {
        bail!("沒有選取任何一行");
    }
    let diffs = crate::diff::workspace_diff(repo, crate::diff::DiffSource::Staged)?;
    let file = diffs
        .iter()
        .find(|file| file.path == path)
        .with_context(|| format!("{path} 沒有已暫存的變更"))?;
    if file.is_binary {
        bail!("{path} 是二進位檔案，只能整檔取消暫存");
    }

    // 反向操作：以 HEAD 為基準，只套用「未選取」的變更，等於把選取的退回。
    let old = Content::parse(&head_content(repo, path)?);
    let keep: Vec<LineRef> = file
        .hunks
        .iter()
        .enumerate()
        .flat_map(|(hunk_index, hunk)| {
            hunk.lines
                .iter()
                .enumerate()
                .filter(move |(line_index, line)| {
                    line.kind != crate::diff::LineKind::Context
                        && !selected
                            .iter()
                            .any(|item| item.hunk == hunk_index && item.line == *line_index)
                })
                .map(move |(line_index, _)| LineRef {
                    hunk: hunk_index,
                    line: line_index,
                })
        })
        .collect();

    let remaining = apply_selection(&old, &file.hunks, &keep);
    write_index_entry(repo, path, &remaining)?;

    Ok(OpOutcome {
        message: format!("已取消暫存 {path} 的 {} 行", selected.len()),
        undo: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_flags_map_to_stable_labels() {
        assert_eq!(staged_kind(git2::Status::INDEX_NEW), "new");
        assert_eq!(staged_kind(git2::Status::INDEX_MODIFIED), "modified");
        assert_eq!(staged_kind(git2::Status::INDEX_DELETED), "deleted");
        assert_eq!(staged_kind(git2::Status::WT_MODIFIED), "none");

        assert_eq!(unstaged_kind(git2::Status::WT_NEW), "new");
        assert_eq!(unstaged_kind(git2::Status::WT_MODIFIED), "modified");
        assert_eq!(unstaged_kind(git2::Status::WT_DELETED), "deleted");
        assert_eq!(unstaged_kind(git2::Status::INDEX_MODIFIED), "none");
    }
}
