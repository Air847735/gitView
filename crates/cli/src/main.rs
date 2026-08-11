//! gitview 的命令列工具。
//!
//! 在圖形介面存在之前，用來驗證核心運算對真實 repository 是否正確：
//! 讀出 commit DAG、計算佈局，並以文字呈現分支走向。
//!
//! 本工具只讀取，不會修改任何 repository。

use std::process::ExitCode;

use anyhow::{Context, Result};
use git2::Repository;
use gitview_core::{lay_out, repo, CommitGraph, Layout};

const DEFAULT_LIMIT: usize = 40;

struct Options {
    path: String,
    limit: usize,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut path = None;
    let mut limit = DEFAULT_LIMIT;
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

fn run(options: Options) -> Result<()> {
    let repository = Repository::discover(&options.path)
        .with_context(|| format!("找不到 git repository：{}", options.path))?;

    let graph = repo::load_graph(&repository)?;
    let summary = repo::summarize(&repository, &graph)?;
    let layout = lay_out(&graph)?;

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
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse_args(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("錯誤：{error:#}");
            eprintln!("用法：gitview [repository 路徑] [--limit N]");
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
