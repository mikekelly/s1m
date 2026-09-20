//! One query end to end: the flags, the walk, and the reading list the caller
//! reads.
//!
//! This is the half of the CLI that does not need clap. [`run`] takes the flags
//! as a plain [`Options`] and the model as an injected [`Judge`], so a test
//! drives the whole pipeline — the walk, the ranking and the JSON — with a fake
//! scorer, no key and no network. Argument parsing lives in `main.rs`, which is
//! also where the exit code becomes a process exit, and where the plan's
//! defaults and the cache are chosen.
//!
//! What the reading list is:
//!
//! - The plan's `Output` section, minus `sections`, which is
//!   [#9](https://github.com/mikekelly/s1m/issues/9): the query, the criterion
//!   the answers were judged against, how many files were visited and how many
//!   calls they cost, and one entry per visited file with its relevance, the
//!   scent of the link that reached it, the `via` path, whether it entered the
//!   walk as a keyword seed, and the outgoing links that were judged.
//! - Sorted by relevance descending, then path. Every path is spelled the way
//!   the caller spelled its entry files, so `--root wiki` with `wiki/index.md`
//!   reads `wiki/payments/cutoffs.md` and not `payments/cutoffs.md`. That is
//!   the spelling the plan's example uses, and the one a caller can hand
//!   straight back to an editor or another command.
//!
//! `--seed-grep` adds the query's keyword hits under the root to the frontier as
//! extra entry files ([`crate::seed`]), and `--seed-count` says how many. A seed
//! walks exactly like an entry file, and the reading list says which results
//! they are with `seeded`. The caller's own entry files are left out of the
//! hits: they are on the frontier already. Seeding changes nothing about the
//! exit codes — a seeded run whose links all fell below the threshold still
//! exits 1, with its seeds in the list.
//!
//! What the exit code is:
//!
//! - 0 when the walk reached a file beyond the entry files, which is a reading
//!   list the caller could not have written itself.
//! - 1 when nothing cleared the threshold: the model judged the entry files'
//!   links and none passed, so the list is the entry files and nothing more.
//!   The list is still printed — a caller that wants it gets it — with one line
//!   on stderr saying why the code is not 0.
//! - 2 for anything that stops a list being an answer: bad flags, no query, an
//!   entry file that cannot be read, a missing `TYPESAFE_API_KEY`, and a
//!   judgment that failed. A *reached* file that cannot be read is not in this
//!   list; see [`run`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde::Serialize;

use crate::cache::{Cacheable, CachedScorer};
use crate::parse::{self, ParseError, ParsedFile};
use crate::scorer::{FileJudgment, Scorer, ScorerError};
use crate::seed;
use crate::traverse::{Config, FailedFile, Failure, Traversal, TraverseError, traverse};

/// One run's inputs: the flags the CLI carries, with their defaults applied.
///
/// Nothing here has a default of its own. The plan's defaults — `--max-files`
/// 25, `--max-depth` 6, `--threshold` 0.6, `--fanout` 8, `--root` the first
/// entry file's directory — belong to the flags that carry them.
#[derive(Debug, Clone)]
pub struct Options {
    /// What the reading list should be useful for, in the asker's own words.
    pub query: String,
    /// Entry files, spelled the caller's way: the walk starts from each of them
    /// at path score 1, and every path in the reading list is spelled like
    /// these.
    pub entries: Vec<PathBuf>,
    /// The directory that bounds the walk; a link resolving outside it is never
    /// followed. `None` means the first entry file's directory.
    pub root: Option<PathBuf>,
    /// Most files visited before the walk stops.
    pub max_files: usize,
    /// Most link hops from an entry file.
    pub max_depth: usize,
    /// Least link scent that queues a target.
    pub threshold: f64,
    /// Frontier files expanded per round.
    pub fanout: usize,
    /// Most keyword hits `--seed-grep` adds as extra entry files under the
    /// root, or `None` when seeding is off, which is the default. The entry
    /// files are left out of the hits: they are on the frontier already, so a
    /// hit on one of them is not an extra entry point.
    pub seed_grep: Option<usize>,
    /// The criterion the answers were judged against, as the reading list
    /// reports it: the mode's name (`about`, `useful-for`, `answers`), or the
    /// criteria file's path when `--criteria` named one. The scorer is what
    /// carries the criterion, because it is built with the questions, so the
    /// CLI takes the name from there.
    pub mode: String,
}

impl Options {
    /// The directory the walk is bounded by and every path is spelled against:
    /// `--root` when it was given, else the first entry file's directory, the
    /// way the plan's flag table says.
    ///
    /// `None` when there is no entry file, whatever `--root` says: `--root`
    /// bounds a walk, and a walk with nothing to start from is not a run. The
    /// CLI wants the root before it can ask for anything else — it is what the
    /// scorer reads link previews against — so this is also where a command
    /// line with nothing to walk is caught, before a key is read or a file is
    /// opened.
    pub fn root(&self) -> Option<PathBuf> {
        let first = self.entries.first()?;
        Some(match &self.root {
            Some(root) => root.clone(),
            None => first
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."))
                .to_path_buf(),
        })
    }
}

/// The model side of a run: the scorer the walk asks, and what its answers
/// cost.
///
/// Two methods rather than a bare [`Scorer`] plus a count, because the reading
/// list publishes both and the count is not the number of files visited: a
/// cache in front of the scorer absorbs most of the judgments, and what a run
/// reports is what the API was asked. `scorer` rather than a `Judge: Scorer`
/// supertrait because `&dyn Judge` does not coerce to `&dyn Scorer` before Rust
/// 1.86, and this crate builds on 1.85.
pub trait Judge: Send + Sync {
    /// The scorer the walk asks, one file at a time.
    fn scorer(&self) -> &dyn Scorer;

    /// Answers bought from the API so far: what the reading list reports as
    /// `calls`.
    fn calls(&self) -> u64;
}

/// A [`CachedScorer`] as the CLI's judge.
///
/// `calls` is the cache's own count, which is the point: it is the misses, so a
/// run whose answers were all on disk reports no calls and costs nothing.
impl<S: Cacheable> Judge for CachedScorer<S> {
    fn scorer(&self) -> &dyn Scorer {
        self
    }

    fn calls(&self) -> u64 {
        CachedScorer::calls(self)
    }
}

/// A scorer with no cache in front of it, which is what `--no-cache` runs.
///
/// Every judgment is bought, so counting the answers that come back is the same
/// count [`CachedScorer`] keeps: what the API was asked, not what the walk
/// visited.
pub struct Uncached<S> {
    inner: S,
    calls: AtomicU64,
}

impl<S: Scorer> Uncached<S> {
    /// Wraps `inner`, counting the answers it returns.
    pub fn new(inner: S) -> Self {
        Uncached {
            inner,
            calls: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl<S: Scorer> Scorer for Uncached<S> {
    async fn score(&self, query: &str, file: &ParsedFile) -> Result<FileJudgment, ScorerError> {
        let judgment = self.inner.score(query, file).await?;
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(judgment)
    }
}

impl<S: Scorer> Judge for Uncached<S> {
    fn scorer(&self) -> &dyn Scorer {
        self
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }
}

/// The reading list, in the shape the plan's `Output` section describes it.
///
/// Field names and order are that section: `query`, `mode`, `visited`, `calls`,
/// `results`, and per result `path`, `relevance`, `scent`, `via`, `links`, each
/// link carrying `target`, `scent` and `followed`. `sections` is the one field
/// the plan has that this does not, because #9 brings it.
#[derive(Debug, Serialize)]
pub struct ReadingList {
    /// The query, unchanged.
    pub query: String,
    /// The relevance criterion the answers were judged against.
    pub mode: String,
    /// Files scored, and so results returned.
    pub visited: usize,
    /// Answers bought from the API: the cache's misses, or every score when
    /// `--no-cache` skipped the cache. A repeat run of the same query is
    /// therefore `0`.
    pub calls: u64,
    /// The visited files, most relevant first, ties broken by path.
    pub results: Vec<RankedFile>,
}

impl ReadingList {
    /// The code the process exits with. See the module documentation for what
    /// each one means.
    pub fn exit_code(&self) -> i32 {
        if self.results.iter().any(|result| !result.via.is_empty()) {
            0
        } else {
            1
        }
    }

    /// The reading list as it goes to stdout: one field per line, because both
    /// a person reading a run and a diff of two runs do better with it.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a reading list is strings, numbers and booleans")
    }
}

/// One file in the reading list.
#[derive(Debug, Serialize)]
pub struct RankedFile {
    /// The file, spelled the way the caller spelled its entry files.
    pub path: String,
    /// How useful the file is for the query, 0 to 1.
    pub relevance: f64,
    /// The scent of the link that reached it; `None` for an entry file, which
    /// no link reached.
    pub scent: Option<f64>,
    /// The files on the best path to this one, in order and excluding it; empty
    /// for an entry file.
    pub via: Vec<String>,
    /// Whether this file entered the walk as a `--seed-grep` keyword seed
    /// rather than as an entry file the caller named or along a link.
    pub seeded: bool,
    /// This file's outgoing links, in the order they appear, one per target.
    pub links: Vec<RankedLink>,
}

/// One outgoing link of a ranked file, as it was judged.
#[derive(Debug, PartialEq, Serialize)]
pub struct RankedLink {
    /// The link's target, spelled like [`RankedFile::path`].
    pub target: String,
    /// The scent the model gave it, `None` when it named no link to this
    /// target.
    pub scent: Option<f64>,
    /// Whether this link queued its target: inside the root, at or above the
    /// threshold, and within the depth budget.
    pub followed: bool,
}

/// Why a run produced no reading list. Every one of these is exit 2.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No query, or no entry file to start from.
    #[error("a query and at least one entry file are required; see --help")]
    MissingArguments,
    /// An entry file the caller named could not be read or parsed. Not the same
    /// as a link to a file that is not there: that one is the wiki's business.
    #[error(transparent)]
    Entry(#[from] ParseError),
    /// The walk could not start. `run` reads every entry file before it walks,
    /// and [`crate::parse::parse`] rejects a file and a root spelled against
    /// different bases, so the one variant [`TraverseError`] has today surfaces
    /// as [`Error::Entry`] first; this is the guard for whatever it grows.
    #[error(transparent)]
    Walk(#[from] TraverseError),
    /// The scorer could not be built: no `TYPESAFE_API_KEY`, or nowhere to put
    /// cached answers.
    #[error(transparent)]
    Scorer(#[from] ScorerError),
    /// A file the walk reached could not be judged. A reading list with a hole
    /// where a judgment should be is a different answer, and a caller could not
    /// tell the difference, so there is no list.
    #[error("{path} could not be judged: {source}")]
    Judge {
        path: String,
        #[source]
        source: ScorerError,
    },
}

/// Runs one query and returns the reading list for it.
///
/// The entry files are read before the walk starts: an entry the caller named
/// and s1m cannot read is the caller's mistake, and finding that out after
/// paying for a round of judgments would be too late. A link to a file that is
/// not there is the opposite case — the wiki's business, not the caller's — so
/// it is named on stderr as skipped and the walk carries on.
pub async fn run(options: &Options, judge: &dyn Judge) -> Result<ReadingList, Error> {
    if options.query.trim().is_empty() {
        return Err(Error::MissingArguments);
    }
    let Some(root) = options.root() else {
        return Err(Error::MissingArguments);
    };

    for entry in &options.entries {
        parse::parse(entry, &root)?;
    }

    let seeds = match options.seed_grep {
        Some(count) => seed::seed(&root, &options.query, count, &options.entries),
        None => Vec::new(),
    };

    let config = Config {
        query: &options.query,
        entries: &options.entries,
        seeds: &seeds,
        root: &root,
        max_files: options.max_files,
        max_depth: options.max_depth,
        fanout: options.fanout,
        threshold: options.threshold,
    };
    let traversal = traverse(&config, judge.scorer()).await?;

    let Traversal {
        results, failed, ..
    } = traversal;
    for FailedFile { path, failure } in failed {
        match failure {
            // One broken link does not cost the reading list, but the gap it
            // leaves must not be silent either.
            Failure::Parse(source) => {
                eprintln!("s1m: skipped {}: {source}", display(&root, &path));
            }
            Failure::Score(source) => {
                return Err(Error::Judge {
                    path: display(&root, &path),
                    source,
                });
            }
        }
    }

    let results = results
        .into_iter()
        .map(|file| RankedFile {
            path: display(&root, &file.path),
            relevance: file.relevance,
            scent: file.scent,
            via: file.via.iter().map(|via| display(&root, via)).collect(),
            seeded: file.seeded,
            links: file
                .links
                .into_iter()
                .map(|link| RankedLink {
                    target: display(&root, &link.target),
                    scent: link.scent,
                    followed: link.followed,
                })
                .collect(),
        })
        .collect::<Vec<_>>();

    Ok(ReadingList {
        query: options.query.clone(),
        mode: options.mode.clone(),
        visited: results.len(),
        calls: judge.calls(),
        results,
    })
}

/// A walk's path as the reading list spells it: the root joined back on, so the
/// caller reads the same paths it passed in.
///
/// [`crate::traverse`] works in paths relative to the root, which is what lets
/// it compare two ways of reaching one file. This is the inverse.
fn display(root: &Path, path: &Path) -> String {
    parse::from_root(root, path).display().to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::scorer::LinkJudgment;

    const ENTRY: &str = "tests/fixtures/cli/entry.md";
    const BROKEN: &str = "tests/fixtures/cli/broken.md";

    // ------------------------------------------------------------- the fake

    /// A scorer with an answer the test sets: the relevance of a file and one
    /// scent for all of its links.
    ///
    /// It counts what it answers the way a scorer that had to ask does, so a
    /// test can tell what the reading list reports `calls` from: `buys` is a
    /// run whose answers were all on disk, which is what a warm cache looks
    /// like from here.
    struct Fake {
        relevance: fn(&ParsedFile) -> f64,
        scent: f64,
        /// A file name to refuse, as the API refusing one page's question does.
        refuse: Option<&'static str>,
        buys: bool,
        calls: AtomicU64,
    }

    impl Fake {
        /// Every answer costs a call, every file is relevant the way
        /// `relevance` says, and every link is scented `scent`.
        fn uncached(relevance: fn(&ParsedFile) -> f64, scent: f64) -> Fake {
            Fake::answering(relevance, scent, true)
        }

        /// The same answers, all of them already on disk: no call, so nothing
        /// counted.
        fn cached(relevance: fn(&ParsedFile) -> f64, scent: f64) -> Fake {
            Fake::answering(relevance, scent, false)
        }

        /// The uncached fake, refusing `stem`'s question.
        fn refusing(relevance: fn(&ParsedFile) -> f64, scent: f64, stem: &'static str) -> Fake {
            Fake {
                refuse: Some(stem),
                ..Fake::uncached(relevance, scent)
            }
        }

        fn answering(relevance: fn(&ParsedFile) -> f64, scent: f64, buys: bool) -> Fake {
            Fake {
                relevance,
                scent,
                refuse: None,
                buys,
                calls: AtomicU64::new(0),
            }
        }

        fn refuses(&self, file: &ParsedFile) -> bool {
            self.refuse
                .is_some_and(|stem| file.path.file_name().is_some_and(|name| name == stem))
        }
    }

    #[async_trait]
    impl Scorer for Fake {
        async fn score(
            &self,
            _query: &str,
            file: &ParsedFile,
        ) -> Result<FileJudgment, ScorerError> {
            if self.refuses(file) {
                return Err(ScorerError::Status {
                    endpoint: "https://api.typesafe.ai/v1/systemone".to_string(),
                    status: 500,
                    body: "{}".to_string(),
                });
            }
            if self.buys {
                self.calls.fetch_add(1, Ordering::Relaxed);
            }
            Ok(FileJudgment {
                relevance: (self.relevance)(file),
                links: file
                    .links
                    .iter()
                    .map(|link| LinkJudgment {
                        target: link.target.clone(),
                        scent: self.scent,
                    })
                    .collect(),
            })
        }
    }

    impl Judge for Fake {
        fn scorer(&self) -> &dyn Scorer {
            self
        }

        fn calls(&self) -> u64 {
            self.calls.load(Ordering::Relaxed)
        }
    }

    /// The entry file is the least relevant page of the three, so a reading
    /// list that came back in walk order rather than by relevance is visible.
    fn by_relevance(file: &ParsedFile) -> f64 {
        match file.path.file_stem().and_then(|stem| stem.to_str()) {
            Some("entry" | "broken") => 0.3,
            _ => 0.9,
        }
    }

    // ------------------------------------------------------------- the runs

    fn options(entry: &str) -> Options {
        Options {
            query: "how do I cut a release".to_string(),
            entries: vec![PathBuf::from(entry)],
            root: None,
            max_files: 25,
            max_depth: 6,
            threshold: 0.6,
            fanout: 8,
            seed_grep: None,
            mode: "useful-for".to_string(),
        }
    }

    async fn run_with(entry: &str, judge: &dyn Judge) -> Result<ReadingList, Error> {
        run(&options(entry), judge).await
    }

    /// The plan's output shape, asserted whole: the field names, the ranking,
    /// the path spelling, and what an entry file looks like in it.
    #[tokio::test]
    async fn the_reading_list_is_the_plan_shape_ranked_and_spelled_the_callers_way() {
        let list = run_with(ENTRY, &Fake::uncached(by_relevance, 0.9))
            .await
            .expect("the fixture walks");

        assert_eq!(
            serde_json::from_str::<Value>(&list.to_json()).expect("the reading list is JSON"),
            json!({
                "query": "how do I cut a release",
                "mode": "useful-for",
                "visited": 3,
                "calls": 3,
                "results": [
                    {
                        "path": "tests/fixtures/cli/deep.md",
                        "relevance": 0.9,
                        "scent": 0.9,
                        "via": ["tests/fixtures/cli/entry.md", "tests/fixtures/cli/next.md"],
                        "seeded": false,
                        "links": [],
                    },
                    {
                        "path": "tests/fixtures/cli/next.md",
                        "relevance": 0.9,
                        "scent": 0.9,
                        "via": ["tests/fixtures/cli/entry.md"],
                        "seeded": false,
                        "links": [
                            {
                                "target": "tests/fixtures/cli/deep.md",
                                "scent": 0.9,
                                "followed": true,
                            },
                        ],
                    },
                    {
                        "path": "tests/fixtures/cli/entry.md",
                        "relevance": 0.3,
                        "scent": null,
                        "via": [],
                        "seeded": false,
                        "links": [
                            {
                                "target": "tests/fixtures/cli/next.md",
                                "scent": 0.9,
                                "followed": true,
                            },
                        ],
                    },
                ],
            })
        );
        assert_eq!(list.exit_code(), 0);
    }

    /// A link below the threshold queues nothing, so the walk reaches nothing
    /// beyond the entry file, and the exit code says so while the list is still
    /// there to read.
    #[tokio::test]
    async fn nothing_above_the_threshold_exits_1_with_the_entry_files() {
        let list = run_with(ENTRY, &Fake::uncached(by_relevance, 0.59))
            .await
            .expect("the entry file still walks");

        assert_eq!(list.exit_code(), 1);
        assert_eq!(list.visited, 1);
        assert_eq!(list.results.len(), 1);
        assert_eq!(list.results[0].path, ENTRY);
        assert_eq!(list.results[0].scent, None);
        assert_eq!(
            list.results[0].links,
            vec![RankedLink {
                target: "tests/fixtures/cli/next.md".to_string(),
                scent: Some(0.59),
                followed: false,
            }]
        );
    }

    /// `calls` is the judge's count, not the number of files visited: a run
    /// whose answers were all stored costs nothing, which is the whole point of
    /// the cache.
    #[tokio::test]
    async fn calls_are_the_judges_count_not_the_files_visited() {
        let list = run_with(ENTRY, &Fake::cached(by_relevance, 0.9))
            .await
            .expect("the fixture walks");

        assert_eq!(list.visited, 3);
        assert_eq!(list.calls, 0);
    }

    /// A link to a page that is not there is the wiki's business: it is left
    /// out of the list, and the walk carries on to the pages that are there.
    #[tokio::test]
    async fn a_reached_file_that_is_not_there_is_skipped_not_fatal() {
        let list = run_with(BROKEN, &Fake::uncached(by_relevance, 0.9))
            .await
            .expect("a broken link does not end the walk");

        let paths: Vec<&str> = list.results.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "tests/fixtures/cli/deep.md",
                "tests/fixtures/cli/next.md",
                "tests/fixtures/cli/broken.md",
            ]
        );
    }

    /// A missing or unreadable entry file is the caller's mistake, and it is
    /// reported before the walk spends anything.
    #[tokio::test]
    async fn an_entry_file_that_is_not_there_is_an_error() {
        let error = run_with(
            "tests/fixtures/cli/gone.md",
            &Fake::uncached(by_relevance, 0.9),
        )
        .await
        .expect_err("nothing to walk from");

        assert!(matches!(error, Error::Entry(_)), "{error:?}");
        assert!(error.to_string().contains("gone.md"), "{error}");
    }

    /// A judgment that fails leaves a hole in the ranking, so there is no
    /// reading list at all, and the file whose question failed is named.
    #[tokio::test]
    async fn a_failed_judgment_is_an_error_naming_the_file() {
        let error = run_with(ENTRY, &Fake::refusing(by_relevance, 0.9, "next.md"))
            .await
            .expect_err("a hole in the ranking is not an answer");

        match error {
            Error::Judge { path, .. } => assert_eq!(path, "tests/fixtures/cli/next.md"),
            other => panic!("expected a failed judgment, got {other:?}"),
        }
    }

    /// Nothing to walk from: no entry file, and a query that is not a question.
    /// `--root` does not change that — a root with no entry file bounds a walk
    /// that never starts, so it is the arguments that are wrong, and it is said
    /// before the model is asked anything.
    #[tokio::test]
    async fn a_run_without_a_query_or_an_entry_file_is_an_error() {
        let judge = Fake::uncached(by_relevance, 0.9);

        let mut no_entries = options(ENTRY);
        no_entries.entries.clear();
        assert!(matches!(
            run(&no_entries, &judge).await,
            Err(Error::MissingArguments)
        ));

        let mut root_only = options(ENTRY);
        root_only.entries.clear();
        root_only.root = Some(PathBuf::from("tests/fixtures/cli"));
        assert_eq!(root_only.root(), None, "a root without entries is no root");
        assert!(matches!(
            run(&root_only, &judge).await,
            Err(Error::MissingArguments)
        ));

        let mut no_query = options(ENTRY);
        no_query.query = "   ".to_string();
        assert!(matches!(
            run(&no_query, &judge).await,
            Err(Error::MissingArguments)
        ));
        assert_eq!(judge.calls(), 0, "nothing was asked of the model");
    }

    /// `--root` is the base every path is spelled against, and the fence the
    /// walk stays inside.
    #[tokio::test]
    async fn paths_are_spelled_against_the_root() {
        let mut options = options("tests/fixtures/cli/entry.md");
        options.root = Some(PathBuf::from("tests/fixtures/cli"));

        let list = run(&options, &Fake::uncached(by_relevance, 0.9))
            .await
            .expect("the fixture walks");

        assert_eq!(
            list.results
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "tests/fixtures/cli/deep.md",
                "tests/fixtures/cli/next.md",
                "tests/fixtures/cli/entry.md",
            ]
        );
    }
}
