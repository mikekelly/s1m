//! The evaluation harness: what s1m's reading list is worth, as numbers.
//!
//! `cargo run --release --bin eval -- --wiki <dir> --gold <file>` walks a wiki
//! whose pages a person has labelled, and reports what the reading list found
//! and what it cost. Both arguments are paths, so the same harness measures the
//! vendored wiki under `eval/wikis/` and a private one with nothing committed —
//! `eval/REPORT.md` is the run on the vendored wiki, and its *How to reproduce*
//! section is the command.
//!
//! Per query, at each file budget:
//!
//! - **Recall and precision** against the gold set: what the agent is asked to
//!   read at that budget, and how much of it was wanted. The list holds only
//!   the files that earn a place on their own
//!   ([`s1m::traverse::VisitedFile::earns_a_place`]), so precision is reported
//!   twice — over the returned list, and over every file the walk judged, which
//!   is what it was while the list returned all of them — and the pair, with
//!   the wanted pages the cutoff drops, is what the cutoff bought and cost.
//! - **Section recall** beside them, and the lines the list returns: a gold
//!   entry may name a heading or a line range, the part of the page that
//!   answers ([#58]), and this is how many of those parts the returned ranges
//!   cover. A page returned with nothing to read is in the file list and not in
//!   this one; an entry that names no part is wanted whole, so its recall is
//!   the file's, and the pair reads as what the section scores cut.
//! - **Tokens the agent reads**, counted at [`CHARS_PER_TOKEN`] — the rule the
//!   spike set its caps with, because nothing here tokenises. Three sets: the
//!   returned line ranges, the same files whole, and the whole corpus. The
//!   first two differ by what the section scores buy, the last by what the
//!   ranking buys over opening files and hoping.
//! - **What the API was asked and what it cost**: answers served, answers
//!   bought, requests, questions, input and output tokens, and the latency of
//!   one answer (not wall time: a round joins its files, so it waits for the
//!   slowest, not for the sum).
//! - **A keyword baseline**: the harness's own keyword ranker — the query's
//!   terms counted in every page under the root — read as a reading list of
//!   whole files. What grep gets without a model.
//! - **Calibration**: the scent a link was given against what following it
//!   reached, which is the number that says where `--threshold` belongs.
//! - **The preview experiment** [#10] deferred: previews off, previews without
//!   frontmatter, previews as they ship.
//!
//! # Reproducibility
//!
//! Every judgment is keyed by the request that produced it, so `--cache <dir>`
//! makes a second run free — and identical, because the cache stores what each
//! call cost beside the answer it came with. A keyless rerun against a
//! committed cache therefore prints the same report, byte for byte. Nothing in
//! the report is a wall clock, and every number is an integer or a ratio of
//! integers accumulated in the walk's own order, so the diff stays clean.
//!
//! [#10]: https://github.com/mikekelly/s1m/issues/10
//! [#11]: https://github.com/mikekelly/s1m/issues/11
//! [#58]: https://github.com/mikekelly/s1m/issues/58

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use clap::Parser;
use serde::Deserialize;

use s1m::cache::{Cacheable, CachedScorer, Scored};
use s1m::ignore::Ignore;
use s1m::jev::{
    self, ChoiceScorer, Context, JevDetail, JevOutcome, JevScorer, KeepRule, Mode, Wording,
};
use s1m::parse::{self, ParsedFile};
use s1m::scorer::{FileJudgment, Scorer, ScorerError};
use s1m::traverse::{Admission, Config, Failure, traverse};

// ------------------------------------------------------------- what it varies

/// What no measurement here is about: the depth limit and the frontier round,
/// the values the CLI walks with. The round size has no flag — it is a
/// constant in `main.rs` — so the harness carries its own copy.
const MAX_DEPTH: usize = 6;
const FANOUT: usize = 8;

/// How many files a walk following shares visits at one depth, which is the
/// spike's own default ([`s1m::traverse::Config::beam`], and `main.rs`'s copy of
/// the same number). A walk that judges each link on its own has no beam: the
/// comparison is between link judgments, and a beam is how the relative one is
/// followed.
const BEAM: usize = 8;

/// Four characters per token, the rule `docs/spike-notes.md` sets its caps
/// with. Nothing here tokenises, and this is the estimate the reading numbers
/// are in.
const CHARS_PER_TOKEN: usize = 4;

/// The thresholds the sweep walks at, around the CLI's default of `0.6`. One
/// value covers the links and the sections, as the CLI's one `--threshold`
/// does.
const SWEEP: [f64; 4] = [0.5, 0.6, 0.7, 0.8];

/// How many bins the calibration tables cut 0 to 1 into.
const BINS: usize = 10;

/// The link state that shipped before [#46]: a preview with the title,
/// frontmatter and first paragraph, and the link question asked about one hop.
/// Every number in this report before that issue was measured against it.
///
/// [#46]: https://github.com/mikekelly/s1m/issues/46
const BEFORE_46: Context = Context {
    previews: true,
    frontmatter: true,
    headings: false,
    leads: false,
    two_hop: false,
};

/// The preview policies the experiment [#10] compares: what ships, then each
/// knob turned off on its own.
///
/// [#10]: https://github.com/mikekelly/s1m/issues/10
const PREVIEWS: [(&str, Context); 3] = [
    ("previews on (default)", Context::DEFAULT),
    (
        "previews, no frontmatter",
        Context {
            frontmatter: false,
            ..Context::DEFAULT
        },
    ),
    (
        "previews off",
        Context {
            previews: false,
            ..Context::DEFAULT
        },
    ),
];

/// What each part of the shipped link state is worth: the ablations of [#46],
/// in the report's order, one part of the state or the question taken away at a
/// time, then the state that shipped before the issue.
///
/// The rows are named for what they take away rather than what they add,
/// because what ships is now everything they add: the decision on the issue was
/// to carry the target's headings and its own link anchors and to ask the link
/// question about two hops, and these are what each of those three earns.
///
/// [#46]: https://github.com/mikekelly/s1m/issues/46
const CONTEXTS: [(&str, Context); 5] = [
    ("what ships (default)", Context::DEFAULT),
    (
        "no headings",
        Context {
            headings: false,
            ..Context::DEFAULT
        },
    ),
    (
        "no leads_to",
        Context {
            leads: false,
            ..Context::DEFAULT
        },
    ),
    (
        "one hop",
        Context {
            two_hop: false,
            ..Context::DEFAULT
        },
    ),
    ("before #46", BEFORE_46),
];

/// The keep rule the spike ships its flag with, and the looser one the last row
/// of the experiment walks at: three options' worth of a question's mass, and
/// one.
const KEEP: KeepRule = KeepRule { floor: 0.02, k: 3 };
const LOOSE: KeepRule = KeepRule { floor: 0.02, k: 1 };

/// The rows of the relative-judge experiment, at the tight budget: what ships,
/// then the same walk with a page's links judged against each other, from the
/// page's own words about them and with a look at each target beside them.
///
/// The shipping row is also the run the gold set already made for `at_a_budget`,
/// so it is not walked twice. `choice` and `choice + previews` are the design's
/// two rows ([#47]), and the last is the same walk at a looser cut — the one
/// knob that decides how much of a page's mass a link has to hold, so a row at
/// another setting says whether a difference is the link judgment or the rule it
/// is followed by.
///
/// `choice` describes its options from the page alone, which is what the
/// design's default is; `choice + previews` gives each option the preview the
/// state carries, headings and leads included, which is what the shipping walk
/// judges its links with.
///
/// [#47]: https://github.com/mikekelly/s1m/issues/47
const POLICIES: [(&str, Policy); 4] = [
    ("noul: what ships", Policy::noul(Context::DEFAULT)),
    (
        "choice",
        Policy {
            links: Links::Choice,
            context: Context {
                previews: false,
                ..Context::DEFAULT
            },
            wording: None,
            keep: KEEP,
        },
    ),
    (
        "choice + previews",
        Policy {
            links: Links::Choice,
            context: Context::DEFAULT,
            wording: None,
            keep: KEEP,
        },
    ),
    (
        "choice + previews, k=1",
        Policy {
            links: Links::Choice,
            context: Context::DEFAULT,
            wording: None,
            keep: LOOSE,
        },
    ),
];

// ---------------------------------------------------------------- the command

#[derive(Debug, Parser)]
#[command(
    name = "eval",
    about = "Measures s1m's reading list against a gold set on a real wiki.",
    after_help = "\
Every judgment is cached on the request that produced it, so a second run is
free and identical: --cache eval/cache against the committed cache reproduces
eval/REPORT.md with no key at all, and --no-cache with a key buys everything
again. Nothing about a wiki or a gold set is committed, so the same command
measures a private wiki:

  cargo run --release --bin eval -- --wiki /path/to/wiki --gold /path/to/gold.json"
)]
struct Args {
    /// The wiki to walk: a directory of markdown, the way `s1m` takes a root.
    #[arg(long)]
    wiki: PathBuf,
    /// The gold set: one JSON file of labelled queries.
    #[arg(long)]
    gold: PathBuf,
    /// Where the stored answers live. Defaults to `S1M_CACHE_DIR`, else the
    /// XDG cache directory, the way the CLI does.
    #[arg(long)]
    cache: Option<PathBuf>,
    /// File budgets to measure, smallest first.
    #[arg(long, value_delimiter = ',', default_value = "10,25")]
    budgets: Vec<usize>,
    /// Least link scent that queues a target. The same number is the reading
    /// list's cutoff: a visited file earns a place on its relevance or on one
    /// of its sections, and only sections at or above it are returned.
    #[arg(long, default_value_t = 0.6)]
    threshold: f64,
    /// Buy every judgment: ignore the answers on disk.
    #[arg(long)]
    no_cache: bool,
    /// Measure the relative judge — one Choice over a page's links — beside the
    /// walk that ships ([#47]), and add its table to the report.
    ///
    /// Off by default because its answers are not in the committed cache: a
    /// report is only worth committing if the cache reproduces it, and a run
    /// with this flag buys rows the cache does not hold. It is the flag that
    /// puts them there, once, and the report a run with it writes is then
    /// reproducible like any other.
    ///
    /// [#47]: https://github.com/mikekelly/s1m/issues/47
    #[arg(long)]
    relative_judge: bool,
    /// Measure the wordings — the three questions put in another register —
    /// beside the walk that ships ([#52]), and add their table to the report.
    ///
    /// Off by default for the same reason `--relative-judge` is: a report is
    /// only worth committing if the cache reproduces it, and a run with this
    /// flag buys rows the cache does not hold. It is the flag that puts them
    /// there, once, and the report a run with it writes is then reproducible
    /// like any other.
    ///
    /// [#52]: https://github.com/mikekelly/s1m/issues/52
    #[arg(long)]
    wordings: bool,
    /// Write the report here instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    match evaluate(&args).await {
        Ok(report) => match &args.out {
            Some(path) => {
                if let Err(error) = fs::write(path, &report) {
                    fail(format!("{}: {error}", path.display()));
                }
                eprintln!("eval: report written to {}", path.display());
            }
            None => print!("{report}"),
        },
        Err(error) => fail(error),
    }
}

/// One line on stderr and exit 2: a measurement that could not be made is not a
/// report with holes in it.
fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("eval: {message}");
    std::process::exit(2);
}

// ------------------------------------------------------------------ the gold

/// The gold set: the queries, and what a person would want for each.
#[derive(Debug, Deserialize)]
struct Gold {
    queries: Vec<Query>,
}

/// One labelled query.
#[derive(Debug, Deserialize)]
struct Query {
    /// A short name for the query, unique in the set: what the tables key on.
    id: String,
    /// The query, in the words the caller would use.
    query: String,
    /// What relevance means for it: `about`, `useful-for` or `answers`.
    mode: String,
    /// The entry file, relative to the wiki root.
    entry: String,
    /// The pages a person would want, most wanted first: a path for a page
    /// wanted whole, or an object naming the part of it that answers ([#58]).
    expected: Vec<Expected>,
    /// Why those pages, for whoever audits the labels.
    #[serde(default)]
    note: Option<String>,
    /// Every wanted page with the lines that answer resolved against the wiki:
    /// what [`Query::expected`] says, read once at load. Skipped by serde
    /// because it is the wiki's answer to the labels, not a label of its own.
    #[serde(skip)]
    wanted: Vec<Wanted>,
}

/// One entry of a gold set's wanted pages: a page, or the part of a page that
/// answers the query.
///
/// A page is file-level scoring, which is what every gold set written before
/// [#58] means. A part is the same page with a reading question attached: the
/// `heading` whose section answers — resolved against the parser the walk uses,
/// its subsections included — or the `lines` that do, 1-based and inclusive as
/// the parser gives them. One or the other, never both: a label names where the
/// answer is, and there is one answer.
///
/// [#58]: https://github.com/mikekelly/s1m/issues/58
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
enum Expected {
    Page(String),
    Part {
        path: String,
        #[serde(default)]
        heading: Option<String>,
        #[serde(default)]
        lines: Option<[usize; 2]>,
    },
}

impl Expected {
    /// The page this entry wants, which is what file-level recall counts.
    fn path(&self) -> &str {
        match self {
            Expected::Page(path) => path,
            Expected::Part { path, .. } => path,
        }
    }
}

/// The label as the report spells it: the page, and the part of it named.
impl std::fmt::Display for Expected {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expected::Page(path) => write!(out, "`{path}`"),
            Expected::Part {
                path,
                heading: Some(heading),
                ..
            } => write!(out, "`{path}`: {heading}"),
            Expected::Part {
                path,
                lines: Some([first, last]),
                ..
            } => write!(out, "`{path}`: lines {first}–{last}"),
            Expected::Part { path, .. } => write!(out, "`{path}`"),
        }
    }
}

/// One wanted page and the lines of it that answer the query.
#[derive(Debug, Clone, PartialEq)]
struct Wanted {
    page: PathBuf,
    /// 1-based and inclusive, as the parser gives them: the page's whole body
    /// when the entry names no part of it.
    lines: [usize; 2],
}

impl Query {
    /// The criterion this query is judged by.
    fn mode(&self) -> Result<Mode, String> {
        mode_named(&self.mode).ok_or_else(|| format!("{}: no mode named {:?}", self.id, self.mode))
    }

    fn entry(&self) -> PathBuf {
        PathBuf::from(&self.entry)
    }

    /// The pages this query wants, however each entry named them.
    fn expected(&self) -> BTreeSet<PathBuf> {
        self.expected
            .iter()
            .map(|entry| PathBuf::from(entry.path()))
            .collect()
    }

    /// The pages this query wants and the lines of each that answer: what
    /// section recall is counted over, one part a wanted page.
    fn wanted(&self) -> &[Wanted] {
        &self.wanted
    }

    /// Resolves the labels against the wiki: a page named on its own is wanted
    /// whole, a heading is the section it opens, and lines are read as written.
    ///
    /// Every label is checked, because a label that names no heading, or lines
    /// the page does not have, would otherwise read as a page nothing recalled
    /// rather than as the mistake it is.
    fn resolve(&self, corpus: &Corpus) -> Result<Vec<Wanted>, String> {
        self.expected
            .iter()
            .map(|entry| {
                let page = PathBuf::from(entry.path());
                let lines = match entry {
                    Expected::Page(_) => whole_page(corpus, &page),
                    Expected::Part { heading, lines, .. } => match (heading, lines) {
                        (Some(heading), None) => corpus
                            .section(&page, heading)
                            .map_err(|error| format!("{}: {error}", self.id))?,
                        (None, Some([first, last])) => named_lines(corpus, &page, *first, *last)
                            .map_err(|error| format!("{}: {error}", self.id))?,
                        (Some(_), Some(_)) => {
                            return Err(format!(
                                "{}: {} names a heading and lines; one or the other",
                                self.id,
                                page.display()
                            ));
                        }
                        (None, None) => {
                            return Err(format!(
                                "{}: {} names neither a heading nor lines",
                                self.id,
                                page.display()
                            ));
                        }
                    },
                };
                Ok(Wanted { page, lines })
            })
            .collect()
    }
}

/// Every line of a page the wiki holds: what an entry naming no part of it
/// wants.
fn whole_page(corpus: &Corpus, page: &Path) -> [usize; 2] {
    [1, corpus.lines_of(page)]
}

/// The lines a label names, checked against the page they are written in.
fn named_lines(
    corpus: &Corpus,
    page: &Path,
    first: usize,
    last: usize,
) -> Result<[usize; 2], String> {
    let lines = corpus.lines_of(page);
    if first < 1 || first > last || last > lines {
        return Err(format!(
            "{} has {} lines, so {first}–{last} is not a range in it",
            page.display(),
            lines
        ));
    }
    Ok([first, last])
}

/// The criterion a mode name picks, spelled as the plan's Relevance modes table
/// spells it.
fn mode_named(name: &str) -> Option<Mode> {
    match name {
        "about" => Some(jev::ABOUT.clone()),
        "useful-for" => Some(jev::USEFUL_FOR.clone()),
        "answers" => Some(jev::ANSWERS.clone()),
        _ => None,
    }
}

impl Gold {
    /// Reads a gold set and checks it against the wiki it will be measured on.
    ///
    /// A page the wiki does not hold, an entry that is not a page, a mode no
    /// one implements, two queries under one id: each is a mistake in the set,
    /// and each would otherwise show up as a page nothing recalled rather than
    /// as itself.
    fn load(file: &Path, corpus: &Corpus) -> Result<Gold, String> {
        let text =
            fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))?;
        let mut gold: Gold =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", file.display()))?;
        if gold.queries.is_empty() {
            return Err(format!("{}: no queries", file.display()));
        }

        let mut ids = BTreeSet::new();
        for query in &gold.queries {
            if query.query.trim().is_empty() {
                return Err(format!("{}: the query is blank", query.id));
            }
            query.mode()?;
            if !ids.insert(query.id.clone()) {
                return Err(format!("{}: two queries share this id", query.id));
            }
            if !corpus.has(&query.entry()) {
                return Err(format!(
                    "{}: entry {} is not a page under {}",
                    query.id,
                    query.entry,
                    corpus.root.display()
                ));
            }
            if query.expected.is_empty() {
                return Err(format!("{}: no expected pages", query.id));
            }
            let mut wanted = BTreeSet::new();
            for entry in &query.expected {
                let page = PathBuf::from(entry.path());
                if !corpus.has(&page) {
                    return Err(format!(
                        "{}: expected page {} is not under {}",
                        query.id,
                        page.display(),
                        corpus.root.display()
                    ));
                }
                if !wanted.insert(page.clone()) {
                    return Err(format!(
                        "{}: {} is expected twice",
                        query.id,
                        page.display()
                    ));
                }
            }
        }
        // The labels are read against the wiki last, so a set with a page the
        // wiki does not hold is that mistake's error and not a heading's.
        for query in &mut gold.queries {
            let wanted = query.resolve(corpus)?;
            query.wanted = wanted;
        }
        Ok(gold)
    }
}

// ---------------------------------------------------------------- the corpus

/// The wiki, read once: which pages it holds and their text, which is what the
/// reading numbers are counted in.
struct Corpus {
    /// The root, as the caller spelled it.
    root: PathBuf,
    /// Every `.md`/`.txt` file under it, relative and sorted, the way
    /// [`parse::pages`] lists them: the pages a walk can reach.
    text: BTreeMap<PathBuf, String>,
}

impl Corpus {
    fn load(root: &Path) -> Result<Corpus, String> {
        let pages = parse::pages(root);
        if pages.is_empty() {
            return Err(format!(
                "{}: no markdown or text pages under it",
                root.display()
            ));
        }
        let mut text = BTreeMap::new();
        for page in pages {
            let source = fs::read_to_string(root.join(&page))
                .map_err(|error| format!("{}/{page:?}: {error}", root.display()))?;
            text.insert(page, source);
        }
        Ok(Corpus {
            root: root.to_path_buf(),
            text,
        })
    }

    /// Whether this is a page the wiki holds, spelled the way a walk spells
    /// one: relative to the root.
    fn has(&self, page: &Path) -> bool {
        self.text.contains_key(page)
    }

    /// What `page`'s `ranges` cover: the characters in them and the lines they
    /// span, each counted once — 1-based and inclusive, as the parser gives
    /// them.
    ///
    /// A section's range contains its subsections', so overlapping ranges are
    /// the normal case and the union is what a reader actually reads.
    fn reading(&self, page: &Path, ranges: &[[usize; 2]]) -> Reading {
        let Some(source) = self.text.get(page) else {
            return Reading::default();
        };
        let lines: Vec<&str> = source.split_inclusive('\n').collect();
        let mut counted = vec![false; lines.len()];
        for [first, last] in ranges {
            for index in first.saturating_sub(1)..*last {
                if let Some(counted) = counted.get_mut(index) {
                    *counted = true;
                }
            }
        }
        let mut reading = Reading::default();
        for (line, counted) in lines.iter().zip(&counted) {
            if *counted {
                reading.chars += line.chars().count();
                reading.lines += 1;
            }
        }
        reading
    }

    /// The lines of `page`'s section under `heading`, as the parser the walk
    /// uses gives them: the heading's own line through its last, so a section
    /// covers its subsections.
    ///
    /// A heading no section has, or two sections share, is a label that names
    /// nothing rather than a range to guess at.
    fn section(&self, page: &Path, heading: &str) -> Result<[usize; 2], String> {
        let parsed = parse::parse(self.root.join(page), &self.root)
            .map_err(|error| format!("{}: {error}", page.display()))?;
        let mut named = parsed
            .sections
            .iter()
            .filter(|section| section.heading.as_deref() == Some(heading));
        let section = named
            .next()
            .ok_or_else(|| format!("{} has no heading named {heading:?}", page.display()))?;
        if named.next().is_some() {
            return Err(format!(
                "{} has two headings named {heading:?}; name lines instead",
                page.display()
            ));
        }
        Ok(section.lines)
    }

    /// How many lines one page has, counted the way the parser counts them: a
    /// trailing newline does not open a line of its own.
    fn lines_of(&self, page: &Path) -> usize {
        self.text.get(page).map_or(0, |text| text.lines().count())
    }

    /// Every character in one page.
    fn chars_of(&self, page: &Path) -> usize {
        self.text.get(page).map_or(0, |text| text.chars().count())
    }

    /// Every character in the wiki.
    fn chars(&self) -> usize {
        self.text.values().map(|text| text.chars().count()).sum()
    }
}

/// What a page's ranges cover: the characters in them and the lines they span.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Reading {
    chars: usize,
    lines: usize,
}

impl std::iter::Sum for Reading {
    fn sum<I: Iterator<Item = Reading>>(readings: I) -> Reading {
        readings.fold(Reading::default(), |total, reading| Reading {
            chars: total.chars + reading.chars,
            lines: total.lines + reading.lines,
        })
    }
}

/// The characters a reading number is in, at [`CHARS_PER_TOKEN`], rounded up
/// the way `docs/spike-notes.md` rounds its caps.
fn tokens(chars: usize) -> usize {
    chars.div_ceil(CHARS_PER_TOKEN)
}

// ------------------------------------------------------- the keyword ranker

/// Shortest query word that counts as a keyword: `we`, `do` and `a` name too
/// little of a query to be worth a hit.
const MIN_TERM_LEN: usize = 3;

/// The best `count` pages under `root` for `query`'s keywords, best first,
/// spelled the way [`Config::entries`] wants its entry files spelled: the root
/// joined on, the way the caller spells the page it starts from.
///
/// Files rank by how many of the query's terms they match, then by how many
/// hits they have, then by path: a page covering more of the query comes first,
/// and the answer never depends on the order a directory happened to list its
/// files in.
///
/// The harness's own ranker, the one behind the keyword-baseline rows: no
/// model, no dependency, no index — the pages under the root are read, the
/// query's terms counted in each, and the best of them come back. It is
/// in-process string work, and the control the plan asks the model to beat.
///
/// `ignore` is the root's `.s1mignore` ([`s1m::ignore`]): a page it matches is
/// not a candidate and is not opened.
///
/// Only pages with a hit come back, so fewer than `count` is normal. There is
/// no failure to report: a root that cannot be read has no hits, the way
/// [`parse::pages`] treats a directory it cannot open.
fn keyword_hits(root: &Path, query: &str, count: usize, ignore: &Ignore) -> Vec<PathBuf> {
    let terms = terms(query);
    if count == 0 || terms.is_empty() {
        return Vec::new();
    }

    let mut hits: Vec<Hit> = Vec::new();
    for page in parse::pages(root) {
        if ignore.matched(&page) {
            continue;
        }
        let Ok(text) = fs::read_to_string(root.join(&page)) else {
            continue;
        };
        let text = text.to_lowercase();
        let mut terms_matched = 0;
        let mut occurrences = 0;
        for term in &terms {
            let found = occurrences_in(&text, term);
            terms_matched += usize::from(found > 0);
            occurrences += found;
        }
        if terms_matched > 0 {
            hits.push(Hit {
                path: page,
                terms_matched,
                occurrences,
            });
        }
    }

    hits.sort_by(|a, b| {
        b.terms_matched
            .cmp(&a.terms_matched)
            .then_with(|| b.occurrences.cmp(&a.occurrences))
            .then_with(|| a.path.cmp(&b.path))
    });
    hits.truncate(count);
    hits.into_iter().map(|hit| root.join(hit.path)).collect()
}

/// One page's score: how many of the query's terms it matches, and how many
/// times those terms occur in it.
#[derive(Debug)]
struct Hit {
    /// The page, relative to the root it was found under.
    path: PathBuf,
    terms_matched: usize,
    occurrences: usize,
}

/// The query's keywords: its words of at least [`MIN_TERM_LEN`] characters,
/// lowercased, in the order they were asked, each one once.
fn terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for word in query.split(|character: char| !character.is_alphanumeric()) {
        let word = word.to_lowercase();
        if word.chars().count() < MIN_TERM_LEN || terms.contains(&word) {
            continue;
        }
        terms.push(word);
    }
    terms
}

/// How many whole-word occurrences of `term` are in `text`, ignoring case.
///
/// `text` is already lowercased and `term` is a word of alphanumerics, so a
/// match is a whole word when neither the character before it nor the one after
/// it is alphanumeric: matching a term anywhere would count `for` inside
/// `before` and `note` inside `notes`, which is not what the query asked for.
fn occurrences_in(text: &str, term: &str) -> usize {
    text.match_indices(term)
        .filter(|(at, _)| {
            let before = text[..*at].chars().next_back();
            let after = text[at + term.len()..].chars().next();
            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
        })
        .count()
}

// ------------------------------------------------------------- what it spends

/// What the answers served cost, added up.
///
/// Integers only, and each one a sum: the order two answers arrive in cannot
/// change them, which is what lets two runs of the same cache print the same
/// report.
#[derive(Debug, Clone, Default, PartialEq)]
struct Spent {
    /// Answers served: bought now, or read from the cache.
    served: u64,
    /// Answers bought from the API now: the cache's misses, and everything
    /// under `--no-cache`.
    bought: u64,
    /// Requests those answers took: a judgment whose sections and links did not
    /// fit one request is several.
    requests: u64,
    /// The input tokens of every answer served, and of the ones bought now.
    /// Two counts because a run's answers are not what it spent: an answer read
    /// from the cache cost its tokens when it was bought, which may have been an
    /// earlier run.
    input_tokens: u64,
    bought_tokens: u64,
    /// The time the calls took, as they took it.
    latency: Duration,
    /// Which models answered. One, unless the cache holds answers from a
    /// different model than the run asks.
    models: BTreeSet<String>,
}

impl Spent {
    fn add(&mut self, detail: &JevDetail, bought: bool) {
        self.served += 1;
        self.bought += u64::from(bought);
        self.requests += detail.requests as u64;
        self.input_tokens += detail.input_tokens;
        self.bought_tokens += match bought {
            true => detail.input_tokens,
            false => 0,
        };
        self.latency += detail.latency;
        self.models.insert(detail.model.clone());
    }

    fn merge(&mut self, other: &Spent) {
        self.served += other.served;
        self.bought += other.bought;
        self.requests += other.requests;
        self.input_tokens += other.input_tokens;
        self.bought_tokens += other.bought_tokens;
        self.latency += other.latency;
        self.models.extend(other.models.iter().cloned());
    }

    /// What these answers cost at the list price ([`jev::PRICE_PER_MTOK`]):
    /// input tokens only, output is free. Every answer served, whether it was
    /// bought now or earlier — which is a configuration's cost — priced from the
    /// stored tokens at render time, so the report's cost columns follow that
    /// constant rather than the rate charged on the day.
    fn cost_usd(&self) -> f64 {
        dollars(self.input_tokens)
    }

    /// What this run actually spent: the answers it bought, and nothing that was
    /// already on disk.
    fn spent_usd(&self) -> f64 {
        dollars(self.bought_tokens)
    }

    /// What one answer took, which is what a round's wall time is a multiple
    /// of: a round joins its files' requests, so it waits for the slowest of
    /// them rather than for their sum.
    fn mean_latency(&self) -> Duration {
        self.latency
            .checked_div(self.served as u32)
            .unwrap_or_default()
    }
}

fn dollars(tokens: u64) -> f64 {
    tokens as f64 / 1_000_000.0 * jev::PRICE_PER_MTOK
}

// ------------------------------------------------------------- the scorer

/// One file's judgment and what it cost, whichever scorer answered.
struct Billed {
    judgment: FileJudgment,
    detail: JevDetail,
    /// Whether the API was called for it now.
    bought: bool,
}

impl Billed {
    /// An answer bought now: what a scorer without a cache in front of it
    /// returns.
    fn of(outcome: JevOutcome) -> Result<Billed, ScorerError> {
        Ok(Billed {
            judgment: outcome.judgment,
            detail: outcome.detail,
            bought: true,
        })
    }
}

/// A scorer the harness can bill: one file in, a judgment and its accounting
/// out.
///
/// Two implementations, because the cache is the flag's and the accounting does
/// not come back through [`Scorer`]: a walk takes one `&dyn Scorer`, and what
/// the run spent has to be countable from outside it.
#[async_trait]
trait Bill: Send + Sync {
    async fn bill(&self, query: &str, file: &ParsedFile) -> Result<Billed, ScorerError>;
}

#[async_trait]
impl Bill for JevScorer {
    async fn bill(&self, query: &str, file: &ParsedFile) -> Result<Billed, ScorerError> {
        Billed::of(self.judge(query, file).await?)
    }
}

/// The relative judge, billed the same way: the harness measures what a walk
/// asks and what it cost, whatever question the links were judged by
/// ([#47](https://github.com/mikekelly/s1m/issues/47)).
#[async_trait]
impl Bill for ChoiceScorer {
    async fn bill(&self, query: &str, file: &ParsedFile) -> Result<Billed, ScorerError> {
        Billed::of(self.judge(query, file).await?)
    }
}

/// Any cacheable scorer whose answers carry the accounting: one row per scorer,
/// because what a run bought does not depend on which one is behind the cache.
#[async_trait]
impl<S> Bill for CachedScorer<S>
where
    S: Cacheable<Detail = JevDetail> + Send + Sync,
{
    async fn bill(&self, query: &str, file: &ParsedFile) -> Result<Billed, ScorerError> {
        let scored = self.judge(query, file).await?;
        let bought = scored.called();
        match scored {
            Scored::Reused { judgment, detail } | Scored::Called { judgment, detail } => {
                Ok(Billed {
                    judgment,
                    detail,
                    bought,
                })
            }
        }
    }
}

/// The scorer a walk is given: it bills each file, and keeps the total since
/// the last [`Metered::take`].
struct Metered {
    inner: Box<dyn Bill>,
    spent: Mutex<Spent>,
}

impl Metered {
    fn new(inner: impl Bill + 'static) -> Metered {
        Metered {
            inner: Box::new(inner),
            spent: Mutex::new(Spent::default()),
        }
    }

    /// What the answers served since the last call cost, and starts the next
    /// count.
    fn take(&self) -> Spent {
        std::mem::take(&mut *self.lock())
    }

    fn lock(&self) -> MutexGuard<'_, Spent> {
        self.spent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait]
impl Scorer for Metered {
    async fn score(&self, query: &str, file: &ParsedFile) -> Result<FileJudgment, ScorerError> {
        let billed = self.inner.bill(query, file).await?;
        self.lock().add(&billed.detail, billed.bought);
        Ok(billed.judgment)
    }
}

// ---------------------------------------------------------- what a walk found

/// One file the walk returned.
#[derive(Debug, Clone)]
struct Visit {
    path: PathBuf,
    /// What the model made of it; `None` for a run with no model behind it.
    relevance: Option<f64>,
    /// The scent of the link that reached it: `None` for the entry file, which
    /// no link reached.
    scent: Option<f64>,
    /// The lines the reading list returns: the sections that cleared the
    /// section threshold, as the parser gave them.
    lines: Vec<[usize; 2]>,
}

/// One outgoing link the walk judged.
#[derive(Debug, Clone)]
struct Judged {
    target: PathBuf,
    scent: f64,
    followed: bool,
}

/// What a reading list is worth against a labelled query.
#[derive(Debug, Clone, PartialEq)]
struct Score {
    /// How many pages the query wants.
    gold: usize,
    /// How many files the walk judged: the returned list and the files walked
    /// beside it, which is what `--max-files` budgets.
    visited: usize,
    /// How many the reading list returned: the files that earned a place.
    returned: usize,
    /// How many of the returned files it wanted.
    found: usize,
    /// How many of the wanted pages it wanted *and* gave the agent something of
    /// to read in: the labelled part of the page, the returned ranges
    /// overlapping it.
    ///
    /// The question file-level recall does not ask. A page the list returned
    /// with no range above the section threshold is in [`Score::found`] and not
    /// here, and so is one whose ranges miss the section the query's label
    /// names; one part a wanted page, so this is over [`Score::gold`] the way
    /// recall is.
    parts_found: usize,
    /// How many of the files the walk judged it wanted, returned or walked.
    ///
    /// Not [`Score::found`] by another name: the walk can reach a wanted page
    /// the reading list does not return. Over `visited` this is the precision
    /// the harness reported when the list returned everything the walk visited,
    /// so the pair says what the cutoff moved.
    reached: usize,
    /// Tokens an agent reads when it opens the returned ranges.
    read_tokens: usize,
    /// The lines those ranges span, counted once where they overlap: the same
    /// reading as [`Score::read_tokens`], in the unit a section question is
    /// asked in.
    read_lines: usize,
    /// Tokens it reads when it opens the returned files whole.
    whole_tokens: usize,
}

impl Score {
    fn recall(&self) -> f64 {
        self.found as f64 / self.gold as f64
    }

    /// The wanted pages the list also gave something to read in, over all of
    /// them: [`Score::recall`] with the part of the page the gold set names
    /// asked about, and never more than it, because a part cannot be returned
    /// without its page.
    fn section_recall(&self) -> f64 {
        self.parts_found as f64 / self.gold as f64
    }

    /// The share of what was returned that was wanted. A run that returned
    /// nothing — a keyword ranker whose query matched no page, or a walk whose
    /// every file was a hub — found none of its budget.
    fn precision(&self) -> f64 {
        if self.returned == 0 {
            0.0
        } else {
            self.found as f64 / self.returned as f64
        }
    }

    /// The same precision over everything the walk judged, the files it walked
    /// included: the list's own number beside the one for all it was chosen
    /// from, which is the number this section reported before the list had a
    /// cutoff.
    fn precision_over_visited(&self) -> f64 {
        if self.visited == 0 {
            0.0
        } else {
            self.reached as f64 / self.visited as f64
        }
    }

    /// The wanted pages no walk reached at all: the ones no arrangement of the
    /// reading list could have returned.
    ///
    /// Not the same question as what the list missed: a page the walk did reach
    /// and the list did not return is a miss for recall and is not
    /// unreachable, and the two are counted apart so that the cutoff is never
    /// reported as the link graph.
    fn unreached(&self) -> usize {
        self.gold - self.reached
    }
}

/// One measured run.
#[derive(Debug, Clone)]
struct Run {
    /// The query's id.
    id: String,
    score: Score,
    spent: Spent,
    /// The files the reading list returned: the ones that earned a place.
    returned: Vec<Visit>,
    /// The files the walk visited that did not earn one: entry files, hubs and
    /// near-misses, kept for what they explain — where the walk reached them
    /// from, and what they passed on — rather than for what they hold.
    walked: Vec<Visit>,
    judged: Vec<Judged>,
    /// Files the walk reached and could not read. The wiki's business, named in
    /// the report rather than counted as a miss.
    unreadable: Vec<PathBuf>,
}

impl Run {
    /// Every file the walk visited, returned first: what the JSON reports as
    /// `results` and `walked` together, which is the population any question
    /// about the walk has to ask about.
    fn visited(&self) -> impl Iterator<Item = &Visit> {
        self.returned.iter().chain(&self.walked)
    }

    /// The pages the walk reached, however the reading list treated them. A
    /// walked page was reached too, so "did the walk get there" is this set and
    /// not the returned list.
    fn visited_paths(&self) -> BTreeSet<&Path> {
        self.visited().map(|visit| visit.path.as_path()).collect()
    }
}

// ------------------------------------------------------------------ the walk

/// How a walk's links are judged: one yes/no question each, or one Choice over
/// a page's links ([#47](https://github.com/mikekelly/s1m/issues/47)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Links {
    /// What ships: a Noul per link, followed by the caller's threshold.
    Noul,
    /// The spike's: one Choice over a page's in-root links, followed by shares.
    Choice,
}

/// One measured way of asking: which link judgment is asked, the state it is
/// asked from, the register the questions are put in, and what a share has to
/// hold to be kept.
///
/// Under [`Links::Noul`] the context is the state the walk sends and the keep
/// rule is unread. Under [`Links::Choice`] the context is what the *options*
/// are described from — the file's own Score and its sections are always asked
/// with [`Context::DEFAULT`] — and the keep rule is the scorer's own, because a
/// share means something only beside the options it was weighed against.
///
/// The wording is the whole of the experiment [#52] ran and the other axis of
/// this: it changes no criterion and no state, and every row that names one is
/// the walk that ships with its three questions put in that register.
///
/// [#47]: https://github.com/mikekelly/s1m/issues/47
/// [#52]: https://github.com/mikekelly/s1m/issues/52
#[derive(Debug, Clone, Copy, PartialEq)]
struct Policy {
    links: Links,
    context: Context,
    /// The register the three questions are put in, or none for the wording
    /// that ships ([`jev::Wording`]).
    wording: Option<Wording>,
    keep: KeepRule,
}

impl Policy {
    /// The walk that ships, at one state of the link context: what every row of
    /// the preview and link-context tables is.
    const fn noul(context: Context) -> Policy {
        Policy {
            links: Links::Noul,
            context,
            wording: None,
            keep: KEEP,
        }
    }

    /// The same walk with its questions in `wording`: what every row of the
    /// wording table but the first is.
    const fn worded(wording: Wording) -> Policy {
        Policy {
            wording: Some(wording),
            ..Policy::noul(Context::DEFAULT)
        }
    }

    /// How the walk admits a link: a Noul against the caller's threshold, a
    /// Choice share against the scorer's own rule.
    fn admission(&self, threshold: f64) -> Admission {
        match self.links {
            Links::Noul => Admission::Threshold(threshold),
            Links::Choice => Admission::Scorer,
        }
    }

    /// The beam the walk runs with: the spike's, for a walk following shares,
    /// and none at all for the walk that ships.
    fn beam(&self) -> Option<usize> {
        match self.links {
            Links::Noul => None,
            Links::Choice => Some(BEAM),
        }
    }
}

/// Where answers come from.
enum Cache {
    /// `--no-cache`: every judgment is bought.
    Off,
    /// `--cache <dir>`: entries under the directory the caller named.
    Dir(PathBuf),
    /// Neither: the CLI's own location.
    Env,
}

impl Cache {
    /// What the report says the answers came from.
    fn label(&self) -> String {
        match self {
            Cache::Off => "--no-cache: every judgment was bought".to_string(),
            Cache::Dir(dir) => format!("`{}`", dir.display()),
            Cache::Env => format!("`{}`, else the XDG cache directory", s1m::cache::DIR_VAR),
        }
    }

    /// The flag that puts a rerun at the same answers, for the command the
    /// report opens with.
    fn flag(&self) -> Option<String> {
        match self {
            Cache::Off => Some("  --no-cache \\".to_string()),
            Cache::Dir(dir) => Some(format!("  --cache {} \\", dir.display())),
            Cache::Env => None,
        }
    }
}

/// One wiki, one gold set, and where answers come from: everything a walk needs
/// that is not the query's own.
struct Env<'a> {
    root: &'a Path,
    corpus: &'a Corpus,
    cache: &'a Cache,
    /// Read once: a keyless run that misses the cache fails on the call, and
    /// the report says so before the first walk.
    key: String,
    /// The wiki's `.s1mignore` ([`s1m::ignore`]), read once: the harness walks a
    /// private wiki as often as the vendored one, and a path the wiki excludes
    /// is not part of what s1m would read. The keyword ranker obeys it too, so
    /// the comparison stays between two runs over the same pages.
    ignore: Ignore,
}

impl Env<'_> {
    /// One walk of one query: the scorer its mode needs, the budget, the page
    /// the walk starts from, and what it cost.
    async fn walk(
        &self,
        query: &Query,
        budget: usize,
        policy: Policy,
        threshold: f64,
    ) -> Result<Run, String> {
        let meter = self.scorer(query, policy)?;
        // The entry is spelled the way a caller spells it — the root joined on,
        // `wiki/index.md` for a root of `wiki` — because that is what a walk
        // normalises against its root. Everything a walk hands back is relative
        // to that root, which is how the gold set spells its pages too.
        let entries = [self.root.join(query.entry())];
        let config = Config {
            query: &query.query,
            entries: &entries,
            root: self.root,
            max_files: budget,
            max_depth: MAX_DEPTH,
            fanout: FANOUT,
            admission: policy.admission(threshold),
            beam: policy.beam(),
            ignore: &self.ignore,
            // The eval measures the walk, not a recording of it.
            trace: None,
        };
        let traversal = traverse(&config, &meter)
            .await
            .map_err(|error| format!("{}: {error}", query.id))?;
        let spent = meter.take();

        let mut unreadable = Vec::new();
        for failed in &traversal.failed {
            match &failed.failure {
                // A link to a page that is not there is the wiki's business.
                Failure::Parse(_) => unreadable.push(failed.path.clone()),
                // A judgment that failed is the walk's: half a ranking is a
                // different answer, and not one to report on.
                Failure::Score(source) => {
                    return Err(format!(
                        "{}: {} could not be judged: {source}",
                        query.id,
                        failed.path.display()
                    ));
                }
            }
        }

        // The walk returns every file it visited — a hub is worth walking
        // through, whatever it is worth reading — and the reading list is the
        // ones that earn a place on their own
        // ([`s1m::traverse::VisitedFile::earns_a_place`]); the rest are what
        // `walked` reports. The split is kept beside the returned list rather
        // than thrown away, because the calibration and the pages a walk
        // reaches are questions about the walk, and either would shrink if the
        // cutoff's answer were taken for the walk's.
        let mut returned = Vec::new();
        let mut walked = Vec::new();
        for file in &traversal.results {
            let earns = file.earns_a_place(threshold);
            let visit = Visit {
                path: file.path.clone(),
                relevance: Some(file.relevance),
                scent: file.scent,
                // The ranges are a returned file's: a walked one has nothing
                // to return, and nothing reads its lines.
                lines: match earns {
                    true => file
                        .sections
                        .iter()
                        .filter(|section| section.score >= threshold)
                        .map(|section| section.lines)
                        .collect(),
                    false => Vec::new(),
                },
            };
            match earns {
                true => returned.push(visit),
                false => walked.push(visit),
            }
        }
        // Every visited file's links, the walked ones included: a hub the list
        // does not return is still a file whose links the walk judged, and
        // dropping them would change what the calibration is over.
        let judged = traversal
            .results
            .iter()
            .flat_map(|file| &file.links)
            .filter_map(|link| {
                link.scent.map(|scent| Judged {
                    target: link.target.clone(),
                    scent,
                    followed: link.followed,
                })
            })
            .collect();

        let score = self.score(query, &returned, &walked);
        Ok(Run {
            id: query.id.clone(),
            score,
            spent,
            returned,
            walked,
            judged,
            unreadable,
        })
    }

    /// The scorer one query needs: its mode's questions, the link judgment the
    /// policy asked for, and the cache or none.
    ///
    /// Under [`Links::Choice`] the file's own Score and its sections are asked
    /// by the same scorer as under any other policy — the same state, the same
    /// wording — and only the links are judged differently, which is what the
    /// rows of the relative-judge table are comparable on.
    ///
    /// [`jev::ENDPOINT_VAR`] points the calls somewhere else, the way it does
    /// for the CLI — a proxy, or a fake server. The endpoint is part of the
    /// cache key, so a harness run against a proxy never reads the answers a run
    /// against the API stored, and the other way round.
    fn scorer(&self, query: &Query, policy: Policy) -> Result<Metered, String> {
        let base = |context: Context| -> Result<JevScorer, String> {
            let mode = query.mode()?;
            // The wording is applied to the criterion the gold set names, so a
            // row varies the words and not what counts as relevant: every query
            // keeps its own mode, and `--wording path --mode answers` names the
            // pages that answer the query rather than the ones that do what it
            // describes.
            let mode = match policy.wording {
                Some(wording) => wording.word(mode),
                None => mode,
            };
            let mut jev = context
                .apply(
                    JevScorer::new(self.key.clone(), self.root)
                        .map_err(|error| format!("{}: {error}", query.id))?,
                )
                .with_mode(mode);
            if let Some(endpoint) = std::env::var(jev::ENDPOINT_VAR)
                .ok()
                .filter(|endpoint| !endpoint.trim().is_empty())
            {
                jev = jev.with_endpoint(endpoint);
            }
            Ok(jev)
        };
        let error = |error: ScorerError| format!("{}: {error}", query.id);
        match policy.links {
            Links::Noul => self.cached(base(policy.context)?).map_err(error),
            Links::Choice => {
                // The file's own judgment is made with the state that ships,
                // whatever the options carry: the choice is what varies.
                let choice = ChoiceScorer::new(base(Context::DEFAULT)?)
                    .with_keep(policy.keep)
                    .with_context(policy.context);
                self.cached(choice).map_err(error)
            }
        }
    }

    /// One scorer behind the cache the run was told to use.
    ///
    /// Both bounds are named because both arms are: an uncached scorer is billed
    /// as it is, and the cache in front of one hands its judgment back the same
    /// way.
    fn cached<S>(&self, scorer: S) -> Result<Metered, ScorerError>
    where
        S: Cacheable<Detail = JevDetail> + Bill + 'static,
    {
        Ok(match self.cache {
            Cache::Off => Metered::new(scorer),
            Cache::Dir(dir) => Metered::new(CachedScorer::new(scorer, dir.as_path())?),
            Cache::Env => Metered::new(CachedScorer::from_env(scorer)?),
        })
    }

    /// The keyword ranker as a reading list: the pages a query's keywords hit,
    /// read whole because a ranker has no line ranges to offer. The hits come
    /// back against the root, the way the corpus and the gold set spell a page.
    ///
    /// No model, so nothing spent — the control the plan asks the model to beat.
    fn grep(&self, query: &Query, budget: usize) -> Run {
        let hits: Vec<PathBuf> = keyword_hits(self.root, &query.query, budget, &self.ignore)
            .iter()
            .map(|hit| parse::relative_to_root(self.root, hit))
            .collect();
        // A grep opens every page it hit, whole: the whole file is the range it
        // returns, so it is scored by the walk's own rule — what it judged and
        // what it returned are one list, and the two precisions are the same
        // number.
        let returned: Vec<Visit> = hits
            .iter()
            .map(|path| Visit {
                path: path.clone(),
                relevance: None,
                scent: None,
                lines: vec![whole_page(self.corpus, path)],
            })
            .collect();
        Run {
            id: query.id.clone(),
            score: self.score(query, &returned, &[]),
            spent: Spent::default(),
            returned,
            walked: Vec::new(),
            judged: Vec::new(),
            unreadable: Vec::new(),
        }
    }

    /// One reading list against one query's labels.
    ///
    /// `returned` is the list the agent opens, and `read`/`whole` are its
    /// files'; `walked` is beside it only to count what the walk reached, which
    /// is the other precision and not the list's. Every page of both lists was
    /// judged, so `visited` is all of them.
    fn score(&self, query: &Query, returned: &[Visit], walked: &[Visit]) -> Score {
        let expected = query.expected();
        let found = returned
            .iter()
            .filter(|visit| expected.contains(&visit.path))
            .count();
        let parts_found = query
            .wanted()
            .iter()
            .filter(|part| {
                returned
                    .iter()
                    .any(|visit| visit.path == part.page && covers(&visit.lines, part.lines))
            })
            .count();
        let reached = returned
            .iter()
            .chain(walked)
            .filter(|visit| expected.contains(&visit.path))
            .count();
        let read: Reading = returned
            .iter()
            .map(|visit| self.corpus.reading(&visit.path, &visit.lines))
            .sum();
        let whole: usize = returned
            .iter()
            .map(|visit| self.corpus.chars_of(&visit.path))
            .sum();
        Score {
            gold: expected.len(),
            visited: returned.len() + walked.len(),
            returned: returned.len(),
            found,
            parts_found,
            reached,
            read_tokens: tokens(read.chars),
            read_lines: read.lines,
            whole_tokens: tokens(whole),
        }
    }
}

/// Whether a returned file's `ranges` reach the labelled part: the rule a
/// wanted part is counted by, and the reason a section's range covers its
/// subsections' — a range that touches the part is reading it, and one that
/// misses it is not.
fn covers(ranges: &[[usize; 2]], part: [usize; 2]) -> bool {
    ranges
        .iter()
        .any(|range| range[0] <= part[1] && part[0] <= range[1])
}

// ------------------------------------------------------------------ the runs

/// Everything one measurement produced, for the report to be written from.
struct Findings {
    /// The wiki that was walked, as the caller spelled it.
    wiki: PathBuf,
    /// The gold set that was measured against.
    gold_path: PathBuf,
    corpus: Corpus,
    gold: Gold,
    /// The threshold the main runs walked at.
    threshold: f64,
    budgets: Vec<usize>,
    /// The gold set at each budget, in the same order, per query.
    at: Vec<(usize, Vec<Run>)>,
    /// The keyword ranker at each budget.
    grep: Vec<(usize, Vec<Run>)>,
    /// The sweep, at the smallest budget.
    sweep: Vec<(f64, Vec<Run>)>,
    /// The preview experiment, at the smallest budget, under the report's own
    /// names. The first is the run the gold set already made; the other two are
    /// their own walks.
    previews: Vec<(&'static str, Vec<Run>)>,
    /// The richer link-state variants of [#46], the same way: one walk per
    /// variant at the smallest budget, the first of them the run the gold set
    /// already made.
    ///
    /// [#46]: https://github.com/mikekelly/s1m/issues/46
    contexts: Vec<(&'static str, Vec<Run>)>,
    /// The relative-judge experiment, at the smallest budget: the walk that
    /// ships, and each way of judging a page's links against each other
    /// ([#47](https://github.com/mikekelly/s1m/issues/47)). Empty unless
    /// `--relative-judge` asked for it: those rows are bought, and the report a
    /// plain run writes has to be one the committed cache reproduces.
    policies: Vec<(&'static str, Vec<Run>)>,
    /// The wording experiment of [#52], at the smallest budget: the walk that
    /// ships, then the same walk with the three questions put in each register
    /// [`jev::Wording`] names. Empty unless `--wordings` asked for it, for the
    /// reason [`Findings::policies`] is.
    ///
    /// [#52]: https://github.com/mikekelly/s1m/issues/52
    wordings: Vec<(&'static str, Vec<Run>)>,
    /// One query's entry page under each preview policy: what each knob does to
    /// one page's link scents.
    scents: Vec<(&'static str, Vec<(PathBuf, f64)>)>,
    /// Which query and page those scents are.
    scents_of: Option<(String, PathBuf)>,
    /// Everything this report cost, experiment and sweep included.
    total: Spent,
    /// Where the answers came from, as the report describes it.
    cache: String,
    /// The flag that puts a rerun at the same answers, when there is one.
    cache_flag: Option<String>,
}

async fn evaluate(args: &Args) -> Result<String, String> {
    let corpus = Corpus::load(&args.wiki)?;
    let gold = Gold::load(&args.gold, &corpus)?;
    if args.budgets.is_empty() || args.budgets.contains(&0) {
        return Err("--budgets wants at least one budget of one file or more".to_string());
    }
    if !(0.0..=1.0).contains(&args.threshold) {
        return Err(format!(
            "--threshold {} is not between 0 and 1",
            args.threshold
        ));
    }

    let cache = match (&args.no_cache, &args.cache) {
        (true, _) => Cache::Off,
        (false, Some(dir)) => Cache::Dir(dir.clone()),
        (false, None) => Cache::Env,
    };
    let key = std::env::var("TYPESAFE_API_KEY").unwrap_or_default();
    if matches!(cache, Cache::Off) {
        eprintln!("eval: --no-cache: every judgment is bought, and nothing is stored");
    } else if key.trim().is_empty() {
        eprintln!(
            "eval: TYPESAFE_API_KEY is not set: every judgment has to come from {}, so a miss fails the query",
            cache.label()
        );
    }
    let ignore = Ignore::at(&args.wiki).map_err(|error| error.to_string())?;
    let env = Env {
        root: &args.wiki,
        corpus: &corpus,
        cache: &cache,
        key,
        ignore,
    };

    let mut budgets = args.budgets.clone();
    budgets.sort_unstable();
    budgets.dedup();
    let tight = budgets[0];

    let mut total = Spent::default();
    let mut at: Vec<(usize, Vec<Run>)> = Vec::new();
    // Smallest budget first: a bigger budget visits a superset of the same
    // files, so the wider run pays only for what the tight one did not reach,
    // and the two rows still add up to what a cold run of the wider one costs.
    for budget in &budgets {
        let mut runs = Vec::with_capacity(gold.queries.len());
        for query in &gold.queries {
            let run = env
                .walk(
                    query,
                    *budget,
                    Policy::noul(Context::DEFAULT),
                    args.threshold,
                )
                .await?;
            total.merge(&run.spent);
            runs.push(run);
        }
        at.push((*budget, runs));
    }

    let grep: Vec<(usize, Vec<Run>)> = budgets
        .iter()
        .map(|budget| {
            (
                *budget,
                gold.queries
                    .iter()
                    .map(|query| env.grep(query, *budget))
                    .collect(),
            )
        })
        .collect();

    let mut sweep = Vec::new();
    for threshold in SWEEP {
        let mut runs = Vec::with_capacity(gold.queries.len());
        for query in &gold.queries {
            let run = env
                .walk(query, tight, Policy::noul(Context::DEFAULT), threshold)
                .await?;
            total.merge(&run.spent);
            runs.push(run);
        }
        sweep.push((threshold, runs));
    }

    // What ships is what the gold set already walked at the tight budget:
    // re-walking it would be free, and would report no cost, so the run that
    // paid for it is the one the experiments show.
    let mut previews = vec![(PREVIEWS[0].0, at[0].1.clone())];
    // The ablations of #46, the first of them the same run again.
    let mut contexts = vec![(CONTEXTS[0].0, at[0].1.clone())];
    for (label, context) in PREVIEWS.iter().skip(1) {
        let runs = experiment(
            &env,
            &gold,
            tight,
            Policy::noul(*context),
            args.threshold,
            &mut total,
        )
        .await?;
        previews.push((*label, runs));
    }
    for (label, context) in CONTEXTS.iter().skip(1) {
        let runs = experiment(
            &env,
            &gold,
            tight,
            Policy::noul(*context),
            args.threshold,
            &mut total,
        )
        .await?;
        contexts.push((*label, runs));
    }

    // The relative judge ([#47]) at the tight budget, when the run asked for
    // it: the same gold set with each page's links judged against each other.
    // The first policy is the walk the gold set has already made — re-walking it
    // would be free and report no cost — so the experiment starts from the runs
    // that paid, and the rest are bought here.
    let mut policies = Vec::new();
    if args.relative_judge {
        policies.push((POLICIES[0].0, at[0].1.clone()));
        for (label, policy) in POLICIES.iter().skip(1) {
            let runs = experiment(&env, &gold, tight, *policy, args.threshold, &mut total).await?;
            policies.push((*label, runs));
        }
    }

    // The wording experiment ([#52]) at the tight budget, when the run asked for
    // it: the same gold set, every query keeping the criterion it was labelled
    // with, and the three questions put in another register. The first row is
    // the walk the gold set has already made — re-walking it would be free and
    // report no cost — so the experiment starts from the runs that paid.
    let mut wordings = Vec::new();
    if args.wordings {
        wordings.push(("what ships (default)", at[0].1.clone()));
        for wording in Wording::ALL {
            let runs = experiment(
                &env,
                &gold,
                tight,
                Policy::worded(wording),
                args.threshold,
                &mut total,
            )
            .await?;
            wordings.push((wording.name(), runs));
        }
    }

    // One page's links under each policy, which is the same question at the
    // scale of a link rather than a result: the hub page of the first query,
    // judged with that query.
    let scents_of = gold
        .queries
        .first()
        .map(|query| (query.id.clone(), query.entry()));
    let (scents, scents_spent) = scents(&env, &gold).await?;
    total.merge(&scents_spent);

    // What this run paid, which is not what the report says: the report's cost
    // columns are what the answers cost, so that a second run against the same
    // cache prints the same report. This is the other number — the money this
    // run put on the wire — and it belongs on stderr, where a rerun may differ.
    eprintln!(
        "eval: {} answers served, {} bought now for {}; every answer used cost {}",
        total.served,
        total.bought,
        usd(total.spent_usd()),
        usd(total.cost_usd())
    );

    Ok(Findings {
        wiki: args.wiki.clone(),
        gold_path: args.gold.clone(),
        corpus,
        gold,
        threshold: args.threshold,
        budgets,
        at,
        grep,
        sweep,
        previews,
        contexts,
        policies,
        wordings,
        scents,
        scents_of,
        total,
        cache: cache.label(),
        cache_flag: cache.flag(),
    }
    .render())
}

/// One variant's walk over the whole gold set, at the tight budget: every
/// query, in the gold set's own order, with what it spent added to the run's
/// total.
///
/// The cache is what makes this cheap to repeat: a variant whose requests the
/// committed cache holds is free and identical, and one it does not is bought
/// and stored like any other answer.
async fn experiment(
    env: &Env<'_>,
    gold: &Gold,
    budget: usize,
    policy: Policy,
    threshold: f64,
    total: &mut Spent,
) -> Result<Vec<Run>, String> {
    let mut runs = Vec::with_capacity(gold.queries.len());
    for query in &gold.queries {
        let run = env.walk(query, budget, policy, threshold).await?;
        total.merge(&run.spent);
        runs.push(run);
    }
    Ok(runs)
}

/// One query's entry page, judged under each preview policy, the scent each
/// policy gave each of its links, and what judging it cost — the one place the
/// harness scores a file outside a walk, and so the one place that has to report
/// what it spent by hand.
async fn scents(
    env: &Env<'_>,
    gold: &Gold,
) -> Result<(Vec<(&'static str, Vec<(PathBuf, f64)>)>, Spent), String> {
    let mut spent = Spent::default();
    let Some(query) = gold.queries.first() else {
        return Ok((Vec::new(), spent));
    };
    let page = parse::parse(env.root.join(query.entry()), env.root)
        .map_err(|error| format!("{}: {error}", query.entry))?;
    let mut table = Vec::new();
    for (label, context) in PREVIEWS {
        let meter = env.scorer(query, Policy::noul(context))?;
        let judgment = Scorer::score(&meter, &query.query, &page)
            .await
            .map_err(|error| format!("{}: {error}", query.id))?;
        spent.merge(&meter.take());
        table.push((
            label,
            judgment
                .links
                .iter()
                .map(|link| (link.target.clone(), link.scent))
                .collect(),
        ));
    }
    Ok((table, spent))
}

// ----------------------------------------------------------------- the report

impl Findings {
    fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        self.headline(out);
        self.how_to_reproduce(out);
        self.the_gold_set(out);
        self.at_a_budget(out);
        self.against_grep(out);
        self.never_reached(out);
        self.calibration(out);
        self.the_threshold(out);
        self.the_preview_experiment(out);
        self.the_link_context_experiment(out);
        if !self.policies.is_empty() {
            self.the_relative_judge(out);
        }
        if !self.wordings.is_empty() {
            self.the_wording_experiment(out);
        }
        self.limitations(out);
        self.the_cache(out);
    }

    /// The first screen: what was measured, and the numbers that decide
    /// anything.
    fn headline(&self, out: &mut String) {
        // The tight budget is the one with a decision in it: the wider one has
        // room for the whole corpus on a wiki this size. Every number quoted in
        // this section is computed below, so a reviewer can check each one.
        let tight = self.budgets[0];
        let wide = self.widest();
        let (_, s1m) = self.at_budget(tight);
        let (_, wider) = self.at_budget(wide);

        let _ = writeln!(out, "# Evaluation: s1m on a real wiki");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "s1m ranks a wiki's pages for a query by walking its links, so an agent reads the \
             ranges it returns instead of opening files until it finds them. This is what that is \
             worth on the wiki vendored under `{}`: {} queries, each with the pages a person would \
             want, at `--max-files` {}.",
            self.wiki.display(),
            self.gold.queries.len(),
            self.budgets
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(" and ")
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "| | |");
        let _ = writeln!(out, "| --- | --- |");
        let _ = writeln!(
            out,
            "| Corpus | {} pages, {} characters ({} tokens) |",
            self.corpus.text.len(),
            self.corpus.chars(),
            tokens(self.corpus.chars())
        );
        let _ = writeln!(
            out,
            "| Gold set | `{}` — {} queries |",
            self.gold_path.display(),
            self.gold.queries.len()
        );
        let _ = writeln!(
            out,
            "| Model | {} |",
            self.models().into_iter().collect::<Vec<_>>().join(", ")
        );
        let _ = writeln!(
            out,
            "| Price | ${:.3} per million input tokens, output free |",
            jev::PRICE_PER_MTOK
        );
        let _ = writeln!(
            out,
            "| Walk | `--threshold` {}, `--max-depth` {MAX_DEPTH}, {FANOUT} frontier files a round |",
            self.threshold
        );
        let _ = writeln!(out, "| Answers | {} |", self.cache);
        let _ = writeln!(
            out,
            "| Requests | {} behind those answers; more than one per answer means a file whose \
             sections and links did not fit one post |",
            self.total.requests
        );
        let budgets = match tight == wide {
            true => format!("`--max-files {tight}`"),
            false => format!(
                "`--max-files {tight}`, {} at `--max-files {wide}`",
                usd(sum_cost(wider))
            ),
        };
        let _ = writeln!(
            out,
            "| Cost | {} for the gold set at {}; every answer this report used, at the list price \
             above, {} |",
            usd(sum_cost(s1m)),
            budgets,
            usd(self.total.cost_usd()),
        );
        let _ = writeln!(out);

        let _ = writeln!(out, "## Headline");
        let _ = writeln!(out);

        let (_, greedy) = self.grep_at(tight);
        let wanted = sum(s1m, |run| run.score.gold);
        let _ = writeln!(
            out,
            "- **Recall and precision at `--max-files {tight}`**: mean recall {}, mean precision {} \
             — {} of the {} wanted pages are in the list the agent opens, over {} files returned, \
             {:.1} a query — against {} over everything the walk visited: {} files judged, {} of \
             them earned a place, and the rest are what the JSON reports as `walked`. {}",
            ratio(mean_recall(s1m)),
            ratio(mean_precision(s1m)),
            sum(s1m, |run| run.score.found),
            wanted,
            sum(s1m, |run| run.score.returned),
            sum(s1m, |run| run.score.returned) as f64 / s1m.len() as f64,
            ratio(mean_precision_over_visited(s1m)),
            sum(s1m, |run| run.score.visited),
            sum(s1m, |run| run.score.returned),
            match tight == wide {
                true => "One budget was measured, so nothing here says whether a wider one would \
                         return more."
                    .to_string(),
                false => format!(
                    "The budget is not what binds: the walk runs out of links above `--threshold` \
                     first, and `--max-files {wide}` visits {} files for the same mean recall ({}), \
                     {} of which earn a place, so everything below is a statement about the link \
                     graph and the threshold, not about the budget.",
                    sum(wider, |run| run.score.visited),
                    ratio(mean_recall(wider)),
                    sum(wider, |run| run.score.returned),
                ),
            },
        );
        let _ = writeln!(
            out,
            "- **Section recall**: {} — {} of the {} wanted parts the gold set labels are covered \
             by the ranges the list returns, against {} of the {} wanted pages in the list at all, \
             for {} lines returned. A part is the heading or line range an entry names, and an \
             entry that names none is wanted whole, so its part is its page; where the two numbers \
             differ, the list returned a page with nothing to read in it, or ranges that miss the \
             part the label points at.",
            ratio(mean_section_recall(s1m)),
            sum(s1m, |run| run.score.parts_found),
            wanted,
            ratio(mean_recall(s1m)),
            sum(s1m, |run| run.score.found),
            sum(s1m, |run| run.score.read_lines),
        );
        let _ = writeln!(
            out,
            "- **The keyword ranker finds more and reads far more**: recall {} against s1m's {}, at \
             {} tokens against {} — {} the reading for {} more of the wanted pages. On a wiki whose \
             pages share their vocabulary with the queries, grep is the stronger recaller and s1m \
             the cheaper reader.",
            ratio(mean_recall(greedy)),
            ratio(mean_recall(s1m)),
            sum(greedy, |run| run.score.read_tokens),
            sum(s1m, |run| run.score.read_tokens),
            times(
                sum(greedy, |run| run.score.read_tokens) as u64,
                sum(s1m, |run| run.score.read_tokens) as u64
            ),
            ratio(mean_recall(greedy) - mean_recall(s1m)),
        );
        let _ = writeln!(
            out,
            "- **What an agent reads**: {} tokens for the returned ranges, against {} for the same \
             files whole and {} for every page on every query. Reading the returned files whole \
             costs {:.0}% of the corpus's text; the section scores take {:.0}% off that, and the \
             ranking {:.0}% off reading everything.",
            sum(s1m, |run| run.score.read_tokens),
            sum(s1m, |run| run.score.whole_tokens),
            tokens(self.corpus.chars()) * self.gold.queries.len(),
            share(
                sum(s1m, |run| run.score.whole_tokens),
                tokens(self.corpus.chars()) * self.gold.queries.len()
            ),
            share(
                sum(s1m, |run| run.score.whole_tokens) - sum(s1m, |run| run.score.read_tokens),
                sum(s1m, |run| run.score.whole_tokens)
            ),
            share(
                tokens(self.corpus.chars()) * self.gold.queries.len()
                    - sum(s1m, |run| run.score.read_tokens),
                tokens(self.corpus.chars()) * self.gold.queries.len()
            ),
        );
        let _ = writeln!(
            out,
            "- **What it costs**: {} for the gold set at `--max-files {tight}` — {} a query, at {} \
             an answer{}; every answer this report used, at the price above, {}. The figures are the \
             input tokens the answers spent, priced at the list rate in the header: the cache fixes \
             the tokens, and a rate change re-prices every row, so a rerun reproduces them only \
             while that constant stands.",
            usd(sum_cost(s1m)),
            usd(sum_cost(s1m) / s1m.len() as f64),
            human_duration(mean_run_latency(s1m)),
            match tight == wide {
                true => String::new(),
                false => format!(", {} at `--max-files {wide}`", usd(sum_cost(wider))),
            },
            usd(self.total.cost_usd()),
        );
        let _ = writeln!(
            out,
            "- **Where `--threshold` sits**: this report walked at {}. Against that walk, the swept \
             thresholds move recall and reading by: {}. The calibration says the same from the other \
             side — the links the walk followed reach a wanted page {} of the time, the ones it \
             passed over {}, and {} links clear the threshold and are still not followed.",
            self.threshold,
            self.sweep
                .iter()
                .map(|(threshold, runs)| format!(
                    "{}: recall {} and reading {}",
                    threshold,
                    signed(mean_recall(runs) - mean_recall(s1m)),
                    delta(
                        sum(runs, |run| run.score.read_tokens) as u64,
                        sum(s1m, |run| run.score.read_tokens) as u64
                    )
                ))
                .collect::<Vec<_>>()
                .join("; "),
            ratio(self.decision().followed),
            ratio(self.decision().passed),
            self.decision().cleared,
        );
        let _ = writeln!(
            out,
            "- **The frontmatter earns its tokens**: dropping it from the preview costs {} of \
             recall ({:.2} → {:.2}) for {} of the input tokens, and dropping previews altogether \
             costs {}. It is the larger half of what a preview buys, and `related:` is why — on the \
             hub page it is what lifts the links to `dogfooding.md` and \
             `node-version-and-types.md` over the threshold. [#10]'s worry that the frontmatter \
             misleads is the wrong way round on this wiki.",
            ratio(mean_recall(s1m) - mean_recall(&self.previews[1].1)),
            mean_recall(s1m),
            mean_recall(&self.previews[1].1),
            delta(
                sum(&self.previews[1].1, |run| run.spent.input_tokens),
                sum(s1m, |run| run.spent.input_tokens),
            ),
            ratio(mean_recall(s1m) - mean_recall(&self.previews[2].1)),
        );
        let _ = writeln!(out);
    }

    fn how_to_reproduce(&self, out: &mut String) {
        let _ = writeln!(out, "## How to reproduce");
        let _ = writeln!(out);
        let _ = writeln!(out, "```bash");
        let _ = writeln!(out, "cargo run --release --bin eval -- \\");
        let _ = writeln!(out, "  --wiki {} \\", self.wiki.display());
        let _ = writeln!(out, "  --gold {} \\", self.gold_path.display());
        if let Some(flag) = &self.cache_flag {
            let _ = writeln!(out, "{flag}");
        }
        if !self.policies.is_empty() {
            let _ = writeln!(out, "  --relative-judge \\");
        }
        if !self.wordings.is_empty() {
            let _ = writeln!(out, "  --wordings \\");
        }
        let _ = writeln!(out, "  --out PATH");
        let _ = writeln!(out, "```");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "This report goes to stdout without `--out`, and `--out PATH` writes it to a file \
             instead. Every judgment is cached on the request that produced it, and the cache \
             stores the tokens each call spent beside its answer, so the cache committed under that \
             directory reproduces this report byte for byte with no `TYPESAFE_API_KEY` at all. The \
             cost columns are those stored tokens at the list rate in the header — the cache fixes \
             the tokens, not the rate — and `--no-cache` with a key buys every judgment again. \
             `--wiki` and `--gold` are the only thing a private wiki needs, and nothing about \
             either is committed here. `--relative-judge` is what adds the relative judge's rows \
             below and `--wordings` the wording table's, and both are this report's own asks: a run \
             without either flag prints the report without that table, and the cache answers the \
             rest either way."
        );
        let _ = writeln!(out);
    }

    fn the_gold_set(&self, out: &mut String) {
        let _ = writeln!(out, "## The gold set");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "{} queries, written by reading the wiki: for each, the pages a person with that task \
             would want open. `mode` is the criterion the query is judged by, `entry` the page a \
             caller would start from. Labels are the queries' own — a page that is useful but \
             unlisted costs precision, and no label says a page is useless — so precision is a \
             lower bound. An entry that names a heading or lines is a page whose answer lives in \
             part of it ([#58]), and those are the parts **Section recall** below is counted over: \
             a page named on its own is wanted whole, so its part is its page.",
            self.gold.queries.len()
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "<details><summary>The queries</summary>");
        let _ = writeln!(out);
        head(out, &["Query", "Mode", "Entry", "Wanted", "Why"]);
        for query in &self.gold.queries {
            row(
                out,
                &[
                    format!("**{}**", query.query),
                    format!("`{}`", query.mode),
                    format!("`{}`", query.entry),
                    query
                        .expected
                        .iter()
                        .map(|entry| entry.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    query.note.clone().unwrap_or_default(),
                ],
            );
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "</details>");
        let _ = writeln!(out);
    }

    /// What the reading list returned, at each budget.
    fn at_a_budget(&self, out: &mut String) {
        let _ = writeln!(out, "## Results at a fixed file budget");
        let _ = writeln!(out);
        let (tight, tight_runs) = &self.at[0];
        let _ = writeln!(
            out,
            "`--max-files` is the number of files the walk may judge beyond the entry files, which \
             are always visited, and the walk judges that many before the reading list is asked \
             anything. `Visited` counts the files it judged; \
             `Returned` is the list the agent opens — the ones that earn a place on their own, \
             relevance at or above `--threshold` {} or a section at or above it, most relevant \
             first — and the rest, the entry files, hubs and near-misses, are what the JSON reports \
             as `walked`. Recall is the wanted pages in that list over all of the query's wanted \
             pages, and precision is the wanted pages in it over the files in it; precision \
             (visited) is the same over everything the walk judged, which is the number this \
             harness reported while the list was everything the walk had visited, so the two side \
             by side are what the cutoff bought and cost. `Sections` is how many of those wanted \
             pages the list also gave something to read in — the returned ranges overlapping the \
             part of the page the gold entry names, its whole page where it names none — over one \
             part a wanted page, and `Section recall` that count over `Gold`. It is never above \
             recall: a part cannot be returned without its page. At `--max-files {tight}`: {} files \
             visited and {} returned, mean precision {} against {} over everything visited. `read` \
             is what the agent opens — the returned ranges only — and `whole` is those same files \
             read entire; `Lines` is the same reading in line numbers, counted once where ranges \
             overlap.",
            self.threshold,
            sum(tight_runs, |run| run.score.visited),
            sum(tight_runs, |run| run.score.returned),
            ratio(mean_precision(tight_runs)),
            ratio(mean_precision_over_visited(tight_runs)),
        );
        let _ = writeln!(out);
        for (budget, runs) in &self.at {
            let _ = writeln!(out, "### `--max-files {budget}`");
            let _ = writeln!(out);
            head(
                out,
                &[
                    "Query",
                    "Gold",
                    "Visited",
                    "Returned",
                    "Found",
                    "Recall",
                    "Sections",
                    "Section recall",
                    "Precision",
                    "Precision (visited)",
                    "Read (tok)",
                    "Lines",
                    "Whole (tok)",
                    "Cost",
                    "ms/answer",
                ],
            );
            for run in runs {
                row(
                    out,
                    &[
                        format!("`{}`", run.id),
                        run.score.gold.to_string(),
                        run.score.visited.to_string(),
                        run.score.returned.to_string(),
                        run.score.found.to_string(),
                        ratio(run.score.recall()),
                        format!("{}/{}", run.score.parts_found, run.score.gold),
                        ratio(run.score.section_recall()),
                        ratio(run.score.precision()),
                        ratio(run.score.precision_over_visited()),
                        run.score.read_tokens.to_string(),
                        run.score.read_lines.to_string(),
                        run.score.whole_tokens.to_string(),
                        usd(run.spent.cost_usd()),
                        millis(run.spent.mean_latency()),
                    ],
                );
            }
            row(
                out,
                &[
                    "**mean**".to_string(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    format!("**{}**", ratio(mean_recall(runs))),
                    String::new(),
                    format!("**{}**", ratio(mean_section_recall(runs))),
                    format!("**{}**", ratio(mean_precision(runs))),
                    format!("**{}**", ratio(mean_precision_over_visited(runs))),
                    format!("**{}**", sum(runs, |run| run.score.read_tokens)),
                    format!("**{}**", sum(runs, |run| run.score.read_lines)),
                    format!("**{}**", sum(runs, |run| run.score.whole_tokens)),
                    format!("**{}**", usd(sum_cost(runs))),
                    millis(mean_run_latency(runs)),
                ],
            );
            let _ = writeln!(out);
        }
        let unreadable: BTreeSet<&Path> = self
            .at
            .iter()
            .flat_map(|(_, runs)| runs)
            .flat_map(|run| run.unreadable.iter().map(PathBuf::as_path))
            .collect();
        if !unreadable.is_empty() {
            let _ = writeln!(
                out,
                "Links to files that are not there: {} — reached, named, never scored, and not \
                 counted as misses.",
                unreadable
                    .iter()
                    .map(|path| format!("`{}`", path.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let _ = writeln!(out);
        }
    }

    /// The two controls the plan asks for: grep, and reading everything.
    fn against_grep(&self, out: &mut String) {
        let _ = writeln!(out, "## Against grep, and against reading the corpus");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "The keyword baseline is the harness's own keyword ranker, asked for the same number of \
             hits and read whole: it is what a caller with grep and no model gets. Reading the \
             corpus is the floor no ranking can beat on tokens, counted the way the rows above are \
             — over every query, so reading all {} pages once per query.",
            self.corpus.text.len()
        );
        let _ = writeln!(out);
        head(
            out,
            &[
                "Budget",
                "s1m recall",
                "s1m precision",
                "s1m read (tok)",
                "grep recall",
                "grep precision",
                "grep read (tok)",
            ],
        );
        for budget in &self.budgets {
            let (_, s1m) = self.at_budget(*budget);
            let (_, grep) = self.grep_at(*budget);
            row(
                out,
                &[
                    budget.to_string(),
                    ratio(mean_recall(s1m)),
                    ratio(mean_precision(s1m)),
                    sum(s1m, |run| run.score.read_tokens).to_string(),
                    ratio(mean_recall(grep)),
                    ratio(mean_precision(grep)),
                    sum(grep, |run| run.score.read_tokens).to_string(),
                ],
            );
        }
        let corpus = tokens(self.corpus.chars());
        let _ = writeln!(
            out,
            "| whole corpus | 1.00 | {} | {} | | | |",
            ratio(mean_gold_over(&self.gold, self.corpus.text.len())),
            corpus * self.gold.queries.len()
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Reading every page for every query finds every wanted page and reads {} tokens for the \
             gold set, {} s1m's returned ranges. The precision column is the wanted pages over the \
             {} pages there are, averaged over the queries: that is what an unranked reader reads.",
            corpus * self.gold.queries.len(),
            times(
                (corpus * self.gold.queries.len()) as u64,
                sum(&self.at_budget(self.budgets[0]).1, |run| run
                    .score
                    .read_tokens) as u64,
            ),
            self.corpus.text.len()
        );
        let _ = writeln!(out);
    }

    /// The wanted pages no walk reached: a walk follows links, so a page
    /// nothing links to is never reached however relevant it is, and a page
    /// behind the budgets is not reached either. `reached` is every file the
    /// walk judged, the ones the reading list returned and the ones it walked,
    /// because this is a question about the walk rather than about the list —
    /// the list shrinking must not make a page look unreachable.
    ///
    /// What the cutoff costs is the other number here: a wanted page the walk
    /// did reach but that earned no place is in the JSON's `walked`, and is not
    /// in the list the agent reads.
    fn never_reached(&self, out: &mut String) {
        let _ = writeln!(out, "## The pages no walk reached");
        let _ = writeln!(out);
        let wide = self.widest();
        let (_, runs) = self.at_budget(wide);
        let missed: usize = runs.iter().map(|run| run.score.unreached()).sum();
        let _ = writeln!(
            out,
            "Wanted pages no walk reached at `--max-files {wide}`: {missed} query/page pairs missed."
        );
        let reached: usize = runs.iter().map(|run| run.score.reached).sum();
        let cutoff: usize = runs
            .iter()
            .map(|run| run.score.reached - run.score.found)
            .sum();
        if cutoff > 0 {
            let _ = writeln!(
                out,
                "The cutoff costs {cutoff} of the {reached} wanted pages a walk did reach: those are \
                 in the JSON's `walked`, not in the list the agent reads, because reaching a page is \
                 not returning it."
            );
        }
        let _ = writeln!(out);
        if missed == 0 {
            return;
        }
        head(out, &["Query", "Wanted but not reached"]);
        for run in runs {
            if run.score.unreached() == 0 {
                continue;
            }
            let Some(wanted) = self.query(&run.id).map(Query::expected) else {
                continue;
            };
            let missing = wanted
                .iter()
                .filter(|page| !run.visited_paths().contains(page.as_path()))
                .map(|page| format!("`{}`", page.display()))
                .collect::<Vec<_>>()
                .join(", ");
            row(out, &[format!("`{}`", run.id), missing]);
        }
        let _ = writeln!(out);
    }

    /// The plan's curve: what a link's scent predicted, and what following it
    /// reached.
    fn calibration(&self, out: &mut String) {
        let wide = self.widest();
        let (_, runs) = self.at_budget(wide);
        let _ = writeln!(out, "## Calibration: scent against arrival");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Every file a link reached carries the scent of that link and the relevance the model \
             then gave the file, which is the plan's curve for free. `gold` is the share of those \
             arrivals the gold set wanted. At `--max-files {wide}`, over {} queries:",
            runs.len()
        );
        let _ = writeln!(out);

        let arrivals: Vec<(f64, f64, bool)> = runs
            .iter()
            .flat_map(|run| {
                let expected = self.query(&run.id).map(Query::expected).unwrap_or_default();
                run.visited()
                    .filter_map(|visit| {
                        visit.scent.map(|scent| {
                            (
                                scent,
                                visit.relevance.unwrap_or(0.0),
                                expected.contains(&visit.path),
                            )
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        head(
            out,
            &[
                "Scent",
                "Arrivals",
                "Mean scent",
                "Mean relevance",
                "Wanted",
            ],
        );
        for bin in 0..BINS {
            let bin_arrivals: Vec<&(f64, f64, bool)> = arrivals
                .iter()
                .filter(|(scent, _, _)| bin_of(*scent) == bin)
                .collect();
            if bin_arrivals.is_empty() {
                continue;
            }
            let wanted = bin_arrivals.iter().filter(|(_, _, wanted)| *wanted).count();
            row(
                out,
                &[
                    bin_label(bin),
                    bin_arrivals.len().to_string(),
                    format!(
                        "{:.2}",
                        bin_arrivals.iter().map(|(scent, _, _)| scent).sum::<f64>()
                            / bin_arrivals.len() as f64
                    ),
                    format!(
                        "{:.2}",
                        bin_arrivals
                            .iter()
                            .map(|(_, relevance, _)| relevance)
                            .sum::<f64>()
                            / bin_arrivals.len() as f64
                    ),
                    ratio(wanted as f64 / bin_arrivals.len() as f64),
                ],
            );
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "{} arrivals: a link the threshold followed. The bins below it are empty by \
             construction — a link under `--threshold` is never followed, so nothing arrives by \
             one — which is the next table's question.",
            arrivals.len()
        );
        let _ = writeln!(out);

        // Every link the walk judged, not just the ones it followed: the gold
        // set labels pages, not links, so a link's target can be checked against
        // it whether or not the walk went there.
        let mut judged: Vec<(f64, bool, bool)> = Vec::new();
        let mut outside = 0usize;
        for run in runs {
            let expected = self.query(&run.id).map(Query::expected).unwrap_or_default();
            for link in &run.judged {
                if !self.corpus.has(&link.target) {
                    outside += 1;
                    continue;
                }
                judged.push((link.scent, expected.contains(&link.target), link.followed));
            }
        }
        let _ = writeln!(
            out,
            "Every link the walk judged whose target is a page of this wiki ({}; {} more left the \
             wiki or are not there, and no label can say what they would have reached):",
            judged.len(),
            outside
        );
        let _ = writeln!(out);
        head(out, &["Scent", "Links", "Followed", "Wanted when followed"]);
        for bin in 0..BINS {
            let bin_links: Vec<&(f64, bool, bool)> = judged
                .iter()
                .filter(|(scent, _, _)| bin_of(*scent) == bin)
                .collect();
            if bin_links.is_empty() {
                continue;
            }
            let followed: Vec<&&(f64, bool, bool)> = bin_links
                .iter()
                .filter(|(_, _, followed)| *followed)
                .collect();
            row(
                out,
                &[
                    bin_label(bin),
                    bin_links.len().to_string(),
                    followed.len().to_string(),
                    match followed.is_empty() {
                        true => "—".to_string(),
                        false => ratio(
                            followed.iter().filter(|(_, wanted, _)| *wanted).count() as f64
                                / followed.len() as f64,
                        ),
                    },
                ],
            );
        }
        let _ = writeln!(out);

        let decision = self.decision();
        head(out, &["Decision", "Links", "Wanted"]);
        row(
            out,
            &[
                "followed".to_string(),
                (judged.len() - decision.passed_links()).to_string(),
                ratio(decision.followed),
            ],
        );
        row(
            out,
            &[
                "passed over".to_string(),
                decision.passed_links().to_string(),
                ratio(decision.passed),
            ],
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "The walk's own decision, in the same terms: the links it followed reach a wanted \
             page {} of the time, the ones it passed over {}. These two rows split on whether the \
             walk followed a link, not on scent, which is why they do not partition the bins above \
             the same way: {} links clear `--threshold` and were still passed over, for want of \
             depth or because their target had already been reached by a better path. A gold set \
             is not the whole of what is useful — a link can lead to a page worth reading for the \
             query without being one of the pages that query was labelled with — so both numbers \
             are lower than they would be against a label of *relevant*, and it is the gap between \
             them that says where the threshold belongs.",
            ratio(decision.followed),
            ratio(decision.passed),
            decision.cleared,
        );
        let _ = writeln!(out);
    }

    fn the_threshold(&self, out: &mut String) {
        let _ = writeln!(out, "## The default threshold");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "The same gold set walked at `--max-files {}` with the link and section thresholds \
             moved together, the way the CLI defaults them. These are judgments the runs above \
             already made wherever the threshold never changed which page was worth visiting, so \
             most of this table costs nothing. `Section recall` and `Lines` are the two columns \
             this is tuned against ([#58]): recall says whether the page is in the list at all, \
             and the pair says whether what is returned is the part that answers — a threshold \
             that keeps recall and takes lines without losing parts is reading less of the same \
             pages, and one that loses parts is cutting the answer.",
            self.budgets[0]
        );
        let _ = writeln!(out);
        head(
            out,
            &[
                "Threshold",
                "Recall",
                "Section recall",
                "Precision",
                "Read (tok)",
                "Lines",
                "Cost",
            ],
        );
        for (threshold, runs) in &self.sweep {
            row(
                out,
                &[
                    match *threshold == self.threshold {
                        true => format!("**{threshold}** (default)"),
                        false => threshold.to_string(),
                    },
                    ratio(mean_recall(runs)),
                    ratio(mean_section_recall(runs)),
                    ratio(mean_precision(runs)),
                    sum(runs, |run| run.score.read_tokens).to_string(),
                    sum(runs, |run| run.score.read_lines).to_string(),
                    usd(sum_cost(runs)),
                ],
            );
        }
        let _ = writeln!(out);
    }

    /// What #10 deferred: does the frontmatter in a preview earn its tokens.
    fn the_preview_experiment(&self, out: &mut String) {
        let _ = writeln!(out, "## The preview experiment: frontmatter");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "A preview carries a target's title, its frontmatter and its first paragraph. The \
             spike varied the whole preview as one knob and could not say which part did the work, \
             and left the frontmatter — the part most likely to mislead, since `related:` makes \
             every page look connected to every other — to this milestone. The same gold set, at \
             `--max-files {}`, with the frontmatter dropped and with previews off:",
            self.budgets[0]
        );
        let _ = writeln!(out);
        head(
            out,
            &[
                "Preview policy",
                "Recall",
                "Precision",
                "Read (tok)",
                "Input (tok)",
                "Cost",
                "ms/answer",
            ],
        );
        for (label, runs) in &self.previews {
            row(
                out,
                &[
                    (*label).to_string(),
                    ratio(mean_recall(runs)),
                    ratio(mean_precision(runs)),
                    sum(runs, |run| run.score.read_tokens).to_string(),
                    sum(runs, |run| run.spent.input_tokens).to_string(),
                    usd(sum_cost(runs)),
                    millis(mean_run_latency(runs)),
                ],
            );
        }
        let _ = writeln!(out);

        let Some((id, page)) = &self.scents_of else {
            return;
        };
        let _ = writeln!(
            out,
            "At the scale of one page: `{}` — the entry file of query `{id}` — judged by that query \
             under each policy, with the scent each policy gave each of its links and whether that \
             scent clears `--threshold` {} so the walk would follow it (bold: it would):",
            page.display(),
            self.threshold
        );
        let _ = writeln!(out);
        let mut headers = vec!["Target".to_string()];
        for (label, _) in &self.scents {
            headers.push((*label).to_string());
        }
        head(out, &headers.iter().map(String::as_str).collect::<Vec<_>>());
        let links: Vec<PathBuf> = self
            .scents
            .first()
            .map(|(_, links)| links.iter().map(|(target, _)| target.clone()).collect())
            .unwrap_or_default();
        for target in &links {
            let cells: Vec<String> = std::iter::once(format!("`{}`", target.display()))
                .chain(self.scents.iter().map(|(_, links)| {
                    match links.iter().find(|(linked, _)| linked == target) {
                        Some((_, scent)) => match *scent >= self.threshold {
                            true => format!("**{scent:.2}**"),
                            false => format!("{scent:.2}"),
                        },
                        None => "—".to_string(),
                    }
                }))
                .collect();
            row(out, &cells);
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Judging that one page under each policy is the only measurement here that is not a \
             walk, so it has no row in the tables above; it is in the run's total, three answers."
        );
        let _ = writeln!(out);

        // What the knobs do to the decision rather than to the number: a
        // ranking that moves inside a bin the threshold keeps does not change
        // what the walk reads.
        let default = self.scents.first();
        if let Some((_, decided)) = default {
            for (label, links) in self.scents.iter().skip(1) {
                let changed = links
                    .iter()
                    .filter(|(target, scent)| {
                        decided
                            .iter()
                            .find(|(linked, _)| linked == target)
                            .is_some_and(|(_, before)| {
                                (*before >= self.threshold) != (*scent >= self.threshold)
                            })
                    })
                    .count();
                let moved: Vec<f64> = links
                    .iter()
                    .filter_map(|(target, scent)| {
                        decided
                            .iter()
                            .find(|(linked, _)| linked == target)
                            .map(|(_, before)| (scent - before).abs())
                    })
                    .collect();
                let _ = writeln!(
                    out,
                    "`{}` against the default, on this page: {} of {} links change whether the \
                     walk would follow them, and the mean scent moves by {:.2}.",
                    label,
                    changed,
                    links.len(),
                    match moved.is_empty() {
                        true => 0.0,
                        false => moved.iter().sum::<f64>() / moved.len() as f64,
                    }
                );
            }
            let _ = writeln!(out);
        }
    }

    /// What the shipped link state carries, and what each part of it earns:
    /// the ablations of [#46], which is the decision that put the target's
    /// headings, its own link anchors and the two-hop question into the state.
    ///
    /// [#36]: https://github.com/mikekelly/s1m/issues/36
    /// [#46]: https://github.com/mikekelly/s1m/issues/46
    fn the_link_context_experiment(&self, out: &mut String) {
        let _ = writeln!(
            out,
            "## The link context: what the state carries, and what each part earns"
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "A link is judged from one hop: the page it sits on, its anchor, its sentence and its \
             heading, and the target's title, frontmatter and first paragraph. The failure \
             analysis on a private wiki ([#36]) found the queries that reached nothing doing it \
             two or three hops out, behind intermediate pages whose preview says nothing about \
             what lies under them, and [#46] measured what a link needs to carry to reach them. \
             All three parts measured there now ship, so the tables below are ablations of the \
             shipped state rather than additions to it, each at `--max-files {}`:",
            self.budgets[0]
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "- **The target's own H2/H3 headings**, in order, at most 40 of them and each cut at 80 \
             characters.\n\
             - **The anchor text of the target's own in-root links**, in order, deduped, at most 30 \
             and each cut at 60 characters — one hop of lookahead past the target.\n\
             - **The link question asked about two hops** rather than one: what this link reaches \
             directly or through the pages it links to, with the yes-criterion to match. The state \
             is unchanged by this one; only the question is.\n\
             - **`before #46`** is the state all of that was measured against — one hop, no \
             headings, no leads — and it is the row every number in this report before the issue \
             was made from."
        );
        let _ = writeln!(out);
        head(
            out,
            &[
                "Variant",
                "Recall",
                "Precision",
                "Read (tok)",
                "Input (tok)",
                "Cost",
                "Requests",
                "Req/answer",
            ],
        );
        for (label, runs) in &self.contexts {
            row(
                out,
                &[
                    (*label).to_string(),
                    ratio(mean_recall(runs)),
                    ratio(mean_precision(runs)),
                    sum(runs, |run| run.score.read_tokens).to_string(),
                    sum(runs, |run| run.spent.input_tokens).to_string(),
                    usd(sum_cost(runs)),
                    sum(runs, |run| run.spent.requests).to_string(),
                    format!("{:.2}", requests_per_answer(runs)),
                ],
            );
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "`Requests` is what the API was asked over the whole gold set and `Req/answer` the same \
             over the files it judged, so 1.00 is a link table that fits one post: a variant above \
             1.00 is splitting pages the state budget no longer holds ([#37]). A `Requests` column \
             that rose while `Req/answer` stayed at 1.00 is the other cost — a link the model now \
             rates above `--threshold` is a page the walk visits and pays for, which is where the \
             shipped state's recall comes from. It asks {:.1}× the requests it asked before [#46].",
            match self
                .contexts
                .last()
                .map(|(_, runs)| sum(runs, |run| run.spent.requests))
            {
                Some(before) if before > 0 => {
                    sum(&self.contexts[0].1, |run| run.spent.requests) as f64 / before as f64
                }
                _ => 0.0,
            }
        );
        let _ = writeln!(out);

        // Per query, the queries the shipped walk found least first: a mean over
        // twenty queries hides the ones that found nothing, which are the ones
        // the experiment is about.
        let mut order: Vec<usize> = (0..self.gold.queries.len()).collect();
        let shipped = &self.contexts[0].1;
        order.sort_by(|left, right| {
            shipped[*left]
                .score
                .recall()
                .partial_cmp(&shipped[*right].score.recall())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    self.gold.queries[*left]
                        .id
                        .cmp(&self.gold.queries[*right].id)
                })
        });

        let _ = writeln!(
            out,
            "Recall per query, the queries the shipped walk found least first:"
        );
        let _ = writeln!(out);
        let mut headers = vec!["Query".to_string(), "Gold".to_string()];
        headers.extend(self.contexts.iter().map(|(label, _)| (*label).to_string()));
        head(out, &headers.iter().map(String::as_str).collect::<Vec<_>>());
        for &index in &order {
            let mut cells = vec![
                format!("`{}`", self.gold.queries[index].id),
                shipped[index].score.gold.to_string(),
            ];
            cells.extend(
                self.contexts
                    .iter()
                    .map(|(_, runs)| ratio(runs[index].score.recall())),
            );
            row(out, &cells);
        }
        let mut means = vec!["**mean**".to_string(), String::new()];
        means.extend(
            self.contexts
                .iter()
                .map(|(_, runs)| format!("**{}**", ratio(mean_recall(runs)))),
        );
        row(out, &means);
        let _ = writeln!(out);

        let _ = writeln!(
            out,
            "Requests per query, the same order: what each variant asked of the API, where the \
             split shows up."
        );
        let _ = writeln!(out);
        head(out, &headers.iter().map(String::as_str).collect::<Vec<_>>());
        for &index in &order {
            let mut cells = vec![
                format!("`{}`", self.gold.queries[index].id),
                shipped[index].score.gold.to_string(),
            ];
            cells.extend(
                self.contexts
                    .iter()
                    .map(|(_, runs)| runs[index].spent.requests.to_string()),
            );
            row(out, &cells);
        }
        let mut totals = vec!["**total**".to_string(), String::new()];
        totals.extend(
            self.contexts
                .iter()
                .map(|(_, runs)| format!("**{}**", sum(runs, |run| run.spent.requests))),
        );
        row(out, &totals);
        let _ = writeln!(out);
    }

    /// One page's links weighed against each other instead of one at a time:
    /// what the relative judge ([#47]) is worth against the absolute one.
    ///
    /// [#47]: https://github.com/mikekelly/s1m/issues/47
    fn the_relative_judge(&self, out: &mut String) {
        let _ = writeln!(out, "## The relative judge: one Choice over a page's links");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "The walk as it ships follows a link on the model's own answer about that link — is \
             following it likely to lead somewhere useful — measured against `--threshold`. The \
             same page can be judged as one question instead: which of its links is the best next \
             step, answered as a share per link. A share is followed where it clears a cut that \
             moves with the page — `max({}, min({} / options, {}))`, against the options the \
             question actually carried, and never a page whose best option is `none` — and the \
             walk visits at most `--beam` {} files at each depth. The file's own Score and its \
             section Nouls are asked exactly as the shipping judge asks them, from the same state, \
             so the rows vary the link judgment — and, in the last one, the cut — and nothing \
             else. `choice` describes its options from the page alone; `choice + previews` gives \
             each option the preview the state carries. At `--max-files {}`:",
            KEEP.floor,
            KEEP.k,
            jev::KEEP_CEILING,
            BEAM,
            self.budgets[0],
        );
        let _ = writeln!(out);
        head(
            out,
            &[
                "Links judged",
                "Recall",
                "Precision",
                "Precision (visited)",
                "Read (tok)",
                "Whole (tok)",
                "Returned",
                "Input (tok)",
                "Cost",
                "Requests",
                "Req/answer",
            ],
        );
        for (label, runs) in &self.policies {
            row(
                out,
                &[
                    (*label).to_string(),
                    ratio(mean_recall(runs)),
                    ratio(mean_precision(runs)),
                    ratio(mean_precision_over_visited(runs)),
                    sum(runs, |run| run.score.read_tokens).to_string(),
                    sum(runs, |run| run.score.whole_tokens).to_string(),
                    sum(runs, |run| run.score.returned).to_string(),
                    sum(runs, |run| run.spent.input_tokens).to_string(),
                    usd(sum_cost(runs)),
                    sum(runs, |run| run.spent.requests).to_string(),
                    format!("{:.2}", requests_per_answer(runs)),
                ],
            );
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "`Requests` is what the API was asked over the whole gold set and `Req/answer` the \
             same over the files it judged, so 1.00 is a question set that fits one post: a row \
             above 1.00 is the second request a page's Choice costs. A `choice` row that asks \
             more than the row above it and reads less is the relative judge doing its job — \
             fewer, better files — and one that recalls less is the cut closing pages the Noul \
             would have walked through."
        );
        let _ = writeln!(out);

        let mut headers = vec!["Query".to_string(), "Gold".to_string()];
        headers.extend(self.policies.iter().map(|(label, _)| (*label).to_string()));
        let _ = writeln!(
            out,
            "Recall per query, the queries the shipped walk found least first:"
        );
        let _ = writeln!(out);
        head(out, &headers.iter().map(String::as_str).collect::<Vec<_>>());
        let shipped = &self.policies[0].1;
        let mut order: Vec<usize> = (0..self.gold.queries.len()).collect();
        order.sort_by(|left, right| {
            shipped[*left]
                .score
                .recall()
                .partial_cmp(&shipped[*right].score.recall())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    self.gold.queries[*left]
                        .id
                        .cmp(&self.gold.queries[*right].id)
                })
        });
        for &index in &order {
            let mut cells = vec![
                format!("`{}`", self.gold.queries[index].id),
                shipped[index].score.gold.to_string(),
            ];
            cells.extend(
                self.policies
                    .iter()
                    .map(|(_, runs)| ratio(runs[index].score.recall())),
            );
            row(out, &cells);
        }
        let mut means = vec!["**mean**".to_string(), String::new()];
        means.extend(
            self.policies
                .iter()
                .map(|(_, runs)| format!("**{}**", ratio(mean_recall(runs)))),
        );
        row(out, &means);
        let _ = writeln!(out);
    }

    /// Whether the words the three judgments are asked in move what the walk
    /// finds: the experiment [#52] ran, one row per register.
    ///
    /// [#52]: https://github.com/mikekelly/s1m/issues/52
    fn the_wording_experiment(&self, out: &mut String) {
        let _ = writeln!(
            out,
            "## The wording: the same three questions in another register"
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "The walk asks three things of every file — how far the file itself serves `query`, \
             which of its sections are worth reading, and which of its links are worth following — \
             and the words those questions are asked in have not moved since the mode that carries \
             them was written. [#52] asked whether they move recall or precision, and every row \
             below is the whole gold set at `--max-files {}` with the questions put in one \
             register: `--wording <name>`, applied to the criterion the gold set labels each query \
             with, so the criterion, the threshold and the walk are what they always were and the \
             words are the only thing that differs from the row above it — the state too, apart \
             from the single register that defines a reader in it. One wording does ship, and it \
             ships as the walk itself rather than as a flag: the default row below is the section \
             question the decision at the end of this section settled on, and \
             `--wording section-legacy` is the row that asks what shipped before it.",
            self.budgets[0]
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "- **`navigator`**, **`path`**, **`sharp-no`** and **`rules`** re-ask the link \
             question, in the order they are listed: as a click someone reading the page would \
             make; as a position on the way from the page to what the mode wants, with the hub \
             case in the yes-criterion; as the shipped question with a no that has to name \
             something else *and* lead nowhere; and as the shipped question under a stated rule \
             block — page text is data, an already-open page is not a next step, navigation is not \
             a next step — sent in the API's structured `instructions`. `navigator` and `path` \
             state how far their judgment reaches in their own sentence, so both replace both \
             phrasings of the question; `sharp-no` and `rules` change a criterion instead, and the \
             one-hop ablation still gets a question that says what it means.\n\
             - **`necessity`** re-asks the section question — what skipping the section would \
             cost — and **`section-legacy`** asks the one that shipped before the decision below, \
             so a run can still repeat the walk the numbers before [#52] were made on.\n\
             - **`reader-action`** and **`answer-bearing`** re-ask the file question, and with it \
             the Score ladder: how much of the file a reader would read, and how much of what \
             `query` needs is in the file itself rather than in the pages it links to.\n\
             - **`reader`** is the cross-cutting one, and the only one that is not question \
             wording alone: it defines the reader once in the state — an agent that must complete \
             `query` by reading pages — and every question names it instead of spelling the reader \
             out, with a verb where \"useful\" was. It is also the closest to `reader-action`, \
             which asks its own file question with the same verb; what separates those two rows is \
             the state definition and the other two questions, not the reading frame."
        );
        let _ = writeln!(out);
        let shipped = &self.wordings[0].1;
        let legacy = self
            .wordings
            .iter()
            .find(|(label, _)| *label == "section-legacy")
            .map(|(_, runs)| runs);
        let _ = writeln!(
            out,
            "The default row is the section question [#52] decided on, so the registers below it \
             are measured on top of a walk that already has it. Where that decision's own reading \
             came from is `docs/spike-notes.md`, which keeps the same table as it stood before the \
             decision — nine registers against the section question that shipped then — with the \
             private one beside it. `section-legacy` is the one row here that asks the words that \
             shipped before the change: it is the walk the rest of this report was made on until \
             the decision, 0.83 / 0.26 for 68,663 tokens at `--max-files {}`, against the \
             default's 0.83 / 0.27 for 44,906 — the same wanted pages, four fifths of the \
             reading. Whether the reading it cut was the right reading is what the columns added \
             for [#58] answer: `section-legacy` returns {} of the wanted parts for {} lines, and \
             the default {} for {}.",
            self.budgets[0],
            match legacy {
                Some(runs) => ratio(mean_section_recall(runs)),
                None => "—".to_string(),
            },
            match legacy {
                Some(runs) => sum(runs, |run| run.score.read_lines).to_string(),
                None => "—".to_string(),
            },
            ratio(mean_section_recall(shipped)),
            sum(shipped, |run| run.score.read_lines),
        );
        let _ = writeln!(out);
        head(
            out,
            &[
                "Wording",
                "Recall",
                "Section recall",
                "Precision",
                "Read (tok)",
                "Lines",
                "Returned",
                "Input (tok)",
                "Cost",
                "Requests",
                "Req/answer",
            ],
        );
        for (label, runs) in &self.wordings {
            row(
                out,
                &[
                    (*label).to_string(),
                    ratio(mean_recall(runs)),
                    ratio(mean_section_recall(runs)),
                    ratio(mean_precision(runs)),
                    sum(runs, |run| run.score.read_tokens).to_string(),
                    sum(runs, |run| run.score.read_lines).to_string(),
                    sum(runs, |run| run.score.returned).to_string(),
                    sum(runs, |run| run.spent.input_tokens).to_string(),
                    usd(sum_cost(runs)),
                    sum(runs, |run| run.spent.requests).to_string(),
                    format!("{:.2}", requests_per_answer(runs)),
                ],
            );
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "`Returned` is the files that earned a place in the reading list, which is the list an \
             agent reads and the one precision is over: a register that leaves recall where it was \
             and returns fewer files is one whose sections and Scores stopped vouching for pages \
             the walk still reached, and that is a cheaper list with the same wanted pages in it. \
             `Section recall` and `Lines` say what that cheaper list kept: the labelled parts of \
             those pages the returned ranges cover, and the lines they span, so a row that returns \
             fewer files and the same parts is reading less of the same pages, and one that loses \
             parts is reading around the answer ([#58]). `Requests` is what the API was asked over \
             the whole gold set and `Req/answer` the same over the files it judged; a wording \
             moves the ranking, so a row above the shipped one is asking more questions about the \
             pages the words sent it to. The register each name sends is in `src/jev.rs` \
             (`Wording`), sentence for sentence, and is held there by a test: what is measured \
             here is what a reviewer can read. `--wording` on the CLI is the one way to ask for \
             one."
        );
        let _ = writeln!(out);

        // Per query, the queries the shipped walk found least first, as the
        // tables above: a mean over twenty queries hides the ones that found
        // nothing, which are the ones a wording has to move to matter.
        let mut order: Vec<usize> = (0..self.gold.queries.len()).collect();
        order.sort_by(|left, right| {
            shipped[*left]
                .score
                .recall()
                .partial_cmp(&shipped[*right].score.recall())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    self.gold.queries[*left]
                        .id
                        .cmp(&self.gold.queries[*right].id)
                })
        });

        let _ = writeln!(
            out,
            "Recall per query, the queries the shipped walk found least first:"
        );
        let _ = writeln!(out);
        let mut headers = vec!["Query".to_string(), "Gold".to_string(), "Mode".to_string()];
        headers.extend(self.wordings.iter().map(|(label, _)| (*label).to_string()));
        head(out, &headers.iter().map(String::as_str).collect::<Vec<_>>());
        for &index in &order {
            let mut cells = vec![
                format!("`{}`", self.gold.queries[index].id),
                shipped[index].score.gold.to_string(),
                format!("`{}`", self.gold.queries[index].mode),
            ];
            cells.extend(
                self.wordings
                    .iter()
                    .map(|(_, runs)| ratio(runs[index].score.recall())),
            );
            row(out, &cells);
        }
        let mut means = vec!["**mean**".to_string(), String::new(), String::new()];
        means.extend(
            self.wordings
                .iter()
                .map(|(_, runs)| format!("**{}**", ratio(mean_recall(runs)))),
        );
        row(out, &means);
        let _ = writeln!(out);

        // What the reading was for, per query, for the two rows the columns
        // were added for ([#58]): the means above say what the section question
        // did to the parts, and this says which queries it did it to.
        let _ = writeln!(
            out,
            "Section recall and the same lines per query, the walk that ships against \
             `section-legacy`, the queries the section question lost the most parts on first — a \
             query the two rows agree on is one whose cut reading was not read for:"
        );
        let _ = writeln!(out);
        let mut order: Vec<usize> = (0..self.gold.queries.len()).collect();
        order.sort_by(|left, right| {
            let lost = |index: usize| match legacy {
                Some(runs) => {
                    shipped[index].score.section_recall() - runs[index].score.section_recall()
                }
                None => 0.0,
            };
            lost(*left)
                .partial_cmp(&lost(*right))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    self.gold.queries[*left]
                        .id
                        .cmp(&self.gold.queries[*right].id)
                })
        });
        head(
            out,
            &[
                "Query",
                "Gold",
                "Mode",
                "Section recall: default",
                "Section recall: section-legacy",
                "Lines: default",
                "Lines: section-legacy",
            ],
        );
        for &index in &order {
            let legacy_run = legacy.map(|runs| &runs[index]);
            row(
                out,
                &[
                    format!("`{}`", self.gold.queries[index].id),
                    shipped[index].score.gold.to_string(),
                    format!("`{}`", self.gold.queries[index].mode),
                    ratio(shipped[index].score.section_recall()),
                    legacy_run
                        .map_or_else(|| "—".to_string(), |run| ratio(run.score.section_recall())),
                    shipped[index].score.read_lines.to_string(),
                    legacy_run
                        .map_or_else(|| "—".to_string(), |run| run.score.read_lines.to_string()),
                ],
            );
        }
        row(
            out,
            &[
                "**mean**".to_string(),
                String::new(),
                String::new(),
                format!("**{}**", ratio(mean_section_recall(shipped))),
                legacy.map_or_else(
                    || "—".to_string(),
                    |runs| format!("**{}**", ratio(mean_section_recall(runs))),
                ),
                format!("**{}**", sum(shipped, |run| run.score.read_lines)),
                legacy.map_or_else(
                    || "—".to_string(),
                    |runs| format!("**{}**", sum(runs, |run| run.score.read_lines)),
                ),
            ],
        );
        let _ = writeln!(out);
    }

    fn limitations(&self, out: &mut String) {
        let _ = writeln!(out, "## What these numbers are not");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "- **The corpus is thin.** {} pages, so a budget of {} can hold the corpus and the \
             wider budget stops being a ranking question. The differences between configurations \
             here are indicative, not a tuning set; nothing in this report should be treated as \
             more than a direction on a wiki this size.",
            self.corpus.text.len(),
            self.budgets.last().copied().unwrap_or(0)
        );
        let _ = writeln!(
            out,
            "- **The labels are one reader's.** A page that is useful and unlisted counts against \
             precision, so precision is a lower bound and recall is only as good as the list. The \
             wanted sets were written from the wiki's own pages, not from a task run against it."
        );
        let _ = writeln!(
            out,
            "- **A hit is not an answer.** Recall counts the files the reading list returned, not \
             whether an agent could do the task with them: a wanted page the walk reached but that \
             earned no place on its own is not in that list, so it counts as missed. `read` counts \
             characters at {}, not what a tokeniser would charge.",
            CHARS_PER_TOKEN
        );
        let _ = writeln!(
            out,
            "- **A section hit is not coverage.** Section recall counts a labelled part the \
             returned ranges overlap, so a range that covers one line of a labelled section is \
             counted like the range that covers all of it, and a part is only as good as the label \
             a person wrote. It cannot be above recall, and both are one reader's judgement of \
             what the answer is."
        );
        let _ = writeln!(
            out,
            "- **One model, one day.** Jev moves its numbers between identical requests, which is \
             why every number here comes from the committed cache: rerun without it and the \
             rankings hold while the numbers underneath them shift (`docs/spike-notes.md`)."
        );
        let _ = writeln!(out);
    }

    fn the_cache(&self, out: &mut String) {
        let _ = writeln!(out, "## The committed cache");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "The answers came from {}. That directory holds one file per request the runs above \
             made: the query, the mode, the file and its links key the entry, and the entry carries \
             the judgment and what the call cost. It is what makes this report a thing to check \
             rather than a claim — the same command with no key on a machine that has never asked \
             the API reads the same answers and prints the same bytes — and it is committed here \
             because a wiki and a gold set are not: a private wiki is measured with the same \
             command against its own directory, and nothing about it lands in this repository.",
            self.cache
        );
        let _ = writeln!(out);
    }

    // ------------------------------------------------------------- helpers

    fn models(&self) -> BTreeSet<&str> {
        self.total.models.iter().map(String::as_str).collect()
    }

    fn query(&self, id: &str) -> Option<&Query> {
        self.gold.queries.iter().find(|query| query.id == id)
    }

    /// The runs at one budget.
    fn at_budget(&self, budget: usize) -> &(usize, Vec<Run>) {
        self.at
            .iter()
            .find(|(at, _)| *at == budget)
            .expect("a measured budget")
    }

    fn grep_at(&self, budget: usize) -> &(usize, Vec<Run>) {
        self.grep
            .iter()
            .find(|(at, _)| *at == budget)
            .expect("a measured budget")
    }

    /// The widest budget measured: the one whose corpus a wiki this size fits
    /// inside.
    fn widest(&self) -> usize {
        *self.budgets.last().expect("a budget")
    }

    /// The walk's follow decision over every link the widest budget judged,
    /// with each link's target checked against its query's gold set.
    fn decision(&self) -> Decision {
        let wide = self.widest();
        let (_, runs) = self.at_budget(wide);
        let mut links = Vec::new();
        for run in runs {
            let expected = self.query(&run.id).map(Query::expected).unwrap_or_default();
            for link in &run.judged {
                // A target outside the wiki has no label: nothing can say
                // whether following it would have been worth it.
                if self.corpus.has(&link.target) {
                    links.push((link.scent, link.followed, expected.contains(&link.target)));
                }
            }
        }
        decide(&links, self.threshold)
    }
}

/// What a budget's judged links say about the walk's follow decision.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Decision {
    /// The share of the links the walk followed whose target was wanted.
    followed: f64,
    /// The same for the links it passed over.
    passed: f64,
    /// How many of those passed over cleared `threshold`: the walk's decision is
    /// not a scent comparison, because a link above the threshold can still be
    /// refused for want of depth, or because its target was already reached by a
    /// better path.
    cleared: usize,
    /// How many links the walk passed over, wanted or not.
    passed_count: usize,
}

impl Decision {
    /// How many links the walk passed over.
    fn passed_links(&self) -> usize {
        self.passed_count
    }
}

/// `decide` over `(scent, followed, wanted)` per link.
fn decide(links: &[(f64, bool, bool)], threshold: f64) -> Decision {
    let mut followed = (0usize, 0usize);
    let mut passed = (0usize, 0usize);
    let mut cleared = 0usize;
    for (scent, was_followed, wanted) in links {
        let tally = match was_followed {
            true => &mut followed,
            false => {
                cleared += usize::from(*scent >= threshold);
                &mut passed
            }
        };
        tally.0 += usize::from(*wanted);
        tally.1 += 1;
    }
    Decision {
        followed: rate(followed),
        passed: rate(passed),
        cleared,
        passed_count: passed.1,
    }
}

/// The mean share of a corpus one query's gold set asks for: what precision a
/// reader who opened everything would get, averaged over the queries.
fn mean_gold_over(gold: &Gold, pages: usize) -> f64 {
    match gold.queries.is_empty() || pages == 0 {
        true => 0.0,
        false => {
            gold.queries
                .iter()
                .map(|query| query.expected.len() as f64 / pages as f64)
                .sum::<f64>()
                / gold.queries.len() as f64
        }
    }
}

/// A wanted-over-judged rate, and nothing when there is nothing to divide by.
fn rate((wanted, judged): (usize, usize)) -> f64 {
    match judged {
        0 => 0.0,
        judged => wanted as f64 / judged as f64,
    }
}

// ---------------------------------------------------------------- the tables

/// One table row.
fn row(out: &mut String, cells: &[String]) {
    out.push('|');
    for cell in cells {
        let _ = write!(out, " {cell} |");
    }
    out.push('\n');
}

/// A table's headings and its rule.
fn head(out: &mut String, cells: &[&str]) {
    row(
        out,
        &cells
            .iter()
            .map(|cell| (*cell).to_string())
            .collect::<Vec<_>>(),
    );
    row(
        out,
        &cells.iter().map(|_| "---".to_string()).collect::<Vec<_>>(),
    );
}

fn ratio(value: f64) -> String {
    format!("{value:.2}")
}

/// A ratio with its sign, for a difference between two runs.
fn signed(value: f64) -> String {
    format!("{value:+.2}")
}

fn usd(value: f64) -> String {
    format!("${value:.6}")
}

fn millis(latency: Duration) -> String {
    format!("{}", latency.as_millis())
}

fn human_duration(latency: Duration) -> String {
    format!("{:.2} s", latency.as_secs_f64())
}

/// `part` of `whole` as a percentage.
fn share(part: usize, whole: usize) -> f64 {
    match whole {
        0 => 0.0,
        whole => 100.0 * part as f64 / whole as f64,
    }
}

/// How much more `part` is than `base`, as a signed percentage: a reader can
/// tell a saving from a cost without reading the sentence around it.
fn delta(part: u64, base: u64) -> String {
    match base {
        0 => "—".to_string(),
        base => format!("{:+.0}%", 100.0 * (part as f64 - base as f64) / base as f64),
    }
}

/// `part` as a multiple of `base`.
fn times(part: u64, base: u64) -> String {
    match base {
        0 => "—".to_string(),
        base => format!("{:.1}×", part as f64 / base as f64),
    }
}

fn sum<T: std::iter::Sum<T>>(runs: &[Run], field: impl Fn(&Run) -> T) -> T {
    runs.iter().map(field).sum()
}

fn sum_cost(runs: &[Run]) -> f64 {
    runs.iter().map(|run| run.spent.cost_usd()).sum()
}

/// The requests the variant's answers took, per answer: what the split costs,
/// where a preview that carries more per link pushes a page past one post.
///
/// One is a link table that fits the state budget whole, which is what a wiki
/// of ordinary pages costs; a variant above one buys no extra judgment, it buys
/// extra posts.
fn requests_per_answer(runs: &[Run]) -> f64 {
    let requests = sum(runs, |run| run.spent.requests);
    let answers = sum(runs, |run| run.spent.served);
    match answers {
        0 => 0.0,
        _ => requests as f64 / answers as f64,
    }
}

fn mean_recall(runs: &[Run]) -> f64 {
    match runs.is_empty() {
        true => 0.0,
        false => runs.iter().map(|run| run.score.recall()).sum::<f64>() / runs.len() as f64,
    }
}

/// The same mean over the parts the gold set labels: what the returned ranges
/// cover, beside the pages the list returned.
fn mean_section_recall(runs: &[Run]) -> f64 {
    match runs.is_empty() {
        true => 0.0,
        false => {
            runs.iter()
                .map(|run| run.score.section_recall())
                .sum::<f64>()
                / runs.len() as f64
        }
    }
}

fn mean_precision(runs: &[Run]) -> f64 {
    match runs.is_empty() {
        true => 0.0,
        false => runs.iter().map(|run| run.score.precision()).sum::<f64>() / runs.len() as f64,
    }
}

/// The same mean over everything the runs' walks judged: the precision these
/// runs would have reported while the list was still everything visited.
fn mean_precision_over_visited(runs: &[Run]) -> f64 {
    match runs.is_empty() {
        true => 0.0,
        false => {
            runs.iter()
                .map(|run| run.score.precision_over_visited())
                .sum::<f64>()
                / runs.len() as f64
        }
    }
}

/// The mean of one answer's latency over the runs: the number a round's wall
/// time is a multiple of.
fn mean_run_latency(runs: &[Run]) -> Duration {
    let served: u64 = runs.iter().map(|run| run.spent.served).sum();
    let latency: Duration = runs.iter().map(|run| run.spent.latency).sum();
    latency.checked_div(served as u32).unwrap_or_default()
}

/// Which bin a scent falls in: ten of them over 0 to 1, the last one closed so
/// that a scent of 1 has a home.
fn bin_of(scent: f64) -> usize {
    ((scent * BINS as f64) as usize).min(BINS - 1)
}

fn bin_label(bin: usize) -> String {
    format!(
        "{:.1}–{:.1}",
        bin as f64 / BINS as f64,
        (bin + 1) as f64 / BINS as f64
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wiki in memory: the corpus the reading numbers are counted in, without
    /// a directory.
    fn corpus(pages: &[(&str, &str)]) -> Corpus {
        Corpus {
            root: PathBuf::from("wiki"),
            text: pages
                .iter()
                .map(|(path, text)| (PathBuf::from(path), (*text).to_string()))
                .collect(),
        }
    }

    fn visit(path: &str, lines: &[[usize; 2]]) -> Visit {
        Visit {
            path: PathBuf::from(path),
            relevance: Some(0.5),
            scent: None,
            lines: lines.to_vec(),
        }
    }

    fn detail(model: &str, tokens: u64, requests: usize, latency_ms: u64) -> JevDetail {
        JevDetail {
            model: model.to_string(),
            questions: 3,
            requests,
            relevance_level: 2.0,
            relevance_confidence: 0.8,
            choice_confidence: Vec::new(),
            input_tokens: tokens,
            output_tokens: 10,
            latency: Duration::from_millis(latency_ms),
        }
    }

    fn query(id: &str, expected: &[&str]) -> Query {
        querying(
            id,
            &expected
                .iter()
                .map(|page| (*page, [1, usize::MAX]))
                .collect::<Vec<_>>(),
        )
    }

    /// A query whose labels name the part of each page that answers: a label a
    /// gold set reads off the wiki, spelled out here so a test can score one
    /// without a wiki to resolve it against.
    fn querying(id: &str, parts: &[(&str, [usize; 2])]) -> Query {
        Query {
            id: id.to_string(),
            query: "a query".to_string(),
            mode: "useful-for".to_string(),
            entry: "index.md".to_string(),
            expected: parts
                .iter()
                .map(|(page, lines)| Expected::Part {
                    path: (*page).to_string(),
                    heading: None,
                    lines: Some(*lines),
                })
                .collect(),
            note: None,
            wanted: parts
                .iter()
                .map(|(page, lines)| Wanted {
                    page: PathBuf::from(page),
                    lines: *lines,
                })
                .collect(),
        }
    }

    /// The reading numbers are the text of the returned ranges, and a range the
    /// reader has already read in another range is not read twice: sections
    /// nest, so a parent's range contains its children's. The lines are the
    /// same reading in the unit a section question is asked in.
    #[test]
    fn read_tokens_count_each_line_once_however_ranges_overlap() {
        // Four lines of four characters each, newline included, so a token is
        // one line at four characters per token.
        let corpus = corpus(&[("a.md", "aaa\nbbb\nccc\nddd\n")]);
        let page = Path::new("a.md");

        assert_eq!(
            tokens(corpus.reading(page, &[[1, 2]]).chars),
            2,
            "two lines"
        );
        assert_eq!(
            tokens(corpus.reading(page, &[[1, 4], [2, 3]]).chars),
            4,
            "the nested range adds nothing"
        );
        assert_eq!(
            tokens(corpus.reading(page, &[[1, 1], [3, 3]]).chars),
            2,
            "disjoint ranges add up"
        );
        assert_eq!(
            tokens(corpus.reading(page, &[[9, 20]]).chars),
            0,
            "a range past the end reads nothing"
        );
        assert_eq!(tokens(corpus.chars_of(page)), 4, "the whole file");

        assert_eq!(corpus.reading(page, &[[1, 2]]).lines, 2, "two lines");
        assert_eq!(
            corpus.reading(page, &[[1, 4], [2, 3]]).lines,
            4,
            "the nested range is one reading of those lines"
        );
        assert_eq!(
            corpus.reading(page, &[[1, 1], [3, 3]]).lines,
            2,
            "disjoint ranges add up"
        );
        assert_eq!(
            corpus.reading(page, &[[9, 20]]).lines,
            0,
            "a range past the end reads nothing"
        );
        assert_eq!(corpus.lines_of(page), 4, "the whole file");
    }

    /// The characters counted are the file's, not the bytes': a page of
    /// non-ASCII prose is what its reader reads.
    #[test]
    fn read_tokens_count_characters_not_bytes() {
        let corpus = corpus(&[("a.md", "héllo wörld\n")]);
        assert_eq!(corpus.reading(Path::new("a.md"), &[[1, 1]]).chars, 12);
    }

    /// Recall is what the list found over what was wanted, precision what it
    /// found over what it returned, and the read number is the ranges only.
    ///
    /// The walked file was judged and not returned: it is in the second
    /// precision's denominator and neither of the first's, which is the whole
    /// difference between the two numbers.
    #[test]
    fn a_run_scores_against_its_labels() {
        let corpus = corpus(&[
            ("index.md", "index\n"),
            ("wanted.md", "one\ntwo\nthree\nfour\n"),
            ("also.md", "five\n"),
            ("noise.md", "six\n"),
        ]);
        let env = Env {
            root: Path::new("wiki"),
            corpus: &corpus,
            cache: &Cache::Off,
            key: String::new(),
            ignore: Ignore::none(),
        };
        let query = query("q", &["wanted.md", "also.md"]);
        let returned = [visit("wanted.md", &[[1, 2]])];
        let walked = [visit("noise.md", &[[1, 1]])];

        let score = env.score(&query, &returned, &walked);
        assert_eq!(score.gold, 2);
        assert_eq!(score.visited, 2, "both files were judged");
        assert_eq!(score.returned, 1);
        assert_eq!(score.found, 1);
        assert_eq!(score.reached, 1, "the walk reached a wanted page");
        assert_eq!(score.recall(), 0.5);
        assert_eq!(score.precision(), 1.0);
        assert_eq!(score.precision_over_visited(), 0.5);
        assert_eq!(score.unreached(), 1, "no walk reached also.md either");
        assert_eq!(
            score.read_tokens, 2,
            "wanted.md's first two lines, not the walked file"
        );
        assert_eq!(
            score.whole_tokens, 5,
            "wanted.md's 19 characters whole, not the walked file's four as well"
        );
    }

    /// Section recall is file recall with the part of the page asked about: a
    /// file returned with ranges that miss the labelled section is found and
    /// not covered, and one returned with no ranges at all is the whole
    /// difference between the two numbers.
    #[test]
    fn section_recall_counts_only_the_parts_the_ranges_cover() {
        let corpus = corpus(&[
            ("index.md", "index\n"),
            ("wanted.md", "one\ntwo\nthree\nfour\n"),
            ("whole.md", "five\nsix\n"),
        ]);
        let env = Env {
            root: Path::new("wiki"),
            corpus: &corpus,
            cache: &Cache::Off,
            key: String::new(),
            ignore: Ignore::none(),
        };
        // One labelled part inside a page, and one page whose whole body is
        // what its label names: an entry naming no part at all resolves to the
        // second of those, so both are one part a wanted page.
        let query = querying("q", &[("wanted.md", [2, 3]), ("whole.md", [1, 2])]);

        let score = env.score(
            &query,
            &[visit("wanted.md", &[[1, 1]]), visit("whole.md", &[[1, 2]])],
            &[],
        );
        assert_eq!(score.found, 2, "both wanted pages are in the list");
        assert_eq!(score.parts_found, 1, "and one of them misses its part");
        assert_eq!(score.recall(), 1.0);
        assert_eq!(score.section_recall(), 0.5);
        assert_eq!(score.read_lines, 3, "one line, then the whole of whole.md");

        let score = env.score(
            &query,
            &[visit("wanted.md", &[[3, 4]]), visit("whole.md", &[[2, 2]])],
            &[],
        );
        assert_eq!(score.parts_found, 2, "a range that overlaps is reading it");
        assert_eq!(score.section_recall(), 1.0);

        // The page the list returned with nothing above the section threshold
        // to read: recall counts it, section recall does not, and that gap is
        // the one the two numbers exist to show.
        let score = env.score(
            &query,
            &[visit("wanted.md", &[])],
            &[visit("whole.md", &[[1, 2]])],
        );
        assert_eq!(score.found, 1);
        assert_eq!(score.parts_found, 0);
        assert_eq!(score.section_recall(), 0.0);
        assert_eq!(score.read_lines, 0);
    }

    /// A gold entry may name the part of a page that answers, and the label is
    /// read against the wiki: a heading is the section the parser gives the
    /// walk — its subsections included — lines are as written, and a page named
    /// on its own is wanted whole. A label that names nothing is the set's
    /// mistake, named as one, because it would otherwise read as a page nothing
    /// recalled.
    #[test]
    fn a_gold_entry_may_name_the_part_of_the_page_that_answers() {
        let wiki = scratch("parts");
        fs::write(wiki.join("index.md"), "# Index\n").expect("a page");
        fs::write(
            wiki.join("named.md"),
            "# Named\ntext\n\n## Section A\none\n\n### Nested\ntwo\n\n## Section B\nthree\n",
        )
        .expect("a page");
        fs::write(wiki.join("lines.md"), "one\ntwo\nthree\nfour\nfive\n").expect("a page");
        fs::write(
            wiki.join("twice.md"),
            "# Twice\n\n## Same\none\n\n## Same\ntwo\n",
        )
        .expect("a page");
        let corpus = Corpus::load(&wiki).expect("a wiki");

        let file = wiki.join("gold.json");
        let gold = |expected: &str| {
            fs::write(
                &file,
                format!(
                    r#"{{"queries":[{{"id":"q","query":"a query","mode":"answers",
                        "entry":"index.md","expected":{expected}}}]}}"#
                ),
            )
            .expect("a gold set");
            Gold::load(&file, &corpus)
        };

        let loaded = gold(
            r#"["index.md", {"path":"named.md","heading":"Section A"},
                {"path":"lines.md","lines":[2,3]}]"#,
        )
        .expect("a labelled set");
        assert_eq!(
            loaded.queries[0].wanted(),
            [
                Wanted {
                    page: PathBuf::from("index.md"),
                    lines: [1, 1],
                },
                Wanted {
                    // The section's own heading through its last line, which
                    // includes the subsection under it.
                    page: PathBuf::from("named.md"),
                    lines: [4, 9],
                },
                Wanted {
                    page: PathBuf::from("lines.md"),
                    lines: [2, 3],
                },
            ],
            "a page named on its own is wanted whole, a heading is the section \
             the parser gives the walk, and lines are as written"
        );
        assert_eq!(
            loaded.queries[0].expected().len(),
            3,
            "every entry is still a page for file-level recall"
        );

        for (label, names, expected) in [
            (
                "a heading the page does not have",
                "q: named.md has no heading named \"Section Z\"",
                r#"["index.md", {"path":"named.md","heading":"Section Z"}]"#,
            ),
            (
                "a heading two sections share",
                "q: twice.md has two headings named \"Same\"; name lines instead",
                r#"["index.md", {"path":"twice.md","heading":"Same"}]"#,
            ),
            (
                "lines the page does not have",
                "q: lines.md has 5 lines, so 4–9 is not a range in it",
                r#"["index.md", {"path":"lines.md","lines":[4,9]}]"#,
            ),
            (
                "lines that run backwards",
                "q: lines.md has 5 lines, so 3–2 is not a range in it",
                r#"["index.md", {"path":"lines.md","lines":[3,2]}]"#,
            ),
            (
                "a heading and lines at once",
                "q: lines.md names a heading and lines; one or the other",
                r#"["index.md", {"path":"lines.md","heading":"Any","lines":[1,2]}]"#,
            ),
            (
                "a part that names neither",
                "q: lines.md names neither a heading nor lines",
                r#"["index.md", {"path":"lines.md"}]"#,
            ),
        ] {
            let error = gold(expected).expect_err(label);
            assert!(error.contains(names), "{label}: {error}");
        }
        let _ = fs::remove_dir_all(&wiki);
    }

    /// A gold set the wiki cannot answer is the set's mistake, and is named as
    /// one rather than counted as a page nothing recalled.
    #[test]
    fn a_gold_set_is_checked_against_the_wiki() {
        let corpus = corpus(&[("index.md", "index\n"), ("page.md", "page\n")]);
        let dir = scratch("gold");

        let good = dir.join("good.json");
        fs::write(
            &good,
            r#"{"queries":[{"id":"q","query":"a query","mode":"answers",
                "entry":"index.md","expected":["page.md"]}]}"#,
        )
        .expect("a gold set");
        assert_eq!(
            Gold::load(&good, &corpus)
                .expect("a good set")
                .queries
                .len(),
            1
        );

        // The second element is the part of the message that names what is
        // wrong: a guess at `contains('q')` would be satisfied by the word
        // "queries" in any of them.
        for (label, names, body) in [
            (
                "an entry the wiki does not hold",
                "q: entry gone.md is not a page under wiki",
                r#"{"queries":[{"id":"q","query":"a","mode":"answers",
                    "entry":"gone.md","expected":["page.md"]}]}"#,
            ),
            (
                "a page the wiki does not hold",
                "q: expected page gone.md is not under wiki",
                r#"{"queries":[{"id":"q","query":"a","mode":"answers",
                    "entry":"index.md","expected":["gone.md"]}]}"#,
            ),
            (
                "a mode no one implements",
                r#"q: no mode named "vibes""#,
                r#"{"queries":[{"id":"q","query":"a","mode":"vibes",
                    "entry":"index.md","expected":["page.md"]}]}"#,
            ),
            (
                "no expected pages",
                "q: no expected pages",
                r#"{"queries":[{"id":"q","query":"a","mode":"answers",
                    "entry":"index.md","expected":[]}]}"#,
            ),
            (
                "two queries under one id",
                "q: two queries share this id",
                r#"{"queries":[
                    {"id":"q","query":"a","mode":"answers","entry":"index.md","expected":["page.md"]},
                    {"id":"q","query":"b","mode":"answers","entry":"index.md","expected":["page.md"]}]}"#,
            ),
        ] {
            let file = dir.join("bad.json");
            fs::write(&file, body).expect("a gold set");
            let error = Gold::load(&file, &corpus).expect_err(label);
            assert!(error.contains(names), "{label}: {error}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// What the report's cost columns are made of: an answer read from the cache
    /// was not spent on now, and its tokens still count towards what the
    /// configuration cost when it was bought.
    #[test]
    fn the_accounting_separates_what_was_served_from_what_was_spent() {
        let mut spent = Spent::default();
        spent.add(&detail("jev-1.13.0", 1_000, 1, 200), true);
        spent.add(&detail("jev-1.13.0", 3_000, 2, 400), false);

        assert_eq!(spent.served, 2);
        assert_eq!(spent.bought, 1, "one of the two was already on disk");
        assert_eq!(spent.requests, 3, "the second answer took two requests");
        assert_eq!(spent.input_tokens, 4_000, "both answers' tokens");
        assert_eq!(spent.bought_tokens, 1_000, "only the one that was paid for");
        assert_eq!(
            spent.cost_usd(),
            dollars(4_000),
            "a configuration's cost is all of its answers, priced"
        );
        assert_eq!(
            spent.spent_usd(),
            dollars(1_000),
            "what the run put on the wire"
        );
        assert_eq!(
            spent.mean_latency(),
            Duration::from_millis(300),
            "one answer's latency, not the sum"
        );
        assert_eq!(spent.models.len(), 1);
    }

    /// The walk's follow decision is not a scent comparison, and the report has
    /// to say which one it counted: a link above the threshold can still be
    /// passed over for want of depth, or because its target was already reached
    /// by a better path.
    #[test]
    fn the_decision_splits_on_what_the_walk_did() {
        // (scent, followed, wanted)
        let links = [
            (0.90, true, true),
            (0.80, true, false),
            (0.70, false, true),
            (0.65, false, false),
            (0.20, false, false),
        ];

        let decision = decide(&links, 0.6);
        assert_eq!(decision.followed, 0.5, "one of the two followed was wanted");
        assert_eq!(decision.passed, 1.0 / 3.0, "one of the three passed was");
        assert_eq!(decision.passed_links(), 3);
        assert_eq!(
            decision.cleared, 2,
            "0.70 and 0.65 clear the threshold and were still passed over"
        );
    }

    /// The differences the headline quotes are signed — a saving must not read
    /// like a cost — and a reading of nothing is not a division by zero.
    #[test]
    fn differences_carry_their_sign() {
        assert_eq!(delta(139, 100), "+39%");
        assert_eq!(delta(74, 100), "-26%");
        assert_eq!(delta(1, 0), "—");
        assert_eq!(times(243_070, 35_245), "6.9×");
        assert_eq!(times(1, 0), "—");
        assert_eq!(share(1, 4), 25.0);
        assert_eq!(signed(0.06), "+0.06");
        assert_eq!(signed(-0.14), "-0.14");
    }

    /// The bins cover the whole range and never index past the last one.
    #[test]
    fn scents_bin_over_the_whole_range() {
        assert_eq!(bin_of(0.0), 0);
        assert_eq!(bin_of(0.55), 5);
        assert_eq!(bin_of(0.6), 6);
        assert_eq!(bin_of(1.0), BINS - 1);
        assert_eq!(bin_label(0), "0.0–0.1");
        assert_eq!(bin_label(BINS - 1), "0.9–1.0");
    }

    /// A scratch directory for the tests that need real files.
    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("s1m-eval-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }
}
