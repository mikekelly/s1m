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
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use s1m::cache::{Cacheable, CachedScorer, Scored};
use s1m::cli::{self, Judge, Options, Uncached};
use s1m::format::Format;
use s1m::ignore::{self, Ignore};
use s1m::jev::{self, ChoiceScorer, Context, JevDetail, JevScorer, KeepRule, Mode, Wording};
use s1m::parse::{self, ParsedFile};
use s1m::scorer::{FileJudgment, LinkJudgment, ScorerError, SectionJudgment};
use s1m::trace::Trace;
use s1m::traverse::Admission;

/// The plan's defaults for the budgets. They live on the flags that carry them
/// rather than in the library: nothing below the CLI has a default of its own.
/// [`FANOUT`] is the exception — the round size is the walk's, and the CLI has
/// no flag for it, so this constant is where it is handed over.
const MAX_FILES: usize = 25;
const MAX_DEPTH: usize = 6;
const THRESHOLD: f64 = 0.6;
const FANOUT: usize = 8;

/// The relative-link spike's defaults ([#47]): the share a Choice answer has to
/// hold to keep its link ([`KeepRule`]), and how many paths the frontier holds
/// at one depth when the walk follows shares.
///
/// They belong to the hidden flags that carry them and to no other caller: a
/// walk under the shipping scorer is not a beam search, and its links are not
/// weighed against each other.
///
/// [#47]: https://github.com/mikekelly/s1m/issues/47
const SHARE_FLOOR: f64 = 0.02;
const SHARE_K: usize = 3;
const BEAM: usize = 8;

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
by path, and every path in it spelled the way the entry files were. `results`
holds the files that earn a place on their own — relevance at or above
--threshold, or a section at or above it — each with the ranges worth reading:
one entry per heading section, with the lines to read, the score it was judged
at, and the sections below --threshold left out. A result with a via path was
reached along a link; one without is an entry file.

The files the walk visited without earning a place — entry pages, hubs, section
indexes, and pages that fell short of --threshold — are reported under `walked`
instead: path, relevance, scent, via and the links it judged, and no sections.
They are what the list was reached through, not pages to read, and `visited`
counts them and the results together.

--format picks how that list is printed. json is the plan's shape, for a caller
that parses it: each link carries the scent it was given and what the walk did
about it — `followed`, or a `reason` naming the rule that queued nothing
(`below-threshold`, `out-of-root`, `past-depth`, `already-reached`, `not-kept`).
md is what to read, to paste into a task: the files that earned a place, each
with the lines worth reading, the heading to look for and the scores, most
relevant first. tree is the walk's link tree: every file it visited, walked files
included, with every link it judged beneath it, each link marked with the scent
the model gave it and what the walk did about it — `followed`, `already reached`
or `pruned` — so it is plain to see what was passed over and how narrowly.
Scores print at two decimals in md and tree; json keeps the model's own numbers.

--mode picks the criterion Jev judges by (about, useful-for, answers); a
--criteria FILE replaces it with a criterion of your own: the file's whole
content, trimmed. --criteria wins when both are given, and the reading list
reports the path as `mode`.

A `.s1mignore` in the root holds the paths that are never read, in gitignore
syntax (`private/`, `*.key.md`, `!keep.md`). A link whose target it matches is
not sent to the model, not previewed and not followed; and an entry file it
matches is an error rather than a silent read.

Exit codes:
  0  the walk reached files beyond the entry files that earned a place
  1  nothing beyond the entry files did: results is the entry files alone, or
     empty with the walk under `walked`. No link cleared the threshold,
     everything reached was a hub, or the page a link reached could not be
     judged
  2  error: the reason on stderr in one line, or a usage message for a flag that
     does not exist or will not take that value

A page the walk reached but could not judge is not an error: it is named on
stderr as skipped, its links are not followed, and the reading list keeps every
page that was judged. A page that was judged and earned no place is not an error
either: it is under `walked`, with the links it offered. Only a run that judged
nothing at all — an entry file whose judgment failed with nothing else reached —
is 2.

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

    /// Ask the three questions in another register, leaving the criterion
    /// alone: each name is a wording [`s1m::jev`] states, and the default is
    /// the wording that ships.
    ///
    /// Global, so it reads the same before or after a subcommand — `score-file`
    /// takes it too, and by the same name rather than one of its own.
    ///
    /// Hidden: the experiment in https://github.com/mikekelly/s1m/issues/52,
    /// not the CLI's interface. This flag and `--mode` compose rather than
    /// conflict — one picks what counts as relevant, the other how the
    /// questions about it are put — and the reading list reports the criterion's
    /// name, not the wording's.
    #[arg(long, value_name = "NAME", value_enum, hide = true, global = true)]
    wording: Option<WordingArg>,

    /// The directory that bounds the walk; a link resolving outside it is not
    /// followed. Defaults to the first entry file's directory.
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// Most files the walk judges beyond the entry files, which are always
    /// visited.
    #[arg(long, value_name = "N", default_value_t = MAX_FILES)]
    max_files: usize,

    /// Most link hops from an entry file.
    #[arg(long, value_name = "N", default_value_t = MAX_DEPTH)]
    max_depth: usize,

    /// Least link scent that queues a target, and least relevance or section
    /// score a file needs to earn a place in the reading list, 0 to 1.
    #[arg(long, value_name = "SCENT", default_value_t = THRESHOLD, value_parser = threshold)]
    threshold: f64,

    /// Call Jev for every file, ignoring the answers already on disk.
    #[arg(long)]
    no_cache: bool,

    /// Write a trace of the walk to FILE: one JSON object per line, each
    /// stamped with the milliseconds since the walk started, so a run can be
    /// replayed as a timed animation of the crawl and the reading list filling
    /// in.
    ///
    /// Every event of the walk is in it — the files popped off the frontier,
    /// the link targets admitted or passed over, each file's answer and each
    /// request it took, and every visit — and the file is written as the walk
    /// goes, so a run that is killed leaves what happened up to the kill.
    ///
    /// Hidden: the format is the interface, not the flag, and it is
    /// documented in the README rather than here
    /// (https://github.com/mikekelly/s1m/issues/57).
    #[arg(long, value_name = "FILE", hide = true)]
    trace: Option<PathBuf>,

    /// Leave each link target's own H2/H3 headings out of its preview, the way
    /// the preview was before [#46]. Hidden: the ablation the evaluation
    /// measures.
    #[arg(long, hide = true)]
    no_preview_headings: bool,

    /// Leave the anchor text of each link target's own in-root links out of its
    /// preview. Hidden: the same ablation.
    #[arg(long, hide = true)]
    no_preview_leads: bool,

    /// Ask the link question about one hop rather than two, the way it was
    /// asked before [#46]. Hidden: the same ablation.
    #[arg(long, hide = true)]
    one_hop_links: bool,

    /// How the reading list is printed: `json` for a caller that parses it,
    /// `md` for what to read, `tree` for the walk's link tree.
    #[arg(long, value_name = "FORMAT", value_enum, default_value_t = FormatArg::Json)]
    format: FormatArg,

    /// Judge each page's links against each other — one Choice over them, by
    /// share — instead of asking a yes/no question about each one.
    ///
    /// Hidden: the spike in https://github.com/mikekelly/s1m/issues/47, not the
    /// CLI's interface, and the default is the walk that ships.
    #[arg(long, value_name = "SCORER", value_enum, default_value_t = ScorerArg::Noul, hide = true)]
    scorer: ScorerArg,

    /// Send each option's target preview with its description, so a link is
    /// judged with a look at where it goes and not only at what the page says
    /// about it.
    #[arg(long, hide = true)]
    previews: bool,

    /// Least share of a page's Choice that keeps a link, when the k-scaled cut
    /// is lower.
    #[arg(long, value_name = "SHARE", default_value_t = SHARE_FLOOR, hide = true)]
    share_floor: f64,

    /// The cut's numerator: a link has to hold `k / options` of the page's
    /// probability, capped at 0.5.
    #[arg(long, value_name = "N", default_value_t = SHARE_K, hide = true)]
    share_k: usize,

    /// Most paths the frontier holds at one depth. Defaults to [`BEAM`] under
    /// `--scorer choice`, and to no ceiling at all otherwise.
    #[arg(long, value_name = "N", hide = true)]
    beam: Option<usize>,

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

/// The wording `--wording` picks, spelled as [`s1m::jev::Wording::name`] spells
/// it. The wording itself — the sentences each one sends — lives in that module;
/// this is only the flag's vocabulary, and clap rejects anything else with the
/// usage message, exit 2.
///
/// Hidden like the flag: the experiment [#52] measured these as, not a caller's
/// choice.
///
/// [#52]: https://github.com/mikekelly/s1m/issues/52
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum WordingArg {
    Navigator,
    Path,
    SharpNo,
    Rules,
    Necessity,
    SectionLegacy,
    ReaderAction,
    AnswerBearing,
    Reader,
}

impl WordingArg {
    /// The wording itself.
    fn wording(self) -> Wording {
        match self {
            WordingArg::Navigator => Wording::Navigator,
            WordingArg::Path => Wording::Path,
            WordingArg::SharpNo => Wording::SharpNo,
            WordingArg::Rules => Wording::Rules,
            WordingArg::Necessity => Wording::Necessity,
            WordingArg::SectionLegacy => Wording::SectionLegacy,
            WordingArg::ReaderAction => Wording::ReaderAction,
            WordingArg::AnswerBearing => Wording::AnswerBearing,
            WordingArg::Reader => Wording::Reader,
        }
    }
}

/// How a run's links are judged: the plan's one question per link, or the
/// spike's one Choice over a page's links ([#47]).
///
/// The default is what ships. The other is what the hidden flag is for, and what
/// the walk's [`Admission::Scorer`] rule and the scorer's keep rule are for: a
/// share means something only beside the options it was weighed against, so the
/// walk cannot threshold it the way it thresholds a Noul.
///
/// [#47]: https://github.com/mikekelly/s1m/issues/47
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ScorerArg {
    /// One Noul per link: how likely following it is to lead somewhere useful.
    Noul,
    /// One Choice over the page's links: which of them is the best next step.
    Choice,
}

impl ScorerArg {
    /// The name the reading list reports it under.
    fn name(self) -> &'static str {
        match self {
            ScorerArg::Noul => "noul",
            ScorerArg::Choice => "choice",
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
    /// What to read, to paste: the files that earned a place, each with the
    /// lines worth reading.
    Md,
    /// The walk's link tree: every file it visited, `walked` included, and
    /// every judged link with its scent and what became of it.
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
        /// Leave the target's H2/H3 headings out of each link's preview.
        /// Hidden: the same ablation the query's hidden flags ask for.
        #[arg(long, hide = true)]
        no_preview_headings: bool,
        /// Leave the anchor text of each target's own in-root links out of its
        /// preview. Hidden, the same ablation.
        #[arg(long, hide = true)]
        no_preview_leads: bool,
        /// Ask the link question about one hop rather than two. Hidden, the
        /// same ablation.
        #[arg(long, hide = true)]
        one_hop_links: bool,
    },
}

impl Cli {
    /// What the request carries about each link, as the hidden ablation flags
    /// take it away ([`Context`]). A run that names none of them sends the
    /// state that ships.
    fn context(&self) -> Context {
        Context {
            headings: !self.no_preview_headings,
            leads: !self.no_preview_leads,
            two_hop: !self.one_hop_links,
            ..Context::default()
        }
    }

    /// The flags as the run wants them. The mode is the scorer's to name,
    /// because the scorer is what carries the criterion; it is filled in once
    /// one has been built, from `--criteria`'s file when there is one and
    /// `--mode` otherwise.
    fn options(&self) -> Options {
        Options {
            query: self.query.clone().unwrap_or_default(),
            entries: self.entries.clone(),
            root: self.root.clone(),
            max_files: self.max_files,
            max_depth: self.max_depth,
            threshold: self.threshold,
            admission: self.admission(),
            beam: self.beam(),
            fanout: FANOUT,
            mode: String::new(),
            scorer: self.scorer.name().to_string(),
        }
    }

    /// How the walk admits a link, which follows from the scorer: a Noul is
    /// compared to the threshold the caller gave, and a Choice share is kept by
    /// the scorer's own rule because it means something only beside the options
    /// it was weighed against.
    fn admission(&self) -> Admission {
        match self.scorer {
            ScorerArg::Noul => Admission::Threshold(self.threshold),
            ScorerArg::Choice => Admission::Scorer,
        }
    }

    /// What the keep rule keeps: the two numbers the hidden flags carry.
    fn keep(&self) -> KeepRule {
        KeepRule {
            floor: self.share_floor,
            k: self.share_k,
        }
    }

    /// The beam: `--beam` when the caller gave one, [`BEAM`] when the walk is
    /// following shares, and no ceiling at all for the walk that ships.
    ///
    /// A beam is part of how the relative judge is followed and not a change to
    /// the walk underneath it, so turning it on with `--scorer noul` is the
    /// caller's business and leaving it off with `--scorer choice` is a
    /// deliberate comparison of one change rather than two.
    fn beam(&self) -> Option<usize> {
        self.beam.or(match self.scorer {
            ScorerArg::Choice => Some(BEAM),
            ScorerArg::Noul => None,
        })
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
            no_preview_headings,
            no_preview_leads,
            one_hop_links,
        }) => {
            let context = Context {
                previews: !no_previews,
                headings: !no_preview_headings,
                leads: !no_preview_leads,
                two_hop: !one_hop_links,
                ..Context::default()
            };
            if let Err(error) =
                score_file(&query, &file, root, context, no_cache, cli.wording).await
            {
                fail(format!("{error:#}"));
            }
        }
        None => match query(&cli).await {
            Ok(code) => {
                if code != 0 {
                    eprintln!(
                        "s1m: nothing beyond the entry files earned a place in the reading \
                         list: nothing cleared the threshold, everything reached was a hub, \
                         or the page a link reached could not be judged"
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
    let mode = match cli.wording {
        Some(wording) => wording.wording().word(mode),
        None => mode,
    };
    // The trace is created before anything is bought: a file that cannot be
    // written is the caller's mistake, and it costs nothing to say so before a
    // round of judgments rather than after one.
    let trace = trace(cli.trace.as_deref(), &root, cli.threshold);
    let context = cli.context();
    let judge: Box<dyn Judge> = match cli.scorer {
        ScorerArg::Noul => {
            let jev = context.apply(scorer(&root)?).with_mode(mode);
            options.mode = jev.mode().name.to_string();
            cached(jev, cli.no_cache, trace)?
        }
        ScorerArg::Choice => {
            // The file's own judgment is made with the state that ships; only
            // the options' descriptions are the `--previews` flag's business.
            let choice = ChoiceScorer::new(context.apply(scorer(&root)?).with_mode(mode))
                .with_keep(cli.keep())
                .with_context(Context {
                    previews: cli.previews,
                    ..context
                });
            options.mode = choice.mode().name.to_string();
            cached(choice, cli.no_cache, trace)?
        }
    };

    let list = cli::run(&options, judge.as_ref()).await?;
    print!("{}", cli.format.format().render(&list));
    Ok(list.exit_code())
}

/// One scorer as the run's judge: the cache in front of it, or nothing at all
/// when `--no-cache` said to buy every judgment.
///
/// Both scorers go through here, so `--no-cache` and the cache directory mean
/// the same thing whichever link judgment the run asked for, and the reading
/// list's `calls` counts the same thing either way. So does the run's trace,
/// when there is one: both sides report the requests they make and the answers
/// they get to it, so a cold run's trace and a warm one's have the same shape
/// and differ in `cached` ([`s1m::trace`]).
fn cached<S: Cacheable + 'static>(
    scorer: S,
    no_cache: bool,
    trace: Option<Arc<Trace>>,
) -> Result<Box<dyn Judge>, ScorerError> {
    Ok(if no_cache {
        Box::new(Uncached::new(scorer).with_trace(trace))
    } else {
        Box::new(CachedScorer::from_env(scorer)?.with_trace(trace))
    })
}

/// The run's trace, when `--trace` named a file, or `None` for a run nobody is
/// watching.
///
/// A file that cannot be written is the caller's mistake and the run's error,
/// reported before anything is read or bought: a run whose trace is not the
/// trace the caller asked for has no business starting.
fn trace(file: Option<&Path>, root: &Path, threshold: f64) -> Option<Arc<Trace>> {
    let file = file?;
    match Trace::create(file, root, threshold) {
        Ok(trace) => Some(Arc::new(trace)),
        Err(source) => fail(format!(
            "could not write the trace to {}: {source}",
            file.display()
        )),
    }
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
    context: Context,
    no_cache: bool,
    wording: Option<WordingArg>,
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
    let jev = context.apply(scorer(&root)?);
    // This view judges by the default criterion, so a wording re-words it; a
    // query's mode comes from `--mode` and is worded there.
    let jev = match wording {
        Some(wording) => {
            let mode = wording.wording().word(jev.mode().clone());
            jev.with_mode(mode)
        }
        None => jev,
    };
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
        report(query, &parsed, &judgment, &mode, context.previews, &source)
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
