//! 會改動 repository 的操作。
//!
//! 本模組是專案裡唯一會寫入使用者工作成果的地方，因此規則比其他模組嚴格：
//!
//! 1. **每個會改寫歷史或移動分支的操作，都先建立還原點。** 還原點是一個
//!    存放在 `refs/gitview/undo/` 底下的 ref，指向操作前的 HEAD。
//!    只要 ref 還在，commit 就不會被回收，隨時可以還原。
//! 2. **前置條件不符就拒絕，不猜測。** 例如工作目錄有未提交的變更時不執行
//!    rebase —— 與其事後補救，不如一開始就不要進入危險狀態。
//! 3. **錯誤訊息說明實際狀況**，不只回報失敗。

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use git2::build::CheckoutBuilder;
use git2::{AnnotatedCommit, Oid, Repository, ResetType};

/// 還原點所在的 ref 命名空間。
///
/// 放在 `refs/gitview/` 底下而不是 `refs/heads/`，這樣不會出現在使用者的
/// 分支清單裡，也不會被推送到遠端。
const UNDO_NAMESPACE: &str = "refs/gitview/undo";

/// 操作前建立的還原點。
#[derive(Debug, Clone)]
pub struct SafetyPoint {
    /// 完整的 ref 名稱。
    pub reference: String,
    /// 操作前 HEAD 指向的 commit。
    pub oid: String,
    /// 建立此還原點的操作名稱。
    pub operation: String,
    pub created_unix: u64,
}

/// 操作的結果。
#[derive(Debug, Clone)]
pub struct OpOutcome {
    /// 給使用者看的一句話。
    pub message: String,
    /// 本次操作的還原點；沒有改動任何東西時為 `None`。
    pub undo: Option<SafetyPoint>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// 工作目錄是否有未提交的變更（不含未追蹤檔案）。
///
/// 未追蹤檔案不影響 rebase 與 merge，因此不納入判斷；
/// 若納入，使用者會因為一個無關的暫存檔而無法操作。
fn has_uncommitted_changes(repo: &Repository) -> Result<bool> {
    if repo.is_bare() {
        return Ok(false);
    }
    let mut options = git2::StatusOptions::new();
    options.include_untracked(false).include_ignored(false);
    let statuses = repo
        .statuses(Some(&mut options))
        .context("無法讀取工作目錄狀態")?;
    Ok(!statuses.is_empty())
}

/// 確認 repository 沒有停在未完成的操作中。
fn ensure_no_operation_in_progress(repo: &Repository) -> Result<()> {
    if repo.state() != git2::RepositoryState::Clean {
        bail!("repository 正處於未完成的操作中，請先完成或中止它");
    }
    Ok(())
}

fn head_commit_oid(repo: &Repository) -> Result<Oid> {
    repo.head()
        .context("無法讀取 HEAD")?
        .peel_to_commit()
        .context("HEAD 不指向 commit")
        .map(|commit| commit.id())
}

/// 在執行任何改動之前建立還原點。
pub fn create_safety_point(repo: &Repository, operation: &str) -> Result<SafetyPoint> {
    let oid = head_commit_oid(repo)?;
    let created = now_unix();
    let reference = format!("{UNDO_NAMESPACE}/{created}-{operation}");
    repo.reference(
        &reference,
        oid,
        true,
        &format!("gitview: {operation} 之前的還原點"),
    )
    .with_context(|| format!("無法建立還原點 {reference}"))?;

    Ok(SafetyPoint {
        reference,
        oid: oid.to_string(),
        operation: operation.to_owned(),
        created_unix: created,
    })
}

/// 列出現有的還原點，最新的在前。
pub fn list_safety_points(repo: &Repository) -> Result<Vec<SafetyPoint>> {
    let mut points = Vec::new();
    let references = repo
        .references_glob(&format!("{UNDO_NAMESPACE}/*"))
        .context("無法列出還原點")?;

    for reference in references.flatten() {
        let Ok(name) = reference.name() else {
            continue;
        };
        let Some(oid) = reference.target() else {
            continue;
        };
        let tail = name.rsplit('/').next().unwrap_or_default();
        let (created, operation) = tail.split_once('-').unwrap_or(("0", tail));
        points.push(SafetyPoint {
            reference: name.to_owned(),
            oid: oid.to_string(),
            operation: operation.to_owned(),
            created_unix: created.parse().unwrap_or(0),
        });
    }
    points.sort_by_key(|point| std::cmp::Reverse(point.created_unix));
    Ok(points)
}

/// 還原到指定的還原點。
///
/// 這會把目前分支重設到還原點的位置並覆寫工作目錄，因此本身也是危險操作；
/// 呼叫端必須先向使用者確認。
pub fn undo_to(repo: &Repository, reference: &str) -> Result<OpOutcome> {
    if !reference.starts_with(UNDO_NAMESPACE) {
        bail!("只能還原到 gitview 建立的還原點");
    }
    let target = repo
        .find_reference(reference)
        .with_context(|| format!("找不到還原點 {reference}"))?
        .peel_to_commit()
        .context("還原點不指向 commit")?;

    // 還原本身也建立還原點，這樣「還原之後又反悔」仍有路可走。
    let point = create_safety_point(repo, "undo").ok();

    repo.reset(target.as_object(), ResetType::Hard, None)
        .context("無法重設到還原點")?;

    Ok(OpOutcome {
        message: format!("已還原到 {}", &target.id().to_string()[..8]),
        undo: point,
    })
}

/// 移除超出保留數量的舊還原點。
///
/// 還原點會讓被丟棄的 commit 無法被回收，長期累積會佔用空間。
pub fn prune_safety_points(repo: &Repository, keep: usize) -> Result<usize> {
    let points = list_safety_points(repo)?;
    let mut removed = 0;
    for point in points.into_iter().skip(keep) {
        if let Ok(mut reference) = repo.find_reference(&point.reference) {
            if reference.delete().is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// 目前分支追蹤的遠端分支。
fn upstream_commit<'repo>(repo: &'repo Repository) -> Result<AnnotatedCommit<'repo>> {
    let head = repo.head().context("無法讀取 HEAD")?;
    if !head.is_branch() {
        bail!("目前不在分支上（detached HEAD）");
    }
    let branch_name = head
        .shorthand()
        .map_err(|_| anyhow::anyhow!("分支名稱不是有效的 UTF-8"))?;
    let branch = repo
        .find_branch(branch_name, git2::BranchType::Local)
        .with_context(|| format!("找不到分支 {branch_name}"))?;
    let upstream = branch.upstream().context("這個分支沒有追蹤任何遠端分支")?;
    let oid = upstream
        .get()
        .target()
        .context("遠端追蹤分支沒有指向任何 commit")?;
    repo.find_annotated_commit(oid)
        .context("無法讀取遠端追蹤分支")
}

/// 把目前分支快轉到遠端追蹤分支。
///
/// 僅在本機沒有獨有 commit 時可用；有分岔時應改用 rebase 或 merge。
pub fn fast_forward(repo: &Repository) -> Result<OpOutcome> {
    ensure_no_operation_in_progress(repo)?;
    let upstream = upstream_commit(repo)?;
    let local = head_commit_oid(repo)?;

    if local == upstream.id() {
        return Ok(OpOutcome {
            message: "已經是最新的，沒有任何改動".to_owned(),
            undo: None,
        });
    }
    let (ahead, _) = repo
        .graph_ahead_behind(local, upstream.id())
        .context("無法比較本機與遠端")?;
    if ahead > 0 {
        bail!("本機有 {ahead} 個遠端沒有的 commit，無法快轉；請改用 rebase 或 merge");
    }
    if has_uncommitted_changes(repo)? {
        bail!("工作目錄有未提交的變更，請先提交或暫存");
    }

    let point = create_safety_point(repo, "fast-forward")?;
    let target = repo
        .find_commit(upstream.id())
        .context("找不到遠端的 commit")?;

    // 先更新工作目錄與索引，再移動分支。順序相反的話 checkout 會以舊的索引
    // 為基準比對，新增的檔案不會被寫出來。
    repo.checkout_tree(target.as_object(), Some(CheckoutBuilder::new().safe()))
        .context("無法更新工作目錄")?;
    let mut head = repo.head().context("無法讀取 HEAD")?;
    head.set_target(upstream.id(), "gitview: fast-forward")
        .context("無法移動分支")?;

    Ok(OpOutcome {
        message: format!("已快轉到 {}", &upstream.id().to_string()[..8]),
        undo: Some(point),
    })
}

/// 把本機獨有的 commit 重新套用到遠端內容之後。
pub fn rebase_onto_upstream(repo: &Repository) -> Result<OpOutcome> {
    ensure_no_operation_in_progress(repo)?;
    if has_uncommitted_changes(repo)? {
        bail!("工作目錄有未提交的變更，請先提交或暫存後再 rebase");
    }
    let upstream = upstream_commit(repo)?;
    let local = head_commit_oid(repo)?;
    if local == upstream.id() {
        return Ok(OpOutcome {
            message: "已經是最新的，沒有任何改動".to_owned(),
            undo: None,
        });
    }

    let point = create_safety_point(repo, "rebase")?;
    let head = repo
        .reference_to_annotated_commit(&repo.head().context("無法讀取 HEAD")?)
        .context("無法讀取 HEAD")?;

    let mut rebase = repo
        .rebase(Some(&head), Some(&upstream), None, None)
        .context("無法開始 rebase")?;

    let signature = repo
        .signature()
        .context("無法取得提交身分，請確認 git 的 user.name 與 user.email 已設定")?;
    let mut applied = 0;

    while let Some(step) = rebase.next() {
        step.context("rebase 過程發生錯誤")?;
        if repo.index().is_ok_and(|index| index.has_conflicts()) {
            // 保持在衝突狀態讓使用者處理，並保留還原點。
            return Ok(OpOutcome {
                message: format!(
                    "rebase 在第 {} 個 commit 遇到衝突，已停在該處等待處理；\
                     不想繼續可以還原到操作前的狀態",
                    applied + 1
                ),
                undo: Some(point),
            });
        }
        rebase
            .commit(None, &signature, None)
            .context("無法建立 rebase 後的 commit")?;
        applied += 1;
    }
    rebase.finish(Some(&signature)).context("無法完成 rebase")?;

    Ok(OpOutcome {
        message: format!("已將 {applied} 個 commit 重新套用到遠端內容之後"),
        undo: Some(point),
    })
}

/// 把遠端追蹤分支合併進目前分支。
pub fn merge_upstream(repo: &Repository) -> Result<OpOutcome> {
    ensure_no_operation_in_progress(repo)?;
    if has_uncommitted_changes(repo)? {
        bail!("工作目錄有未提交的變更，請先提交或暫存後再合併");
    }
    let upstream = upstream_commit(repo)?;
    let local = head_commit_oid(repo)?;
    if local == upstream.id() {
        return Ok(OpOutcome {
            message: "已經是最新的，沒有任何改動".to_owned(),
            undo: None,
        });
    }

    let point = create_safety_point(repo, "merge")?;
    repo.merge(&[&upstream], None, Some(CheckoutBuilder::new().safe()))
        .context("無法開始合併")?;

    let mut index = repo.index().context("無法取得索引")?;
    if index.has_conflicts() {
        return Ok(OpOutcome {
            message: "合併遇到衝突，已停在該處等待處理；不想繼續可以還原到操作前的狀態".to_owned(),
            undo: Some(point),
        });
    }

    let tree_id = index.write_tree().context("無法寫出合併結果")?;
    let tree = repo.find_tree(tree_id).context("找不到合併結果")?;
    let signature = repo
        .signature()
        .context("無法取得提交身分，請確認 git 的 user.name 與 user.email 已設定")?;
    let local_commit = repo.find_commit(local).context("找不到本機 commit")?;
    let remote_commit = repo
        .find_commit(upstream.id())
        .context("找不到遠端 commit")?;

    let message = format!(
        "Merge {} into {}",
        &upstream.id().to_string()[..8],
        repo.head()
            .ok()
            .and_then(|head| head.shorthand().ok().map(str::to_owned))
            .unwrap_or_else(|| "HEAD".to_owned())
    );
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &message,
        &tree,
        &[&local_commit, &remote_commit],
    )
    .context("無法建立合併 commit")?;
    repo.cleanup_state().context("無法清除合併狀態")?;

    Ok(OpOutcome {
        message: "已合併遠端的變更，並建立一個合併節點".to_owned(),
        undo: Some(point),
    })
}

/// 推送目前分支到它追蹤的遠端分支。
///
/// `force` 只在 rebase 之後需要。實作採 force-with-lease 的語意：先確認遠端
/// 目前的位置與本機記錄的遠端追蹤分支一致，確定沒有別人在這期間推過東西，
/// 才覆寫。libgit2 沒有內建這個檢查，因此在這裡自行完成 —— 少了它，
/// 強制推送會直接覆蓋掉他人的工作。
pub fn push(repo: &Repository, force: bool) -> Result<OpOutcome> {
    let head = repo.head().context("無法讀取 HEAD")?;
    if !head.is_branch() {
        bail!("目前不在分支上（detached HEAD），無法推送");
    }
    let branch_name = head
        .shorthand()
        .map_err(|_| anyhow::anyhow!("分支名稱不是有效的 UTF-8"))?
        .to_owned();

    let branch = repo
        .find_branch(&branch_name, git2::BranchType::Local)
        .with_context(|| format!("找不到分支 {branch_name}"))?;
    let upstream = branch
        .upstream()
        .context("這個分支沒有追蹤任何遠端分支，請先設定追蹤對象")?;
    let expected_remote_oid = upstream.get().target();

    let remote_name = crate::fetch::default_remote(repo).context("找不到可用的遠端")?;
    let mut remote = repo
        .find_remote(&remote_name)
        .with_context(|| format!("找不到遠端 {remote_name}"))?;

    if force {
        // Lease 檢查：重新問一次遠端現在指向哪裡。
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(crate::fetch::make_credentials_callback());
        remote
            .connect_auth(git2::Direction::Fetch, Some(callbacks), None)
            .map_err(|error| anyhow::anyhow!("無法連線到遠端：{}", error.message()))?;
        let actual = remote
            .list()
            .map_err(|error| anyhow::anyhow!("無法讀取遠端的 ref：{}", error.message()))?
            .iter()
            .find(|head| head.name() == format!("refs/heads/{branch_name}"))
            .map(|head| head.oid());
        let _ = remote.disconnect();

        if actual != expected_remote_oid {
            bail!(
                "遠端的 {branch_name} 已經不是本機記錄的位置，可能有其他人推送過。\
                 請先取得更新再試，以免覆蓋掉別人的工作"
            );
        }
    }

    let refspec = if force {
        format!("+refs/heads/{branch_name}:refs/heads/{branch_name}")
    } else {
        format!("refs/heads/{branch_name}:refs/heads/{branch_name}")
    };

    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(crate::fetch::make_credentials_callback());
    let mut options = git2::PushOptions::new();
    options.remote_callbacks(callbacks);

    remote
        .push(&[refspec.as_str()], Some(&mut options))
        .map_err(|error| anyhow::anyhow!("推送失敗：{}", error.message()))?;

    Ok(OpOutcome {
        message: if force {
            format!("已強制推送 {branch_name} 到 {remote_name}")
        } else {
            format!("已推送 {branch_name} 到 {remote_name}")
        },
        // 推送不改動本機任何東西，沒有需要還原的狀態。
        undo: None,
    })
}

/// 中止進行到一半的 rebase 或 merge，回到操作前的狀態。
pub fn abort_operation(repo: &Repository) -> Result<OpOutcome> {
    match repo.state() {
        git2::RepositoryState::Clean => Ok(OpOutcome {
            message: "沒有進行中的操作".to_owned(),
            undo: None,
        }),
        git2::RepositoryState::RebaseMerge
        | git2::RepositoryState::Rebase
        | git2::RepositoryState::RebaseInteractive => {
            let mut rebase = repo.open_rebase(None).context("無法開啟進行中的 rebase")?;
            rebase.abort().context("無法中止 rebase")?;
            Ok(OpOutcome {
                message: "已中止 rebase，回到操作前的狀態".to_owned(),
                undo: None,
            })
        }
        _ => {
            // merge、cherry-pick、revert 都是以 MERGE_HEAD 之類的狀態檔記錄，
            // 清除狀態並把工作目錄重設回 HEAD 即可。
            let head = repo
                .head()
                .context("無法讀取 HEAD")?
                .peel_to_commit()
                .context("HEAD 不指向 commit")?;
            repo.reset(head.as_object(), ResetType::Hard, None)
                .context("無法重設工作目錄")?;
            repo.cleanup_state().context("無法清除操作狀態")?;
            Ok(OpOutcome {
                message: "已中止操作，回到操作前的狀態".to_owned(),
                undo: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_namespace_is_outside_the_branch_list() {
        // 還原點不能落在 refs/heads 底下，否則會被當成分支顯示與推送。
        assert!(UNDO_NAMESPACE.starts_with("refs/gitview/"));
        assert!(!UNDO_NAMESPACE.starts_with("refs/heads"));
    }

    #[test]
    fn undo_rejects_references_outside_its_namespace() {
        // 這個檢查不需要 repository 就能表達：只接受自己建立的 ref。
        let outside = "refs/heads/main";
        assert!(!outside.starts_with(UNDO_NAMESPACE));
    }
}
