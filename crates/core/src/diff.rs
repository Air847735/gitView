//! 差異計算：結構化的 hunk 與行，供並排／行內呈現與部分暫存使用。
//!
//! 相較於直接輸出 patch 文字，這裡輸出結構化資料，原因有三：
//! 介面要能並排呈現、要能逐行選取來做部分暫存、也要能標示行內的字元差異。
//!
//! 本模組只計算，不寫入。部分暫存的套用在 [`crate::workspace`]。

use anyhow::{Context, Result};
use git2::{Diff, DiffOptions, Oid, Patch, Repository};

/// 差異的來源，決定比較哪兩個版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSource {
    /// 工作目錄相對於索引：尚未暫存的變更。
    Unstaged,
    /// 索引相對於 HEAD：已暫存的變更。
    Staged,
    /// 某個 commit 相對於它的第一個父節點。
    Commit,
}

impl DiffSource {
    pub fn as_str(self) -> &'static str {
        match self {
            DiffSource::Unstaged => "unstaged",
            DiffSource::Staged => "staged",
            DiffSource::Commit => "commit",
        }
    }
}

/// 一行的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

impl LineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LineKind::Context => "context",
            LineKind::Added => "added",
            LineKind::Removed => "removed",
        }
    }
}

/// 行內有變化的字元範圍，以位元組位移表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// 差異中的一行。
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: LineKind,
    pub content: String,
    /// 舊版本中的行號；新增的行為 `None`。
    pub old_lineno: Option<u32>,
    /// 新版本中的行號；刪除的行為 `None`。
    pub new_lineno: Option<u32>,
    /// 行內實際變動的字元範圍。只有配對成功的增刪行才有值。
    pub spans: Vec<Span>,
    /// 這一行的變更是否只有空白差異。
    pub whitespace_only: bool,
}

/// 一段連續的變更。
#[derive(Debug, Clone)]
pub struct Hunk {
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
    /// 這一段觸及的行，也會被即將進來的遠端變更改到。
    ///
    /// 這是本工具與一般 diff 檢視器的差別：在你還沒提交之前就先告訴你
    /// 哪幾段會撞到，而不是等 pull 之後才發現。
    pub collides_with_incoming: bool,
}

impl Hunk {
    /// 這一段是否只包含空白變更。整段都是的話介面可以預設收合。
    pub fn whitespace_only(&self) -> bool {
        let changed: Vec<&DiffLine> = self
            .lines
            .iter()
            .filter(|line| line.kind != LineKind::Context)
            .collect();
        !changed.is_empty() && changed.iter().all(|line| line.whitespace_only)
    }
}

/// 單一檔案的差異。
#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: String,
    /// 改名時的舊路徑。
    pub old_path: Option<String>,
    pub hunks: Vec<Hunk>,
    pub is_binary: bool,
    pub added: usize,
    pub removed: usize,
}

impl FileDiff {
    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }
}

/// 把一行切成用於比對的字詞。
///
/// 以「字母數字連續段」與「其餘單一字元」為單位。比逐字元比對更貼近人的
/// 閱讀方式：改一個識別字時會整段標起來，而不是散落幾個字母。
fn tokenize(text: &str) -> Vec<(usize, &str)> {
    let mut tokens = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        if is_word(bytes[index]) {
            while index < bytes.len() && is_word(bytes[index]) {
                index += 1;
            }
        } else {
            // 非 ASCII 以字元邊界前進，避免切斷多位元組字元。
            let ch_len = text[index..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(1);
            index += ch_len;
        }
        tokens.push((start, &text[start..index]));
    }
    tokens
}

/// 計算兩行之間實際變動的字元範圍。
///
/// 先去掉共同的前綴與後綴，中間剩下的就是變動處。這比完整的 LCS 便宜得多，
/// 而對「改了一個字詞」這種最常見的情況結果一樣好。
fn changed_spans(old: &str, new: &str) -> (Vec<Span>, Vec<Span>) {
    let old_tokens = tokenize(old);
    let new_tokens = tokenize(new);

    let mut prefix = 0;
    while prefix < old_tokens.len()
        && prefix < new_tokens.len()
        && old_tokens[prefix].1 == new_tokens[prefix].1
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_tokens.len() - prefix
        && suffix < new_tokens.len() - prefix
        && old_tokens[old_tokens.len() - 1 - suffix].1
            == new_tokens[new_tokens.len() - 1 - suffix].1
    {
        suffix += 1;
    }

    let span_of = |tokens: &[(usize, &str)], text: &str| -> Vec<Span> {
        if prefix + suffix >= tokens.len() {
            return Vec::new();
        }
        let start = tokens[prefix].0;
        let end = tokens
            .get(tokens.len() - suffix)
            .map(|(offset, _)| *offset)
            .unwrap_or_else(|| text.len());
        if start >= end {
            Vec::new()
        } else {
            vec![Span { start, end }]
        }
    };

    (span_of(&old_tokens, old), span_of(&new_tokens, new))
}

fn is_whitespace_only_change(old: &str, new: &str) -> bool {
    let strip = |text: &str| -> String { text.chars().filter(|c| !c.is_whitespace()).collect() };
    strip(old) == strip(new)
}

/// 把 hunk 內相鄰的刪除行與新增行配對，標出行內差異。
///
/// git 的 diff 是以行為單位的，不知道「這兩行其實是同一行的前後版本」。
/// 依出現順序配對是實務上有效的近似：改動一行時，刪除與新增會相鄰出現。
fn annotate_intra_line(lines: &mut [DiffLine]) {
    let mut index = 0;
    while index < lines.len() {
        if lines[index].kind != LineKind::Removed {
            index += 1;
            continue;
        }
        // 收集連續的刪除行與其後連續的新增行。
        let removed_start = index;
        while index < lines.len() && lines[index].kind == LineKind::Removed {
            index += 1;
        }
        let added_start = index;
        while index < lines.len() && lines[index].kind == LineKind::Added {
            index += 1;
        }
        let removed_count = added_start - removed_start;
        let added_count = index - added_start;

        // 只在數量相同時配對；數量不同代表是整段增刪，逐行標示反而誤導。
        if removed_count == 0 || removed_count != added_count {
            continue;
        }
        for offset in 0..removed_count {
            let old_text = lines[removed_start + offset].content.clone();
            let new_text = lines[added_start + offset].content.clone();
            let (old_spans, new_spans) = changed_spans(&old_text, &new_text);
            let whitespace = is_whitespace_only_change(&old_text, &new_text);
            lines[removed_start + offset].spans = old_spans;
            lines[removed_start + offset].whitespace_only = whitespace;
            lines[added_start + offset].spans = new_spans;
            lines[added_start + offset].whitespace_only = whitespace;
        }
    }
}

fn build_file_diffs(diff: &Diff<'_>) -> Result<Vec<FileDiff>> {
    let mut files = Vec::new();
    for index in 0..diff.deltas().len() {
        let delta = diff.get_delta(index).context("無法讀取差異項目")?;
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let old_path = delta
            .old_file()
            .path()
            .map(|path| path.to_string_lossy().into_owned())
            .filter(|old| *old != path);

        let patch = Patch::from_diff(diff, index).context("無法讀取 patch")?;
        let Some(patch) = patch else {
            // 二進位檔案沒有可列出的行。
            files.push(FileDiff {
                path,
                old_path,
                hunks: Vec::new(),
                is_binary: true,
                added: 0,
                removed: 0,
            });
            continue;
        };

        let mut hunks = Vec::new();
        let mut added = 0;
        let mut removed = 0;

        for hunk_index in 0..patch.num_hunks() {
            let (hunk, line_count) = patch.hunk(hunk_index).context("無法讀取 hunk")?;
            let mut lines = Vec::with_capacity(line_count);

            for line_index in 0..line_count {
                let line = patch
                    .line_in_hunk(hunk_index, line_index)
                    .context("無法讀取差異行")?;
                let kind = match line.origin() {
                    '+' => LineKind::Added,
                    '-' => LineKind::Removed,
                    _ => LineKind::Context,
                };
                match kind {
                    LineKind::Added => added += 1,
                    LineKind::Removed => removed += 1,
                    LineKind::Context => {}
                }
                lines.push(DiffLine {
                    kind,
                    content: String::from_utf8_lossy(line.content())
                        .trim_end_matches('\n')
                        .to_owned(),
                    old_lineno: line.old_lineno(),
                    new_lineno: line.new_lineno(),
                    spans: Vec::new(),
                    whitespace_only: false,
                });
            }
            annotate_intra_line(&mut lines);

            hunks.push(Hunk {
                header: String::from_utf8_lossy(hunk.header())
                    .trim_end_matches('\n')
                    .to_owned(),
                old_start: hunk.old_start(),
                old_lines: hunk.old_lines(),
                new_start: hunk.new_start(),
                new_lines: hunk.new_lines(),
                lines,
                collides_with_incoming: false,
            });
        }

        files.push(FileDiff {
            path,
            old_path,
            hunks,
            is_binary: false,
            added,
            removed,
        });
    }
    Ok(files)
}

fn default_options() -> DiffOptions {
    let mut options = DiffOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .context_lines(3);
    options
}

/// 工作目錄或索引的差異。
pub fn workspace_diff(repo: &Repository, source: DiffSource) -> Result<Vec<FileDiff>> {
    let mut options = default_options();
    let diff = match source {
        DiffSource::Unstaged => repo
            .diff_index_to_workdir(None, Some(&mut options))
            .context("無法計算未暫存的差異")?,
        DiffSource::Staged => {
            let tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
            repo.diff_tree_to_index(tree.as_ref(), None, Some(&mut options))
                .context("無法計算已暫存的差異")?
        }
        DiffSource::Commit => anyhow::bail!("commit 的差異請改用 commit_diff"),
    };
    build_file_diffs(&diff)
}

/// 單一 commit 相對於其第一個父節點的差異。
pub fn commit_diff(repo: &Repository, oid: &str) -> Result<Vec<FileDiff>> {
    let oid = Oid::from_str(oid).context("commit 識別碼格式錯誤")?;
    let commit = repo.find_commit(oid).context("找不到該 commit")?;
    let tree = commit.tree().context("無法讀取 commit 的內容")?;
    // 根節點沒有父節點，與空樹比較即為「全部新增」。
    let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());

    let mut options = DiffOptions::new();
    options.context_lines(3);
    let diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options))
        .context("無法計算 commit 的差異")?;
    build_file_diffs(&diff)
}

/// 觸及某個檔案的 commit，由新到舊。
pub fn file_history(repo: &Repository, path: &str, limit: usize) -> Result<Vec<String>> {
    let mut walk = repo.revwalk().context("無法建立 revwalk")?;
    walk.push_head().context("無法從 HEAD 開始走訪")?;
    walk.set_sorting(git2::Sort::TIME).context("無法設定順序")?;

    let mut result = Vec::new();
    for oid in walk {
        if result.len() >= limit {
            break;
        }
        let oid = oid.context("走訪時發生錯誤")?;
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        let Ok(tree) = commit.tree() else {
            continue;
        };
        let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());

        let mut options = DiffOptions::new();
        options.pathspec(path);
        let Ok(diff) =
            repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options))
        else {
            continue;
        };
        if diff.deltas().len() > 0 {
            result.push(oid.to_string());
        }
    }
    Ok(result)
}

/// 即將進來的遠端變更會改到哪些檔案的哪些行。
///
/// 以 HEAD 與遠端追蹤分支的差異計算。回傳的行號是相對於 HEAD 的，
/// 因此可以直接和工作目錄的差異比對。
pub fn incoming_line_ranges(
    repo: &Repository,
) -> Result<std::collections::HashMap<String, Vec<(u32, u32)>>> {
    let mut result = std::collections::HashMap::new();

    let Ok(head) = repo.head() else {
        return Ok(result);
    };
    let Ok(branch_name) = head.shorthand() else {
        return Ok(result);
    };
    let Ok(branch) = repo.find_branch(branch_name, git2::BranchType::Local) else {
        return Ok(result);
    };
    let Ok(upstream) = branch.upstream() else {
        return Ok(result);
    };
    let (Ok(head_tree), Ok(upstream_tree)) = (head.peel_to_tree(), upstream.get().peel_to_tree())
    else {
        return Ok(result);
    };

    let mut options = DiffOptions::new();
    options.context_lines(0);
    let diff = repo
        .diff_tree_to_tree(Some(&head_tree), Some(&upstream_tree), Some(&mut options))
        .context("無法計算即將進來的差異")?;

    for index in 0..diff.deltas().len() {
        let Some(patch) = Patch::from_diff(&diff, index).context("無法讀取 patch")? else {
            continue;
        };
        let Some(delta) = diff.get_delta(index) else {
            continue;
        };
        let Some(path) = delta.old_file().path().or_else(|| delta.new_file().path()) else {
            continue;
        };
        let path = path.to_string_lossy().into_owned();

        let mut ranges = Vec::new();
        for hunk_index in 0..patch.num_hunks() {
            let Ok((hunk, _)) = patch.hunk(hunk_index) else {
                continue;
            };
            let start = hunk.old_start();
            ranges.push((start, start + hunk.old_lines()));
        }
        result.insert(path, ranges);
    }
    Ok(result)
}

/// 兩個行區間是否重疊。
fn ranges_overlap(left: (u32, u32), right: (u32, u32)) -> bool {
    left.0 <= right.1 && right.0 <= left.1
}

/// 標記出會與即將進來的變更相撞的區段。
///
/// 兩份差異的基準都是 HEAD，因此行號可直接比對。索引與 HEAD 不同時
/// 會有誤差，所以這是風險提示而非保證 —— 介面用語必須反映這一點。
pub fn mark_incoming_collisions(
    diffs: &mut [FileDiff],
    incoming: &std::collections::HashMap<String, Vec<(u32, u32)>>,
) {
    for file in diffs.iter_mut() {
        let Some(ranges) = incoming.get(&file.path) else {
            continue;
        };
        for hunk in file.hunks.iter_mut() {
            let span = (hunk.old_start, hunk.old_start + hunk.old_lines);
            hunk.collides_with_incoming = ranges.iter().any(|range| ranges_overlap(span, *range));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_keeps_identifiers_together() {
        let tokens: Vec<&str> = tokenize("let value_1 = 2;")
            .iter()
            .map(|(_, t)| *t)
            .collect();
        assert!(tokens.contains(&"value_1"));
        assert!(tokens.contains(&"let"));
        assert!(tokens.contains(&";"));
    }

    #[test]
    fn tokenizer_does_not_split_multibyte_characters() {
        let tokens: Vec<&str> = tokenize("你好 world").iter().map(|(_, t)| *t).collect();
        assert!(tokens.contains(&"你"));
        assert!(tokens.contains(&"好"));
        assert!(tokens.contains(&"world"));
    }

    #[test]
    fn intra_line_spans_cover_only_the_changed_part() {
        let (old_spans, new_spans) = changed_spans("let total = 1;", "let total = 42;");
        assert_eq!(old_spans.len(), 1);
        assert_eq!(new_spans.len(), 1);
        // 變動處應落在數字上，而不是整行。
        assert_eq!(&"let total = 1;"[old_spans[0].start..old_spans[0].end], "1");
        assert_eq!(
            &"let total = 42;"[new_spans[0].start..new_spans[0].end],
            "42"
        );
    }

    #[test]
    fn identical_lines_have_no_spans() {
        let (old_spans, new_spans) = changed_spans("same", "same");
        assert!(old_spans.is_empty());
        assert!(new_spans.is_empty());
    }

    #[test]
    fn whitespace_only_changes_are_detected() {
        assert!(is_whitespace_only_change("  let a = 1;", "\tlet a = 1;"));
        assert!(is_whitespace_only_change("a b", "a  b"));
        assert!(!is_whitespace_only_change("let a = 1;", "let a = 2;"));
    }

    #[test]
    fn overlapping_ranges_are_detected() {
        assert!(ranges_overlap((10, 20), (15, 25)));
        assert!(ranges_overlap((10, 20), (20, 30)), "邊界相接視為重疊");
        assert!(!ranges_overlap((10, 20), (21, 30)));
        assert!(ranges_overlap((10, 20), (12, 14)), "完全包含也算");
    }

    #[test]
    fn collisions_are_marked_only_on_overlapping_hunks() {
        let mut diffs = vec![FileDiff {
            path: "a.rs".to_owned(),
            old_path: None,
            is_binary: false,
            added: 0,
            removed: 0,
            hunks: vec![
                Hunk {
                    header: String::new(),
                    old_start: 10,
                    old_lines: 5,
                    new_start: 10,
                    new_lines: 5,
                    lines: Vec::new(),
                    collides_with_incoming: false,
                },
                Hunk {
                    header: String::new(),
                    old_start: 100,
                    old_lines: 2,
                    new_start: 100,
                    new_lines: 2,
                    lines: Vec::new(),
                    collides_with_incoming: false,
                },
            ],
        }];
        let mut incoming = std::collections::HashMap::new();
        incoming.insert("a.rs".to_owned(), vec![(12u32, 18u32)]);

        mark_incoming_collisions(&mut diffs, &incoming);
        assert!(diffs[0].hunks[0].collides_with_incoming, "第一段重疊");
        assert!(!diffs[0].hunks[1].collides_with_incoming, "第二段不重疊");
    }

    #[test]
    fn files_without_incoming_changes_are_left_alone() {
        let mut diffs = vec![FileDiff {
            path: "b.rs".to_owned(),
            old_path: None,
            is_binary: false,
            added: 0,
            removed: 0,
            hunks: vec![Hunk {
                header: String::new(),
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                lines: Vec::new(),
                collides_with_incoming: false,
            }],
        }];
        mark_incoming_collisions(&mut diffs, &std::collections::HashMap::new());
        assert!(!diffs[0].hunks[0].collides_with_incoming);
    }

    fn line(kind: LineKind, content: &str) -> DiffLine {
        DiffLine {
            kind,
            content: content.to_owned(),
            old_lineno: None,
            new_lineno: None,
            spans: Vec::new(),
            whitespace_only: false,
        }
    }

    #[test]
    fn paired_changes_get_intra_line_annotation() {
        let mut lines = vec![
            line(LineKind::Context, "unchanged"),
            line(LineKind::Removed, "let a = 1;"),
            line(LineKind::Added, "let a = 2;"),
        ];
        annotate_intra_line(&mut lines);
        assert!(!lines[1].spans.is_empty(), "刪除行應標出變動處");
        assert!(!lines[2].spans.is_empty(), "新增行應標出變動處");
        assert!(lines[0].spans.is_empty(), "未變更的行不應被標記");
    }

    #[test]
    fn unpaired_changes_are_left_unannotated() {
        // 刪一行、加三行：這是整段替換，逐行標示會誤導。
        let mut lines = vec![
            line(LineKind::Removed, "old"),
            line(LineKind::Added, "new one"),
            line(LineKind::Added, "new two"),
            line(LineKind::Added, "new three"),
        ];
        annotate_intra_line(&mut lines);
        assert!(lines.iter().all(|line| line.spans.is_empty()));
    }

    #[test]
    fn a_hunk_of_only_whitespace_changes_is_flagged() {
        let mut lines = vec![
            line(LineKind::Context, "fn main() {"),
            line(LineKind::Removed, "  let a = 1;"),
            line(LineKind::Added, "\tlet a = 1;"),
        ];
        annotate_intra_line(&mut lines);
        let hunk = Hunk {
            header: String::new(),
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 2,
            lines,
            collides_with_incoming: false,
        };
        assert!(hunk.whitespace_only());
    }

    #[test]
    fn a_hunk_with_real_changes_is_not_flagged_as_whitespace() {
        let mut lines = vec![
            line(LineKind::Removed, "let a = 1;"),
            line(LineKind::Added, "let a = 2;"),
        ];
        annotate_intra_line(&mut lines);
        let hunk = Hunk {
            header: String::new(),
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            lines,
            collides_with_incoming: false,
        };
        assert!(!hunk.whitespace_only());
    }

    #[test]
    fn an_all_context_hunk_is_not_whitespace_only() {
        let hunk = Hunk {
            header: String::new(),
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            lines: vec![line(LineKind::Context, "unchanged")],
            collides_with_incoming: false,
        };
        assert!(!hunk.whitespace_only());
    }
}
