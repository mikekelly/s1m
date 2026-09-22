//! The player: a trace of a walk, drawn as a page that needs nothing but
//! itself.
//!
//! `--trace` writes what a walk did, one JSON object per line, in the order it
//! happened ([`crate::trace`] is that end of the file); this module is the
//! other one. [`play`] reads a trace, [`Run::render`] draws it, and what lands
//! beside the trace is one HTML page with the player, its styles and the
//! records inside it: no server, no request, no asset next to it, so it opens
//! from the disk with the network unplugged
//! ([#73](https://github.com/mikekelly/s1m/issues/73)).
//!
//! What the page is for is the question a reading list answers without
//! evidence: why is this file in it. A replay shows the crawl — files as they
//! come off the frontier, links as they are judged, what was passed over and
//! why, the list filling in — and any file in it can be opened for the path
//! that reached it, the scent of every hop, and the scores that earned it its
//! place.
//!
//! Reading is where the file is judged. A file that is not JSON Lines of trace
//! records is the caller's mistake, named with its line number, rather than a
//! page with a hole in it. Two imperfections are not mistakes, because they are
//! what a run that died leaves: a last line that is not JSON — written when the
//! process was killed part way through it — is dropped and reported, and an
//! event kind this player does not know is ignored, so a trace from a newer s1m
//! still plays.
//!
//! A trace holds the paths of whatever wiki it was made over and the query it
//! was made for, and the page holds the trace. Both are artifacts of the run,
//! to be kept where an eval's `--out` directory is kept and never committed.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::scorer::{LinkJudgment, SectionJudgment};
use crate::trace::Reason;

/// The page, with its styles and its player in it, and one hole where the run
/// and its records go.
const PAGE: &str = include_str!("player.html");

/// The hole: a JavaScript comment, so the file is a whole page — and a page
/// that draws an empty run — before anything is put in it.
const RUN: &str = "/*__S1M_RUN__*/";

/// What stops a trace from being played.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The trace is not there, or cannot be read.
    #[error("could not read the trace {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    /// The file has no records in it: an empty file, or one whose only line was
    /// half written.
    #[error("{} holds no trace records", path.display())]
    Empty { path: PathBuf },
    /// A line is not a record, so the file is not a trace this player knows.
    #[error("{} line {line} is not a trace record: {source}", path.display())]
    Line {
        path: PathBuf,
        line: usize,
        source: serde_json::Error,
    },
    /// The page could not be written.
    #[error("could not write the page {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    /// The page could not be written to stdout, which is what `--out -` asks
    /// for.
    #[error("could not write the page to stdout: {source}")]
    Stdout { source: io::Error },
}

/// The run the walk was for: the trace's `started` record, and the only place
/// the query is written down.
///
/// A trace from before the record existed has none, and plays with a title
/// taken from its own file name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Started {
    /// The query the caller asked, as the request carried it.
    pub query: String,
    /// The criterion the reading list reports: a mode's name, or the path of
    /// the `--criteria` file that replaced it.
    pub mode: String,
    /// Least relevance or section score that earned a file a place in the
    /// reading list.
    pub threshold: f64,
}

/// One record of a trace, read back.
///
/// This is the reading side of [`crate::trace::Event`] and a type of its own,
/// because a trace is an owned file on disk and a trace being written borrows
/// from a live walk. The two are held together by the test that writes a trace
/// with [`crate::trace::Trace`] and reads what it wrote.
///
/// An event kind that is not one of these is [`Record::Unknown`] rather than an
/// error: a player meets traces written by whatever s1m drew them, and a record
/// it cannot use is one it can leave out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Record {
    /// What the run was for: `started`.
    Started {
        t_ms: u64,
        #[serde(flatten)]
        run: Started,
    },
    /// A file the walk took off the frontier: `popped`.
    Popped {
        t_ms: u64,
        path: PathBuf,
        path_score: f64,
        depth: usize,
        via: Vec<PathBuf>,
    },
    /// One request for a file, once per post: `requested`.
    Requested {
        t_ms: u64,
        path: PathBuf,
        post_index: usize,
    },
    /// The scorer's answer for a file: `answered`.
    Answered {
        t_ms: u64,
        path: PathBuf,
        latency_ms: u64,
        relevance: f64,
        cached: bool,
        sections: Vec<SectionJudgment>,
        links: Vec<LinkJudgment>,
    },
    /// A link that queued its target: `admitted`.
    Admitted {
        t_ms: u64,
        source: PathBuf,
        target: PathBuf,
        scent: f64,
    },
    /// A link that queued nothing, or a path the walk queued and dropped:
    /// `pruned`.
    Pruned {
        t_ms: u64,
        source: PathBuf,
        target: PathBuf,
        scent: Option<f64>,
        reason: Reason,
    },
    /// A file the walk visited, and what the reading list made of it: `result`.
    Result {
        t_ms: u64,
        path: PathBuf,
        relevance: f64,
        earned_a_place: bool,
    },
    /// A record of an event kind this player does not know, which it ignores.
    #[serde(other)]
    Unknown,
}

/// What a run cost, in the numbers the page's header reports.
///
/// The walk's own accounting and not the reading list's: `files` counts files
/// and not visits, so a file a round put back on the frontier is one of them,
/// and `bought` against `cached` says what the run paid for rather than what it
/// looked at.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    /// Files the walk visited: the distinct paths of its `result` records.
    pub files: usize,
    /// Turns a file took off the frontier. More than [`Summary::files`] when a
    /// round's own answers overtook a file it had popped and put it back.
    pub pops: usize,
    /// Requests sent, which is more than one per file for a page judged in
    /// several posts.
    pub requests: usize,
    /// Answers the model gave, and how many of them were bought now rather than
    /// served from the cache.
    pub answered: usize,
    pub bought: usize,
    pub cached: usize,
    /// Links the walk followed, links it passed over, and paths it queued and a
    /// budget then dropped.
    pub followed: usize,
    pub passed_over: usize,
    pub dropped: usize,
    /// Files that earned a place in the reading list. Counted per file, like
    /// [`Summary::files`]: a file a round put back on the frontier and visited
    /// twice earns one place.
    pub earned: usize,
    /// The last record's `t_ms`: how long the walk took on the clock.
    pub duration_ms: u64,
}

/// One trace, read: the records the page draws, what the run was for, and what
/// it cost.
#[derive(Debug)]
pub struct Run {
    /// The trace's path as the caller spelled it, for the page to name itself
    /// after.
    trace: PathBuf,
    /// The run's own record, when the trace has one.
    started: Option<Started>,
    /// Every record, in the order the walk wrote them, and without the ones
    /// this player does not know.
    records: Vec<Record>,
    summary: Summary,
    /// The line a killed run left half-written and this player dropped.
    dropped_line: Option<usize>,
}

impl Run {
    /// Reads the trace at `path`.
    ///
    /// A line that cannot be read is an error, with its number, unless it is
    /// the last one: a run killed mid-write leaves exactly that, and everything
    /// before it is still the run.
    pub fn read(path: &Path) -> Result<Run, Error> {
        let read = fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Run::parse(path, &read)
    }

    /// The same, from a trace already in hand.
    fn parse(path: &Path, text: &str) -> Result<Run, Error> {
        // Blank lines are not records and are not lines the walk wrote: a trace
        // appended to, or edited by hand, is read for what it says.
        let lines: Vec<(usize, &str)> = text
            .lines()
            .enumerate()
            .map(|(index, line)| (index + 1, line))
            .filter(|(_, line)| !line.trim().is_empty())
            .collect();

        let mut records = Vec::with_capacity(lines.len());
        let mut dropped_line = None;
        let last = lines.len();
        for (at, &(line, text)) in lines.iter().enumerate() {
            match serde_json::from_str::<Record>(text) {
                // A record this player does not know is left where it is: it is
                // not a page with a hole in it, it is a page of the run this
                // player can draw.
                Ok(Record::Unknown) => {}
                Ok(record) => records.push(record),
                // The last line of a trace is the one a killed run can leave
                // half-written, and nothing follows it to be misread: the rest
                // is a whole trace of the run so far. Any other line is a file
                // that is not this file's format, and saying so beats drawing a
                // walk that never happened.
                Err(_) if at + 1 == last => dropped_line = Some(line),
                Err(source) => {
                    return Err(Error::Line {
                        path: path.to_path_buf(),
                        line,
                        source,
                    });
                }
            }
        }

        if records.is_empty() {
            return Err(Error::Empty {
                path: path.to_path_buf(),
            });
        }
        let summary = summarise(&records);
        Ok(Run {
            trace: path.to_path_buf(),
            started: records.iter().find_map(|record| match record {
                Record::Started { run, .. } => Some(run.clone()),
                _ => None,
            }),
            records,
            summary,
            dropped_line,
        })
    }

    /// The run's own record, when the trace has one.
    pub fn started(&self) -> Option<&Started> {
        self.started.as_ref()
    }

    /// What the run cost.
    pub fn summary(&self) -> &Summary {
        &self.summary
    }

    /// The line a killed run left half-written, when there was one.
    pub fn dropped_line(&self) -> Option<usize> {
        self.dropped_line
    }

    /// The page: the records, the run's own facts, and the player that draws
    /// them, in one file.
    pub fn render(&self) -> String {
        let page = Page {
            trace: self.trace.display().to_string(),
            started: self.started.as_ref(),
            summary: &self.summary,
            records: &self.records,
        };
        let json = serde_json::to_string(&page).expect("the records of a trace serialize");
        PAGE.replace(RUN, &inside_a_script(&json))
    }
}

/// What the page is handed: the trace it draws, what the run was for, what it
/// cost, and every record, in order.
#[derive(Serialize)]
struct Page<'a> {
    trace: String,
    started: Option<&'a Started>,
    summary: &'a Summary,
    records: &'a [Record],
}

/// The JSON, spelled so that it cannot leave the `<script>` element it goes in.
///
/// A wiki's paths are the caller's text and one of them can hold `</script>`,
/// which ends the element whether it is inside a JSON string or not, so every
/// `<`, `>` and `&` goes out as its `\u` escape: the same JSON once it is
/// parsed, and no markup while it is read as HTML. The two line separators go
/// the same way — legal in JSON, and not in the JavaScript this page hands them
/// to.
fn inside_a_script(json: &str) -> String {
    let mut escaped = String::with_capacity(json.len() + json.len() / 8);
    for character in json.chars() {
        match character {
            '<' => escaped.push_str("\\u003c"),
            '>' => escaped.push_str("\\u003e"),
            '&' => escaped.push_str("\\u0026"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// What one trace's records add up to.
fn summarise(records: &[Record]) -> Summary {
    let mut summary = Summary {
        files: 0,
        pops: 0,
        requests: 0,
        answered: 0,
        bought: 0,
        cached: 0,
        followed: 0,
        passed_over: 0,
        dropped: 0,
        earned: 0,
        duration_ms: 0,
    };
    let mut files = BTreeSet::new();
    let mut earned = BTreeSet::new();
    for record in records {
        match record {
            Record::Started { t_ms, .. } => summary.duration_ms = summary.duration_ms.max(*t_ms),
            Record::Popped { t_ms, .. } => {
                summary.pops += 1;
                summary.duration_ms = summary.duration_ms.max(*t_ms);
            }
            Record::Requested { t_ms, .. } => {
                summary.requests += 1;
                summary.duration_ms = summary.duration_ms.max(*t_ms);
            }
            Record::Answered {
                t_ms,
                cached: from_cache,
                ..
            } => {
                summary.answered += 1;
                if *from_cache {
                    summary.cached += 1;
                } else {
                    summary.bought += 1;
                }
                summary.duration_ms = summary.duration_ms.max(*t_ms);
            }
            Record::Admitted { t_ms, .. } => {
                summary.followed += 1;
                summary.duration_ms = summary.duration_ms.max(*t_ms);
            }
            Record::Pruned { t_ms, reason, .. } => {
                match reason {
                    // A queued path a budget dropped, and not a link the walk
                    // passed over: the two are worth telling apart, because one
                    // is what the walk refused and the other what it never got
                    // to.
                    Reason::Beam | Reason::MaxFiles => summary.dropped += 1,
                    _ => summary.passed_over += 1,
                }
                summary.duration_ms = summary.duration_ms.max(*t_ms);
            }
            Record::Result {
                t_ms,
                path,
                earned_a_place,
                ..
            } => {
                if *earned_a_place {
                    earned.insert(path.clone());
                }
                files.insert(path.clone());
                summary.duration_ms = summary.duration_ms.max(*t_ms);
            }
            Record::Unknown => {}
        }
    }
    summary.files = files.len();
    summary.earned = earned.len();
    summary
}

/// A page drawn from `trace`, at `out` — the trace's own path with `.html` when
/// the caller named none, and stdout when `out` is `-`.
pub fn play(trace: &Path, out: Option<&Path>) -> Result<Played, Error> {
    let run = Run::read(trace)?;
    let page = run.render();
    let path = match out {
        Some(to) if to == Path::new("-") => {
            io::stdout()
                .write_all(page.as_bytes())
                .map_err(|source| Error::Stdout { source })?;
            None
        }
        Some(to) => {
            fs::write(to, page).map_err(|source| Error::Write {
                path: to.to_path_buf(),
                source,
            })?;
            Some(to.to_path_buf())
        }
        None => {
            let to = trace.with_extension("html");
            fs::write(&to, page).map_err(|source| Error::Write {
                path: to.clone(),
                source,
            })?;
            Some(to)
        }
    };
    Ok(Played { path, run })
}

/// What [`play`] did: where the page went — `None` when it went to stdout — and
/// the run it draws, so the caller can report a trace that lost its last line.
pub struct Played {
    pub path: Option<PathBuf>,
    pub run: Run,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::Value;

    use super::*;
    use crate::scorer::LinkJudgment;
    use crate::testkit::TempDir;
    use crate::trace::Trace;

    /// A trace file in its own directory, written and left there.
    fn trace_of(dir: &TempDir, name: &str, lines: &[&str]) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, lines.join("\n")).expect("the fixture trace should be writable");
        path
    }

    /// One file's answer, so a record can carry the judgment's own numbers.
    fn judgment(relevance: f64, scent: f64) -> crate::scorer::FileJudgment {
        crate::scorer::FileJudgment {
            relevance,
            sections: vec![SectionJudgment {
                heading: Some("Cutoffs".to_string()),
                lines: [3, 9],
                score: relevance,
            }],
            links: vec![LinkJudgment {
                target: PathBuf::from("payments/settlement.md"),
                scent,
                keep: true,
            }],
        }
    }

    /// A trace of a small walk: an entry, one page it reached, one page it
    /// passed over, and one it queued and a budget dropped.
    fn walked(dir: &TempDir) -> PathBuf {
        let path = dir.path().join("run.jsonl");
        let trace = Trace::create(&path, Path::new("wiki"), 0.6).expect("a trace");
        trace.started("what is there to read", "useful-for");
        trace.popped(Path::new("index.md"), 1.0, 0, &[]);
        trace.requested(Path::new("index.md"), 1);
        trace.answered(
            Path::new("index.md"),
            Duration::from_millis(200),
            false,
            &judgment(0.75, 0.9),
        );
        trace.admitted(
            Path::new("index.md"),
            Path::new("payments/settlement.md"),
            0.82,
        );
        trace.pruned(
            Path::new("index.md"),
            Path::new("notes/scratch.md"),
            Some(0.31),
            Reason::BelowThreshold,
        );
        trace.pruned(
            Path::new("index.md"),
            Path::new("notes/ledger.md"),
            Some(0.71),
            Reason::MaxFiles,
        );
        trace.result(Path::new("index.md"), 0.75, true);
        drop(trace);
        path
    }

    /// The records of a page as the JSON the browser is handed, and not the
    /// markup: what the player draws is read back the way the player reads it.
    fn records(page: &str) -> Value {
        let start = page.find("id=\"run\"").expect("the page's run element");
        let start = page[start..].find('>').expect("the element's text") + start + 1;
        let end = page[start..].find("</script>").expect("the element's end") + start;
        serde_json::from_str(&page[start..end]).expect("the page should hold JSON")
    }

    /// A trace the walk wrote reads back as the events it wrote: the reader and
    /// the writer are two types over one format, and this is what holds them
    /// together.
    #[test]
    fn a_trace_reads_back_as_the_events_that_were_written() {
        let dir = TempDir::new("player-reads");
        let run = Run::read(&walked(&dir)).expect("the trace should read");

        let started = run.started().expect("the run's own record");
        assert_eq!(started.query, "what is there to read");
        assert_eq!(started.mode, "useful-for");
        assert_eq!(started.threshold, 0.6);

        let events: Vec<&str> = run
            .records
            .iter()
            .map(|record| match record {
                Record::Started { .. } => "started",
                Record::Popped { path, depth, .. } => {
                    assert!(path.ends_with("index.md"), "{path:?}");
                    assert_eq!(*depth, 0);
                    "popped"
                }
                Record::Requested { post_index, .. } => {
                    assert_eq!(*post_index, 0);
                    "requested"
                }
                Record::Answered {
                    relevance,
                    cached,
                    sections,
                    links,
                    ..
                } => {
                    assert_eq!(*relevance, 0.75);
                    assert!(!cached);
                    assert_eq!(sections.len(), 1);
                    assert_eq!(sections[0].heading.as_deref(), Some("Cutoffs"));
                    assert_eq!(links[0].scent, 0.9);
                    "answered"
                }
                Record::Admitted { scent, .. } => {
                    assert_eq!(*scent, 0.82);
                    "admitted"
                }
                Record::Pruned { reason, .. } => {
                    assert!(matches!(reason, Reason::BelowThreshold | Reason::MaxFiles));
                    "pruned"
                }
                Record::Result { earned_a_place, .. } => {
                    assert!(earned_a_place);
                    "result"
                }
                Record::Unknown => "unknown",
            })
            .collect();
        assert_eq!(
            events,
            [
                "started",
                "popped",
                "requested",
                "answered",
                "admitted",
                "pruned",
                "pruned",
                "result"
            ]
        );

        let summary = run.summary();
        assert_eq!(summary.files, 1);
        assert_eq!(summary.pops, 1);
        assert_eq!(summary.requests, 1);
        assert_eq!(summary.answered, 1);
        assert_eq!(summary.bought, 1);
        assert_eq!(summary.cached, 0);
        assert_eq!(summary.followed, 1);
        assert_eq!(summary.passed_over, 1, "the link under the threshold");
        assert_eq!(summary.dropped, 1, "the path the file budget dropped");
        assert_eq!(summary.earned, 1);
        assert_eq!(run.dropped_line(), None);
    }

    /// A cached answer is not work this run did: it is counted as cached, not
    /// as bought.
    #[test]
    fn a_cached_answer_is_not_what_the_run_paid_for() {
        let dir = TempDir::new("player-cached");
        let path = trace_of(
            &dir,
            "run.jsonl",
            &[
                r#"{"event":"started","t_ms":0,"query":"q","mode":"about","threshold":0.6}"#,
                r#"{"event":"requested","t_ms":1,"path":"a.md","post_index":0}"#,
                r#"{"event":"answered","t_ms":1,"path":"a.md","latency_ms":411,"relevance":0.8,"cached":true,"sections":[],"links":[]}"#,
                r#"{"event":"result","t_ms":1,"path":"a.md","relevance":0.8,"earned_a_place":true}"#,
            ],
        );
        let run = Run::read(&path).expect("the trace should read");

        assert_eq!(run.summary().answered, 1);
        assert_eq!(run.summary().cached, 1);
        assert_eq!(run.summary().bought, 0);
        assert_eq!(run.summary().duration_ms, 1);
    }

    /// An event kind this player does not know is left out of the page rather
    /// than failing it, so a trace from a newer s1m plays.
    #[test]
    fn an_event_kind_the_player_does_not_know_is_ignored() {
        let dir = TempDir::new("player-unknown");
        let path = trace_of(
            &dir,
            "run.jsonl",
            &[
                r#"{"event":"started","t_ms":0,"query":"q","mode":"about","threshold":0.6}"#,
                r#"{"event":"teleported","t_ms":2,"path":"a.md","somewhere":"else"}"#,
                r#"{"event":"result","t_ms":3,"path":"a.md","relevance":0.8,"earned_a_place":true}"#,
            ],
        );
        let run = Run::read(&path).expect("the trace should read");
        let page = run.render();

        assert_eq!(run.records.len(), 2, "the known records, and no other");
        let records = records(&page);
        let events: Vec<&str> = records["records"]
            .as_array()
            .expect("the page's records")
            .iter()
            .map(|record| record["event"].as_str().expect("an event name"))
            .collect();
        assert_eq!(events, ["started", "result"]);
        assert!(
            !page.contains("teleported"),
            "an event the player cannot draw is not handed to it"
        );
    }

    /// A run killed part way through its last record leaves a line that is not
    /// JSON: everything before it is still the run, and the page says which
    /// line was left out. Every other line has to be a record.
    #[test]
    fn a_half_written_last_line_is_dropped_and_a_broken_one_between_is_an_error() {
        let dir = TempDir::new("player-partial");
        let path = trace_of(
            &dir,
            "run.jsonl",
            &[
                r#"{"event":"started","t_ms":0,"query":"q","mode":"about","threshold":0.6}"#,
                r#"{"event":"result","t_ms":3,"path":"a.md","relevance":0.8,"earned_a_place":true}"#,
                r#"{"event":"popped","t_ms":4,"path":"b.md","path_scape"#,
            ],
        );
        let run = Run::read(&path).expect("the run up to the kill should play");

        assert_eq!(run.dropped_line(), Some(3));
        assert_eq!(run.summary().files, 1);
        assert_eq!(run.records.len(), 2);

        let broken = trace_of(
            &dir,
            "broken.jsonl",
            &[
                r#"{"event":"started","t_ms":0,"query":"q","mode":"about","threshold":0.6}"#,
                r#"{"event":"result","t_ms":3,"path":"a.md"}"#,
                r#"{"event":"popped","t_ms":4,"path":"b.md","path_score":1.0,"depth":0,"via":[]}"#,
            ],
        );
        let error = Run::read(&broken).expect_err("a line that is not a record is an error");
        assert!(
            matches!(error, Error::Line { line: 2, .. }),
            "the line is named: {error}"
        );
    }

    /// A file that is not a trace at all, and one with nothing in it: both are
    /// the caller's mistake, named, rather than an empty page.
    #[test]
    fn a_file_that_is_not_a_trace_says_so() {
        let dir = TempDir::new("player-not-a-trace");
        let prose = trace_of(&dir, "prose.txt", &["# Notes", "", "A page of prose."]);
        let error = Run::read(&prose).expect_err("prose is not a trace");
        assert!(matches!(error, Error::Line { line: 1, .. }), "{error}");
        assert!(error.to_string().contains("prose.txt"), "{error}");

        let empty = trace_of(&dir, "empty.jsonl", &[""]);
        let error = Run::read(&empty).expect_err("nothing to play");
        assert!(matches!(error, Error::Empty { .. }), "{error}");

        let missing = dir.path().join("gone.jsonl");
        let error = Run::read(&missing).expect_err("nothing to read");
        assert!(matches!(error, Error::Read { .. }), "{error}");
    }

    /// A path is a caller's text, so it can hold the one sequence that ends the
    /// element the records are read out of. What the browser is handed is the
    /// same JSON either way.
    #[test]
    fn a_path_cannot_close_the_element_the_records_go_in() {
        let dir = TempDir::new("player-script");
        let path = trace_of(
            &dir,
            "run.jsonl",
            &[
                r#"{"event":"started","t_ms":0,"query":"</script><script>alert(1)</script>","mode":"about","threshold":0.6}"#,
                r#"{"event":"result","t_ms":3,"path":"a</script>&amp;.md","relevance":0.8,"earned_a_place":true}"#,
            ],
        );
        let page = Run::read(&path).expect("the trace should read").render();

        assert!(
            !page.contains("<script>alert"),
            "a path cannot become markup"
        );
        let records = records(&page);
        assert_eq!(
            records["started"]["query"],
            "</script><script>alert(1)</script>"
        );
        assert_eq!(records["records"][1]["path"], "a</script>&amp;.md");
    }

    /// The page is one file: what it draws is in it, and it asks nothing of the
    /// network or of the disk beside it, because a wiki's paths are in it too.
    #[test]
    fn the_page_needs_nothing_but_itself() {
        let dir = TempDir::new("player-standalone");
        let trace = walked(&dir);
        let page = Run::read(&trace).expect("the trace should read").render();

        assert!(page.starts_with("<!doctype html>"), "a whole page");
        assert!(!page.contains("/*__S1M_RUN__*/"), "the hole is filled");
        // The page writes the SVG namespace, which is a name and not an
        // address; what it may not have is anything to fetch.
        for request in [
            "src=\"http",
            "href=\"http",
            "url(http",
            "fetch(",
            "@import",
            "<link",
            "<img",
            "<iframe",
        ] {
            assert!(!page.contains(request), "nothing to fetch: {request}");
        }
        assert!(page.contains("id=\"run\""), "the records are in the page");
        assert_eq!(
            page,
            Run::read(&trace).expect("again").render(),
            "the same trace is the same page"
        );
    }

    /// `--out` is the page's path, a trace beside its own is the page's default
    /// name, and `-` is stdout.
    #[test]
    fn the_page_lands_beside_the_trace_unless_told_otherwise() {
        let dir = TempDir::new("player-out");
        let trace = walked(&dir);

        let played = play(&trace, None).expect("a page");
        assert_eq!(
            played.path.as_deref(),
            Some(dir.path().join("run.html").as_path())
        );
        assert_eq!(played.run.summary().earned, 1);
        assert!(dir.path().join("run.html").exists());

        let named = dir.path().join("elsewhere").join("run.html");
        fs::create_dir_all(named.parent().expect("a directory")).expect("the directory");
        let played = play(&trace, Some(&named)).expect("a page");
        assert_eq!(played.path.as_deref(), Some(named.as_path()));
        assert!(named.exists());

        let played = play(&trace, Some(Path::new("-"))).expect("a page on stdout");
        assert_eq!(played.path, None);
    }
}
