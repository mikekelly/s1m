//! s1m CLI entry point.
//!
//! One command: a query and one or more entry files, walked and ranked into the
//! reading list [`s1m::cli`] describes. The hidden `score-file` debug view of a
//! single file, which [#5]'s spike was measured with, stays for eyeballing the
//! numbers behind one page.
//!
//! The flags are here, the work is in the library: this file turns `argv` into
//! [`cli::Options`], chooses the scorer and its cache, and turns the outcome
//! into an exit code.
//!
//! [#5]: https://github.com/mikekelly/s1m/issues/5

use std::fmt::{Display, Write as _};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use s1m::cache::{CachedScorer, Scored};
use s1m::cli::{self, Judge, Options, Uncached};
use s1m::jev::{self, JevDetail, JevScorer, Mode};
use s1m::parse::{self, ParsedFile};
use s1m::scorer::{FileJudgment, LinkJudgment, ScorerError};

/// The plan's defaults for the budgets. They live on the flags that carry them
/// rather than in the library: nothing below the CLI has a default of its own.
const MAX_FILES: usize = 25;
const MAX_DEPTH: usize = 6;
const THRESHOLD: f64 = 0.6;
const FANOUT: usize = 8;

const AFTER_HELP: &str = "\
The reading list goes to stdout as JSON: most relevant first, then by path, and
every path in it spelled the way the entry files were.

Exit codes:
  0  the walk reached files beyond the entry files
  1  nothing cleared the threshold: only the entry files were reached
  2  error: the reason on stderr in one line, or a usage message for a flag that
     does not exist or will not take that value

Not implemented yet: --mode, --criteria, --seed-grep, and the md and tree
formats. json is the only format.

A hidden debug view of one file is still here: `s1m score-file <query> <file>`
prints a file's relevance, what the call cost, and a scent per link (see
docs/spike-notes.md).";

#[derive(Debug, Parser)]
#[command(
    name = "s1m",
    version,
    about = "Ranks local files for a query so an agent reads only what matters",
    override_usage = "s1m <query> <entry-file...> [OPTIONS]",
    after_help = AFTER_HELP,
    arg_required_else_help = true,
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
struct Cli {
    /// What the reading list should be useful for, in the asker's own words.
    query: Option<String>,

    /// Files to start the walk from: markdown or text files, whose links are
    /// followed.
    #[arg(value_name = "ENTRY", num_args = 1..)]
    entries: Vec<PathBuf>,

    /// The directory that bounds the walk; a link resolving outside it is not
    /// followed. Defaults to the first entry file's directory.
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// Most files visited before the walk stops.
    #[arg(long, value_name = "N", default_value_t = MAX_FILES)]
    max_files: usize,

    /// Most link hops from an entry file.
    #[arg(long, value_name = "N", default_value_t = MAX_DEPTH)]
    max_depth: usize,

    /// Least link scent that queues a target, 0 to 1.
    #[arg(long, value_name = "SCENT", default_value_t = THRESHOLD, value_parser = threshold)]
    threshold: f64,

    /// Frontier files expanded per round.
    #[arg(long, value_name = "N", default_value_t = FANOUT)]
    fanout: usize,

    /// Call Jev for every file, ignoring the answers already on disk.
    #[arg(long)]
    no_cache: bool,

    /// The shape of the reading list on stdout.
    #[arg(long, value_name = "FORMAT", value_enum, default_value_t = Format::Json)]
    format: Format,

    #[command(subcommand)]
    command: Option<Command>,
}

/// The shape of the reading list on stdout.
///
/// `json` is the only one so far: [#9] adds the section scores to this shape,
/// and the human views after that.
///
/// [#9]: https://github.com/mikekelly/s1m/issues/9
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Json,
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

impl Cli {
    /// The flags as the run wants them. The mode is the scorer's to name,
    /// because the scorer is what carries the criterion; it is filled in once
    /// one has been built.
    fn options(&self) -> Options {
        Options {
            query: self.query.clone().unwrap_or_default(),
            entries: self.entries.clone(),
            root: self.root.clone(),
            max_files: self.max_files,
            max_depth: self.max_depth,
            threshold: self.threshold,
            fanout: self.fanout,
            mode: String::new(),
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::ScoreFile {
            query,
            file,
            root,
            no_previews,
            no_cache,
        }) => {
            if let Err(error) = score_file(&query, &file, root, !no_previews, no_cache).await {
                fail(format!("{error:#}"));
            }
        }
        None => match query(&cli).await {
            Ok(code) => {
                if code != 0 {
                    eprintln!(
                        "s1m: no file beyond the entry files was reached: \
                         nothing cleared the threshold"
                    );
                }
                std::process::exit(code);
            }
            Err(error) => fail(error),
        },
    }
}

/// One query: walk, rank, print, and say which code the run earned.
///
/// The scorer is built before anything is read, so a missing `TYPESAFE_API_KEY`
/// costs no file reads at all, and it is what names the criterion the reading
/// list reports.
async fn query(cli: &Cli) -> Result<i32, cli::Error> {
    let mut options = cli.options();
    let Some(root) = options.root() else {
        return Err(cli::Error::MissingArguments);
    };

    let jev = scorer(&root)?;
    options.mode = jev.mode().name.to_string();

    let judge: Box<dyn Judge> = if cli.no_cache {
        Box::new(Uncached::new(jev))
    } else {
        Box::new(CachedScorer::from_env(jev)?)
    };

    let list = cli::run(&options, judge.as_ref()).await?;
    match cli.format {
        Format::Json => println!("{}", list.to_json()),
    }
    Ok(list.exit_code())
}

/// The Jev scorer this run asks, pointed at `S1M_ENDPOINT` when something else
/// answers there: a proxy, or a test's fake server.
fn scorer(root: &Path) -> Result<JevScorer, ScorerError> {
    let scorer = JevScorer::from_env(root)?;
    Ok(match endpoint() {
        Some(endpoint) => scorer.with_endpoint(endpoint),
        None => scorer,
    })
}

/// The endpoint to ask instead of the API: `S1M_ENDPOINT`, when it is set to
/// something. Set but blank counts as unset, the way the cache directory treats
/// it.
fn endpoint() -> Option<String> {
    std::env::var(jev::ENDPOINT_VAR)
        .ok()
        .filter(|endpoint| !endpoint.trim().is_empty())
}

/// `--threshold` is a probability, and a scent is 0 to 1: a value outside that
/// follows nothing or follows everything, which is never what a caller meant.
fn threshold(text: &str) -> Result<f64, String> {
    let value: f64 = text
        .parse()
        .map_err(|_| format!("{text} is not a number"))?;
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{text} is not between 0 and 1"))
    }
}

/// Prints one line on stderr and exits 2.
///
/// One line because a caller reading stderr is parsing it, and an error can
/// carry a response body with newlines in it.
fn fail(message: impl Display) -> ! {
    eprintln!("s1m: {}", message.to_string().replace(['\n', '\r'], " "));
    std::process::exit(2);
}

/// One file, one Jev request if the answer is not already stored, one table.
async fn score_file(
    query: &str,
    file: &Path,
    root: Option<PathBuf>,
    previews: bool,
    no_cache: bool,
) -> anyhow::Result<()> {
    let root = root.unwrap_or_else(|| {
        file.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf()
    });

    let parsed = parse::parse(file, &root)?;
    let jev = scorer(&root)?.with_previews(previews);
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
