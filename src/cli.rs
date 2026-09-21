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
//! - The plan's `Output` section: the query, the criterion the answers were
//!   judged against, how many files were visited and how many calls they cost,
//!   and one entry per file that earned a place with its relevance, the scent
//!   of the link that reached it, the `via` path, the line ranges worth reading
//!   and the outgoing links that were judged.
//! - `results` holds the files that earn a place on their own: relevance at or
//!   above `threshold`, or a section at or above it
//!   ([`crate::traverse::VisitedFile::earns_a_place`]). A hub is worth walking
//!   through and not worth reading, so the entry pages and section indexes the
//!   walk only passed through are reported under [`ReadingList::walked`]
//!   instead: their path, relevance, scent, `via` and judged links, and no
//!   `sections`. `visited` counts both lists, and each is sorted by relevance
//!   descending, then path.
//! - Every path is spelled the way the caller spelled its entry files, so
//!   `--root wiki` with `wiki/index.md` reads `wiki/payments/cutoffs.md` and
//!   not `payments/cutoffs.md`. That is the spelling the plan's example uses,
//!   and the one a caller can hand straight back to an editor or another
//!   command.
//! - Sections are the parser's ranges and the model's scores, most useful
//!   first, with the ones below `threshold` left out. A section's range
//!   contains its subsections', so a caller that reads a returned range has
//!   read everything returned inside it.
//!
//! What the exit code is:
//!
//! - 0 when the walk reached a file beyond the entry files that earned a place,
//!   which is a reading list the caller could not have written itself.
//! - 1 when it did not: `results` is entry files alone, or empty with the walk
//!   in [`ReadingList::walked`]. The model judged the entry files' links and
//!   none passed, or everything it reached was a hub, or the page one of them
//!   reached could not be judged. The list is still printed — a caller that
//!   wants it gets it — with one line on stderr saying why the code is not 0.
//! - 2 for anything that stops a list being an answer: bad flags, no query, an
//!   entry file that cannot be read, an entry file the root's `.s1mignore`
//!   covers ([`Error::Ignored`]), a `.s1mignore` that cannot be read or parsed,
//!   a missing `TYPESAFE_API_KEY`, and a run that judged nothing at all. A
//!   *reached* file that cannot be read, and a reached file whose judgment
//!   failed, are not in this list; see [`run`].

use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde::Serialize;

use crate::cache::{Cacheable, CachedScorer};
use crate::ignore::{Ignore, IgnoreError};
use crate::parse::{self, ParseError, ParsedFile};
use crate::scorer::{FileJudgment, Scorer, ScorerError};
use crate::trace::Trace;
use crate::traverse::{
    Admission, Config, FailedFile, Failure, JudgedSection, Traversal, TraverseError, traverse,
};

/// One run's inputs: the flags the CLI carries, with their defaults applied.
///
/// Nothing here has a default of its own. The plan's defaults — `--max-files`
/// 25, `--max-depth` 6, `--threshold` 0.6, `--root` the first entry file's
/// directory — belong to the flags that carry them; the round size is the
/// constant `main.rs` hands the walk.
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
    /// Most files the walk judges beyond the entry files. The entry files are
    /// always visited and never count against it.
    pub max_files: usize,
    /// Most link hops from an entry file.
    pub max_depth: usize,
    /// Least link scent that queues a target, under
    /// [`Admission::Threshold`]: the plan's `--threshold`, and the same number
    /// the reading list cuts at.
    ///
    /// The walk's frontier and the reading list are two cutoffs, and the CLI
    /// gives them one number: the same `--threshold` decides what is followed
    /// and what is worth reading. A scorer whose numbers are not on that scale
    /// — a Choice share — is admitted by
    /// [`Admission::Scorer`](crate::traverse::Admission::Scorer) instead, and
    /// this stays the reading list's cutoff on the file's own relevance.
    pub threshold: f64,
    /// How the walk's links are admitted to the frontier, which follows from
    /// the scorer: a Noul against `threshold`, a Choice share against the
    /// scorer's own rule. `main.rs` is where the pair is decided.
    pub admission: Admission,
    /// Most paths the walk holds at one depth, or `None` for no ceiling: the
    /// beam the relative-scent spike walks with, and off for the walk that
    /// ships.
    pub beam: Option<usize>,
    /// Frontier files expanded per round. The CLI has no flag for it; `main.rs`
    /// sets it from the plan's constant.
    pub fanout: usize,
    /// The criterion the answers were judged against, as the reading list
    /// reports it: the mode's name (`about`, `useful-for`, `answers`), or the
    /// criteria file's path when `--criteria` named one. The scorer is what
    /// carries the criterion, because it is built with the questions, so the
    /// CLI takes the name from there.
    pub mode: String,
    /// How the links were judged, as the reading list reports it: `noul` for a
    /// yes/no question per link, `choice` for one Choice over a page's links.
    /// The two numbers on a link's `scent` are not the same kind of number, so
    /// the reading list says which one it is ([`ReadingList::scorer`]).
    pub scorer: String,
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

    /// The trace this run writes, when `--trace` named a file, or `None`.
    ///
    /// `main.rs` builds one trace and hands it to the judge, which reports the
    /// requests and answers to it; the walk reports its own events to the same
    /// one, which is why it asks the judge for it rather than being passed one
    /// of its own ([`crate::trace`]).
    fn trace(&self) -> Option<&Trace> {
        None
    }
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

    fn trace(&self) -> Option<&Trace> {
        CachedScorer::trace(self)
    }
}

/// A scorer with no cache in front of it, which is what `--no-cache` runs.
///
/// Every judgment is bought, so counting the answers that come back is the same
/// count [`CachedScorer`] keeps: what the API was asked, not what the walk
/// visited.
///
/// It wraps a [`Cacheable`] rather than a bare [`Scorer`] because a run with no
/// cache is the same run with nothing kept: it builds the request the cache
/// would key — so the two mean the same thing whatever the scorer did with it —
/// and it is what lets a trace report one `requested` event per post whether
/// the answers were bought or served ([`crate::trace`]).
pub struct Uncached<S> {
    inner: S,
    calls: AtomicU64,
    /// The run's trace, when one was asked for: the same trace the walk writes
    /// to, and where this reports the requests and answers the cache would
    /// otherwise report ([`crate::cache::CachedScorer::with_trace`]).
    trace: Option<Arc<Trace>>,
}

impl<S: Cacheable> Uncached<S> {
    /// Wraps `inner`, counting the answers it returns.
    pub fn new(inner: S) -> Self {
        Uncached {
            inner,
            calls: AtomicU64::new(0),
            trace: None,
        }
    }

    /// Reports this run's requests and answers to `trace`, when there is one.
    pub fn with_trace(mut self, trace: Option<Arc<Trace>>) -> Self {
        self.trace = trace;
        self
    }
}

#[async_trait]
impl<S: Cacheable> Scorer for Uncached<S> {
    async fn score(&self, query: &str, file: &ParsedFile) -> Result<FileJudgment, ScorerError> {
        let request = self.inner.request(query, file)?;
        if let Some(trace) = &self.trace {
            trace.requested(&file.path, self.inner.posts(&request));
        }
        let (judgment, detail) = self.inner.call(&request, file).await?;
        self.calls.fetch_add(1, Ordering::Relaxed);
        if let Some(trace) = &self.trace {
            // No cache was asked, so no answer can have come off the disk.
            trace.answered(&file.path, S::latency(&detail), false, &judgment);
        }
        Ok(judgment)
    }
}

impl<S: Cacheable> Judge for Uncached<S> {
    fn scorer(&self) -> &dyn Scorer {
        self
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }

    fn trace(&self) -> Option<&Trace> {
        self.trace.as_deref()
    }
}

/// The reading list, in the shape the plan's `Output` section describes it.
///
/// Field names and order are that section: `query`, `mode`, `visited`, `calls`,
/// `results`, and per result `path`, `relevance`, `scent`, `via`, `sections`,
/// `links`, each section carrying `heading`, `lines` and `score` and each link
/// `target`, `scent` and `followed`. [`Self::walked`] is the walk's other half,
/// appended: the same file as a result without `sections`.
#[derive(Debug, Serialize)]
pub struct ReadingList {
    /// The query, unchanged.
    pub query: String,
    /// The relevance criterion the answers were judged against.
    pub mode: String,
    /// How the links were judged: `noul`, one yes/no question per link, or
    /// `choice`, one Choice over a page's links.
    ///
    /// A link's `scent` is a probability of leading somewhere useful under
    /// `noul`, and one option's share of a page's Choice under `choice` — a
    /// number that means something only beside the other links of that page.
    /// The field is here so a reader knows which number it is looking at.
    pub scorer: String,
    /// Files judged: the ones in [`Self::results`] and the ones in
    /// [`Self::walked`], which are what `--max-files` budgets.
    pub visited: usize,
    /// Answers bought from the API: the cache's misses, or every score when
    /// `--no-cache` skipped the cache. A repeat run of the same query is
    /// therefore `0`.
    ///
    /// A judgment, not an HTTP request: a file whose sections and links did not
    /// fit the API's state budget in one request costs several, and is still
    /// one answer here (`s1m score-file` reports the requests).
    pub calls: u64,
    /// The visited files that earn a place, most relevant first, ties broken by
    /// path.
    pub results: Vec<RankedFile>,
    /// The visited files that did not earn a place on their own: entry files,
    /// hubs and section indexes, and any page whose relevance and every section
    /// fell below `threshold`.
    ///
    /// They are reported rather than dropped so the walk stays explainable —
    /// the path that reached each one, the links it judged, its own relevance —
    /// which is what [`crate::format`]'s tree is drawn from. Nothing here is
    /// something to read: `md` leaves them out. Sorted like [`Self::results`],
    /// and no `sections`: a file that earns no place has no ranges to return.
    pub walked: Vec<WalkedFile>,
}

impl ReadingList {
    /// The code the process exits with. See the module documentation for what
    /// each one means.
    ///
    /// The question is whether the list holds anything the caller could not
    /// have written itself, so a file a link reached has to be in `results` to
    /// earn a 0: entry files alone are the caller's own starting points, and a
    /// hub that the walk only passed through is not a page to read.
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

/// One visited file that did not earn a place in [`ReadingList::results`].
///
/// The same file as a [`RankedFile`] minus its sections: it is reported for what
/// it explains rather than for what it holds — where the walk reached it from,
/// what it thought of the links it offered — so a hub that led to the pages that
/// matter is still visible in the JSON and in `tree`, without being offered as
/// something to read.
#[derive(Debug, Serialize)]
pub struct WalkedFile {
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
    /// This file's outgoing links, in the order they appear, one per target.
    pub links: Vec<RankedLink>,
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
    /// The file's heading sections that cleared `threshold`, most useful first
    /// and ties broken by the file's own order, each with the line range to
    /// read.
    ///
    /// This is where the caller reads from: the range is the parser's, so the
    /// lines named are the text that was scored, and a section's range contains
    /// its subsections' — a reader that has read one range has read everything
    /// inside it.
    pub sections: Vec<RankedSection>,
    /// This file's outgoing links, in the order they appear, one per target.
    pub links: Vec<RankedLink>,
}

/// One heading section of a ranked file, as it was judged.
#[derive(Debug, PartialEq, Serialize)]
pub struct RankedSection {
    /// Heading text, `null` for content before the first heading.
    pub heading: Option<String>,
    /// `[first, last]` line, inclusive, 1-based, as the parser gave them.
    pub lines: [usize; 2],
    /// How useful the section is for the query, 0 to 1.
    pub score: f64,
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
    /// The root's `.s1mignore` could not be used ([`crate::ignore`]).
    #[error(transparent)]
    Ignore(#[from] IgnoreError),
    /// An entry file the caller named is one the root's `.s1mignore` covers.
    /// The file is never read, so there is no list to return: the caller either
    /// means it — and can say so by narrowing `.s1mignore` — or named the wrong
    /// path. Exit 2, because a reading list built around a file that was
    /// silently dropped would answer a question the caller did not ask.
    #[error("{path} is matched by {file}: s1m never reads an ignored file")]
    Ignored { path: String, file: String },
    /// Nothing was judged: a file the walk reached could not be judged and no
    /// other page was, so there is no reading list at all. Exit 2, because a
    /// caller that asked about a wiki and got an empty answer could not tell
    /// that from a wiki that holds nothing.
    ///
    /// A judgment that fails on any other file is not this: the file is named
    /// on stderr as skipped, its links are not followed, and the pages that
    /// were judged are the reading list ([#37]).
    ///
    /// [#37]: https://github.com/mikekelly/s1m/issues/37
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
/// it is named on stderr as skipped and the walk carries on. A page that is
/// there but cannot be judged is the wiki's business too, for the same reason
/// and with the same one line on stderr: the walk keeps what it reached, and
/// only a run that judged nothing at all is an error
/// ([#37](https://github.com/mikekelly/s1m/issues/37)).
///
/// The root's `.s1mignore` is read first, and it is what the whole run is
/// bounded by: an entry file the caller names that matches is
/// [`Error::Ignored`] rather than a read, and nothing else the walk touches —
/// a link target, a link preview — is read if it matches
/// ([`crate::ignore`]).
pub async fn run(options: &Options, judge: &dyn Judge) -> Result<ReadingList, Error> {
    if options.query.trim().is_empty() {
        return Err(Error::MissingArguments);
    }
    let Some(root) = options.root() else {
        return Err(Error::MissingArguments);
    };

    let ignore = Ignore::at(&root)?;
    for entry in &options.entries {
        if ignore.matched(&parse::relative_to_root(&root, entry)) {
            return Err(Error::Ignored {
                path: entry.display().to_string(),
                file: root.join(crate::ignore::FILE).display().to_string(),
            });
        }
        parse::parse(entry, &root)?;
    }

    let config = Config {
        query: &options.query,
        entries: &options.entries,
        root: &root,
        max_files: options.max_files,
        max_depth: options.max_depth,
        fanout: options.fanout,
        admission: options.admission,
        beam: options.beam,
        ignore: &ignore,
        // The trace the judge writes its requests and answers to, when
        // `--trace` asked for one: the walk reports its own events to the same
        // file, so a reader sees one run rather than two ([`crate::trace`]).
        trace: judge.trace(),
    };
    let traversal = traverse(&config, judge.scorer()).await?;

    let Traversal {
        results: judged,
        failed,
        ..
    } = traversal;
    // One page that cannot be judged is a hole in the ranking, not the end of
    // the walk: it is named on stderr, the rest of the frontier keeps its
    // place, and the reading list is what was judged — the walk does not pay
    // again for what it already bought ([#37]).
    //
    // A run that judged nothing at all is the exception. An entry file whose
    // judgment failed, with no other page reached, has no reading list to
    // print, so the failure is the run's error rather than a line on stderr.
    //
    // [#37]: https://github.com/mikekelly/s1m/issues/37
    let mut unjudged = Vec::new();
    for FailedFile { path, failure } in failed {
        match failure {
            // One broken link does not cost the reading list, but the gap it
            // leaves must not be silent either.
            Failure::Parse(source) => skipped(&root, &path, &source),
            Failure::Score(source) => unjudged.push((path, source)),
        }
    }
    let mut unjudged = unjudged.into_iter();
    if judged.is_empty()
        && let Some((path, source)) = unjudged.next()
    {
        return Err(Error::Judge {
            path: display(&root, &path),
            source,
        });
    }
    for (path, source) in unjudged {
        skipped(&root, &path, format_args!("could not be judged: {source}"));
    }

    // The walk returns every file it visited — a hub is worth walking through
    // — and the reading list keeps only the ones that earn a place on their own
    // ([`crate::traverse::VisitedFile::earns_a_place`]). The rest are reported
    // as walked: the tree and the `via` paths beside them are what the caller
    // reads to see how the list was reached, so they are not dropped.
    let visited = judged.len();
    let mut results = Vec::with_capacity(visited);
    let mut walked = Vec::with_capacity(visited);
    for file in judged {
        let earns = file.earns_a_place(options.threshold);
        let path = display(&root, &file.path);
        let via = file
            .via
            .iter()
            .map(|via| display(&root, via))
            .collect::<Vec<_>>();
        let links = file
            .links
            .into_iter()
            .map(|link| RankedLink {
                target: display(&root, &link.target),
                scent: link.scent,
                followed: link.followed,
            })
            .collect::<Vec<_>>();

        if earns {
            results.push(RankedFile {
                path,
                relevance: file.relevance,
                scent: file.scent,
                via,
                sections: ranked_sections(&file.sections, options.threshold),
                links,
            });
        } else {
            walked.push(WalkedFile {
                path,
                relevance: file.relevance,
                scent: file.scent,
                via,
                links,
            });
        }
    }

    Ok(ReadingList {
        query: options.query.clone(),
        mode: options.mode.clone(),
        scorer: options.scorer.clone(),
        visited,
        calls: judge.calls(),
        results,
        walked,
    })
}

/// One line on stderr for a page the walk reached and dropped, and why.
///
/// One line because a caller reading stderr is parsing it, and the reason can
/// come from outside: a judgment that failed carries the API's own response
/// body, which a proxy is free to send with newlines in it
/// ([`crate::scorer::ScorerError::Status`]). `main.rs` flattens the same kind
/// of message on its way out of a run.
fn skipped(root: &Path, path: &Path, reason: impl Display) {
    eprintln!(
        "s1m: skipped {}: {}",
        display(root, path),
        reason.to_string().replace(['\n', '\r'], " ")
    );
}

/// A walk's path as the reading list spells it: the root joined back on, so the
/// caller reads the same paths it passed in.
///
/// [`crate::traverse`] works in paths relative to the root, which is what lets
/// it compare two ways of reaching one file. This is the inverse.
fn display(root: &Path, path: &Path) -> String {
    parse::from_root(root, path).display().to_string()
}

/// A visited file's sections as the reading list carries them: the ones that
/// cleared `threshold`, most useful first.
///
/// A section below the threshold is one the model did not call useful, and the
/// list exists so that the caller reads the ranges in it and nothing else.
/// Ties are broken by the file's own order — the sections' starting lines are
/// strictly increasing, so that order is total — which is what keeps the list
/// from depending on the order the answers arrived in.
fn ranked_sections(sections: &[JudgedSection], threshold: f64) -> Vec<RankedSection> {
    let mut ranked: Vec<RankedSection> = sections
        .iter()
        .filter(|section| section.score >= threshold)
        .map(|section| RankedSection {
            heading: section.heading.clone(),
            lines: section.lines,
            score: section.score,
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.lines[0].cmp(&b.lines[0]))
    });
    ranked
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::parse::Section;
    use crate::scorer::{LinkJudgment, SectionJudgment};

    const ENTRY: &str = "tests/fixtures/cli/entry.md";
    const BROKEN: &str = "tests/fixtures/cli/broken.md";
    const HUB: &str = "tests/fixtures/cli/hub.md";
    const NESTED: &str = "tests/fixtures/cli/nested.md";
    const PREAMBLE: &str = "tests/fixtures/cli/preamble.md";

    // ------------------------------------------------------------- the fake

    /// A scorer with an answer the test sets: the relevance of a file, one
    /// scent for all of its links, and one score per section through
    /// `section`.
    ///
    /// It counts what it answers the way a scorer that had to ask does, so a
    /// test can tell what the reading list reports `calls` from: `buys` is a
    /// run whose answers were all on disk, which is what a warm cache looks
    /// like from here.
    struct Fake {
        relevance: fn(&ParsedFile) -> f64,
        /// The score every section gets, section by section; `flat` unless a
        /// test wants the reading list's own ordering to show.
        section: fn(&Section) -> f64,
        scent: f64,
        /// A file name to refuse, as the API refusing one page's question does.
        refuse: Option<&'static str>,
        /// Whether the scorer keeps the links it judges: what a walk following
        /// the scorer's verdict reads instead of the scent.
        keep: bool,
        buys: bool,
        calls: AtomicU64,
    }

    /// Every section scored alike, which is what a test that is not about
    /// section scores wants.
    fn flat(_: &Section) -> f64 {
        0.9
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

        /// The same answers, with every section scored by `section`.
        fn sectioning(mut self, section: fn(&Section) -> f64) -> Fake {
            self.section = section;
            self
        }

        /// The same answers, with the scorer's verdict on every link: `false`
        /// is a scorer that keeps none of them.
        fn keeping(mut self, keep: bool) -> Fake {
            self.keep = keep;
            self
        }

        fn answering(relevance: fn(&ParsedFile) -> f64, scent: f64, buys: bool) -> Fake {
            Fake {
                relevance,
                section: flat,
                scent,
                refuse: None,
                keep: true,
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
                sections: file
                    .sections
                    .iter()
                    .map(|section| SectionJudgment {
                        heading: section.heading.clone(),
                        lines: section.lines,
                        score: (self.section)(section),
                    })
                    .collect(),
                links: file
                    .links
                    .iter()
                    .map(|link| LinkJudgment {
                        target: link.target.clone(),
                        scent: self.scent,
                        keep: self.keep,
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
            admission: Admission::Threshold(0.6),
            beam: None,
            fanout: 8,
            mode: "useful-for".to_string(),
            scorer: "noul".to_string(),
        }
    }

    async fn run_with(entry: &str, judge: &dyn Judge) -> Result<ReadingList, Error> {
        run(&options(entry), judge).await
    }

    /// A run whose walk follows the scorer's verdict rather than a threshold is
    /// the relative judge's ([#47](https://github.com/mikekelly/s1m/issues/47)):
    /// the reading list says which one it was, and the files it reaches are the
    /// ones whose links the scorer kept.
    #[tokio::test]
    async fn a_run_following_the_scorers_verdict_says_which_judge_it_was() {
        let judge = Fake::uncached(by_relevance, 0.9).keeping(false);
        let mut options = options(ENTRY);
        options.admission = Admission::Scorer;
        options.beam = Some(8);
        options.scorer = "choice".to_string();

        let list = run(&options, &judge).await.expect("the fixture walks");
        let json: Value = serde_json::from_str(&list.to_json()).expect("the reading list is JSON");

        assert_eq!(json["scorer"], "choice");
        assert_eq!(
            json["visited"], 1,
            "the entry alone: the only link out of it was judged and not kept"
        );
        assert_eq!(
            json["results"][0]["links"][0]["followed"], false,
            "a link at 0.9 is not followed when the scorer did not keep it"
        );
        assert_eq!(
            list.exit_code(),
            1,
            "and it is a list the caller could write"
        );
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
                "scorer": "noul",
                "visited": 3,
                "calls": 3,
                "results": [
                    {
                        "path": "tests/fixtures/cli/deep.md",
                        "relevance": 0.9,
                        "scent": 0.9,
                        "via": ["tests/fixtures/cli/entry.md", "tests/fixtures/cli/next.md"],
                        "sections": [
                            {"heading": "Deep", "lines": [1, 3], "score": 0.9},
                        ],
                        "links": [],
                    },
                    {
                        "path": "tests/fixtures/cli/next.md",
                        "relevance": 0.9,
                        "scent": 0.9,
                        "via": ["tests/fixtures/cli/entry.md"],
                        "sections": [
                            {"heading": "Next", "lines": [1, 3], "score": 0.9},
                        ],
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
                        "sections": [
                            {"heading": "Entry", "lines": [1, 4], "score": 0.9},
                        ],
                        "links": [
                            {
                                "target": "tests/fixtures/cli/next.md",
                                "scent": 0.9,
                                "followed": true,
                            },
                        ],
                    },
                ],
                // Every file here earned a place, so the walk's other half is
                // empty: `--format tree` prints it, and `md` would not.
                "walked": [],
            })
        );
        assert_eq!(list.exit_code(), 0);
    }

    /// A file earns a place on its own: relevance at or above the threshold, or
    /// a section at or above it. A hub earns neither — it is worth walking
    /// through, not reading — so it is reported as walked, with what the
    /// reading list says about any file but no sections, and the paths to what
    /// it led to stay legible.
    #[tokio::test]
    async fn a_file_earns_a_place_on_its_relevance_or_a_section_of_it() {
        // The hub at 0.2; the page it leads to at 0.8, which earns on its own
        // relevance; and one at 0.3 with a "Preamble" section at 0.9. The three
        // cases the cutoff tells apart.
        fn relevance(file: &ParsedFile) -> f64 {
            match file.path.file_stem().and_then(|stem| stem.to_str()) {
                Some("hub") => 0.2,
                Some("deep") => 0.8,
                _ => 0.3,
            }
        }
        fn section(section: &Section) -> f64 {
            match section.heading.as_deref() {
                Some("Preamble") => 0.9,
                _ => 0.1,
            }
        }

        let list = run_with(HUB, &Fake::uncached(relevance, 0.9).sectioning(section))
            .await
            .expect("the fixture walks");

        assert_eq!(list.visited, 3, "the walk visits the hub and both leaves");
        assert_eq!(
            list.results
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            [
                "tests/fixtures/cli/deep.md",
                "tests/fixtures/cli/preamble.md"
            ],
            "0.8 on its own relevance, and 0.3 with one section at 0.9"
        );
        assert_eq!(
            list.walked
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            [HUB],
            "a hub below the cutoff, with no section above it, is not something to read"
        );
        assert_eq!(
            list.exit_code(),
            0,
            "the walk reached pages beyond the entry"
        );

        let json: Value = serde_json::from_str(&list.to_json()).expect("the reading list is JSON");
        assert_eq!(
            json["walked"][0],
            json!({
                "path": "tests/fixtures/cli/hub.md",
                "relevance": 0.2,
                "scent": null,
                "via": [],
                "links": [
                    {
                        "target": "tests/fixtures/cli/deep.md",
                        "scent": 0.9,
                        "followed": true,
                    },
                    {
                        "target": "tests/fixtures/cli/preamble.md",
                        "scent": 0.9,
                        "followed": true,
                    },
                ],
            }),
            "a walked file is a result without its sections: where it was reached from, and what it offered"
        );
    }

    /// The line ranges in the list are #4's parser's, section for section, and
    /// the list carries them most useful first with the weak ones left out: a
    /// page whose sections nest comes back with the two the scores put above
    /// the threshold, in score order, each on its own lines.
    #[tokio::test]
    async fn sections_are_the_parsers_ranges_ranked_by_score() {
        // Every section scored from its own first line, over fifteen: the
        // document order and the score order are different, and two of the
        // four sections fall below the threshold of six tenths.
        fn by_first_line(section: &Section) -> f64 {
            section.lines[0] as f64 / 15.0
        }

        let list = run_with(
            NESTED,
            &Fake::uncached(by_relevance, 0.9).sectioning(by_first_line),
        )
        .await
        .expect("the fixture walks");

        assert_eq!(list.results.len(), 1);
        let parsed = parse::parse(NESTED, "tests/fixtures/cli").expect("the fixture parses");
        assert_eq!(
            parsed
                .sections
                .iter()
                .map(|section| (section.heading.clone(), section.lines))
                .collect::<Vec<_>>(),
            [
                (Some("Nested".to_string()), [1, 15]),
                (Some("First".to_string()), [5, 12]),
                (Some("Deeper".to_string()), [9, 12]),
                (Some("Second".to_string()), [13, 15]),
            ],
            "the fixture the expectations below are read off"
        );

        assert_eq!(
            list.results[0].sections,
            vec![
                RankedSection {
                    heading: Some("Second".to_string()),
                    lines: [13, 15],
                    score: 13.0 / 15.0,
                },
                RankedSection {
                    heading: Some("Deeper".to_string()),
                    lines: [9, 12],
                    score: 9.0 / 15.0,
                },
            ],
            "most useful first, the parser's ranges, and nothing below the threshold"
        );
    }

    /// Content before the first heading comes back as a section with a `null`
    /// heading, on the parser's lines: it is the one range no heading names,
    /// and two sections the model scored alike keep the file's own order.
    #[tokio::test]
    async fn the_preamble_is_reported_as_a_section_with_no_heading() {
        let list = run_with(PREAMBLE, &Fake::uncached(by_relevance, 0.9))
            .await
            .expect("the fixture walks");

        assert_eq!(
            list.results[0].sections,
            vec![
                RankedSection {
                    heading: None,
                    // To the line before the first heading, blank line and all.
                    lines: [1, 3],
                    score: 0.9,
                },
                RankedSection {
                    heading: Some("Preamble".to_string()),
                    lines: [4, 6],
                    score: 0.9,
                },
            ],
            "the preamble and the heading, in the file's order on a tie"
        );

        let json: Value = serde_json::from_str(&list.to_json()).expect("the reading list is JSON");
        assert_eq!(
            json["results"][0]["sections"][0]["heading"],
            Value::Null,
            "a section no heading names is `null`, not absent"
        );
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

    /// A judgment that fails on a reached page is a hole in the ranking, not
    /// the end of the walk: the page is dropped, the walk carries on with what
    /// it already reached, and the exit code is the list's own ([#37]).
    ///
    /// [#37]: https://github.com/mikekelly/s1m/issues/37
    #[tokio::test]
    async fn a_failed_judgment_drops_its_page_and_the_walk_carries_on() {
        // The entry links to `next.md`, which is where the failure is; `deep.md`
        // is one hop further on and unreachable without it, so the list is the
        // entry file and the exit code says nothing cleared the threshold.
        let list = run_with(ENTRY, &Fake::refusing(by_relevance, 0.9, "next.md"))
            .await
            .expect("one page failing is not the end of the walk");

        assert_eq!(list.exit_code(), 1);
        assert_eq!(list.visited, 1);
        assert_eq!(list.results[0].path, ENTRY);
        assert_eq!(
            list.results
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec![ENTRY],
            "the page that failed is not in the list, and nothing reached past it"
        );
        assert_eq!(
            list.results[0].links,
            vec![RankedLink {
                target: "tests/fixtures/cli/next.md".to_string(),
                scent: Some(0.9),
                followed: true,
            }],
            "the entry file's own judgment is untouched by the page it points at"
        );
    }

    /// The same, where the walk reaches a page beyond the entry by another
    /// path: the result is a reading list that got past the entry files, so it
    /// exits 0, and only the page that failed is missing ([#37]).
    ///
    /// [#37]: https://github.com/mikekelly/s1m/issues/37
    #[tokio::test]
    async fn a_failed_judgment_on_a_reached_page_leaves_the_rest_of_the_walk() {
        // `broken.md` links on to `next.md`, and `next.md` is the only way to
        // `deep.md` — which is the page that fails. `broken.md` and `next.md`
        // are judged, so the run has a reading list and earns a 0.
        let list = run_with(BROKEN, &Fake::refusing(by_relevance, 0.9, "deep.md"))
            .await
            .expect("one page failing is not the end of the walk");

        assert_eq!(list.exit_code(), 0, "the walk reached beyond the entry");
        let paths: Vec<&str> = list.results.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["tests/fixtures/cli/next.md", BROKEN],
            "every page that was judged is in the list"
        );
        assert_eq!(
            list.results[0].via,
            vec![BROKEN.to_string()],
            "and the page that failed is not in the path that reached it"
        );
    }

    /// Nothing judged at all: an entry file whose judgment failed, with no
    /// other page reached, is the one judgment failure that is an error. There
    /// is no reading list to print, so a caller is told rather than handed an
    /// empty answer ([#37]).
    ///
    /// [#37]: https://github.com/mikekelly/s1m/issues/37
    #[tokio::test]
    async fn an_entry_file_that_cannot_be_judged_with_nothing_else_is_an_error() {
        let error = run_with(ENTRY, &Fake::refusing(by_relevance, 0.9, "entry.md"))
            .await
            .expect_err("there is no reading list");

        match error {
            Error::Judge { path, .. } => assert_eq!(path, "tests/fixtures/cli/entry.md"),
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
