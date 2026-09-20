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
use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use s1m::cache::{CachedScorer, Scored};
use s1m::cli::{self, Judge, Options, Uncached};
use s1m::format::Format;
use s1m::ignore::{self, Ignore};
use s1m::jev::{self, JevDetail, JevScorer, Mode};
use s1m::parse::{self, ParsedFile};
use s1m::scorer::{FileJudgment, LinkJudgment, ScorerError, SectionJudgment};

/// The plan's defaults for the budgets, and the seed count
/// [#14](https://github.com/mikekelly/s1m/issues/14) asks for. They live on the
/// flags that carry them rather than in the library: nothing below the CLI has a
/// default of its own.
const MAX_FILES: usize = 25;
const MAX_DEPTH: usize = 6;
const THRESHOLD: f64 = 0.6;
const FANOUT: usize = 8;
const SEED_COUNT: usize = 5;

/// The one paragraph that says what s1m is, for `--help`: the same wording the
/// README opens with and `SKILL.md` carries, so a person or an agent that meets
/// s1m anywhere is told the same thing.
///
/// It leads with what the tool does because the name reads as "sim" and the
/// first guess is a simulator, which is the one thing this is not.
const DESCRIPTION: &str = "\
s1m reads local markdown files and ranks them for a query, so an agent opens
only what matters. The name is pronounced \"sim\" — it is short for System 1
memex, after Vannevar Bush's memex — and s1m is not a simulator.

Give it a query and one or more entry files: it scores each file and each of
its outgoing links with a fast judgment model (Jev, TypeSafe's System One
model), follows the most promising links first, and prints a ranked reading
list with the line ranges worth reading. JSON is the default; md and tree are
for a person. It generates no text, answers no question and builds no index:
the output is what to open, and the caller decides.

The files the walk visits, and the links it judges, are sent to the TypeSafe
API — that is what the judgment is bought with — so a private wiki needs a
`.s1mignore` at the root saying what must not leave the machine.";

const AFTER_HELP: &str = "\
The reading list goes to stdout as JSON by default: most relevant first, then
by path, and every path in it spelled the way the entry files were. Each result
carries the ranges worth reading: one entry per heading section, with the lines
to read, the score it was judged at, and the sections below
--section-threshold left out. A result with a via path was reached along a
link; one without is an entry file, and `seeded` says whether --seed-grep put
it on the frontier.

--format picks how that list is printed. json is the plan's shape, for a caller
that parses it. md is the same list to read or paste: each file with the lines
worth reading, the heading to look for and the scores, most relevant first.
tree is the walk's link tree: every file it visited with every link it judged
beneath it, each link with the scent the model gave it and whether the walk
followed it, so it is plain to see what was passed over and how narrowly.
Scores print at two decimals in md and tree; json keeps the model's own numbers.

--mode picks the criterion Jev judges by (about, useful-for, answers); a
--criteria FILE replaces it with a criterion of your own: the file's whole
content, trimmed. --criteria wins when both are given, and the reading list
reports the path as `mode`.

A `.s1mignore` in the root holds the paths that are never read, in gitignore
syntax (`private/`, `*.key.md`, `!keep.md`). A link whose target it matches is
not sent to the model, not previewed and not followed; a page it matches is not
seeded by --seed-grep; and an entry file it matches is an error rather than a
silent read.

Exit codes:
  0  the walk reached files beyond the entry files
  1  nothing cleared the threshold: only the entry files were reached
  2  error: the reason on stderr in one line, or a usage message for a flag that
     does not exist or will not take that value

A hidden debug view of one file is still here: `s1m score-file <query> <file>`
prints a file's relevance, what the call cost, a score per section and a scent
per link (see docs/spike-notes.md).";

#[derive(Debug, Parser)]
#[command(
    name = "s1m",
    version,
    about = "Reads local markdown files and ranks them for a query, so an agent opens only what matters",
    long_about = DESCRIPTION,
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

    /// How relevance is judged: `about` collects everything on a subject,
    /// `useful-for` finds what helps someone doing what the query describes,
    /// `answers` finds the page that answers the question.
    #[arg(long, value_name = "MODE", value_enum, default_value_t = ModeArg::UsefulFor)]
    mode: ModeArg,

    /// A file holding a criterion of your own: its whole content, trimmed,
    /// replaces the criterion `--mode` would judge by, and the reading list
    /// reports the path as `mode`.
    #[arg(long, value_name = "FILE")]
    criteria: Option<PathBuf>,

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

    /// Least section score the reading list keeps, 0 to 1; a section below it is
    /// left out. Defaults to --threshold.
    #[arg(long, value_name = "SCORE", value_parser = threshold)]
    section_threshold: Option<f64>,

    /// Frontier files expanded per round.
    #[arg(long, value_name = "N", default_value_t = FANOUT)]
    fanout: usize,

    /// Add the top keyword hits under the root as extra entry files, so pages
    /// that are orphaned or weakly linked are still reached. The hits enter the
    /// walk like entry files, and the reading list marks them with `seeded`.
    #[arg(long)]
    seed_grep: bool,

    /// How many keyword hits `--seed-grep` adds.
    #[arg(
        long,
        value_name = "N",
        default_value_t = SEED_COUNT,
        requires = "seed_grep"
    )]
    seed_count: usize,

    /// Call Jev for every file, ignoring the answers already on disk.
    #[arg(long)]
    no_cache: bool,

    /// How the reading list is printed: `json` for a caller that parses it,
    /// `md` for a reading list to paste, `tree` for the walk's link tree.
    #[arg(long, value_name = "FORMAT", value_enum, default_value_t = FormatArg::Json)]
    format: FormatArg,

    #[command(subcommand)]
    command: Option<Command>,
}

/// The criterion `--mode` picks, spelled as the plan's Relevance modes table
/// names it. The wording each one sends lives in [`s1m::jev`], one const per
/// mode; this is only the flag's vocabulary, and clap rejects anything else
/// with the usage message, exit 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ModeArg {
    About,
    UsefulFor,
    Answers,
}

impl ModeArg {
    /// The mode itself: the questions and the criteria this run asks.
    fn mode(self) -> Mode {
        match self {
            ModeArg::About => jev::ABOUT.clone(),
            ModeArg::UsefulFor => jev::USEFUL_FOR.clone(),
            ModeArg::Answers => jev::ANSWERS.clone(),
        }
    }
}

/// The shape of the reading list on stdout: the plan's `--format` flag, spelled
/// as [`s1m::format`] names its views. This is only the flag's vocabulary; what
/// each view prints lives in that module, so it is testable without a CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum FormatArg {
    /// The plan's `Output` section: what a caller parses.
    Json,
    /// A reading list to paste: each file with the lines worth reading.
    Md,
    /// The walk's link tree: every judged link, with its scent and whether the
    /// walk followed it.
    Tree,
}

impl FormatArg {
    /// The view itself.
    fn format(self) -> Format {
        match self {
            FormatArg::Json => Format::Json,
            FormatArg::Md => Format::Md,
            FormatArg::Tree => Format::Tree,
        }
    }
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
    /// one has been built, from `--criteria`'s file when there is one and
    /// `--mode` otherwise.
    ///
    /// `--section-threshold` defaults to `--threshold`, which is the plan's
    /// flag table and is resolved here because it is one flag's value standing
    /// in for another's, not a constant.
    fn options(&self) -> Options {
        Options {
            query: self.query.clone().unwrap_or_default(),
            entries: self.entries.clone(),
            root: self.root.clone(),
            max_files: self.max_files,
            max_depth: self.max_depth,
            threshold: self.threshold,
            section_threshold: self.section_threshold.unwrap_or(self.threshold),
            fanout: self.fanout,
            seed_grep: self.seed_grep.then_some(self.seed_count),
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
/// The criterion is resolved, and the scorer built, before anything is read: a
/// missing `TYPESAFE_API_KEY`, a mode that is not one of the three, and a
/// `--criteria` file that cannot be read or holds nothing each cost no file
/// reads at all. The scorer is what names the criterion the reading list
/// reports, because it is what carries the questions.
async fn query(cli: &Cli) -> Result<i32, cli::Error> {
    let mut options = cli.options();
    let Some(root) = options.root() else {
        return Err(cli::Error::MissingArguments);
    };

    let mode = criterion(cli.mode, cli.criteria.as_deref()).unwrap_or_else(|message| fail(message));
    let jev = scorer(&root)?.with_mode(mode);
    options.mode = jev.mode().name.to_string();

    let judge: Box<dyn Judge> = if cli.no_cache {
        Box::new(Uncached::new(jev))
    } else {
        Box::new(CachedScorer::from_env(jev)?)
    };

    let list = cli::run(&options, judge.as_ref()).await?;
    print!("{}", cli.format.format().render(&list));
    Ok(list.exit_code())
}

/// The criterion this run judges by: the `--criteria` file's when there is one,
/// else `--mode`'s.
///
/// The file's whole content is the criterion, trimmed, because a criterion is a
/// sentence about what makes content relevant and that is what a caller has to
/// write. It overrides `--mode` rather than conflicting with it, the way the
/// plan's flag table says. A file that cannot be read, or that holds nothing, is
/// the caller's mistake: the message names the path and the run exits 2 before
/// anything is bought.
fn criterion(mode: ModeArg, criteria: Option<&Path>) -> Result<Mode, String> {
    let Some(path) = criteria else {
        return Ok(mode.mode());
    };
    let text = fs::read_to_string(path)
        .map_err(|source| format!("failed to read criteria file {}: {source}", path.display()))?;
    let criterion = text.trim();
    if criterion.is_empty() {
        return Err(format!("criteria file {} holds nothing", path.display()));
    }
    Ok(Mode::custom(path.display().to_string(), criterion))
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

    // The same rules a query runs under, for the same reason: this view reads
    // the file and previews its link targets, and a matched path is not read.
    let ignore = Ignore::at(&root)?;
    if ignore.matched(&parse::relative_to_root(&root, file)) {
        anyhow::bail!(
            "{} is matched by {}: s1m never reads an ignored file",
            file.display(),
            root.join(ignore::FILE).display()
        );
    }

    let mut parsed = parse::parse(file, &root)?;
    parsed.links.retain(|link| !ignore.matched(&link.target));
    let jev = scorer(&root)?.with_previews(previews);
    let mode = jev.mode().clone();

    let (judgment, source) = if no_cache {
        let outcome = jev.judge(query, &parsed).await?;
        (outcome.judgment, Source::Uncached(outcome.detail))
    } else {
        let cached = CachedScorer::from_env(jev)?;
        let dir = cached.dir().to_path_buf();
        match cached.judge(query, &parsed).await? {
            Scored::Called { judgment, detail } => (judgment, Source::Call { dir, detail }),
            Scored::Reused { judgment, .. } => (judgment, Source::Entry(dir)),
        }
    };

    print!(
        "{}",
        report(query, &parsed, &judgment, &mode, previews, &source)
    );
    Ok(())
}

/// Where the answer came from, which is what the debug view is for: this is
/// what the run in front of the caller did, so a cached answer reports the hit
/// and no call of its own, however much the call that stored it cost.
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
    field(&mut out, "mode", &mode.name);
    field(&mut out, "previews", if previews { "on" } else { "off" });
    match source {
        Source::Entry(dir) => field(&mut out, "cache", &format!("hit   {}", dir.display())),
        Source::Call { dir, .. } => field(&mut out, "cache", &format!("miss  {}", dir.display())),
        Source::Uncached(_) => field(&mut out, "cache", "off   (--no-cache)"),
    }
    // The two numbers the plan's output keeps apart: this command scores one
    // file, and `calls` is what that cost the API.
    field(&mut out, "files", "1");
    // The cache's count, not the API's: a file whose sections and links did not
    // fit one request is one judgment bought with several requests, and what
    // this command reports as a call is the judgment. The `call` line below
    // says how many requests it took.
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
                "{}   {} questions in {} request(s)   {} tokens in + {} out   {:.2}s   ${:.6}",
                detail.model,
                detail.questions,
                detail.requests,
                detail.input_tokens,
                detail.output_tokens,
                detail.latency.as_secs_f64(),
                detail.cost_usd()
            ),
        );
    }
    out.push('\n');

    // Best score first, like the links below: the point of the table is which
    // of the file's own ranges the model would have a reader open, and the
    // range is the parser's, so a line here is a range to read.
    let mut sections: Vec<(&parse::Section, &SectionJudgment)> =
        file.sections.iter().zip(&judgment.sections).collect();
    sections.sort_by(|a, b| {
        b.1.score
            .total_cmp(&a.1.score)
            .then_with(|| a.0.lines[0].cmp(&b.0.lines[0]))
    });

    let ranges: Vec<String> = sections
        .iter()
        .map(|(section, _)| format!("{}-{}", section.lines[0], section.lines[1]))
        .collect();
    let range_width = ranges
        .iter()
        .map(|range| range.chars().count())
        .chain(["lines".len()])
        .max()
        .unwrap_or_default();

    let _ = writeln!(out, "score  {:<range_width$}  heading", "lines");
    let _ = writeln!(
        out,
        "-----  {}  {}",
        "-".repeat(range_width),
        "-".repeat(30)
    );
    for ((section, judged), range) in sections.into_iter().zip(ranges) {
        let heading = section.heading.as_deref().unwrap_or("(no heading)");
        let _ = writeln!(
            out,
            "{:<5.2}  {range:<range_width$}  {}",
            judged.score,
            shorten(heading, 30)
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
