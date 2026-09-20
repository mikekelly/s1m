//! Traversal tests against the fixture wiki in `tests/fixtures/`, driven by a
//! fake scorer: the contract from
//! [#6](https://github.com/mikekelly/s1m/issues/6).
//!
//! The fake answers from a fixed table, so what is under test is the walk —
//! priority, budgets, pruning, the visited set — and not the model. Its
//! answers can be delayed by a varying amount, which is how the determinism
//! test makes the order answers arrive in irrelevant.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::sleep;

use s1m::parse::{self, ParsedFile, relative_to_root};
use s1m::scorer::{FileJudgment, LinkJudgment, Scorer, ScorerError, SectionJudgment};
use s1m::traverse::{Config, Failure, Traversal, TraverseError, VisitedFile, traverse};

const QUERY: &str = "settlement timing for instant payouts";

/// The fake's delay per answer: long enough that two futures of one round
/// really do interleave, short enough that the suite stays quick.
const DELAY_MS: u64 = 20;
const DELAY_SPREAD_MS: u64 = 20;

/// One row of the fake's table: what it answers for a file, and how often it
/// was asked about it. A file is scored by one future at a time, so counters
/// are all the bookkeeping a shared fake needs.
struct Entry {
    relevance: f64,
    links: &'static [(&'static str, f64)],
    calls: AtomicUsize,
}

impl Entry {
    fn new(relevance: f64, links: &'static [(&'static str, f64)]) -> Entry {
        Entry {
            relevance,
            links,
            calls: AtomicUsize::new(0),
        }
    }
}

/// A scorer with a fixed table, a delay per answer and a count of the files it
/// was asked about.
struct Fake {
    /// The fixture root, to spell `ParsedFile::path` back relative to it.
    root: PathBuf,
    table: HashMap<&'static str, Entry>,
    jitter: bool,
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    seed: AtomicUsize,
}

impl Fake {
    fn new(table: &[(&'static str, Entry)]) -> Fake {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |since| since.subsec_nanos() as usize);
        Fake {
            root: root(),
            table: table
                .iter()
                .map(|(name, entry)| (*name, Entry::new(entry.relevance, entry.links)))
                .collect(),
            jitter: false,
            in_flight: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            seed: AtomicUsize::new(seed),
        }
    }

    /// Varies each answer's latency, so answers come back in a different order
    /// from the one they were asked in.
    fn jittered(mut self) -> Fake {
        self.jitter = true;
        self
    }

    /// The files the scorer was asked about, sorted, so a test can hold the
    /// walk to exactly the files it should have spent a call on.
    fn called(&self) -> Vec<String> {
        let mut called: Vec<String> = self
            .table
            .iter()
            .filter(|(_, entry)| entry.calls.load(Ordering::SeqCst) > 0)
            .map(|(name, _)| name.to_string())
            .collect();
        called.sort();
        called
    }

    /// Files asked about more than once: there must be none, however many paths
    /// reach them.
    fn asked_twice(&self) -> Vec<String> {
        let mut twice: Vec<String> = self
            .table
            .iter()
            .filter(|(_, entry)| entry.calls.load(Ordering::SeqCst) > 1)
            .map(|(name, _)| name.to_string())
            .collect();
        twice.sort();
        twice
    }

    /// Most answers that were in flight at once.
    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Scorer for Fake {
    async fn score(&self, query: &str, file: &ParsedFile) -> Result<FileJudgment, ScorerError> {
        assert_eq!(query, QUERY, "the traversal's query reaches the scorer");
        let path = relative_to_root(&self.root, &file.path);
        let name = path.to_string_lossy().into_owned();
        let Some(entry) = self.table.get(name.as_str()) else {
            return Err(ScorerError::MissingAnswer { id: name });
        };
        entry.calls.fetch_add(1, Ordering::SeqCst);

        let in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(in_flight, Ordering::SeqCst);
        if self.jitter {
            sleep(Duration::from_millis(delay(&self.seed))).await;
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);

        Ok(FileJudgment {
            relevance: entry.relevance,
            // Section `i` scores `0.5 + i/100`, so a section judgment landing
            // on the wrong section of the file is visible, and the score order
            // is the file's own order. The heading and the range are the
            // fake's own and deliberately not the parser's, so a walk that
            // reported the judgment's heading or range rather than the parsed
            // file's would show `"not the parser's"` or `[0, 0]` in a result.
            sections: file
                .sections
                .iter()
                .enumerate()
                .map(|(index, _)| SectionJudgment {
                    heading: Some("not the parser's".to_string()),
                    lines: [0, 0],
                    score: 0.5 + index as f64 / 100.0,
                })
                .collect(),
            links: entry
                .links
                .iter()
                .map(|(target, scent)| LinkJudgment {
                    target: PathBuf::from(target),
                    scent: *scent,
                })
                .collect(),
        })
    }
}

/// A different delay per call, mixed from a counter seeded off the clock so no
/// two runs see the same order. It only shuffles when answers become ready:
/// nothing in a result may depend on it.
fn delay(seed: &AtomicUsize) -> u64 {
    let mut value = seed.fetch_add(1, Ordering::SeqCst) as u64 | 1;
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^= value >> 31;
    DELAY_MS + value % DELAY_SPREAD_MS
}

/// A traversal over the fixture wiki, holding the root and entry files the
/// config borrows. Starts from the same budgets the plan proposes, shrunk to
/// the fixture's nine pages.
struct Settings {
    root: PathBuf,
    entries: Vec<PathBuf>,
    seeds: Vec<PathBuf>,
    max_files: usize,
    max_depth: usize,
    fanout: usize,
    threshold: f64,
}

impl Settings {
    fn new(entries: &[&str]) -> Settings {
        let root = root();
        Settings {
            entries: entries.iter().map(|entry| root.join(entry)).collect(),
            root,
            seeds: Vec::new(),
            max_files: 8,
            max_depth: 6,
            fanout: 4,
            threshold: 0.6,
        }
    }

    /// Seeds the walk: the extra entry files `--seed-grep` found, given the way
    /// the seeder spells them.
    fn seeds(mut self, seeds: &[&str]) -> Self {
        self.seeds = seeds.iter().map(|seed| self.root.join(seed)).collect();
        self
    }

    fn max_files(mut self, max_files: usize) -> Self {
        self.max_files = max_files;
        self
    }

    fn max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    fn fanout(mut self, fanout: usize) -> Self {
        self.fanout = fanout;
        self
    }

    fn threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold;
        self
    }

    async fn run(&self, scorer: &dyn Scorer) -> Traversal {
        let config = Config {
            query: QUERY,
            entries: &self.entries,
            seeds: &self.seeds,
            root: &self.root,
            max_files: self.max_files,
            max_depth: self.max_depth,
            fanout: self.fanout,
            threshold: self.threshold,
        };
        traverse(&config, scorer)
            .await
            .expect("traversal should run")
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wiki")
}

/// The visited paths, in the order the result lists them.
fn paths(traversal: &Traversal) -> Vec<String> {
    traversal
        .results
        .iter()
        .map(|file| file.path.display().to_string())
        .collect()
}

fn visited<'a>(traversal: &'a Traversal, path: &str) -> &'a VisitedFile {
    traversal
        .results
        .iter()
        .find(|file| file.path == Path::new(path))
        .unwrap_or_else(|| panic!("never visited {path}: {:?}", paths(traversal)))
}

/// What a test compares about a judged link: its target, the scent it was
/// judged at, and whether it queued that target.
type Judged = (String, Option<f64>, bool);

/// Every link of a record, in order, as a test compares them: target, scent,
/// followed. Pins the document order, the one-entry-per-target dedup and the
/// links the scorer named no judgment for.
fn links(file: &VisitedFile) -> Vec<Judged> {
    file.links
        .iter()
        .map(|link| (link.target.display().to_string(), link.scent, link.followed))
        .collect()
}

/// The links the scorer judged, which is what most tests are about.
fn judged(file: &VisitedFile) -> Vec<Judged> {
    links(file)
        .into_iter()
        .filter(|(_, scent, _)| scent.is_some())
        .collect()
}

/// What a test compares about a judged section: its heading, the lines it
/// covers and its score.
type JudgedSection = (Option<String>, [usize; 2], f64);

/// Every section of a record, in the order the file has them.
fn sections(file: &VisitedFile) -> Vec<JudgedSection> {
    file.sections
        .iter()
        .map(|section| (section.heading.clone(), section.lines, section.score))
        .collect()
}

/// Whether a record's link to `target` queued its target.
fn followed(file: &VisitedFile, target: &str) -> bool {
    file.links
        .iter()
        .find(|link| link.target == Path::new(target))
        .unwrap_or_else(|| panic!("no link to {target} in {:?}", judged(file)))
        .followed
}

fn link(target: &str, scent: f64, followed: bool) -> Judged {
    (target.to_string(), Some(scent), followed)
}

/// A link the scorer named no judgment for: reported, never followed.
fn unjudged(target: &str) -> Judged {
    (target.to_string(), None, false)
}

/// The files that could not be scored, as `(path, reason)`.
fn failures(traversal: &Traversal) -> Vec<(String, String)> {
    traversal
        .failed
        .iter()
        .map(|failed| {
            (
                failed.path.display().to_string(),
                failed.failure.to_string(),
            )
        })
        .collect()
}

/// Every visited file carries its sections: the parser's own ranges — parent
/// sections included, so a parent's range covers its subsections' — in the
/// file's order, each with the score the scorer answered for that section.
#[tokio::test]
async fn a_visited_file_carries_its_sections_with_the_parsers_ranges() {
    let scorer = Fake::new(&[
        ("index.md", Entry::new(0.5, &[("payments/README.md", 0.9)])),
        ("payments/README.md", Entry::new(0.4, &[])),
    ]);

    let found = Settings::new(&["index.md"]).run(&scorer).await;

    // The ranges the list is held to, as #4's parser reports them.
    let parsed = parse::parse(root().join("payments/README.md"), root()).expect("a parse");
    assert_eq!(
        parsed
            .sections
            .iter()
            .map(|section| (section.heading.clone(), section.lines))
            .collect::<Vec<_>>(),
        [
            (Some("Payments".to_string()), [1, 25]),
            (Some("Instant payouts".to_string()), [5, 12]),
            (Some("Windows".to_string()), [9, 12]),
            (Some("Settlement".to_string()), [13, 25]),
        ],
        "the fixture the expectation below is read off"
    );

    assert_eq!(
        sections(visited(&found, "payments/README.md")),
        [
            (Some("Payments".to_string()), [1, 25], 0.5),
            (Some("Instant payouts".to_string()), [5, 12], 0.51),
            (Some("Windows".to_string()), [9, 12], 0.52),
            (Some("Settlement".to_string()), [13, 25], 0.53),
        ],
        "document order, the parser's lines, and the fake's score by position"
    );
}

/// Content before the first heading is a section like any other, reported with
/// `heading: None` and the lines the parser gave it: it is the one part of a
/// file no heading names, so dropping or renaming it would lose the only range
/// that covers it.
#[tokio::test]
async fn the_preamble_before_the_first_heading_is_a_section() {
    let scorer = Fake::new(&[("notes/scratch.md", Entry::new(0.5, &[]))]);

    let found = Settings::new(&["notes/scratch.md"]).run(&scorer).await;

    let parsed = parse::parse(root().join("notes/scratch.md"), root()).expect("a parse");
    assert_eq!(
        parsed
            .sections
            .iter()
            .map(|section| (section.heading.clone(), section.lines))
            .collect::<Vec<_>>(),
        [(None, [1, 2]), (Some("Scratch".to_string()), [3, 5]),],
        "the fixture the expectation below is read off"
    );
    assert_eq!(
        sections(visited(&found, "notes/scratch.md")),
        [
            (None, [1, 2], 0.5),
            (Some("Scratch".to_string()), [3, 5], 0.51)
        ],
        "the preamble keeps its place, its lines and no heading"
    );
}

#[tokio::test]
async fn a_hub_page_with_low_relevance_still_has_its_links_followed() {
    let scorer = Fake::new(&[
        (
            "index.md",
            Entry::new(
                0.02,
                &[
                    ("payments/README.md", 0.9),
                    ("notes/ledger.md", 0.8),
                    ("../outside.md", 0.99),
                    // Not one of the index's links: a scorer cannot put a link
                    // in a file that is not there.
                    ("notes/scratch.md", 0.99),
                ],
            ),
        ),
        ("payments/README.md", Entry::new(0.4, &[])),
        ("notes/ledger.md", Entry::new(0.5, &[])),
        ("notes/scratch.md", Entry::new(1.0, &[])),
    ]);

    let found = Settings::new(&["index.md"]).run(&scorer).await;

    // The index is nearly irrelevant and its links were still queued, which is
    // the point of keeping relevance and scent apart.
    assert_eq!(
        paths(&found),
        ["notes/ledger.md", "payments/README.md", "index.md"]
    );
    assert_eq!(found.calls, 3);

    // The record is the file's own links in document order, one per target
    // (the index links to the cutoffs page twice), each with the scent it was
    // judged at and nothing else.
    assert_eq!(
        links(visited(&found, "index.md")),
        [
            link("payments/README.md", 0.9, true),
            unjudged("payments/cutoffs.md"),
            unjudged("payments/settlement.md"),
            link("notes/ledger.md", 0.8, true),
            link("../outside.md", 0.99, false),
            unjudged("payments/missing.md"),
        ]
    );
}

#[tokio::test]
async fn entry_files_are_visited_at_depth_zero_with_no_scent() {
    let scorer = Fake::new(&[("index.md", Entry::new(0.5, &[("payments/README.md", 0.9)]))]);

    let found = Settings::new(&["index.md"]).run(&scorer).await;

    let index = visited(&found, "index.md");
    assert_eq!(index.scent, None);
    assert_eq!(index.path_score, 1.0);
    assert_eq!(index.depth, 0);
    assert!(index.via.is_empty());
}

#[tokio::test]
async fn the_frontier_stops_at_the_file_budget() {
    let scorer = Fake::new(&[
        (
            "index.md",
            Entry::new(
                0.9,
                &[("payments/README.md", 0.9), ("notes/ledger.md", 0.5)],
            ),
        ),
        (
            "payments/README.md",
            Entry::new(0.9, &[("notes/ledger.md", 0.9)]),
        ),
        ("notes/ledger.md", Entry::new(0.9, &[])),
    ]);

    let found = Settings::new(&["index.md"]).max_files(2).run(&scorer).await;

    assert_eq!(paths(&found), ["index.md", "payments/README.md"]);
    assert_eq!(found.calls, 2);
    // The README queued its ledger link, and the budget stopped the walk
    // before spending a call on it.
    assert_eq!(
        judged(visited(&found, "payments/README.md")),
        [link("notes/ledger.md", 0.9, true)]
    );
    assert_eq!(scorer.called(), ["index.md", "payments/README.md"]);
}

#[tokio::test]
async fn the_walk_stops_at_the_depth_budget() {
    let table: &[(&str, Entry)] = &[
        ("index.md", Entry::new(0.5, &[("payments/README.md", 0.9)])),
        (
            "payments/README.md",
            Entry::new(0.6, &[("notes/ledger.md", 0.9)]),
        ),
        ("notes/ledger.md", Entry::new(0.9, &[])),
    ];

    // One hop: the README is in, the file it links to is not.
    let scorer = Fake::new(table);
    let found = Settings::new(&["index.md"]).max_depth(1).run(&scorer).await;
    assert_eq!(paths(&found), ["payments/README.md", "index.md"]);
    assert_eq!(visited(&found, "payments/README.md").depth, 1);
    assert_eq!(
        visited(&found, "payments/README.md").via,
        [PathBuf::from("index.md")]
    );
    assert_eq!(scorer.called(), ["index.md", "payments/README.md"]);

    // No hops: the entry files are the whole reading list.
    let scorer = Fake::new(table);
    let found = Settings::new(&["index.md"]).max_depth(0).run(&scorer).await;
    assert_eq!(paths(&found), ["index.md"]);
    assert_eq!(found.calls, 1);
}

#[tokio::test]
async fn links_below_the_threshold_are_not_followed() {
    let scorer = Fake::new(&[
        (
            "index.md",
            Entry::new(
                0.9,
                &[("payments/README.md", 0.75), ("notes/ledger.md", 0.74)],
            ),
        ),
        ("payments/README.md", Entry::new(0.9, &[])),
        ("notes/ledger.md", Entry::new(0.9, &[])),
    ]);

    let found = Settings::new(&["index.md"])
        .threshold(0.75)
        .run(&scorer)
        .await;

    assert_eq!(paths(&found), ["index.md", "payments/README.md"]);
    // A scent exactly at the threshold queues; a hair below it does not, and
    // neither scent changes the file's own record.
    assert_eq!(
        judged(visited(&found, "index.md")),
        [
            link("payments/README.md", 0.75, true),
            link("notes/ledger.md", 0.74, false),
        ]
    );
    assert_eq!(scorer.called(), ["index.md", "payments/README.md"]);
}

#[tokio::test]
async fn a_scent_that_is_not_a_probability_is_not_followed() {
    let scorer = Fake::new(&[
        (
            "index.md",
            Entry::new(
                0.9,
                &[("payments/README.md", f64::NAN), ("notes/ledger.md", 1.5)],
            ),
        ),
        ("payments/README.md", Entry::new(1.0, &[])),
        ("notes/ledger.md", Entry::new(1.0, &[])),
    ]);

    let found = Settings::new(&["index.md"]).run(&scorer).await;

    // Neither is above the threshold, and neither is given a priority that
    // would put it first: both links are reported and the walk stays where it
    // was.
    assert_eq!(paths(&found), ["index.md"]);
    assert_eq!(scorer.called(), ["index.md"]);
    assert!(!followed(visited(&found, "index.md"), "payments/README.md"));
    assert!(!followed(visited(&found, "index.md"), "notes/ledger.md"));
}

#[tokio::test]
async fn a_file_keeps_the_best_path_found_by_a_file_of_its_own_round() {
    // The index queues the cutoffs page at 0.25, then the README — popped
    // first, so recorded first — finds a better path to it at 0.375. The
    // cutoffs page is popped in the same round as the README, so the round's
    // order must not decide which path it keeps.
    let table: &[(&str, Entry)] = &[
        (
            "index.md",
            Entry::new(
                0.4,
                &[("payments/README.md", 0.5), ("payments/cutoffs.md", 0.25)],
            ),
        ),
        (
            "payments/README.md",
            Entry::new(0.6, &[("payments/cutoffs.md", 0.75)]),
        ),
        ("payments/cutoffs.md", Entry::new(0.7, &[])),
    ];

    let scorer = Fake::new(table);
    let together = Settings::new(&["index.md"])
        .threshold(0.2)
        .fanout(2)
        .run(&scorer)
        .await;
    let scorer = Fake::new(table);
    let one_at_a_time = Settings::new(&["index.md"])
        .threshold(0.2)
        .fanout(1)
        .run(&scorer)
        .await;

    let cutoffs = visited(&together, "payments/cutoffs.md");
    assert_eq!(cutoffs.path_score, 0.375);
    assert_eq!(cutoffs.depth, 2);
    assert_eq!(cutoffs.scent, Some(0.75));
    assert_eq!(
        cutoffs.via,
        [
            PathBuf::from("index.md"),
            PathBuf::from("payments/README.md")
        ]
    );
    // Fanout is latency only: one round or two, the same records come back.
    assert_eq!(paths(&together), paths(&one_at_a_time));
    assert_eq!(
        visited(&one_at_a_time, "payments/cutoffs.md").path_score,
        0.375
    );
}

#[tokio::test]
async fn the_best_path_to_a_file_wins() {
    let scorer = Fake::new(&[
        (
            "index.md",
            Entry::new(
                0.9,
                &[("payments/README.md", 0.9), ("payments/settlement.md", 0.9)],
            ),
        ),
        (
            "payments/README.md",
            Entry::new(0.5, &[("payments/settlement.md", 0.8)]),
        ),
        ("payments/settlement.md", Entry::new(0.95, &[])),
    ]);

    let found = Settings::new(&["index.md"]).run(&scorer).await;

    // Settlement is one hop from the index at scent 0.9; the README's longer,
    // weaker path to it was found in the same round and queued nothing.
    let settlement = visited(&found, "payments/settlement.md");
    assert_eq!(settlement.scent, Some(0.9));
    assert_eq!(settlement.path_score, 0.9);
    assert_eq!(settlement.depth, 1);
    assert_eq!(settlement.via, [PathBuf::from("index.md")]);
    assert_eq!(
        judged(visited(&found, "payments/README.md")),
        [link("payments/settlement.md", 0.8, false)]
    );
    // One call per file, however many paths reach it.
    assert_eq!(
        scorer.called(),
        ["index.md", "payments/README.md", "payments/settlement.md"]
    );
    assert!(
        scorer.asked_twice().is_empty(),
        "asked twice: {:?}",
        scorer.asked_twice()
    );
}

#[tokio::test]
async fn a_link_out_of_the_root_is_reported_but_never_followed() {
    let scorer = Fake::new(&[
        (
            "index.md",
            Entry::new(0.9, &[("payments/README.md", 0.9), ("../outside.md", 0.99)]),
        ),
        ("payments/README.md", Entry::new(0.9, &[])),
        // On disk and judged well by the table: only the root keeps the walk
        // out of it.
        ("../outside.md", Entry::new(1.0, &[])),
    ]);

    let found = Settings::new(&["index.md"]).run(&scorer).await;

    assert_eq!(
        judged(visited(&found, "index.md")),
        [
            link("payments/README.md", 0.9, true),
            link("../outside.md", 0.99, false),
        ]
    );
    assert_eq!(paths(&found), ["index.md", "payments/README.md"]);
    // Not even asked about: it never leaves the machine.
    assert_eq!(scorer.called(), ["index.md", "payments/README.md"]);
}

#[tokio::test]
async fn a_broken_link_is_reported_and_the_walk_continues() {
    let scorer = Fake::new(&[
        (
            "index.md",
            Entry::new(
                0.9,
                &[("payments/missing.md", 0.9), ("payments/cutoffs.md", 0.8)],
            ),
        ),
        ("payments/cutoffs.md", Entry::new(0.7, &[])),
    ]);

    let found = Settings::new(&["index.md"]).run(&scorer).await;

    assert_eq!(paths(&found), ["index.md", "payments/cutoffs.md"]);
    assert_eq!(found.calls, 2);
    assert_eq!(scorer.called(), ["index.md", "payments/cutoffs.md"]);
    assert!(matches!(&found.failed[0].failure, Failure::Parse(_)));
    let failed = failures(&found);
    let [missing] = &failed[..] else {
        panic!("one file should have failed: {failed:?}");
    };
    assert_eq!(missing.0, "payments/missing.md");
    assert!(missing.1.starts_with("failed to read"));
}

#[tokio::test]
async fn a_scorer_that_fails_is_reported_and_the_walk_continues() {
    let scorer = Fake::new(&[
        (
            "index.md",
            Entry::new(
                0.9,
                &[("notes/ledger.md", 0.9), ("payments/cutoffs.md", 0.8)],
            ),
        ),
        // No judgment for the ledger: the scorer fails for that one file.
        ("payments/cutoffs.md", Entry::new(0.7, &[])),
    ]);

    let found = Settings::new(&["index.md"]).run(&scorer).await;

    assert_eq!(paths(&found), ["index.md", "payments/cutoffs.md"]);
    // A failed call was still a call.
    assert_eq!(found.calls, 3);
    assert!(matches!(
        &found.failed[0].failure,
        Failure::Score(ScorerError::MissingAnswer { id }) if id.as_str() == "notes/ledger.md"
    ));
}

#[tokio::test]
async fn a_tie_on_path_score_is_broken_by_path() {
    let scorer = Fake::new(&[
        ("index.md", Entry::new(0.9, &[])),
        ("notes/scratch.md", Entry::new(0.9, &[])),
    ]);

    // Both entries start at path score 1, and the budget of one file leaves
    // the tie to the path rather than to the order the entries were given in.
    let found = Settings::new(&["notes/scratch.md", "index.md"])
        .max_files(1)
        .run(&scorer)
        .await;

    assert_eq!(paths(&found), ["index.md"]);
    assert_eq!(scorer.called(), ["index.md"]);
}

#[tokio::test]
async fn an_entry_is_spelled_the_way_the_links_that_reach_it_are() {
    let scorer = Fake::new(&[
        ("notes/ledger.md", Entry::new(0.9, &[])),
        (
            "notes/scratch.md",
            Entry::new(0.9, &[("notes/ledger.md", 0.9)]),
        ),
    ]);

    // Four spellings of two entry files: the walk visits each once, and the
    // scratch page's link to the entry queues nothing.
    let found = Settings::new(&[
        "notes/ledger.md",
        "notes/./ledger.md",
        "notes/scratch.md/../ledger.md",
        "notes/scratch.md",
    ])
    .run(&scorer)
    .await;

    assert_eq!(scorer.called(), ["notes/ledger.md", "notes/scratch.md"]);
    assert_eq!(paths(&found), ["notes/ledger.md", "notes/scratch.md"]);
    assert_eq!(
        judged(visited(&found, "notes/scratch.md")),
        [link("notes/ledger.md", 0.9, false)]
    );
    assert!(
        scorer.asked_twice().is_empty(),
        "asked twice: {:?}",
        scorer.asked_twice()
    );
}

#[tokio::test]
async fn results_are_ordered_by_relevance_then_path() {
    let scorer = Fake::new(&[
        (
            "index.md",
            Entry::new(
                0.5,
                &[
                    ("payments/README.md", 0.9),
                    ("payments/cutoffs.md", 0.9),
                    ("payments/settlement.md", 0.9),
                    ("notes/ledger.md", 0.9),
                ],
            ),
        ),
        ("payments/README.md", Entry::new(0.5, &[])),
        ("payments/cutoffs.md", Entry::new(0.9, &[])),
        ("payments/settlement.md", Entry::new(0.5, &[])),
        ("notes/ledger.md", Entry::new(0.5, &[])),
    ]);

    let found = Settings::new(&["index.md"]).run(&scorer).await;

    assert_eq!(
        paths(&found),
        [
            "payments/cutoffs.md",
            "index.md",
            "notes/ledger.md",
            "payments/README.md",
            "payments/settlement.md"
        ]
    );
}

#[tokio::test]
async fn a_round_scores_at_most_fanout_files_at_once() {
    let table: &[(&str, Entry)] = &[
        ("index.md", Entry::new(0.9, &[])),
        ("notes/scratch.md", Entry::new(0.8, &[])),
        ("notes/reading.md", Entry::new(0.7, &[])),
    ];
    let entries = ["index.md", "notes/scratch.md", "notes/reading.md"];

    // A round that awaited its files in turn would never have two answers in
    // flight at once.
    let scorer = Fake::new(table).jittered();
    let found = Settings::new(&entries).fanout(2).run(&scorer).await;
    assert_eq!(scorer.peak(), 2);

    // Fanout is latency, not the reading list: the same three files come back,
    // one at a time.
    let scorer = Fake::new(table).jittered();
    let one_at_a_time = Settings::new(&entries).fanout(1).run(&scorer).await;
    assert_eq!(scorer.peak(), 1);
    assert_eq!(paths(&found), paths(&one_at_a_time));
    assert_eq!(
        paths(&one_at_a_time),
        ["index.md", "notes/scratch.md", "notes/reading.md"]
    );
}

/// A graph with multi-file rounds, two equal-score paths to one file, a broken
/// link and a link out of the root: everything a round's answer order could
/// disturb. The index and the README both reach settlement at scent 0.85 in the
/// same round, so which of them the record names as the way in is decided by
/// the order the round was recorded in, not by who answered first.
fn mixed_graph() -> Fake {
    Fake::new(&[
        (
            "index.md",
            Entry::new(
                0.4,
                &[
                    ("payments/README.md", 0.9),
                    ("payments/cutoffs.md", 0.6),
                    ("notes/ledger.md", 0.8),
                    ("payments/settlement.md", 0.85),
                    ("payments/missing.md", 0.7),
                    ("../outside.md", 0.99),
                ],
            ),
        ),
        (
            "payments/README.md",
            Entry::new(
                0.6,
                &[
                    ("payments/settlement.md", 0.85),
                    ("notes/ledger.md", 0.85),
                    ("payments/cutoffs.md", 0.9),
                ],
            ),
        ),
        (
            "notes/scratch.md",
            Entry::new(0.7, &[("notes/ledger.md", 0.8)]),
        ),
        (
            "payments/settlement.md",
            Entry::new(0.8, &[("payments/cutoffs.md", 0.9)]),
        ),
        ("notes/ledger.md", Entry::new(0.5, &[])),
        ("payments/cutoffs.md", Entry::new(0.7, &[])),
        ("../outside.md", Entry::new(1.0, &[])),
    ])
}

/// Everything about a run that may not depend on how fast a scorer answers.
#[derive(Debug, PartialEq)]
struct Run {
    calls: usize,
    results: Vec<VisitedFile>,
    failed: Vec<(String, String)>,
    called: Vec<String>,
}

async fn run(settings: &Settings, scorer: Fake) -> Run {
    let found = settings.run(&scorer).await;
    Run {
        calls: found.calls,
        failed: failures(&found),
        results: found.results,
        called: scorer.called(),
    }
}

#[tokio::test]
async fn the_same_input_gives_the_same_output_with_random_latency() {
    let settings = Settings::new(&["index.md", "payments/README.md", "notes/scratch.md"]).fanout(3);
    let first = run(&settings, mixed_graph()).await;
    assert!(first.results.len() >= 4, "the graph should span rounds");
    assert_eq!(first.failed.len(), 1, "one link of the graph is broken");
    assert_eq!(first.calls, 6);
    assert_eq!(
        first.called,
        [
            "index.md",
            "notes/ledger.md",
            "notes/scratch.md",
            "payments/README.md",
            "payments/cutoffs.md",
            "payments/settlement.md"
        ],
        "one call per file visited, none for the out-of-root link"
    );
    // Two entries reach it at path score 0.85 in the same round, so the round's
    // order — not the answers' order — decided the way in.
    let settlement = first
        .results
        .iter()
        .find(|file| file.path == Path::new("payments/settlement.md"))
        .expect("settlement should be visited");
    assert_eq!(settlement.path_score, 0.85);
    assert_eq!(settlement.via, [PathBuf::from("index.md")]);
    // Queued at 0.6 from the index, then reached again at 0.9 from the README:
    // the better path is the one kept.
    let cutoffs = first
        .results
        .iter()
        .find(|file| file.path == Path::new("payments/cutoffs.md"))
        .expect("cutoffs should be visited");
    assert_eq!(cutoffs.path_score, 0.9);
    assert_eq!(cutoffs.via, [PathBuf::from("payments/README.md")]);

    for attempt in 1..=10 {
        assert_eq!(
            run(&settings, mixed_graph().jittered()).await,
            first,
            "run {attempt} differed from the first"
        );
    }
}

#[tokio::test]
async fn entries_must_be_given_against_the_same_base_as_the_root() {
    let scorer = Fake::new(&[]);
    let root = root();
    let entries = [PathBuf::from("tests/fixtures/wiki/index.md")];
    let config = Config {
        query: QUERY,
        entries: &entries,
        seeds: &[],
        root: &root,
        max_files: 8,
        max_depth: 6,
        fanout: 4,
        threshold: 0.6,
    };

    assert!(matches!(
        traverse(&config, &scorer).await,
        Err(TraverseError::BaseMismatch { .. })
    ));
}

/// A seed is an entry file the walk was not given: it starts at path score 1
/// with no scent and no `via` path, expands like any entry file, and the result
/// says it was a seed rather than a file a link reached.
#[tokio::test]
async fn a_seed_enters_the_walk_like_an_entry_file() {
    let scorer = Fake::new(&[
        ("index.md", Entry::new(0.3, &[("payments/cutoffs.md", 0.9)])),
        (
            "notes/scratch.md",
            Entry::new(1.0, &[("notes/ledger.md", 0.9)]),
        ),
        ("notes/ledger.md", Entry::new(0.5, &[])),
        ("payments/cutoffs.md", Entry::new(0.7, &[])),
    ]);

    let found = Settings::new(&["index.md"])
        .seeds(&["notes/scratch.md"])
        .run(&scorer)
        .await;

    let scratch = visited(&found, "notes/scratch.md");
    assert!(scratch.seeded, "a seed says so");
    assert_eq!(scratch.path_score, 1.0);
    assert_eq!(scratch.depth, 0);
    assert_eq!(scratch.scent, None, "no link reached it");
    assert_eq!(scratch.via, Vec::<PathBuf>::new());
    assert!(
        followed(scratch, "notes/ledger.md"),
        "a seed's links are followed like an entry file's"
    );

    let ledger = visited(&found, "notes/ledger.md");
    assert_eq!(ledger.via, [PathBuf::from("notes/scratch.md")]);
    assert!(!ledger.seeded, "a link reached this one");
    assert!(
        !visited(&found, "index.md").seeded,
        "the entry file is not a seed"
    );
    assert_eq!(
        scorer.called(),
        [
            "index.md",
            "notes/ledger.md",
            "notes/scratch.md",
            "payments/cutoffs.md"
        ],
        "only the files the walk reached were judged"
    );
    assert!(
        scorer.asked_twice().is_empty(),
        "one call per file, however it was entered"
    );
}

/// A file named as both an entry file and a seed stays the entry file the
/// caller named: entries are queued first, and nothing reaches a file at a
/// better path score than 1.
#[tokio::test]
async fn a_seed_that_is_also_an_entry_file_stays_an_entry() {
    let scorer = Fake::new(&[("index.md", Entry::new(0.3, &[]))]);

    let found = Settings::new(&["index.md"])
        .seeds(&["index.md"])
        .run(&scorer)
        .await;

    assert_eq!(paths(&found), ["index.md"]);
    assert!(!visited(&found, "index.md").seeded);
    assert_eq!(found.calls, 1, "judged once, not once per spelling");
}
