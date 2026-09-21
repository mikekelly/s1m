//! The run's trace: one JSON Lines record per event of a walk, each stamped
//! with the milliseconds since the walk started
//! ([#57](https://github.com/mikekelly/s1m/issues/57)).
//!
//! What a trace is for is replay. A reading list says what the walk found; a
//! trace says what it did, in the order it did it, so a player can animate the
//! crawl and the list filling in. Every record is written as its event
//! happens, and nothing is held back beyond the line being written, so a run
//! that is killed leaves a trace of everything up to the kill — and a record is
//! a whole line or no line at all: a write that fails part way through is rolled
//! back to the last whole line, and there the trace ends.
//!
//! Off is off. No record is built and no call site does anything but ask
//! whether a trace was asked for; a record never changes what the walk decides;
//! and nothing here goes into a cache key, so a traced run and an untraced one
//! make the same requests and return the same reading list.
//!
//! Paths are relative to the root the walk is bounded by — the spelling the
//! walk works in, and the one `via` and link targets already use. The walk's
//! own records report a path as it is; [`Trace::requested`] and
//! [`Trace::answered`] take a path from a scorer, which is the root joined on,
//! and spell it that way, so one file has one name whatever reported it.
//!
//! [#57]: https://github.com/mikekelly/s1m/issues/57

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::parse::relative_to_root;
use crate::scorer::{FileJudgment, LinkJudgment, SectionJudgment};

/// One run's trace: the file the records go to, the root every path is spelled
/// against, and the instant `t_ms` is measured from.
///
/// The trace is also told the reading list's cutoff, because a `result` record
/// says whether the file earned a place on it. That is the list's rule rather
/// than the walk's — the walk itself has no use for the number — which is why
/// it is handed to the trace and not to [`crate::traverse::Config`].
pub struct Trace {
    out: Mutex<Out>,
    /// The run's start: what `t_ms` counts from.
    start: Instant,
    root: PathBuf,
    /// Least relevance or section score that earns a file a place in the
    /// reading list, as [`crate::traverse::VisitedFile::earns_a_place`] reads
    /// it.
    threshold: f64,
    /// Whether a write has already been reported: one line on stderr per run,
    /// however many records it fails on.
    warned: AtomicBool,
}

/// The writer: the file, the one line at a time that is being written, and how
/// much of the file is whole lines.
///
/// The buffer is kept across records rather than allocated per record, and a
/// record is serialized into it whole and written with one `write_all`. `written`
/// is the length of the file at the last whole line, which is what a write that
/// fails part way through is rolled back to: what is left on disk is a shorter
/// trace and never a fragment for a reader to trip over.
struct Out {
    file: File,
    line: Vec<u8>,
    written: u64,
}

/// The root and the cutoff, and not the file or the line being written:
/// [`crate::traverse::Config`] is `Debug`, and a failure message about a walk
/// should say which trace it had rather than print its last record.
impl std::fmt::Debug for Trace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Trace")
            .field("root", &self.root)
            .field("threshold", &self.threshold)
            .finish_non_exhaustive()
    }
}

impl Trace {
    /// The trace of a run over `root`, written to `path`, which is created or
    /// truncated.
    ///
    /// A file that cannot be created is the caller's mistake and the run's
    /// error: `--trace` named a path, and a run whose trace is not the trace
    /// the caller asked for has no business starting.
    pub fn create(path: &Path, root: &Path, threshold: f64) -> io::Result<Trace> {
        Ok(Trace {
            out: Mutex::new(Out {
                // `create` truncates, so nothing is written before the first
                // record and the whole-line length starts at zero.
                file: File::create(path)?,
                line: Vec::new(),
                written: 0,
            }),
            start: Instant::now(),
            root: root.to_path_buf(),
            threshold,
            warned: AtomicBool::new(false),
        })
    }

    /// Least relevance or section score that earns a file a place in the
    /// reading list: what a `result` record reports as `earned_a_place`.
    pub fn threshold(&self) -> f64 {
        self.threshold
    }

    /// A file the walk took off the frontier, entries and all: the entry files
    /// the caller named are popped first, at path score 1, depth 0 and no
    /// `via`.
    ///
    /// A path can be popped more than once. A round's own answers can overtake a
    /// file the round popped — a better path to it arrived while the round was
    /// being scored — and the walk puts it back on the frontier with the answer
    /// it has already bought, so its turn comes again. Both pops are in the
    /// trace: the first is where the request and the answer for that file are,
    /// the last is the one the visit follows.
    pub fn popped(&self, path: &Path, path_score: f64, depth: usize, via: &[PathBuf]) {
        self.record(Event::Popped {
            t_ms: self.now(),
            path,
            path_score,
            depth,
            via,
        });
    }

    /// One request made for a file, once per post: a page whose sections and
    /// links did not fit the API's state budget in one, and a page over the
    /// content cap that was split by its heading tree, are asked about in
    /// several, and each is its own record, in the order they were sent.
    ///
    /// A file whose answer was on disk is reported the same way, and its
    /// `answered` says `cached`.
    pub fn requested(&self, path: &Path, posts: usize) {
        let path = relative_to_root(&self.root, path);
        for post_index in 0..posts {
            self.record(Event::Requested {
                t_ms: self.now(),
                path: &path,
                post_index,
            });
        }
    }

    /// The answer for one file: what it was judged, how long the call took, and
    /// whether it was bought now or served from the cache.
    ///
    /// A cached answer's `latency_ms` is what the call that stored it took, not
    /// nothing: the two runs are told apart by `cached` and by the reading
    /// list's `calls`, not by a latency of zero.
    pub fn answered(&self, path: &Path, latency: Duration, cached: bool, judgment: &FileJudgment) {
        let path = relative_to_root(&self.root, path);
        self.record(Event::Answered {
            t_ms: self.now(),
            path: &path,
            latency_ms: latency.as_millis() as u64,
            relevance: judgment.relevance,
            cached,
            sections: &judgment.sections,
            links: &judgment.links,
        });
    }

    /// One link the walk queued, from the file that made it.
    pub fn admitted(&self, source: &Path, target: &Path, scent: f64) {
        self.record(Event::Admitted {
            t_ms: self.now(),
            source,
            target,
            scent,
        });
    }

    /// One link the walk passed over, or a queued path it dropped, and why.
    pub fn pruned(&self, source: &Path, target: &Path, scent: Option<f64>, reason: Reason) {
        self.record(Event::Pruned {
            t_ms: self.now(),
            source,
            target,
            scent,
            reason,
        });
    }

    /// One file the walk visited and judged, and whether it earned a place in
    /// the reading list.
    pub fn result(&self, path: &Path, relevance: f64, earned_a_place: bool) {
        self.record(Event::Result {
            t_ms: self.now(),
            path,
            relevance,
            earned_a_place,
        });
    }

    /// Milliseconds since the trace was created, which is the walk's start.
    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// One record, written as one line.
    ///
    /// Best-effort, the way a cache entry that cannot be stored is: a trace that
    /// cannot be written must not fail a run whose judgments are already paid
    /// for. The first failure says so once on stderr and stops the tracing
    /// rather than silently carrying on, because a caller who asked for a trace
    /// is reading it — and a file that failed once is one no later record can be
    /// trusted to land in.
    fn record(&self, event: Event<'_>) {
        // A thread that panicked while holding the lock left a file that is
        // still usable: a trace with a gap in it beats no trace.
        let mut out = self
            .out
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.warned.load(Ordering::Relaxed) {
            return;
        }
        let mut failure = None;
        {
            let Out {
                file,
                line,
                written,
            } = &mut *out;
            line.clear();
            match serde_json::to_writer(&mut *line, &event) {
                Ok(()) => {
                    line.push(b'\n');
                    match file.write_all(line) {
                        Ok(()) => *written += line.len() as u64,
                        Err(source) => {
                            // `write_all` can leave part of the record behind,
                            // so the file goes back to the last whole line: the
                            // trace is shorter than the run, and a reader never
                            // meets a line it cannot parse.
                            let _ = file.set_len(*written);
                            failure = Some(source.to_string());
                        }
                    }
                }
                Err(source) => failure = Some(source.to_string()),
            }
        }
        if let Some(reason) = failure {
            self.failed(&reason);
        }
    }

    /// One line on stderr the first time a record cannot be written, and the end
    /// of this run's tracing: the file is left as the trace of what happened up
    /// to there.
    fn failed(&self, reason: &str) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            eprintln!("s1m: could not write the trace: {reason}");
        }
    }
}

/// One record of the trace, as the file spells it.
///
/// The names are the trace's own and not the reading list's: `t_ms`,
/// `path_score`, `latency_ms` and `earned_a_place` are spelled the way the
/// issue that asked for the trace spelled them, rather than the camelCase
/// `--format json` uses. `event` names the record and is always first.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event<'a> {
    /// The walk took a file off the frontier: `popped`.
    Popped {
        t_ms: u64,
        path: &'a Path,
        path_score: f64,
        depth: usize,
        via: &'a [PathBuf],
    },
    /// One request made for a file: one per post.
    Requested {
        t_ms: u64,
        path: &'a Path,
        post_index: usize,
    },
    /// The scorer's answer for a file, and what it cost: the file's relevance,
    /// a score per heading section and a scent per link, as the scorer judged
    /// them — the same [`SectionJudgment`]s and [`LinkJudgment`]s the walk read.
    ///
    /// The links are the scorer's own answer and not the file's link table: a
    /// link the file does not have is not in the reading list and a link the
    /// root's `.s1mignore` took out of the request is not something a real
    /// scorer can answer about, and what the walk made of the file's own links
    /// is what the `admitted` and `pruned` records are.
    Answered {
        t_ms: u64,
        path: &'a Path,
        latency_ms: u64,
        relevance: f64,
        cached: bool,
        sections: &'a [SectionJudgment],
        links: &'a [LinkJudgment],
    },
    /// A link that queued its target: the target is on the frontier at `scent`
    /// of the source's path score.
    Admitted {
        t_ms: u64,
        source: &'a Path,
        target: &'a Path,
        scent: f64,
    },
    /// A link that queued nothing, or a path the walk queued and then dropped:
    /// `scent` is the link's — `null` when the scorer judged none — and
    /// `reason` says which of the two it was and why.
    Pruned {
        t_ms: u64,
        source: &'a Path,
        target: &'a Path,
        scent: Option<f64>,
        reason: Reason,
    },
    /// A file the walk visited and judged, and what the reading list made of
    /// it.
    Result {
        t_ms: u64,
        path: &'a Path,
        relevance: f64,
        earned_a_place: bool,
    },
}

/// Why a link was passed over, or a queued path dropped.
///
/// A path can be admitted and pruned afterwards — the beam and the file budget
/// are read when the walk takes a path off the frontier, not when the link
/// queues it — so a `pruned` record is not always the other half of an
/// `admitted` one. What a player knows from the pair is that the path never
/// became a visit.
///
/// One vocabulary, two documents: a `pruned` record in the trace and a link's
/// `reason` in the reading list spell the same reasons the same way, hyphenated
/// the way the CLI's own values are (`useful-for`), so a reader comparing them
/// never meets two names for one thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// Its scent did not clear the walk's floor
    /// ([`crate::traverse::Admission::Threshold`]). A scent outside 0 to 1
    /// never clears it, whatever the floor is.
    BelowThreshold,
    /// The scorer's own rule kept it back
    /// ([`crate::traverse::Admission::Scorer`]): a Choice share that lost to
    /// the options beside it.
    NotKept,
    /// The scorer named no scent for it, so the walk had nothing to follow.
    Unjudged,
    /// Its target is outside the root, which is never followed.
    OutOfRoot,
    /// Its target is a hop past the depth budget.
    PastDepth,
    /// Its target has been visited, or a path at least as good was already
    /// queued for it: the first path to reach a file at its best score is the
    /// one kept.
    AlreadyReached,
    /// The root's `.s1mignore` matched its target, which is never read, never
    /// sent and never judged.
    Ignored,
    /// The path was queued and the walk had no turn left for it at its depth:
    /// what a beam is, one depth at a time.
    Beam,
    /// The path was queued and the walk judged as many files as its budget
    /// allows before the path had its turn.
    MaxFiles,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;

    use super::*;
    use crate::testkit::TempDir;

    /// One file's answer: a relevance, one section and one link, so a record
    /// that reported the walk's own numbers rather than the scorer's would
    /// show.
    fn judgment() -> FileJudgment {
        FileJudgment {
            relevance: 0.75,
            sections: vec![SectionJudgment {
                heading: Some("Cutoffs".to_string()),
                lines: [3, 9],
                score: 0.8,
            }],
            links: vec![LinkJudgment {
                target: PathBuf::from("payments/settlement.md"),
                scent: 0.9,
                keep: true,
            }],
        }
    }

    /// The records written so far, each parsed on its own: a trace a reader
    /// cannot read line by line is not a JSON Lines file.
    fn records(path: &Path) -> Vec<Value> {
        fs::read_to_string(path)
            .expect("the trace file should be readable")
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line should be one JSON object"))
            .collect()
    }

    #[test]
    fn every_record_is_one_line_with_the_time_it_happened() {
        let dir = TempDir::new("trace-events");
        let path = dir.path().join("trace.jsonl");
        let trace = Trace::create(&path, Path::new("wiki"), 0.6).expect("a trace");
        let judgment = judgment();

        trace.popped(Path::new("index.md"), 1.0, 0, &[]);
        trace.requested(Path::new("index.md"), 2);
        trace.answered(
            Path::new("index.md"),
            Duration::from_millis(412),
            false,
            &judgment,
        );
        trace.admitted(
            Path::new("index.md"),
            Path::new("payments/settlement.md"),
            0.9,
        );
        trace.pruned(
            Path::new("index.md"),
            Path::new("notes/ledger.md"),
            None,
            Reason::Unjudged,
        );
        trace.result(Path::new("index.md"), 0.75, true);

        let records = records(&path);
        let events: Vec<&str> = records
            .iter()
            .map(|record| record["event"].as_str().expect("an event name"))
            .collect();
        assert_eq!(
            events,
            [
                "popped",
                "requested",
                "requested",
                "answered",
                "admitted",
                "pruned",
                "result"
            ],
            "a split page is one requested record per post"
        );
        for record in &records {
            assert!(
                record["t_ms"].as_u64().is_some(),
                "every record says when it happened: {record}"
            );
        }

        assert_eq!(records[0]["path"], "index.md");
        assert_eq!(records[0]["path_score"], 1.0);
        assert_eq!(records[0]["depth"], 0);
        assert_eq!(records[0]["via"], serde_json::json!([]));
        assert_eq!(records[1]["post_index"], 0);
        assert_eq!(records[2]["post_index"], 1);
        assert_eq!(records[3]["latency_ms"], 412);
        assert_eq!(records[3]["relevance"], 0.75);
        assert_eq!(records[3]["cached"], false);
        assert_eq!(records[3]["sections"][0]["heading"], "Cutoffs");
        assert_eq!(
            records[3]["sections"][0]["lines"],
            serde_json::json!([3, 9])
        );
        assert_eq!(records[3]["sections"][0]["score"], 0.8);
        assert_eq!(records[3]["links"][0]["target"], "payments/settlement.md");
        assert_eq!(records[3]["links"][0]["scent"], 0.9);
        assert_eq!(records[4]["source"], "index.md");
        assert_eq!(records[4]["target"], "payments/settlement.md");
        assert_eq!(records[5]["scent"], Value::Null, "a scent nobody judged");
        assert_eq!(records[5]["reason"], "unjudged");
        assert_eq!(records[6]["earned_a_place"], true);
    }

    /// The trace is a reader's file while the run is still going: what has
    /// happened is on disk, without the writer being flushed or closed.
    #[test]
    fn a_record_is_on_disk_by_the_time_it_is_written() {
        let dir = TempDir::new("trace-as-it-happens");
        let path = dir.path().join("trace.jsonl");
        let trace = Trace::create(&path, Path::new("wiki"), 0.6).expect("a trace");

        trace.popped(Path::new("index.md"), 1.0, 0, &[]);
        assert_eq!(records(&path).len(), 1, "the record that has happened");

        trace.result(Path::new("index.md"), 0.75, true);
        let records = records(&path);
        assert_eq!(records.len(), 2);
        assert_eq!(records[1]["event"], "result");
    }

    /// A path is spelled the same however it arrives: the walk works in paths
    /// relative to the root, and a scorer reports the path it read with the root
    /// joined on.
    #[test]
    fn a_scorers_path_is_spelled_relative_to_the_root() {
        let dir = TempDir::new("trace-paths");
        let path = dir.path().join("trace.jsonl");
        let trace = Trace::create(&path, Path::new("eval/wikis/wiki"), 0.6).expect("a trace");

        trace.popped(
            Path::new("notes/ledger.md"),
            0.8,
            1,
            &[PathBuf::from("index.md")],
        );
        trace.answered(
            Path::new("eval/wikis/wiki/notes/ledger.md"),
            Duration::ZERO,
            false,
            &judgment(),
        );

        let records = records(&path);
        assert_eq!(records[0]["path"], "notes/ledger.md");
        assert_eq!(
            records[1]["path"], "notes/ledger.md",
            "the root is not part of the name"
        );
    }

    /// Writing to a path that cannot be a file is the caller's mistake, and it
    /// is caught when the trace is created rather than on the first record.
    #[test]
    fn a_trace_that_cannot_be_created_is_an_error() {
        let dir = TempDir::new("trace-unwritable");
        assert!(Trace::create(dir.path(), Path::new("wiki"), 0.6).is_err());
    }
}
