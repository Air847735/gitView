//! gitview 的命令列工具。
//!
//! 在圖形介面存在之前，用來驗證核心運算對真實 repository 是否正確：
//! 讀出 commit DAG、計算佈局，並以文字呈現分支走向。
//!
//! 本工具只讀取，不會修改任何 repository。

use std::process::ExitCode;

use anyhow::{Context, Result};
use git2::Repository;
use gitview_core::diff;
use gitview_core::divergence::{self, Divergence};
use gitview_core::{lay_out, repo, CommitGraph, Layout};

const DEFAULT_LIMIT: usize = 40;

struct Options {
    path: String,
    limit: usize,
    /// 顯示未提交的差異而非歷史圖。
    show_diff: bool,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut path = None;
    let mut limit = DEFAULT_LIMIT;
    let mut show_diff = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--limit" | "-n" => {
                index += 1;
                let value = args.get(index).context("--limit 後面需要一個數字")?;
                limit = value
                    .parse()
                    .with_context(|| format!("--limit 的值不是有效數字：{value}"))?;
            }
            "--diff" | "-d" => show_diff = true,
            other if other.starts_with('-') => {
                anyhow::bail!("未知的選項：{other}");
            }
            other => {
                if path.is_some() {
                    anyhow::bail!("只能指定一個 repository 路徑");
                }
                path = Some(other.to_owned());
            }
        }
        index += 1;
    }

    Ok(Options {
        path: path.unwrap_or_else(|| ".".to_owned()),
        limit,
        show_diff,
    })
}

/// 標記每一列上有哪些線道被穿越的線佔用。
///
/// 端點所在的列由節點本身佔用，因此只標記兩端之間的列。
fn passing_lanes(layout: &Layout) -> Vec<Vec<bool>> {
    let mut busy = vec![vec![false; layout.lane_count]; layout.rows()];
    for edge in &layout.edges {
        let lane = edge.parent_lane;
        if lane >= layout.lane_count {
            continue;
        }
        let start = edge.child_row.min(edge.parent_row) + 1;
        let end = edge.child_row.max(edge.parent_row);
        if start < end {
            for row in &mut busy[start..end] {
                row[lane] = true;
            }
        }
    }
    busy
}

/// 文字輸出最多繪製的線道數。
///
/// 實際 repository 的線道數可以到數百條（git 專案的整合分支就有 282 條），
/// 全部畫出來一列會超過上千個字元。超出的部分以計數表示。
const MAX_RENDERED_LANES: usize = 12;

fn render(graph: &CommitGraph, layout: &Layout, limit: usize) {
    let busy = passing_lanes(layout);
    let shown = limit.min(layout.rows());
    let width = layout.lane_count.min(MAX_RENDERED_LANES);

    for (row_lanes, &node) in busy.iter().zip(layout.order.iter()).take(shown) {
        let lane = layout.lane_of[node];
        let commit = graph.commit(node);

        let mut track = String::new();
        for (column, &occupied) in row_lanes.iter().take(width).enumerate() {
            if column == lane {
                track.push(if commit.is_merge() { '◍' } else { '●' });
            } else if occupied {
                track.push('│');
            } else {
                track.push(' ');
            }
            track.push(' ');
        }

        // 節點落在可繪製範圍之外時，以線道編號標示，避免它從畫面上消失。
        let overflow = if lane >= width {
            format!("→{lane:<3} ")
        } else {
            String::from("     ")
        };

        let short_oid: String = commit.oid.chars().take(8).collect();
        let summary: String = commit.summary.chars().take(56).collect();
        println!("{track}{overflow}{short_oid}  {summary}");
    }

    if layout.lane_count > width {
        println!(
            "（僅繪製前 {width} 條線道，另有 {} 條未繪製）",
            layout.lane_count - width
        );
    }
    if layout.rows() > shown {
        println!("… 其餘 {} 個 commit 未顯示", layout.rows() - shown);
    }
}

/// 印出同步狀態與分岔分析。
///
/// 這是產品的核心功能，圖形介面不可用時需要能從命令列驗證。
fn print_sync(divergence: &Divergence) {
    let Some(upstream) = divergence.upstream.as_deref() else {
        println!("  同步狀態：沒有追蹤的遠端分支");
        return;
    };

    println!(
        "\n同步狀態（對 {upstream}）：{}",
        divergence.recommendation.headline()
    );
    println!(
        "  本機獨有 {} 個 commit · 遠端獨有 {} 個 commit",
        divergence.ahead.len(),
        divergence.behind.len()
    );

    if !divergence.behind.is_empty() {
        println!(
            "  即將進來的變更觸及 {} 個檔案",
            divergence.incoming_files.len()
        );
    }
    if divergence.overlapping_files.is_empty() {
        if divergence.is_diverged() {
            println!("  兩側沒有改到同一個檔案，可以確定不會衝突");
        }
    } else {
        println!(
            "  兩側都改到的檔案 {} 個（可能衝突，非必然）：",
            divergence.overlapping_files.len()
        );
        for path in divergence.overlapping_files.iter().take(8) {
            println!("    {path}");
        }
        if divergence.overlapping_files.len() > 8 {
            println!("    … 另有 {} 個", divergence.overlapping_files.len() - 8);
        }
    }
    if !divergence.uncommitted_overlap.is_empty() {
        println!(
            "  警告：{} 個尚未提交的檔案會被進來的變更影響：",
            divergence.uncommitted_overlap.len()
        );
        for path in divergence.uncommitted_overlap.iter().take(8) {
            println!("    {path}");
        }
    }

    if divergence.is_diverged() {
        println!(
            "  rebase 之後主線 {} 個 commit；merge 之後 {} 個（多一個合併節點）",
            divergence.commits_after_rebase(),
            divergence.commits_after_merge()
        );
    }

    for (label, commits) in [
        ("即將進來", &divergence.behind),
        ("尚未推送", &divergence.ahead),
    ] {
        for commit in commits.iter().take(5) {
            println!("  {label}  {}  {}", commit.short_oid, commit.summary);
        }
        if commits.len() > 5 {
            println!("  {label}  … 另有 {} 個", commits.len() - 5);
        }
    }
}

/// 印出未提交的差異，含行內標示與撞擊警告。
fn print_diff(repository: &Repository) -> Result<()> {
    let mut files = diff::workspace_diff(repository, diff::DiffSource::Unstaged)?;
    if let Ok(incoming) = diff::incoming_line_ranges(repository) {
        diff::mark_incoming_collisions(&mut files, &incoming);
    }
    if files.is_empty() {
        println!("沒有未提交的變更");
        return Ok(());
    }

    for file in &files {
        println!("\n── {}  +{} −{}", file.path, file.added, file.removed);
        if file.is_binary {
            println!("   （二進位檔案）");
            continue;
        }
        for hunk in &file.hunks {
            let mut tags = Vec::new();
            if hunk.collides_with_incoming {
                tags.push("← 即將進來的變更也會改到這一段");
            }
            if hunk.whitespace_only() {
                tags.push("（只有空白差異）");
            }
            println!("   {} {}", hunk.header, tags.join(" "));

            for line in &hunk.lines {
                let sign = match line.kind {
                    diff::LineKind::Added => '+',
                    diff::LineKind::Removed => '-',
                    diff::LineKind::Context => ' ',
                };
                // 以 [ ] 標出行內實際變動的片段，讓終端機也看得到字元層級差異。
                let rendered = if line.spans.is_empty() {
                    line.content.clone()
                } else {
                    let mut out = String::new();
                    let mut cursor = 0;
                    for span in &line.spans {
                        let start = span.start.min(line.content.len());
                        let end = span.end.min(line.content.len());
                        if start > cursor {
                            out.push_str(&line.content[cursor..start]);
                        }
                        out.push('[');
                        out.push_str(&line.content[start..end]);
                        out.push(']');
                        cursor = end;
                    }
                    if cursor < line.content.len() {
                        out.push_str(&line.content[cursor..]);
                    }
                    out
                };
                println!("   {sign}{rendered}");
            }
        }
    }
    Ok(())
}

fn run(options: Options) -> Result<()> {
    let repository = Repository::discover(&options.path)
        .with_context(|| format!("找不到 git repository：{}", options.path))?;

    if options.show_diff {
        return print_diff(&repository);
    }

    let graph = repo::load_graph(&repository)?;
    let summary = repo::summarize(&repository, &graph)?;
    let layout = lay_out(&graph)?;
    let divergence = divergence::analyse(&repository)?;

    println!("{}", summary.path);
    println!(
        "  分支 {} · {} commits · {} merges · 工作目錄異動 {} 個檔案{}",
        summary.branch.as_deref().unwrap_or("(detached)"),
        summary.commit_count,
        summary.merge_count,
        summary.dirty_files,
        if summary.operation_in_progress {
            " · 有未完成的操作"
        } else {
            ""
        }
    );
    println!(
        "  佈局：{} 列 · {} 條線道 · {} 條需要跨線道",
        layout.rows(),
        layout.lane_count,
        layout.crossing_edges()
    );
    println!();

    render(&graph, &layout, options.limit);
    print_sync(&divergence);
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse_args(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("錯誤：{error:#}");
            eprintln!("用法：gitview [repository 路徑] [--limit N] [--diff]");
            return ExitCode::FAILURE;
        }
    };

    match run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("錯誤：{error:#}");
            ExitCode::FAILURE
        }
    }
}
