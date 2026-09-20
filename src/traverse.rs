//! Best-first traversal of the link graph: the frontier, its budgets, and the
//! visited set.
//!
//! Entry files start on a frontier at path score 1. Each round pops the best
//! `fanout` files, scores them together, and queues the links that clear the
//! threshold. A link's scent multiplies into the path score, so an entry's
//! priority is the score of the best path found to it: long chains of weak
//! links sink, and a file reached twice keeps its best path and is visited
//! once.
//!
//! The walk is async because the scorer is: a round joins one future per file,
//! so a round costs one round trip rather than one per file, and the caller
//! supplies the runtime.
//!
//! Determinism is a contract, not a property of the machine that ran it:
//!
//! - Ties on path score are broken by path, so the queue order is a function of
//!   the input alone.
//! - A round's answers are collected in the order the batch was popped, so the
//!   result does not depend on which answer arrives first.
//! - Nothing is pruned by a file's own relevance: an unhelpful index page still
//!   passes its links on.
//!
//! Scents are probabilities, so a path score never rises along a path. That is
//! what makes the first pop of a file its best path, and what lets a file that
//! has been visited stay settled rather than be re-opened. A scent outside 0 to
//! 1 is not followed: scoring junk is not allowed to break that ordering.
//!
//! Every path in the result — `path`, `via`, link targets — is spelled the way
//! [`parse`] spells link targets: normalised and relative to the root, so
//! `config.root.join(path)` is the file to read.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use futures_util::future::join_all;
use serde::Serialize;

use crate::parse::{ParseError, ParsedFile, parse, relative_to_root};
use crate::scorer::{FileJudgment, Scorer, ScorerError};

/// One traversal: the query, where to start, and the budgets that stop it.
///
/// Nothing here has a default. The plan's defaults (`max_files` 25,
/// `max_depth` 6, `threshold` 0.6, `fanout` 8) belong to the CLI flags that
/// carry them.
#[derive(Debug, Clone)]
pub struct Config<'a> {
    /// The query, passed unchanged to every [`Scorer::score`] call.
    pub query: &'a str,
    /// Entry files, given against the same base as `root` the way [`parse`]
    /// wants them. Each is visited with path score 1 and depth 0.
    pub entries: &'a [PathBuf],
    /// The directory that bounds the walk. A link resolving outside it is
    /// reported and never followed.
    pub root: &'a Path,
    /// Most files visited before the walk stops.
    pub max_files: usize,
    /// Most link hops from an entry file. Entries are at depth 0, so at
    /// `max_depth` 0 only the entries are visited.
    pub max_depth: usize,
    /// Most files scored in one round; the round waits for the slowest of them.
    pub fanout: usize,
    /// Least link scent that queues a target. A Noul near 0.5 means uncertain,
    /// so this is meant to sit above it.
    pub threshold: f64,
}

/// What one traversal found.
#[derive(Debug)]
pub struct Traversal {
    /// The visited files, most relevant first, ties broken by path.
    pub results: Vec<VisitedFile>,
    /// Scorer calls made. A file that failed to parse cost none; a call that
    /// failed still counts.
    pub calls: usize,
    /// Files the walk reached but could not score, by path. These do not count
    /// against `max_files` and are not retried.
    pub failed: Vec<FailedFile>,
}

/// One file the walk visited.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisitedFile {
    /// The file, relative to the root.
    pub path: PathBuf,
    /// The scorer's relevance for this file.
    pub relevance: f64,
    /// The scent of the link that reached this file; `None` for an entry file,
    /// which no link reached.
    pub scent: Option<f64>,
    /// The product of the link scents on the best path to this file; 1 for an
    /// entry file.
    pub path_score: f64,
    /// Link hops from the entry file that reached it.
    pub depth: usize,
    /// The files on the best path, in order, excluding this one; empty for an
    /// entry file.
    pub via: Vec<PathBuf>,
    /// This file's outgoing links, in the order they appear, one per target,
    /// each with the scent it was judged at.
    pub links: Vec<JudgedLink>,
}

/// One outgoing link of a visited file.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgedLink {
    /// The link's target, relative to the root.
    pub target: PathBuf,
    /// The scent the scorer gave it, `None` when the scorer named no link to
    /// this target.
    pub scent: Option<f64>,
    /// Whether the target is inside the root. A link outside it is never
    /// followed, whatever its scent.
    pub in_root: bool,
    /// Whether this link queued its target: inside the root, at or above the
    /// threshold, within the depth budget, and ahead of any better path already
    /// found to that target.
    pub followed: bool,
}

/// A file the walk reached but could not score.
#[derive(Debug)]
pub struct FailedFile {
    /// The file, relative to the root.
    pub path: PathBuf,
    pub failure: Failure,
}

/// Why a reached file could not be scored.
#[derive(Debug, thiserror::Error)]
pub enum Failure {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Score(#[from] ScorerError),
}

/// Why a traversal could not run at all. One file failing is not on this list:
/// it is reported in [`Traversal::failed`].
#[derive(Debug, thiserror::Error)]
pub enum TraverseError {
    /// An entry file and the root must be given against the same base — both
    /// relative to the working directory, or both absolute.
    #[error("{path} and {root} must both be relative or both absolute")]
    BaseMismatch { path: PathBuf, root: PathBuf },
}

/// Walks the link graph from the config's entry files.
///
/// Ends when `max_files` files are visited or the frontier is empty, whichever
/// comes first. A file that cannot be parsed or scored is recorded in
/// [`Traversal::failed`] and the walk continues, so one broken link does not
/// cost the reading list.
pub async fn traverse(
    config: &Config<'_>,
    scorer: &dyn Scorer,
) -> Result<Traversal, TraverseError> {
    Search::new(config, scorer).run().await
}

/// The frontier: a file to visit, with the best path found to it so far.
#[derive(Debug, Clone)]
struct Frontier {
    path: PathBuf,
    score: f64,
    depth: usize,
    scent: Option<f64>,
    via: Vec<PathBuf>,
}

/// A max-heap on path score, ties broken by the smaller path, so what is popped
/// never depends on the order things were pushed.
impl Ord for Frontier {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| other.path.cmp(&self.path))
    }
}

impl PartialOrd for Frontier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Frontier {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Frontier {}

/// One walk's state.
struct Search<'a> {
    config: &'a Config<'a>,
    scorer: &'a dyn Scorer,
    frontier: BinaryHeap<Frontier>,
    /// Best path found for each file, so a worse path never queues and a file
    /// keeps the best path it was reached by.
    best: HashMap<PathBuf, Frontier>,
    /// Files already dealt with — visited, or failed and not retried.
    settled: HashSet<PathBuf>,
    results: Vec<VisitedFile>,
    failed: Vec<FailedFile>,
    calls: usize,
}

impl<'a> Search<'a> {
    fn new(config: &'a Config<'a>, scorer: &'a dyn Scorer) -> Self {
        Self {
            config,
            scorer,
            frontier: BinaryHeap::new(),
            best: HashMap::new(),
            settled: HashSet::new(),
            results: Vec::new(),
            failed: Vec::new(),
            calls: 0,
        }
    }

    async fn run(mut self) -> Result<Traversal, TraverseError> {
        self.seed()?;
        while self.results.len() < self.config.max_files {
            let batch = self.next_batch();
            if batch.is_empty() {
                break;
            }
            let outcomes = self.score(&batch).await;
            // A file that failed to parse never reached the scorer, so it was
            // no call; a call that failed still was one.
            self.calls += outcomes
                .iter()
                .filter(|outcome| !matches!(outcome.as_ref().err(), Some(Failure::Parse(_))))
                .count();
            for (entry, outcome) in batch.into_iter().zip(outcomes) {
                self.record(entry, outcome);
            }
        }

        self.results.sort_by(by_relevance);
        self.failed.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Traversal {
            results: self.results,
            calls: self.calls,
            failed: self.failed,
        })
    }

    /// Puts the entry files on the frontier at path score 1.
    fn seed(&mut self) -> Result<(), TraverseError> {
        for entry in self.config.entries {
            if entry.is_absolute() != self.config.root.is_absolute() {
                return Err(TraverseError::BaseMismatch {
                    path: entry.clone(),
                    root: self.config.root.to_path_buf(),
                });
            }
            self.enqueue(Frontier {
                path: relative_to_root(self.config.root, entry),
                score: 1.0,
                depth: 0,
                scent: None,
                via: Vec::new(),
            });
        }
        Ok(())
    }

    /// One round's work: up to `fanout` files, never more than the budget
    /// leaves room for.
    ///
    /// An entry that a better path has since overtaken, or whose file is
    /// already dealt with, is dropped rather than scored, and does not use up a
    /// place in the round.
    fn next_batch(&mut self) -> Vec<Frontier> {
        let mut batch = Vec::new();
        while batch.len() < self.config.fanout
            && self.results.len() + batch.len() < self.config.max_files
        {
            let Some(entry) = self.frontier.pop() else {
                break;
            };
            if self.settled.contains(&entry.path) {
                continue;
            }
            if self
                .best
                .get(&entry.path)
                .is_some_and(|best| best.score > entry.score)
            {
                continue;
            }
            batch.push(entry);
        }
        batch
    }

    /// Scores one round: one future per file, polled together, so a round costs
    /// one round trip rather than one per file. Answers come back in the order
    /// the batch was popped, whatever order they are ready in.
    async fn score(&self, batch: &[Frontier]) -> Vec<Result<(ParsedFile, FileJudgment), Failure>> {
        let root = self.config.root;
        let query = self.config.query;
        let scorer = self.scorer;
        join_all(batch.iter().map(|entry| async move {
            let file = parse(root.join(&entry.path), root)?;
            let judgment = scorer.score(query, &file).await?;
            Ok((file, judgment))
        }))
        .await
    }

    /// Records one answered file and queues its links.
    fn record(&mut self, entry: Frontier, outcome: Result<(ParsedFile, FileJudgment), Failure>) {
        self.settled.insert(entry.path.clone());
        // The record and the expansion use the best path found to the file, not
        // the one it was popped at: an earlier file of this round can have
        // queued a better path to it while the round was being recorded.
        let Frontier {
            path,
            score,
            depth,
            scent,
            via,
        } = self.best.remove(&entry.path).unwrap_or(entry);
        let (file, judgment) = match outcome {
            Ok(answered) => answered,
            Err(failure) => {
                self.failed.push(FailedFile { path, failure });
                return;
            }
        };

        let mut links = judged_links(&file, &judgment);
        for link in &mut links {
            // A link the scorer named no scent for, a scent that is not a
            // probability, one below the threshold, one out of the root or one
            // past the depth budget all queue nothing.
            let Some(link_scent) = link.scent else {
                continue;
            };
            if !link.in_root
                || !(0.0..=1.0).contains(&link_scent)
                || link_scent < self.config.threshold
                || depth + 1 > self.config.max_depth
            {
                continue;
            }
            let mut child_via = via.clone();
            child_via.push(path.clone());
            link.followed = self.enqueue(Frontier {
                path: link.target.clone(),
                score: score * link_scent,
                depth: depth + 1,
                scent: Some(link_scent),
                via: child_via,
            });
        }

        self.results.push(VisitedFile {
            path,
            relevance: judgment.relevance,
            scent,
            path_score: score,
            depth,
            via,
            links,
        });
    }

    /// Queues a file unless it is already dealt with or a path at least as good
    /// has been found, and reports whether it queued. The first path to reach a
    /// file at its best score is the one kept.
    fn enqueue(&mut self, entry: Frontier) -> bool {
        if self.settled.contains(&entry.path) {
            return false;
        }
        if self
            .best
            .get(&entry.path)
            .is_some_and(|best| best.score >= entry.score)
        {
            return false;
        }
        self.best.insert(entry.path.clone(), entry.clone());
        self.frontier.push(entry);
        true
    }
}

/// Most relevant first, ties broken by path, so the reading list never depends
/// on the order the walk happened to visit in.
fn by_relevance(a: &VisitedFile, b: &VisitedFile) -> Ordering {
    b.relevance
        .total_cmp(&a.relevance)
        .then_with(|| a.path.cmp(&b.path))
}

/// A visited file's outgoing links in the order they appear in the file, one
/// entry per target, each with the scent the scorer gave it.
///
/// Only targets the file actually links to are reported, and a target the
/// scorer named no link to keeps `scent: None`. A scorer cannot add a link that
/// is not in the file, so it cannot send the walk somewhere the file does not
/// point.
fn judged_links(file: &ParsedFile, judgment: &FileJudgment) -> Vec<JudgedLink> {
    let mut judged: HashMap<&Path, f64> = HashMap::with_capacity(judgment.links.len());
    for link in &judgment.links {
        // A target named twice keeps the scent it was first judged at.
        judged.entry(link.target.as_path()).or_insert(link.scent);
    }

    let mut seen: HashSet<&Path> = HashSet::with_capacity(file.links.len());
    let mut links = Vec::with_capacity(file.links.len());
    for link in &file.links {
        // A file that links twice to one target judges it once.
        if !seen.insert(link.target.as_path()) {
            continue;
        }
        links.push(JudgedLink {
            target: link.target.clone(),
            scent: judged.get(link.target.as_path()).copied(),
            in_root: link.in_root,
            followed: false,
        });
    }
    links
}
