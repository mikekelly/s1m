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
//!   read at that budget, and how much of it was wanted.
//! - **Tokens the agent reads**, counted at [`CHARS_PER_TOKEN`] — the rule the
//!   spike set its caps with, because nothing here tokenises. Three sets: the
//!   returned line ranges, the same files whole, and the whole corpus. The
//!   first two differ by what the section scores buy, the last by what the
//!   ranking buys over opening files and hoping.
//! - **What the API was asked and what it cost**: answers served, answers
//!   bought, requests, questions, input and output tokens, and the latency of
//!   one answer (not wall time: a round joins its files, so it waits for the
//!   slowest, not for the sum).
//! - **A keyword baseline**: the ranker `--seed-grep` uses, read as a reading
//!   list of whole files. What grep gets without a model.
//! - **Seeding**, because a walk follows links and some pages nothing links to
//!   can only be reached by keyword.
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

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use clap::Parser;
use serde::Deserialize;

use s1m::cache::{CachedScorer, Scored};
use s1m::ignore::Ignore;
use s1m::jev::{self, JevDetail, JevScorer, Mode};
use s1m::parse::{self, ParsedFile};
use s1m::scorer::{FileJudgment, Scorer, ScorerError};
use s1m::seed;
use s1m::traverse::{Config, Failure, traverse};

// ------------------------------------------------------------- what it varies

/// The plan's defaults for what no measurement here is about: the depth limit
/// and the frontier round.
const MAX_DEPTH: usize = 6;
const FANOUT: usize = 8;

/// Four characters per token, the rule `docs/spike-notes.md` sets its caps
/// with. Nothing here tokenises, and this is the estimate the reading numbers
/// are in.
const CHARS_PER_TOKEN: usize = 4;

/// How many keyword hits the seeded run adds: the CLI's `--seed-count` default.
const SEED_COUNT: usize = 5;

/// The thresholds the sweep walks at, around the CLI's default of `0.6`. The
/// section threshold follows the link threshold, the way the CLI defaults it.
const SWEEP: [f64; 4] = [0.5, 0.6, 0.7, 0.8];

/// How many bins the calibration tables cut 0 to 1 into.
const BINS: usize = 10;

/// The preview policies the experiment compares: what ships, then each knob
/// turned off on its own.
const PREVIEWS: [Preview; 3] = [
    Preview {
        previews: true,
        frontmatter: true,
    },
    Preview {
        previews: true,
        frontmatter: false,
    },
    Preview {
        previews: false,
        frontmatter: false,
    },
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
    /// Least link scent that queues a target, and least section score the
    /// reading list keeps.
    #[arg(long, default_value_t = 0.6)]
    threshold: f64,
    /// Buy every judgment: ignore the answers on disk.
    #[arg(long)]
    no_cache: bool,
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
    /// The pages a person would want, relative to the wiki root, most wanted
    /// first.
    expected: Vec<String>,
    /// Why those pages, for whoever audits the labels.
    #[serde(default)]
    note: Option<String>,
}

impl Query {
    /// The criterion this query is judged by.
    fn mode(&self) -> Result<Mode, String> {
        mode_named(&self.mode).ok_or_else(|| format!("{}: no mode named {:?}", self.id, self.mode))
    }

    fn entry(&self) -> PathBuf {
        PathBuf::from(&self.entry)
    }

    /// The pages this query wants.
    fn expected(&self) -> BTreeSet<PathBuf> {
        self.expected.iter().map(PathBuf::from).collect()
    }
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
        let gold: Gold =
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
            for page in &query.expected {
                let page = PathBuf::from(page);
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

    /// The characters in `page`'s `ranges` — 1-based and inclusive, as the
    /// parser gives them — counted once where ranges overlap.
    ///
    /// A section's range contains its subsections', so overlapping ranges are
    /// the normal case and the union is what a reader actually reads.
    fn chars_in(&self, page: &Path, ranges: &[[usize; 2]]) -> usize {
        let Some(source) = self.text.get(page) else {
            return 0;
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
        lines
            .iter()
            .zip(&counted)
            .filter(|(_, counted)| **counted)
            .map(|(line, _)| line.chars().count())
            .sum()
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

/// The characters a reading number is in, at [`CHARS_PER_TOKEN`], rounded up
/// the way `docs/spike-notes.md` rounds its caps.
fn tokens(chars: usize) -> usize {
    chars.div_ceil(CHARS_PER_TOKEN)
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
        let outcome = self.judge(query, file).await?;
        Ok(Billed {
            judgment: outcome.judgment,
            detail: outcome.detail,
            bought: true,
        })
    }
}

#[async_trait]
impl Bill for CachedScorer<JevScorer> {
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
    /// The scent of the link that reached it: `None` for the entry file and for
    /// a keyword seed, which no link reached.
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
    /// How many the reading list returned.
    returned: usize,
    /// How many of those it wanted.
    found: usize,
    /// Tokens an agent reads when it opens the returned ranges.
    read_tokens: usize,
    /// Tokens it reads when it opens the returned files whole.
    whole_tokens: usize,
}

impl Score {
    fn recall(&self) -> f64 {
        self.found as f64 / self.gold as f64
    }

    /// The share of what was returned that was wanted. A run that returned
    /// nothing — a keyword ranker whose query matched no page — found none of
    /// its budget.
    fn precision(&self) -> f64 {
        if self.returned == 0 {
            0.0
        } else {
            self.found as f64 / self.returned as f64
        }
    }

    /// The wanted pages the reading list did not return.
    fn missed(&self) -> usize {
        self.gold - self.found
    }
}

/// One measured run.
#[derive(Debug, Clone)]
struct Run {
    /// The query's id.
    id: String,
    score: Score,
    spent: Spent,
    visits: Vec<Visit>,
    judged: Vec<Judged>,
    /// Files the walk reached and could not read. The wiki's business, named in
    /// the report rather than counted as a miss.
    unreadable: Vec<PathBuf>,
}

impl Run {
    /// The pages this run returned.
    fn returned(&self) -> BTreeSet<&Path> {
        self.visits
            .iter()
            .map(|visit| visit.path.as_path())
            .collect()
    }
}

// ------------------------------------------------------------------ the walk

/// One preview policy: what a request carries about each link's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Preview {
    /// The target's title and first paragraph.
    previews: bool,
    /// Its frontmatter as well.
    frontmatter: bool,
}

impl Preview {
    fn label(&self) -> &'static str {
        match (self.previews, self.frontmatter) {
            (true, true) => "previews on (default)",
            (true, false) => "previews, no frontmatter",
            (false, _) => "previews off",
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
    /// One walk of one query: the scorer its mode needs, the budget, the
    /// entries and the seeds, and what it cost.
    async fn walk(
        &self,
        query: &Query,
        budget: usize,
        seed_count: usize,
        preview: Preview,
        threshold: f64,
    ) -> Result<Run, String> {
        let meter = self.scorer(query, preview)?;
        // Entries and seeds are spelled the way a caller spells them — the root
        // joined on, `wiki/index.md` for a root of `wiki` — because that is what
        // a walk normalises against its root. Everything a walk hands back is
        // relative to that root, which is how the gold set spells its pages too.
        let entries = [self.root.join(query.entry())];
        let seeds = match seed_count {
            0 => Vec::new(),
            count => seed::seed(self.root, &query.query, count, &entries, &self.ignore),
        };
        let config = Config {
            query: &query.query,
            entries: &entries,
            seeds: &seeds,
            root: self.root,
            max_files: budget,
            max_depth: MAX_DEPTH,
            fanout: FANOUT,
            threshold,
            ignore: &self.ignore,
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

        let visits: Vec<Visit> = traversal
            .results
            .iter()
            .map(|file| Visit {
                path: file.path.clone(),
                relevance: Some(file.relevance),
                scent: file.scent,
                lines: file
                    .sections
                    .iter()
                    .filter(|section| section.score >= threshold)
                    .map(|section| section.lines)
                    .collect(),
            })
            .collect();
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

        let score = self.score(query, &visits);
        Ok(Run {
            id: query.id.clone(),
            score,
            spent,
            visits,
            judged,
            unreadable,
        })
    }

    /// The scorer one query needs: its mode's questions, the preview policy,
    /// and the cache or none.
    ///
    /// [`jev::ENDPOINT_VAR`] points the calls somewhere else, the way it does
    /// for the CLI — a proxy, or a fake server. The endpoint is part of the
    /// cache key, so a harness run against a proxy never reads the answers a run
    /// against the API stored, and the other way round.
    fn scorer(&self, query: &Query, preview: Preview) -> Result<Metered, String> {
        let mut jev = JevScorer::new(self.key.clone(), self.root)
            .map_err(|error| format!("{}: {error}", query.id))?
            .with_mode(query.mode()?)
            .with_previews(preview.previews)
            .with_preview_frontmatter(preview.frontmatter);
        if let Some(endpoint) = std::env::var(jev::ENDPOINT_VAR)
            .ok()
            .filter(|endpoint| !endpoint.trim().is_empty())
        {
            jev = jev.with_endpoint(endpoint);
        }
        Ok(match self.cache {
            Cache::Off => Metered::new(jev),
            Cache::Dir(dir) => Metered::new(
                CachedScorer::new(jev, dir.as_path())
                    .map_err(|error| format!("{}: {error}", query.id))?,
            ),
            Cache::Env => Metered::new(
                CachedScorer::from_env(jev).map_err(|error| format!("{}: {error}", query.id))?,
            ),
        })
    }

    /// The keyword ranker as a reading list: the hits `--seed-grep` would add,
    /// read whole because a ranker has no line ranges to offer.
    ///
    /// No model, so nothing spent — the control the plan asks the model to beat.
    fn grep(&self, query: &Query, budget: usize) -> Run {
        let hits: Vec<PathBuf> = seed::seed(self.root, &query.query, budget, &[], &self.ignore)
            .iter()
            .map(|hit| parse::relative_to_root(self.root, hit))
            .collect();
        let expected = query.expected();
        let found = hits.iter().filter(|hit| expected.contains(*hit)).count();
        let whole_tokens: usize = tokens(hits.iter().map(|page| self.corpus.chars_of(page)).sum());
        Run {
            id: query.id.clone(),
            score: Score {
                gold: expected.len(),
                returned: hits.len(),
                found,
                // An agent that greps reads the files it hit, whole.
                read_tokens: whole_tokens,
                whole_tokens,
            },
            spent: Spent::default(),
            visits: hits
                .into_iter()
                .map(|path| Visit {
                    path,
                    relevance: None,
                    scent: None,
                    lines: Vec::new(),
                })
                .collect(),
            judged: Vec::new(),
            unreadable: Vec::new(),
        }
    }

    /// One reading list against one query's labels.
    fn score(&self, query: &Query, visits: &[Visit]) -> Score {
        let expected = query.expected();
        let found = visits
            .iter()
            .filter(|visit| expected.contains(&visit.path))
            .count();
        let read: usize = visits
            .iter()
            .map(|visit| self.corpus.chars_in(&visit.path, &visit.lines))
            .sum();
        let whole: usize = visits
            .iter()
            .map(|visit| self.corpus.chars_of(&visit.path))
            .sum();
        Score {
            gold: expected.len(),
            returned: visits.len(),
            found,
            read_tokens: tokens(read),
            whole_tokens: tokens(whole),
        }
    }
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
    /// The same, with the keyword seeds on.
    seeded: Vec<(usize, Vec<Run>)>,
    /// The keyword ranker at each budget.
    grep: Vec<(usize, Vec<Run>)>,
    /// The sweep, at the smallest budget.
    sweep: Vec<(f64, Vec<Run>)>,
    /// The preview experiment, at the smallest budget. The first is the run the
    /// gold set already made; the other two are their own walks.
    previews: Vec<(Preview, Vec<Run>)>,
    /// One query's entry page under each preview policy: what each knob does to
    /// one page's link scents.
    scents: Vec<(Preview, Vec<(PathBuf, f64)>)>,
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
                .walk(query, *budget, 0, PREVIEWS[0], args.threshold)
                .await?;
            total.merge(&run.spent);
            runs.push(run);
        }
        at.push((*budget, runs));
    }

    let mut seeded: Vec<(usize, Vec<Run>)> = Vec::new();
    for budget in &budgets {
        let mut runs = Vec::with_capacity(gold.queries.len());
        for query in &gold.queries {
            let run = env
                .walk(query, *budget, SEED_COUNT, PREVIEWS[0], args.threshold)
                .await?;
            total.merge(&run.spent);
            runs.push(run);
        }
        seeded.push((*budget, runs));
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
            let run = env.walk(query, tight, 0, PREVIEWS[0], threshold).await?;
            total.merge(&run.spent);
            runs.push(run);
        }
        sweep.push((threshold, runs));
    }

    // The default preview policy is what the gold set already walked at the
    // tight budget: re-walking it would be free, and would report no cost, so
    // the run that paid for it is the one the experiment shows.
    let mut previews = vec![(PREVIEWS[0], at[0].1.clone())];
    for preview in PREVIEWS.iter().skip(1) {
        let mut runs = Vec::with_capacity(gold.queries.len());
        for query in &gold.queries {
            let run = env.walk(query, tight, 0, *preview, args.threshold).await?;
            total.merge(&run.spent);
            runs.push(run);
        }
        previews.push((*preview, runs));
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
        seeded,
        grep,
        sweep,
        previews,
        scents,
        scents_of,
        total,
        cache: cache.label(),
        cache_flag: cache.flag(),
    }
    .render())
}

/// One query's entry page, judged under each preview policy, the scent each
/// policy gave each of its links, and what judging it cost — the one place the
/// harness scores a file outside a walk, and so the one place that has to report
/// what it spent by hand.
async fn scents(
    env: &Env<'_>,
    gold: &Gold,
) -> Result<(Vec<(Preview, Vec<(PathBuf, f64)>)>, Spent), String> {
    let mut spent = Spent::default();
    let Some(query) = gold.queries.first() else {
        return Ok((Vec::new(), spent));
    };
    let page = parse::parse(env.root.join(query.entry()), env.root)
        .map_err(|error| format!("{}: {error}", query.entry))?;
    let mut table = Vec::new();
    for preview in PREVIEWS {
        let meter = env.scorer(query, preview)?;
        let judgment = Scorer::score(&meter, &query.query, &page)
            .await
            .map_err(|error| format!("{}: {error}", query.id))?;
        spent.merge(&meter.take());
        table.push((
            preview,
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
        self.seeding(out);
        self.never_reached(out);
        self.calibration(out);
        self.the_threshold(out);
        self.the_preview_experiment(out);
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
            "| Walk | `--threshold` {}, `--max-depth` {MAX_DEPTH}, `--fanout` {FANOUT} |",
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
        let (_, seeded) = self.seeded_at(tight);
        let wanted = sum(s1m, |run| run.score.gold);
        let _ = writeln!(
            out,
            "- **Recall and precision at `--max-files {tight}`**: mean recall {}, mean precision {} \
             — {} of the {} wanted pages, over {} files returned, {:.1} a query. {}",
            ratio(mean_recall(s1m)),
            ratio(mean_precision(s1m)),
            sum(s1m, |run| run.score.found),
            wanted,
            sum(s1m, |run| run.score.returned),
            sum(s1m, |run| run.score.returned) as f64 / s1m.len() as f64,
            match tight == wide {
                true => "One budget was measured, so nothing here says whether a wider one would \
                         return more."
                    .to_string(),
                false => format!(
                    "The budget is not what binds: the walk runs out of links above `--threshold` \
                     first, and `--max-files {wide}` returns {} files for the same mean recall ({}), \
                     so everything below is a statement about the link graph and the threshold, not \
                     about the budget.",
                    sum(wider, |run| run.score.returned),
                    ratio(mean_recall(wider)),
                ),
            },
        );
        let _ = writeln!(
            out,
            "- **The keyword ranker finds more and reads far more**: recall {} against s1m's {}, at \
             {} tokens against {} — {} the reading for {} more of the wanted pages. On a wiki whose \
             pages share their vocabulary with the queries, grep is the stronger recaller and s1m \
             the cheaper reader; `--seed-grep {SEED_COUNT}` on top of the walk is the middle, at \
             recall {} and {} tokens.",
            ratio(mean_recall(greedy)),
            ratio(mean_recall(s1m)),
            sum(greedy, |run| run.score.read_tokens),
            sum(s1m, |run| run.score.read_tokens),
            times(
                sum(greedy, |run| run.score.read_tokens) as u64,
                sum(s1m, |run| run.score.read_tokens) as u64
            ),
            ratio(mean_recall(greedy) - mean_recall(s1m)),
            ratio(mean_recall(seeded)),
            sum(seeded, |run| run.score.read_tokens),
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
             either is committed here."
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
             lower bound.",
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
                        .map(|page| format!("`{page}`"))
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
        let _ = writeln!(
            out,
            "`--max-files` is the number of files the walk may visit, and the reading list returns \
             everything it visited, most relevant first: the agent opens what it was handed. Recall \
             is the wanted pages that are in the list over all of them; precision is the wanted \
             pages in the list over everything in it. `read` is what the agent opens — the returned \
             ranges only."
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
                    "Returned",
                    "Found",
                    "Recall",
                    "Precision",
                    "Read (tok)",
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
                        run.score.returned.to_string(),
                        run.score.found.to_string(),
                        ratio(run.score.recall()),
                        ratio(run.score.precision()),
                        run.score.read_tokens.to_string(),
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
                    format!("**{}**", ratio(mean_recall(runs))),
                    format!("**{}**", ratio(mean_precision(runs))),
                    format!("**{}**", sum(runs, |run| run.score.read_tokens)),
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
            "The keyword baseline is the ranker `--seed-grep` uses, asked for the same number of \
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

    /// The pages links cannot reach, and what seeding does about them.
    fn seeding(&self, out: &mut String) {
        let _ = writeln!(out, "## Seeding, and the pages links cannot reach");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "A walk follows links, so a page nothing links to is never reached at any budget. \
             `--seed-grep {SEED_COUNT}` puts the query's keyword hits on the frontier beside the \
             entry file, and the seeded run reads their sections like any other page's:"
        );
        let _ = writeln!(out);
        head(
            out,
            &[
                "Budget",
                "Configuration",
                "Recall",
                "Precision",
                "Read (tok)",
                "Cost",
            ],
        );
        for budget in &self.budgets {
            let (_, plain) = self.at_budget(*budget);
            let (_, seeded) = self.seeded_at(*budget);
            for (label, runs) in [("walk", plain), ("walk + `--seed-grep`", seeded)] {
                row(
                    out,
                    &[
                        budget.to_string(),
                        label.to_string(),
                        ratio(mean_recall(runs)),
                        ratio(mean_precision(runs)),
                        sum(runs, |run| run.score.read_tokens).to_string(),
                        usd(sum_cost(runs)),
                    ],
                );
            }
        }
        let _ = writeln!(out);
        let (_, plain) = self.at_budget(self.budgets[0]);
        let (_, seeded) = self.seeded_at(self.budgets[0]);
        let _ = writeln!(
            out,
            "The recalled pages are read, not free: at `--max-files {}` the seeded run reads {} \
             where the walk reads {}, because a seed's sections come back like any other reached \
             page's. An agent that wants the recall pays for it either way — this is the same \
             trade as the threshold sweep, made against links instead of scent.",
            self.budgets[0],
            sum(seeded, |run| run.score.read_tokens),
            sum(plain, |run| run.score.read_tokens),
        );
        let _ = writeln!(out);
    }

    /// Which wanted pages no walk returned.
    fn never_reached(&self, out: &mut String) {
        let wide = self.widest();
        let (_, plain) = self.at_budget(wide);
        let (_, seeded) = self.seeded_at(wide);
        let missed = |runs: &[Run]| -> usize { runs.iter().map(|run| run.score.missed()).sum() };
        let _ = writeln!(
            out,
            "Wanted pages no walk reached at `--max-files {wide}`: {} query/page pairs missed \
             without seeding, {} with it.",
            missed(plain),
            missed(seeded),
        );
        let _ = writeln!(out);
        if missed(plain) == 0 {
            return;
        }
        head(out, &["Query", "Wanted but not returned"]);
        for (plain, seeded) in plain.iter().zip(seeded) {
            if plain.score.missed() == 0 {
                continue;
            }
            let Some(wanted) = self.query(&plain.id).map(Query::expected) else {
                continue;
            };
            let returned = seeded.returned();
            let missing = wanted
                .iter()
                .filter(|page| !plain.returned().contains(page.as_path()))
                .map(|page| {
                    format!(
                        "`{}`{}",
                        page.display(),
                        match returned.contains(page.as_path()) {
                            true => " — seeding reaches it",
                            false => "",
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            row(out, &[format!("`{}`", plain.id), missing]);
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
                run.visits
                    .iter()
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
             most of this table costs nothing.",
            self.budgets[0]
        );
        let _ = writeln!(out);
        head(
            out,
            &["Threshold", "Recall", "Precision", "Read (tok)", "Cost"],
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
                    ratio(mean_precision(runs)),
                    sum(runs, |run| run.score.read_tokens).to_string(),
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
        for (preview, runs) in &self.previews {
            row(
                out,
                &[
                    preview.label().to_string(),
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
        for (preview, _) in &self.scents {
            headers.push(preview.label().to_string());
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
            for (preview, links) in self.scents.iter().skip(1) {
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
                    preview.label(),
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
            "- **A hit is not an answer.** Recall counts returned files, not whether an agent \
             could do the task with them, and `read` counts characters at {}, not what a tokeniser \
             would charge.",
            CHARS_PER_TOKEN
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

    fn seeded_at(&self, budget: usize) -> &(usize, Vec<Run>) {
        self.seeded
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

fn mean_recall(runs: &[Run]) -> f64 {
    match runs.is_empty() {
        true => 0.0,
        false => runs.iter().map(|run| run.score.recall()).sum::<f64>() / runs.len() as f64,
    }
}

fn mean_precision(runs: &[Run]) -> f64 {
    match runs.is_empty() {
        true => 0.0,
        false => runs.iter().map(|run| run.score.precision()).sum::<f64>() / runs.len() as f64,
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
            input_tokens: tokens,
            output_tokens: 10,
            latency: Duration::from_millis(latency_ms),
        }
    }

    fn query(id: &str, expected: &[&str]) -> Query {
        Query {
            id: id.to_string(),
            query: "a query".to_string(),
            mode: "useful-for".to_string(),
            entry: "index.md".to_string(),
            expected: expected.iter().map(|page| (*page).to_string()).collect(),
            note: None,
        }
    }

    /// The reading numbers are the text of the returned ranges, and a range the
    /// reader has already read in another range is not read twice: sections
    /// nest, so a parent's range contains its children's.
    #[test]
    fn read_tokens_count_each_line_once_however_ranges_overlap() {
        // Four lines of four characters each, newline included, so a token is
        // one line at four characters per token.
        let corpus = corpus(&[("a.md", "aaa\nbbb\nccc\nddd\n")]);
        let page = Path::new("a.md");

        assert_eq!(tokens(corpus.chars_in(page, &[[1, 2]])), 2, "two lines");
        assert_eq!(
            tokens(corpus.chars_in(page, &[[1, 4], [2, 3]])),
            4,
            "the nested range adds nothing"
        );
        assert_eq!(
            tokens(corpus.chars_in(page, &[[1, 1], [3, 3]])),
            2,
            "disjoint ranges add up"
        );
        assert_eq!(
            tokens(corpus.chars_in(page, &[[9, 20]])),
            0,
            "a range past the end reads nothing"
        );
        assert_eq!(tokens(corpus.chars_of(page)), 4, "the whole file");
    }

    /// The characters counted are the file's, not the bytes': a page of
    /// non-ASCII prose is what its reader reads.
    #[test]
    fn read_tokens_count_characters_not_bytes() {
        let corpus = corpus(&[("a.md", "héllo wörld\n")]);
        assert_eq!(corpus.chars_in(Path::new("a.md"), &[[1, 1]]), 12);
    }

    /// Recall is what the list found over what was wanted, precision what it
    /// found over what it returned, and the read number is the ranges only.
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
        let visits = [visit("wanted.md", &[[1, 2]]), visit("noise.md", &[[1, 1]])];

        let score = env.score(&query, &visits);
        assert_eq!(score.gold, 2);
        assert_eq!(score.returned, 2);
        assert_eq!(score.found, 1);
        assert_eq!(score.recall(), 0.5);
        assert_eq!(score.precision(), 0.5);
        assert_eq!(score.missed(), 1);
        assert_eq!(
            score.read_tokens, 3,
            "wanted.md's first two lines and noise.md's one line, not the files"
        );
        assert_eq!(
            score.whole_tokens, 6,
            "wanted.md whole and noise.md whole, not the ranges"
        );
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
