//! 衝突的檢視與解決。
//!
//! 使用者在 rebase 或 merge 撞到衝突時，需要在同一個地方看到雙方的內容並
//! 做出決定，而不是被丟回終端機。本模組提供介面所需的資料與解決動作。
//!
//! 解決的方式有三種，涵蓋實際會遇到的情況：
//! 採用自己的版本、採用對方的版本、或直接編輯合併後的內容。
//! 逐區塊挑選由介面在編輯內容上完成，核心只負責收下最終結果。

use anyhow::{bail, Context, Result};
use git2::{Repository, ResetType};

use crate::ops::OpOutcome;

/// 衝突檔案的一個版本。
#[derive(Debug, Clone)]
pub struct ConflictSide {
    /// 檔案內容；二進位檔案為 `None`。
    pub text: Option<String>,
    /// 該版本是否存在（某一側刪除檔案時為 `false`）。
    pub exists: bool,
}

impl ConflictSide {
    fn missing() -> Self {
        Self {
            text: None,
            exists: false,
        }
    }
}

/// 一個處於衝突狀態的檔案。
#[derive(Debug, Clone)]
pub struct ConflictFile {
    pub path: String,
    /// 共同祖先的版本。
    pub base: ConflictSide,
    /// 目前分支的版本。
    pub ours: ConflictSide,
    /// 併入方的版本。
    pub theirs: ConflictSide,
    /// git 寫進工作目錄、含衝突標記的合併結果。
    pub merged: Option<String>,
    /// 是否為二進位檔案；此時只能整檔擇一，無法逐行編輯。
    pub is_binary: bool,
}

/// 兩側在目前操作下的實際意義。
///
/// git 的 ours / theirs 在 rebase 時與直覺相反：ours 是被接上去的那一端
/// （也就是遠端的內容），theirs 才是正在重放的自己的 commit。把這兩個字
/// 直接翻成「我的」與「他們的」會讓使用者選錯邊，因此標籤必須依操作決定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideLabels {
    pub ours: String,
    pub theirs: String,
    /// 一句話說明目前的處境。
    pub note: String,
}

/// 依目前進行中的操作決定兩側的說法。
pub fn side_labels(repo: &Repository) -> SideLabels {
    match repo.state() {
        git2::RepositoryState::Rebase
        | git2::RepositoryState::RebaseMerge
        | git2::RepositoryState::RebaseInteractive => SideLabels {
            ours: "接上去的基底（遠端已有的內容）".to_owned(),
            theirs: "你的 commit（正在被重新套用）".to_owned(),
            note: "rebase 是把你的 commit 逐一接到遠端內容之後，                   因此「基底」是遠端那一邊。"
                .to_owned(),
        },
        git2::RepositoryState::Merge => SideLabels {
            ours: "你目前的分支".to_owned(),
            theirs: "併入的內容（遠端）".to_owned(),
            note: "合併是把對方的內容併進你的分支。".to_owned(),
        },
        _ => SideLabels {
            ours: "目前的版本".to_owned(),
            theirs: "另一個版本".to_owned(),
            note: String::new(),
        },
    }
}

/// 解決衝突時採用的版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Ours,
    Theirs,
}

fn read_blob(repo: &Repository, id: git2::Oid) -> ConflictSide {
    match repo.find_blob(id) {
        Ok(blob) => {
            let bytes = blob.content();
            if blob.is_binary() {
                ConflictSide {
                    text: None,
                    exists: true,
                }
            } else {
                ConflictSide {
                    text: Some(String::from_utf8_lossy(bytes).into_owned()),
                    exists: true,
                }
            }
        }
        Err(_) => ConflictSide::missing(),
    }
}

fn entry_path(entry: &git2::IndexEntry) -> String {
    String::from_utf8_lossy(&entry.path).into_owned()
}

/// 目前所有處於衝突狀態的檔案。
pub fn conflicts(repo: &Repository) -> Result<Vec<ConflictFile>> {
    let index = repo.index().context("無法取得索引")?;
    if !index.has_conflicts() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for conflict in index.conflicts().context("無法列出衝突")? {
        let conflict = conflict.context("讀取衝突項目時發生錯誤")?;

        // 三個項目中至少會有一個存在，用它取得路徑。
        let path = conflict
            .our
            .as_ref()
            .or(conflict.their.as_ref())
            .or(conflict.ancestor.as_ref())
            .map(entry_path)
            .context("衝突項目沒有路徑")?;

        let base = conflict
            .ancestor
            .as_ref()
            .map(|entry| read_blob(repo, entry.id))
            .unwrap_or_else(ConflictSide::missing);
        let ours = conflict
            .our
            .as_ref()
            .map(|entry| read_blob(repo, entry.id))
            .unwrap_or_else(ConflictSide::missing);
        let theirs = conflict
            .their
            .as_ref()
            .map(|entry| read_blob(repo, entry.id))
            .unwrap_or_else(ConflictSide::missing);

        // git 已經把含衝突標記的內容寫進工作目錄，直接讀它。
        let merged = repo
            .workdir()
            .map(|dir| dir.join(&path))
            .and_then(|full| std::fs::read(full).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok());

        let is_binary =
            (ours.exists && ours.text.is_none()) || (theirs.exists && theirs.text.is_none());

        files.push(ConflictFile {
            path,
            base,
            ours,
            theirs,
            merged,
            is_binary,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

/// 把最終內容寫入檔案並標記為已解決。
pub fn resolve_with_content(repo: &Repository, path: &str, content: &str) -> Result<OpOutcome> {
    let workdir = repo.workdir().context("裸 repository 沒有工作目錄")?;
    let full = workdir.join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).context("無法建立目錄")?;
    }
    std::fs::write(&full, content).with_context(|| format!("無法寫入 {path}"))?;

    let mut index = repo.index().context("無法取得索引")?;
    // add_path 會同時移除該路徑的衝突項目，這就是「標記為已解決」。
    index
        .add_path(std::path::Path::new(path))
        .with_context(|| format!("無法標記 {path} 為已解決"))?;
    index.write().context("無法寫入索引")?;

    Ok(OpOutcome {
        message: format!("已解決 {path}"),
        undo: None,
    })
}

/// 整檔採用其中一方的版本。
pub fn resolve_using(repo: &Repository, path: &str, side: Side) -> Result<OpOutcome> {
    let files = conflicts(repo)?;
    let file = files
        .iter()
        .find(|file| file.path == path)
        .with_context(|| format!("{path} 目前不處於衝突狀態"))?;

    let chosen = match side {
        Side::Ours => &file.ours,
        Side::Theirs => &file.theirs,
    };

    if !chosen.exists {
        // 該側刪除了這個檔案，解決方式是刪除它。
        let workdir = repo.workdir().context("裸 repository 沒有工作目錄")?;
        let full = workdir.join(path);
        let _ = std::fs::remove_file(&full);
        let mut index = repo.index().context("無法取得索引")?;
        index
            .remove_path(std::path::Path::new(path))
            .with_context(|| format!("無法移除 {path}"))?;
        index.write().context("無法寫入索引")?;
        return Ok(OpOutcome {
            message: format!("已採用刪除 {path}"),
            undo: None,
        });
    }

    match &chosen.text {
        Some(text) => resolve_with_content(repo, path, text),
        None => bail!("{path} 是二進位檔案，請在檔案總管中處理後再標記為已解決"),
    }
}

/// 提交 rebase 的目前這一步。
///
/// 解決衝突時若採用了對方的版本，這個 commit 的變更可能完全被涵蓋，
/// 套用後內容沒有任何改變。libgit2 會回報「已套用過」，此時正確的處置
/// 是略過它而不是視為錯誤 —— 使用者的意圖已經達成了。
///
/// 回傳 `true` 表示確實建立了 commit，`false` 表示這一步是空的而被略過。
fn commit_step(rebase: &mut git2::Rebase<'_>, signature: &git2::Signature<'_>) -> Result<bool> {
    match rebase.commit(None, signature, None) {
        Ok(_) => Ok(true),
        Err(error) if error.code() == git2::ErrorCode::Applied => Ok(false),
        Err(error) => Err(anyhow::anyhow!("無法提交這一步：{}", error.message())),
    }
}

/// 是否所有衝突都已解決。
pub fn all_resolved(repo: &Repository) -> Result<bool> {
    let index = repo.index().context("無法取得索引")?;
    Ok(!index.has_conflicts())
}

/// 衝突解決完之後，繼續進行中的 rebase 或完成合併。
pub fn continue_operation(repo: &Repository) -> Result<OpOutcome> {
    if !all_resolved(repo)? {
        bail!("還有未解決的衝突");
    }
    let signature = repo
        .signature()
        .context("無法取得提交身分，請確認 git 的 user.name 與 user.email 已設定")?;

    match repo.state() {
        git2::RepositoryState::Rebase
        | git2::RepositoryState::RebaseMerge
        | git2::RepositoryState::RebaseInteractive => {
            let mut rebase = repo.open_rebase(None).context("無法開啟進行中的 rebase")?;
            // 先把目前這一步提交，再繼續走完剩下的。
            let mut applied = 0;
            let mut skipped = 0;
            if commit_step(&mut rebase, &signature)? {
                applied += 1;
            } else {
                skipped += 1;
            }

            while let Some(step) = rebase.next() {
                step.context("rebase 過程發生錯誤")?;
                if repo.index().is_ok_and(|index| index.has_conflicts()) {
                    return Ok(OpOutcome {
                        message: format!(
                            "又遇到衝突，已停在第 {} 步等待處理",
                            applied + skipped + 1
                        ),
                        undo: None,
                    });
                }
                if commit_step(&mut rebase, &signature)? {
                    applied += 1;
                } else {
                    skipped += 1;
                }
            }
            rebase.finish(Some(&signature)).context("無法完成 rebase")?;

            let mut message = format!("rebase 完成，共套用 {applied} 個 commit");
            if skipped > 0 {
                message.push_str(&format!(
                    "；另有 {skipped} 個因為解決衝突後內容變成空的而略過"
                ));
            }
            Ok(OpOutcome {
                message,
                undo: None,
            })
        }
        git2::RepositoryState::Merge => {
            let mut index = repo.index().context("無法取得索引")?;
            let tree_id = index.write_tree().context("無法寫出合併結果")?;
            let tree = repo.find_tree(tree_id).context("找不到合併結果")?;

            let head = repo
                .head()
                .context("無法讀取 HEAD")?
                .peel_to_commit()
                .context("HEAD 不指向 commit")?;

            // MERGE_HEAD 記錄了被併入的一方。
            let merge_head = repo
                .find_reference("MERGE_HEAD")
                .context("找不到 MERGE_HEAD")?
                .peel_to_commit()
                .context("MERGE_HEAD 不指向 commit")?;

            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                &format!("Merge {}", &merge_head.id().to_string()[..8]),
                &tree,
                &[&head, &merge_head],
            )
            .context("無法建立合併 commit")?;
            repo.cleanup_state().context("無法清除合併狀態")?;

            Ok(OpOutcome {
                message: "合併完成".to_owned(),
                undo: None,
            })
        }
        git2::RepositoryState::Clean => Ok(OpOutcome {
            message: "沒有進行中的操作".to_owned(),
            undo: None,
        }),
        other => {
            // cherry-pick、revert 等：解決後以一般 commit 收尾。
            let mut index = repo.index().context("無法取得索引")?;
            let tree_id = index.write_tree().context("無法寫出結果")?;
            let tree = repo.find_tree(tree_id).context("找不到結果")?;
            let head = repo
                .head()
                .context("無法讀取 HEAD")?
                .peel_to_commit()
                .context("HEAD 不指向 commit")?;
            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                &format!("解決 {other:?} 的衝突"),
                &tree,
                &[&head],
            )
            .context("無法建立 commit")?;
            repo.cleanup_state().context("無法清除狀態")?;
            Ok(OpOutcome {
                message: "已完成並清除操作狀態".to_owned(),
                undo: None,
            })
        }
    }
}

/// 放棄目前這一個 rebase 步驟，不套用它。
pub fn skip_current_step(repo: &Repository) -> Result<OpOutcome> {
    match repo.state() {
        git2::RepositoryState::Rebase
        | git2::RepositoryState::RebaseMerge
        | git2::RepositoryState::RebaseInteractive => {
            // 丟掉這一步造成的所有變更，再讓 rebase 走到下一步。
            let head = repo
                .head()
                .context("無法讀取 HEAD")?
                .peel_to_commit()
                .context("HEAD 不指向 commit")?;
            repo.reset(head.as_object(), ResetType::Hard, None)
                .context("無法清除這一步的變更")?;

            let mut rebase = repo.open_rebase(None).context("無法開啟進行中的 rebase")?;
            let signature = repo.signature().context("無法取得提交身分")?;
            let mut applied = 0;
            while let Some(step) = rebase.next() {
                step.context("rebase 過程發生錯誤")?;
                if repo.index().is_ok_and(|index| index.has_conflicts()) {
                    return Ok(OpOutcome {
                        message: "已略過這一步，但下一步又遇到衝突".to_owned(),
                        undo: None,
                    });
                }
                rebase
                    .commit(None, &signature, None)
                    .context("無法建立 commit")?;
                applied += 1;
            }
            rebase.finish(Some(&signature)).context("無法完成 rebase")?;
            Ok(OpOutcome {
                message: format!("已略過該 commit，其餘 {applied} 個已套用"),
                undo: None,
            })
        }
        _ => bail!("只有 rebase 可以略過單一步驟"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_side_is_marked_as_not_existing() {
        let side = ConflictSide::missing();
        assert!(!side.exists);
        assert!(side.text.is_none());
    }

    #[test]
    fn sides_are_distinct() {
        assert_ne!(Side::Ours, Side::Theirs);
    }
}
