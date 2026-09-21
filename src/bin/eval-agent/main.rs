//! `eval-agent`: what a reading list is worth against an agent that explores.
//!
//! Three commands, each taking the wiki and the gold set as paths so that the
//! same tooling measures a wiki with nothing committed about it:
//!
//! - `graph-stats --wiki <dir> --out <dir>` — what the wiki's link graph looks
//!   like, as numbers.
//! - `run --wiki <dir> --gold <file> --out <dir>` — every query under every
//!   condition, one JSONL row a run, reduced to `aggregates.json`. Resumable,
//!   and priced before it buys anything: see [`run`].
//! - `report --out <dir>` — the committed half: a markdown report of numbers.
//!
//! The split is the point. `--out` holds the raw rows, which name files and
//! quote queries, and is never committed. The report is rendered from the
//! aggregates alone, which hold numbers, query ids, category labels, the models
//! measured and the revision of the wiki the runs were measured against — a
//! content hash and not a page. [`report::render`] refuses a label that looks
//! like a path or reads like a question. See `eval/agent/README.md`.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};

mod aggregate;
mod explore;
mod gold;
mod graph;
mod hook;
mod ledger;
mod reading;
mod report;
mod row;
mod run;
#[cfg(test)]
mod testkit;
mod wiki;

#[derive(Debug, Parser)]
#[command(
    name = "eval-agent",
    about = "Measures s1m against a Claude Code Explore agent on a wiki.",
    after_help = "\
Nothing about a wiki is committed: --wiki, --gold and --out are paths, and the
raw rows under --out stay there. The report is numbers, query ids and category
labels, and nothing else.

  cargo run --release --bin eval-agent -- graph-stats --wiki DIR --out DIR
  cargo run --release --bin eval-agent -- run --wiki DIR --gold FILE --out DIR
  cargo run --release --bin eval-agent -- report --out DIR"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, clap::Args)]
struct RunArgs {
    #[arg(long)]
    wiki: PathBuf,
    /// The gold set: one JSON file of labelled queries.
    #[arg(long)]
    gold: PathBuf,
    /// Where the rows, the raw output and the aggregates are written. Not
    /// a directory to commit: the rows quote queries and name files.
    #[arg(long)]
    out: PathBuf,
    /// Measure only these query ids: a whole gold set costs real money,
    /// and two queries say whether a run is worth making.
    #[arg(long, value_delimiter = ',')]
    queries: Vec<String>,
    /// How many times each query is measured under each condition.
    #[arg(long, default_value_t = 1)]
    repeats: usize,
    /// Which conditions to measure.
    #[arg(long, value_delimiter = ',', default_value = "explore,s1m,s1m-agent")]
    conditions: Vec<String>,
    /// Where s1m's stored answers live, shared across runs. Defaults to a
    /// directory under --out.
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    /// Print what the runs that are owed are expected to cost, priced by what
    /// runs of the same condition have cost before, and buy nothing.
    #[arg(long)]
    estimate: bool,
    /// Run even when that estimate is above the limit this harness spends
    /// without being told; the estimate and the limit are printed either way.
    #[arg(long)]
    yes: bool,
    /// Re-run the runs this directory recorded as failed, and nothing else.
    #[arg(long)]
    retry_failed: bool,
    /// The ledger of what has been bought, shared by every run directory so
    /// that a second --out cannot buy what the first one already has. Defaults
    /// to `$S1M_LEDGER`, else a file under `$XDG_DATA_HOME`, else under `$HOME`.
    #[arg(long)]
    ledger: Option<PathBuf>,
    /// The page a query with no `entry` of its own starts from. Repeat it for
    /// a wiki with several ways in; the default is the `index.md` convention.
    #[arg(long, default_values_t = [String::from("index.md")])]
    entry: Vec<String>,
    /// The model the agent runs on. With none named, no `--model` reaches
    /// Claude Code and every agent in the run — the subagent included — takes
    /// its own default; the models that answered are recorded per run either
    /// way.
    #[arg(long)]
    model: Option<String>,
    /// The s1m binary. Defaults to the one beside this one.
    #[arg(long)]
    s1m: Option<PathBuf>,
    /// The Claude Code binary.
    #[arg(long, default_value = "claude")]
    claude: PathBuf,
    /// Where Claude Code keeps its project transcripts. Defaults to
    /// `$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects`.
    #[arg(long)]
    transcripts: Option<PathBuf>,
    /// How many repeats of the s1m condition are also measured cold, with
    /// every judgment bought. Each one is bought again.
    #[arg(long, default_value_t = 1)]
    cold_repeats: usize,
    /// How long one run may take before it is killed.
    #[arg(long, default_value_t = 900)]
    timeout_secs: u64,
}

/// The `run` command takes far more flags than the other two, so its variant
/// is far larger. A command line is parsed once into one of these and matched
/// immediately; an indirection to even the variants up would buy nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
enum Command {
    /// What the wiki's link graph looks like, as numbers.
    GraphStats {
        /// The wiki to walk: a directory of markdown.
        #[arg(long)]
        wiki: PathBuf,
        /// Where `graph_stats.json` and `graph_stats.md` are written.
        #[arg(long)]
        out: PathBuf,
        /// The page depth is measured from, relative to the wiki root, for a
        /// wiki whose root holds neither `index.md` nor `README.md`. Repeat it
        /// for a wiki with several ways in: every one of them is at depth
        /// zero. The report says only that an entry page was given, never
        /// which.
        #[arg(long)]
        entry: Vec<String>,
    },
    /// Every query under every condition, one JSONL row a run.
    Run(RunArgs),
    /// The committed half: a markdown report of numbers.
    Report {
        /// The directory `run` wrote `aggregates.json` into.
        #[arg(long)]
        out: PathBuf,
        /// The graph statistics to open the report with. Defaults to
        /// `graph_stats.json` under --out when it is there.
        #[arg(long)]
        stats: Option<PathBuf>,
        /// Write the report here instead of stdout.
        #[arg(long)]
        report: Option<PathBuf>,
    },
}

fn main() {
    let args = Args::parse();
    if let Err(error) = dispatch(args.command) {
        eprintln!("eval-agent: {error}");
        std::process::exit(2);
    }
}

fn dispatch(command: Command) -> Result<(), String> {
    match command {
        Command::GraphStats { wiki, out, entry } => graph_stats(&wiki, &out, &entry),
        Command::Run(RunArgs {
            wiki,
            gold,
            out,
            queries,
            repeats,
            conditions,
            cache_dir,
            estimate,
            yes,
            retry_failed,
            ledger,
            entry,
            model,
            s1m,
            claude,
            transcripts,
            cold_repeats,
            timeout_secs,
        }) => run::run(&run::Options {
            wiki,
            gold,
            out,
            queries,
            repeats,
            conditions,
            cache_dir,
            estimate,
            yes,
            retry_failed,
            ledger,
            entry,
            model,
            s1m: s1m.unwrap_or_else(beside_me),
            claude,
            transcripts,
            cold_repeats,
            timeout: Duration::from_secs(timeout_secs),
        }),
        Command::Report { out, stats, report } => render(&out, stats, report),
    }
}

/// The s1m binary beside this one, which is where `cargo build` puts it.
fn beside_me() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("s1m")))
        .unwrap_or_else(|| PathBuf::from("s1m"))
}

fn graph_stats(
    wiki: &std::path::Path,
    out: &std::path::Path,
    entry: &[String],
) -> Result<(), String> {
    let stats = graph::collect(wiki, entry)?;
    std::fs::create_dir_all(out).map_err(|error| format!("{}: {error}", out.display()))?;

    let json = out.join("graph_stats.json");
    let text = serde_json::to_string_pretty(&stats).map_err(|error| error.to_string())?;
    std::fs::write(&json, text).map_err(|error| format!("{}: {error}", json.display()))?;

    let table = graph::table(&stats);
    let markdown = out.join("graph_stats.md");
    std::fs::write(&markdown, &table)
        .map_err(|error| format!("{}: {error}", markdown.display()))?;

    eprintln!(
        "eval-agent: {} and {} written",
        json.display(),
        markdown.display()
    );
    print!("{table}");
    Ok(())
}

fn render(
    out: &std::path::Path,
    stats: Option<PathBuf>,
    report: Option<PathBuf>,
) -> Result<(), String> {
    let path = out.join("aggregates.json");
    let text = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "{}: {error} — `run` writes it, and `report` renders what it wrote",
            path.display()
        )
    })?;
    let aggregates: aggregate::Aggregates =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;

    let stats = stats.or_else(|| Some(out.join("graph_stats.json")).filter(|path| path.is_file()));
    let stats = match stats {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            Some(
                serde_json::from_str::<graph::GraphStats>(&text)
                    .map_err(|error| format!("{}: {error}", path.display()))?,
            )
        }
        None => None,
    };

    let method = aggregates.method.clone().unwrap_or_default();
    let rendered = report::render(&aggregates, stats.as_ref(), &method)?;
    match report {
        Some(path) => {
            std::fs::write(&path, &rendered)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            eprintln!("eval-agent: report written to {}", path.display());
        }
        None => print!("{rendered}"),
    }
    Ok(())
}
