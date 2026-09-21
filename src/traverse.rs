//! Best-first traversal of the link graph: the frontier, its budgets, and the
//! visited set.
//!
//! The entry files the caller named are visited first and are free: each is a
//! file the caller asked about, and `max_files` — which counts the files the
//! walk judges *beyond* them — has nothing to say about them. A run at 0 is the
//! entry files alone.
//!
//! Beyond them the walk is one frontier ordered by path score, across rounds
//! and not within one: nothing reorders the queue per round. A link is admitted
//! by the config's [`Admission`] rule — in the root, kept by the scorer or at or
//! above the threshold, within the depth budget — and is queued at the score of
//! the path it was found on, the product of the link scents from an entry file.
//! So a file's priority is the score of the best path found to it: long chains
//! of weak links sink, a file reached twice keeps its best path and is visited
//! once, and a link the rule admits is queued whatever its path score came out
//! as.
//!
//! A config may also make the walk a beam search: with `beam`, at most that many
//! files are visited at each depth, and a path the walk has no turn for is
//! dropped rather than expanded, so what it pursues at a depth is the best of
//! what it found there. Priority is the product of the shares either way, and a
//! beam is a budget of the same kind as `max_files`, one depth at a time: the
//! links that queued a path are still reported as followed, whatever became of
//! the path afterwards.
//!
//! Every entry file is one the caller named: it starts at path score 1, depth
//! 0, with no scent and no `via`, and nothing can reach a file at a better
//! score than that.
//!
//! The walk is async because the scorer is: a round scores up to `fanout` files
//! at once, so a round costs one round trip rather than one per file, and the
//! caller supplies the runtime. `fanout` is a concurrency cap and nothing else
//! — the reading list is the one a file-at-a-time walk would produce, because a
//! round is not a visit:
//!
//! - A file is recorded only while it is the best path the walk knows of. After
//!   each file of a round, a file the round's own answers overtook — one whose
//!   link was queued at a better score than this file was popped at — goes back
//!   on the frontier with the answer already bought for it, and is visited when
//!   it is the best path again. So no file is visited ahead of a better path
//!   the walk had already found, and no file is asked about twice.
//!
//! Determinism is a contract, not a property of the machine that ran it:
//!
//! - Ties on path score are broken by path, so the queue order is a function of
//!   the input alone.
//! - A round's answers are collected in the order the batch was popped and
//!   recorded in it, so the result does not depend on which answer arrives
//!   first.
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
//!
//! Nothing the root's `.s1mignore` matches is read: an entry file drops out of
//! the frontier, and a link whose target matches is out of the file before the
//! scorer sees it ([`Config::ignore`], [`crate::ignore`]).

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use futures_util::future::join_all;
use serde::Serialize;

use crate::ignore::Ignore;
use crate::parse::{ParseError, ParsedFile, parse, relative_to_root};
use crate::scorer::{FileJudgment, LinkJudgment, Scorer, ScorerError};

/// One traversal: the query, where to start, and the budgets that stop it.
///
/// Nothing here has a default. The plan's defaults (`max_files` 25,
/// `max_depth` 6, `threshold` 0.6) belong to the CLI flags that carry them;
/// `fanout` is the constant `main.rs` hands the walk, and `beam` is off except
/// where a caller asks for a beam search.
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
    /// Most files the walk judges beyond the entry files. The entry files the
    /// caller named are always visited and are free: this is what the walk may
    /// spend on the graph past them, so a run at 0 is the entry files alone.
    pub max_files: usize,
    /// Most link hops from an entry file. Entries are at depth 0, so at
    /// `max_depth` 0 only the entries are visited.
    pub max_depth: usize,
    /// Most files scored at once, at least one: a round size of none would be no
    /// walk at all. A round waits for the slowest of them, so this is a
    /// concurrency cap and not the reading list: a round of `fanout` files
    /// leaves the ones it overtook on the frontier for their turn, and a walk
    /// scores this many files rather than visiting them.
    pub fanout: usize,
    /// How a link earns its place on the frontier.
    pub admission: Admission,
    /// Most files the walk visits at one depth, or `None` for no ceiling: the
    /// walk as a beam search, which is how the relative-scent spike follows
    /// shares ([#47], where priority is the product of shares and a depth's
    /// worst paths are dropped rather than expanded).
    ///
    /// A budget of the same kind as [`Config::max_files`], one depth at a time:
    /// a path the beam has no turn for is taken off the frontier and never
    /// visited, so what the walk expands at a depth is the best `beam` paths it
    /// found there. What is visited is a function of the walk's own decisions
    /// and not of the round size, so a beam does not cost the walk its
    /// determinism.
    ///
    /// The entry files are not on the frontier — they are visited as the
    /// entries they were named as — so a beam never starves a walk of what the
    /// caller named, whatever its size.
    ///
    /// [#47]: https://github.com/mikekelly/s1m/issues/47
    pub beam: Option<usize>,
    /// The root's `.s1mignore` patterns ([`crate::ignore`]), applied to every
    /// path the walk could read.
    ///
    /// An entry file that matches is dropped before it is parsed, and a
    /// link whose target matches is taken out of the file before it is scored —
    /// so a matched target is never read, never previewed, and never in a
    /// request. The CLI turns the entry-file case into an error rather than a
    /// silent drop ([`crate::cli::Error::Ignored`]); the drop is here so that no
    /// caller of the walk can read a matched path by passing one.
    pub ignore: &'a Ignore,
}

/// How a file's links earn their place on the frontier.
///
/// A Noul and a Choice share are both 0 to 1 and are not the same kind of
/// number: 0.6 on a Noul is "likely useful", and 0.6 on a Choice share is a page
/// of two links all but decided. So the rule that reads them lives beside the
/// scorer that gives them, and the walk is told which one it is walking under.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Admission {
    /// The link's own scent, at or above the threshold: the rule for a scorer
    /// whose number is on a scale of its own, where 0.5 means unsure and the
    /// caller's floor sits above it.
    Threshold(f64),
    /// Whatever the scorer kept ([`crate::scorer::LinkJudgment::keep`]),
    /// whatever number is beside it: the rule for a number that means something
    /// only beside the options it was weighed against.
    Scorer,
}

impl Admission {
    /// Whether one link's judgment earns it a place on the frontier: its scent
    /// against the caller's floor, or the scorer's own verdict.
    ///
    /// A scent outside 0 to 1 is not admitted by either rule, and is not the
    /// caller's to admit: path scores are products of scents and may never rise
    /// along a path, which is what lets a file that has been popped be settled
    /// rather than re-opened. A scorer that answered outside that range is not
    /// followed, whatever it says about keeping the link.
    fn admits(self, scent: f64, keep: bool) -> bool {
        if !(0.0..=1.0).contains(&scent) {
            return false;
        }
        match self {
            Admission::Threshold(threshold) => scent >= threshold,
            Admission::Scorer => keep,
        }
    }
}

/// What one traversal found.
#[derive(Debug)]
pub struct Traversal {
    /// The visited files, most relevant first, ties broken by path.
    pub results: Vec<VisitedFile>,
    /// Scorer calls made. A file that failed to parse cost none; a call that
    /// failed still counts. A round buys its files together: a file it bought
    /// and the budget then stopped short of counts too.
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
    /// This file's heading sections, in the order the file has them, each with
    /// the lines the parser gave it and the score the scorer gave it.
    ///
    /// In document order, not by score: the judgment pairs with the file the
    /// way [`FileJudgment::sections`] does, and the reading list is what ranks
    /// them.
    pub sections: Vec<JudgedSection>,
    /// This file's outgoing links, in the order they appear, one per target,
    /// each with the scent it was judged at.
    pub links: Vec<JudgedLink>,
}

impl VisitedFile {
    /// Whether this file earns a place in the reading list on its own: its
    /// relevance is at or above `threshold`, or one of its sections is.
    ///
    /// The walk returns every file it visited — a hub is worth walking through
    /// whatever it is worth reading — and this is the one question the reading
    /// list asks of a visited file. A file that fails it is still reported, as
    /// a file the walk visited rather than one to read ([`crate::cli`]), so the
    /// tree and the link path that reached it survive.
    ///
    /// The CLI and the evaluation harness both ask it here, so neither can
    /// measure a rule the other does not ship.
    pub fn earns_a_place(&self, threshold: f64) -> bool {
        self.relevance >= threshold
            || self
                .sections
                .iter()
                .any(|section| section.score >= threshold)
    }
}

/// One heading section of a visited file, as it was judged.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgedSection {
    /// Heading text, `None` for content before the first heading.
    pub heading: Option<String>,
    /// `[first, last]` line, inclusive, exactly as
    /// [`crate::parse::Section::lines`] gave it: the range that was judged, not
    /// one derived from the score.
    pub lines: [usize; 2],
    /// The score the scorer gave it, 0 to 1.
    pub score: f64,
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
    /// Whether the scorer kept it ([`crate::scorer::LinkJudgment::keep`]): what
    /// [`Admission::Scorer`] reads, and `false` for a link the scorer named no
    /// judgment for.
    pub keep: bool,
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
/// Visits every entry file, then walks on: the walk ends when `max_files` files
/// beyond the entries are judged, or the frontier is empty, whichever comes
/// first. A file that cannot be parsed or scored is recorded in
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

/// What the walk holds for a file: its parse and its judgment, or why it could
/// not be had.
type Answer = Result<(ParsedFile, FileJudgment), Failure>;

/// One walk's state.
struct Search<'a> {
    config: &'a Config<'a>,
    scorer: &'a dyn Scorer,
    frontier: BinaryHeap<Frontier>,
    /// Best path found for each file, so a worse path never queues and a file
    /// keeps the best path it was reached by.
    best: HashMap<PathBuf, Frontier>,
    /// Files already dealt with — visited, or failed and not retried. The entry
    /// files are here from the start: they are dealt with as the entries they
    /// were named as, whatever reaches them, and the budget has nothing to say
    /// about them.
    settled: HashSet<PathBuf>,
    /// Answers a round bought and did not use, by path: a file the round's own
    /// answers overtook waits on the frontier with its answer here, so the walk
    /// asks about a file once whatever the round size.
    held: HashMap<PathBuf, Answer>,
    results: Vec<VisitedFile>,
    failed: Vec<FailedFile>,
    /// Files judged beyond the entry files: what `max_files` bounds.
    spent: usize,
    /// Files judged at each depth: what `beam` bounds, and the reason a beam is
    /// not a property of the round size — the depth a file is judged at is the
    /// depth its best path reached it at, and the walk's decisions do not depend
    /// on how many files a round scores at once ([#47]).
    ///
    /// [#47]: https://github.com/mikekelly/s1m/issues/47
    depths: HashMap<usize, usize>,
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
            held: HashMap::new(),
            results: Vec::new(),
            failed: Vec::new(),
            spent: 0,
            depths: HashMap::new(),
            calls: 0,
        }
    }

    async fn run(mut self) -> Result<Traversal, TraverseError> {
        let entries = self.start()?;
        // The files the caller named are visited first and are free: `max_files`
        // is what the walk spends beyond them, so a run at 0 is the entry files
        // alone and still visits every one of them. Scoring them a round at a
        // time keeps the round trip at one whatever the entry set is.
        for batch in entries.chunks(self.config.fanout.max(1)) {
            let answers = self.answers(batch).await;
            for (entry, answer) in batch.iter().cloned().zip(answers) {
                self.record(entry, answer);
            }
        }

        while self.spent < self.config.max_files {
            let batch = self.next_batch();
            if batch.is_empty() {
                break;
            }
            let answers = self.answers(&batch).await;
            for (entry, answer) in batch.into_iter().zip(answers) {
                // A round is not a visit. The answers of this one can reach a
                // file that outranks the rest of it — the batch was popped
                // before they existed — and that file gets its turn first: the
                // rest goes back on the frontier with the answer just bought
                // for it, rather than being visited ahead of a better path the
                // walk already has.
                if self.overtaken(&entry) {
                    self.hold(entry, answer);
                    continue;
                }
                self.record(entry, answer);
            }
        }

        // A round buys its files together, so the walk can end with an answer it
        // never used: a better path took the file's place and the budget then
        // ran out. One that answered is what the walk bought and did not spend;
        // one that failed is the hole in the ranking it would have been had the
        // file had its turn, and is reported the same way.
        for (path, answer) in std::mem::take(&mut self.held) {
            if let Err(failure) = answer {
                self.failed.push(FailedFile { path, failure });
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

    /// The files the caller named, at path score 1 and ready to visit.
    ///
    /// Each is marked dealt with here, before anything is read of it: no link
    /// can reach an entry file as anything but the entry it was named as, and
    /// the budget is not spent on one. An entry file the root's `.s1mignore`
    /// matches never reaches the round, so nothing reads it, and one named twice
    /// — under two spellings, or twice the same — is visited once.
    ///
    /// The batch order is the path's, which is the order the frontier's tie
    /// break gave them when they were queued together, so the reading list does
    /// not depend on the order the caller named them in.
    fn start(&mut self) -> Result<Vec<Frontier>, TraverseError> {
        let mut entries = Vec::new();
        for path in self.config.entries {
            if path.is_absolute() != self.config.root.is_absolute() {
                return Err(TraverseError::BaseMismatch {
                    path: path.clone(),
                    root: self.config.root.to_path_buf(),
                });
            }
            let path = relative_to_root(self.config.root, path);
            if self.config.ignore.matched(&path) {
                continue;
            }
            if !self.settled.insert(path.clone()) {
                continue;
            }
            entries.push(Frontier {
                path,
                score: 1.0,
                depth: 0,
                scent: None,
                via: Vec::new(),
            });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        for entry in &entries {
            self.best.insert(entry.path.clone(), entry.clone());
        }
        Ok(entries)
    }

    /// One round's work: up to `fanout` files, never more than the budget
    /// leaves room for.
    ///
    /// A file that has been dealt with, or that a better path has since
    /// overtaken, is dropped rather than scored, and does not use up a place in
    /// the round.
    fn next_batch(&mut self) -> Vec<Frontier> {
        let mut batch = Vec::new();
        while batch.len() < self.config.fanout && self.spent + batch.len() < self.config.max_files {
            let Some(entry) = self.next_pending(&batch) else {
                break;
            };
            batch.push(entry);
        }
        batch
    }

    /// The frontier's best file, taken off the heap: the ones the walk has
    /// already dealt with are thrown away as they are met, and so are the ones
    /// the beam has no turn left for.
    ///
    /// A path the beam drops is dropped for good rather than put back: the walk
    /// has visited all it will at that depth, and a path no turn will come for
    /// is not something to hold. Keeping it in `best` is what the walk already
    /// does with the paths it has taken — a file is visited by the best path
    /// found to it, once — so a later path to the same file is judged against
    /// the one the beam dropped, not against nothing.
    fn next_pending(&mut self, batch: &[Frontier]) -> Option<Frontier> {
        loop {
            let entry = self.frontier.pop()?;
            if self.dealt_with(&entry) || self.past_beam(entry.depth, batch) {
                continue;
            }
            return Some(entry);
        }
    }

    /// Whether the walk has taken as many files at this depth as its beam
    /// allows: the ones it has visited, and the ones this round has popped and
    /// not yet recorded. `None` is no beam at all, which is the walk that
    /// ships.
    ///
    /// The round's own batch counts because it is popped together: a file the
    /// batch holds is a file the walk is about to visit, and a beam that did
    /// not count it would take a whole round's worth of paths at a depth.
    fn past_beam(&self, depth: usize, batch: &[Frontier]) -> bool {
        self.config.beam.is_some_and(|beam| {
            let taken = self.depths.get(&depth).copied().unwrap_or(0)
                + batch.iter().filter(|entry| entry.depth == depth).count();
            taken >= beam
        })
    }

    /// Whether the walk has dealt with a frontier entry: its file is visited or
    /// failed, or a better path to it has been queued since the entry was
    /// pushed.
    fn dealt_with(&self, entry: &Frontier) -> bool {
        self.settled.contains(&entry.path)
            || self
                .best
                .get(&entry.path)
                .is_some_and(|best| best.score > entry.score)
    }

    /// Whether the frontier holds a better path than this file's, throwing away
    /// the entries the walk has dealt with on the way. What a round's own
    /// answers can do to the rest of the round, and the reason a round is not a
    /// visit.
    fn overtaken(&mut self, entry: &Frontier) -> bool {
        while self.frontier.peek().is_some_and(|top| self.dealt_with(top)) {
            self.frontier.pop();
        }
        self.frontier.peek().is_some_and(|top| top > entry)
    }

    /// Puts a scored file back on the frontier, keeping the answer: the walk has
    /// bought that judgment, so the file waits for its turn rather than being
    /// asked about twice. An answer still held when the walk ends was bought and
    /// never spent: [`Search::run`] reports the ones that failed and drops the
    /// rest.
    fn hold(&mut self, entry: Frontier, answer: Answer) {
        self.held.insert(entry.path.clone(), answer);
        self.frontier.push(entry);
    }

    /// The answers for one round's files, in the order the batch was popped.
    ///
    /// A file a round overtook is already answered, and that answer is taken
    /// here: only the files nobody has answered are scored, together, and the
    /// answers are put back in batch order, so the order a round committed in
    /// is the order it popped in whatever order the scorer answered in.
    async fn answers(&mut self, batch: &[Frontier]) -> Vec<Answer> {
        let mut answers: Vec<Option<Answer>> = batch
            .iter()
            .map(|entry| self.held.remove(&entry.path))
            .collect();
        let fresh: Vec<Frontier> = batch
            .iter()
            .zip(&answers)
            .filter(|(_, answer)| answer.is_none())
            .map(|(entry, _)| entry.clone())
            .collect();
        let scored = self.score(&fresh).await;
        // A file that failed to parse never reached the scorer, so it was no
        // call; a call that failed still was one.
        self.calls += scored
            .iter()
            .filter(|answer| !matches!(answer.as_ref().err(), Some(Failure::Parse(_))))
            .count();

        let mut scored = scored.into_iter();
        for answer in &mut answers {
            if answer.is_none() {
                *answer = Some(
                    scored
                        .next()
                        .expect("every file that was asked about has an answer"),
                );
            }
        }
        answers
            .into_iter()
            .map(|answer| answer.expect("every file of a round has an answer"))
            .collect()
    }

    /// Scores one round: one future per file, polled together, so a round costs
    /// one round trip rather than one per file. Answers come back in the order
    /// the batch was popped, whatever order they are ready in.
    ///
    /// A link whose target the root's `.s1mignore` matches is taken out of the
    /// file here, before the scorer sees it: the target is not read for a
    /// preview, its path is not in the request, and the judgment has no question
    /// to answer about it. What comes back is the file's own links minus those,
    /// which is also what the reading list reports.
    async fn score(&self, batch: &[Frontier]) -> Vec<Answer> {
        let root = self.config.root;
        let query = self.config.query;
        let ignore = self.config.ignore;
        let scorer = self.scorer;
        join_all(batch.iter().map(|entry| async move {
            let mut file = parse(root.join(&entry.path), root)?;
            file.links.retain(|link| !ignore.matched(&link.target));
            let judgment = scorer.score(query, &file).await?;
            Ok((file, judgment))
        }))
        .await
    }

    /// Records one answered file and queues its links.
    fn record(&mut self, entry: Frontier, outcome: Answer) {
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
                // A file the walk popped and could not judge still spends the
                // turn the beam reserved for it at this depth. That is what
                // makes the turn a function of the pop order and nothing else:
                // the reservation [`Search::past_beam`] makes for a round is
                // realized here whether or not the file could be judged, so the
                // paths a depth drops do not depend on how many files a round
                // happened to hold. `spent` is not charged for it — the file
                // budget counts the files the walk judged, and this is not one.
                if depth > 0 {
                    *self.depths.entry(depth).or_default() += 1;
                }
                self.failed.push(FailedFile { path, failure });
                return;
            }
        };
        // The budget is the files the walk judges beyond the entry files, so a
        // file that could not be judged cost it nothing; an entry file is not
        // the budget's business at all. A beam is the same kind of budget one
        // depth at a time, and counts the files the walk visited there.
        if depth > 0 {
            self.spent += 1;
            *self.depths.entry(depth).or_default() += 1;
        }

        let mut links = judged_links(&file, &judgment);
        for link in &mut links {
            // A link the scorer named no scent for, one it did not keep, a
            // scent that is not a probability, one below the threshold, one out
            // of the root or one past the depth budget all queue nothing.
            let Some(link_scent) = link.scent else {
                continue;
            };
            if !link.in_root
                || !self.config.admission.admits(link_scent, link.keep)
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
            sections: judged_sections(&file, &judgment),
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

/// A visited file's heading sections in the order the file has them: the
/// parser's heading and lines, paired by index with the score the scorer gave
/// that section.
///
/// The range is the parser's and never the judgment's, for the reason the links
/// above are the file's: a range the scorer named would be a range the parser
/// never produced, and what the caller reads has to be the text that was
/// judged. Only sections the scorer answered for are reported.
fn judged_sections(file: &ParsedFile, judgment: &FileJudgment) -> Vec<JudgedSection> {
    file.sections
        .iter()
        .zip(&judgment.sections)
        .map(|(section, judged)| JudgedSection {
            heading: section.heading.clone(),
            lines: section.lines,
            score: judged.score,
        })
        .collect()
}

/// A visited file's outgoing links in the order they appear in the file, one
/// entry per target, each with the scent the scorer gave it.
///
/// Only targets the file actually links to are reported, and a target the
/// scorer named no link to keeps `scent: None` and `keep: false`. A scorer
/// cannot add a link that is not in the file, so it cannot send the walk
/// somewhere the file does not point.
fn judged_links(file: &ParsedFile, judgment: &FileJudgment) -> Vec<JudgedLink> {
    let mut judged: HashMap<&Path, &LinkJudgment> = HashMap::with_capacity(judgment.links.len());
    for link in &judgment.links {
        // A target named twice keeps the judgment it was first given.
        judged.entry(link.target.as_path()).or_insert(link);
    }

    let mut seen: HashSet<&Path> = HashSet::with_capacity(file.links.len());
    let mut links = Vec::with_capacity(file.links.len());
    for link in &file.links {
        // A file that links twice to one target judges it once.
        if !seen.insert(link.target.as_path()) {
            continue;
        }
        let judged = judged.get(link.target.as_path());
        links.push(JudgedLink {
            target: link.target.clone(),
            scent: judged.map(|judged| judged.scent),
            keep: judged.is_some_and(|judged| judged.keep),
            in_root: link.in_root,
            followed: false,
        });
    }
    links
}
