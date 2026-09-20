//! s1m CLI entry point.
//!
//! The scaffold's usage text and exit codes are the fixed part (0 = reading list
//! returned, 1 = nothing cleared the threshold, 2 = error). The query pipeline
//! still lands in later issues; what is here is the spike's debug view of one
//! file, `s1m score-file <query> <file>` (hidden), so the numbers can be
//! eyeballed before traversal exists.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

use s1m::cache::{CachedScorer, Scored};
use s1m::jev::{JevDetail, JevScorer, Mode};
use s1m::parse::{self, ParsedFile};
use s1m::scorer::{FileJudgment, LinkJudgment};

/// Listed in the help so an agent reading `--help` today is not misled about
/// the interface #8 and later issues will implement.
const AFTER_HELP: &str = "\
Planned options, not implemented yet:
  --mode, --criteria, --no-cache, --max-files, --max-depth, --threshold,
  --fanout, --seed-grep, --format, --root

Exit codes:
  0  reading list returned
  1  nothing cleared the threshold
  2  error

The query pipeline is not implemented yet. One hidden command works:
`s1m score-file <query> <file>` scores a single file with Jev and prints the
numbers behind it (see docs/spike-notes.md).";

#[derive(Debug, Parser)]
#[command(
    name = "s1m",
    version,
    about = "Ranks local files for a query so an agent reads only what matters",
    override_usage = "s1m <query> <entry-file...> [OPTIONS]",
    after_help = AFTER_HELP,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Score one file against a query with Jev and print a table.
    ///
    /// Hidden: a debug view of the spike in
    /// https://github.com/mikekelly/s1m/issues/5, not the CLI's interface.
    #[command(hide = true)]
    ScoreFile {
        /// What the file should be useful for, in the asker's own words.
        query: String,
        /// The markdown or text file to score.
        file: PathBuf,
        /// The directory the file's links were written against. Defaults to the
        /// file's own directory.
        #[arg(long, value_name = "DIR")]
        root: Option<PathBuf>,
        /// Leave the target's title, frontmatter and first paragraph out of the
        /// request: the control case for whether a preview earns its tokens.
        #[arg(long)]
        no_previews: bool,
        /// Call Jev even for a request already answered and stored, so the
        /// numbers are this run's rather than the cache's.
        #[arg(long)]
        no_cache: bool,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let outcome = match cli.command {
        Command::ScoreFile {
            query,
            file,
            root,
            no_previews,
            no_cache,
        } => score_file(&query, &file, root, !no_previews, no_cache).await,
    };

    if let Err(error) = outcome {
        eprintln!("s1m: {error:#}");
        std::process::exit(2);
    }
}

/// One file, one Jev request if the answer is not already stored, one table.
async fn score_file(
    query: &str,
    file: &Path,
    root: Option<PathBuf>,
    previews: bool,
    no_cache: bool,
) -> Result<()> {
    let root = root.unwrap_or_else(|| {
        file.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf()
    });

    let parsed = parse::parse(file, &root)?;
    let jev = JevScorer::from_env(&root)?.with_previews(previews);
    let mode = jev.mode();

    let (judgment, source) = if no_cache {
        let outcome = jev.judge(query, &parsed).await?;
        (outcome.judgment, Source::Uncached(outcome.detail))
    } else {
        let cached = CachedScorer::from_env(jev)?;
        let dir = cached.dir().to_path_buf();
        match cached.judge(query, &parsed).await? {
            Scored::Called { judgment, detail } => (judgment, Source::Call { dir, detail }),
            Scored::Reused(judgment) => (judgment, Source::Entry(dir)),
        }
    };

    print!(
        "{}",
        report(query, &parsed, &judgment, mode, previews, &source)
    );
    Ok(())
}

/// Where the answer came from, which is what the debug view is for: a cached
/// answer has no model, tokens or latency of its own to report.
enum Source {
    /// Read from the entry under this directory.
    Entry(PathBuf),
    /// Called now, and stored under this directory.
    Call { dir: PathBuf, detail: JevDetail },
    /// Called now, with `--no-cache`: nothing was stored.
    Uncached(JevDetail),
}

/// The debug view: what was asked, what it cost, and one row per link.
fn report(
    query: &str,
    file: &ParsedFile,
    judgment: &FileJudgment,
    mode: &Mode,
    previews: bool,
    source: &Source,
) -> String {
    let mut out = String::new();
    field(&mut out, "query", query);
    field(&mut out, "file", &file.path.display().to_string());
    field(&mut out, "title", &file.title);
    field(&mut out, "mode", mode.name);
    field(&mut out, "previews", if previews { "on" } else { "off" });
    match source {
        Source::Entry(dir) => field(&mut out, "cache", &format!("hit   {}", dir.display())),
        Source::Call { dir, .. } => field(&mut out, "cache", &format!("miss  {}", dir.display())),
        Source::Uncached(_) => field(&mut out, "cache", "off   (--no-cache)"),
    }
    // The two numbers the plan's output keeps apart: this command scores one
    // file, and `calls` is what that cost the API.
    field(&mut out, "files", "1");
    field(
        &mut out,
        "calls",
        if matches!(source, Source::Entry(_)) {
            "0"
        } else {
            "1"
        },
    );
    out.push('\n');

    field(&mut out, "relevance", &format!("{:.2}", judgment.relevance));
    if let Source::Call { detail, .. } | Source::Uncached(detail) = source {
        field(
            &mut out,
            "",
            &format!(
                "score {:.2} of {}   confidence {:.2}",
                detail.relevance_level,
                mode.top_level(),
                detail.relevance_confidence
            ),
        );
        field(
            &mut out,
            "call",
            &format!(
                "{}   {} questions   {} tokens in + {} out   {:.2}s   ${:.6}",
                detail.model,
                detail.questions,
                detail.input_tokens,
                detail.output_tokens,
                detail.latency.as_secs_f64(),
                detail.cost_usd()
            ),
        );
    }
    out.push('\n');

    // Best scent first: the point of the table is which links the model would
    // follow, and the response order is the file's own.
    let mut rows: Vec<(&parse::Link, &LinkJudgment)> =
        file.links.iter().zip(&judgment.links).collect();
    rows.sort_by(|a, b| {
        b.1.scent
            .total_cmp(&a.1.scent)
            .then_with(|| a.0.target.cmp(&b.0.target))
    });

    let targets: Vec<String> = rows
        .iter()
        .map(|(link, _)| link.target.display().to_string())
        .collect();
    let target_width = targets
        .iter()
        .map(|target| target.chars().count())
        .chain(["target".len()])
        .max()
        .unwrap_or_default();

    let _ = writeln!(out, "scent  {:<target_width$}  anchor", "target");
    let _ = writeln!(
        out,
        "-----  {}  {}",
        "-".repeat(target_width),
        "-".repeat(30)
    );
    for ((link, judgment), target) in rows.into_iter().zip(targets) {
        let _ = writeln!(
            out,
            "{:<5.2}  {target:<target_width$}  {}",
            judgment.scent,
            shorten(&link.anchor, 30)
        );
    }
    out
}

/// One `name  value` line of the report, with the names aligned.
fn field(out: &mut String, name: &str, value: &str) {
    let _ = writeln!(out, "{name:<10}{value}");
}

/// `text` cut to `limit` characters, marked when something was dropped.
fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(limit.saturating_sub(1)).collect();
    format!("{head}…")
}
