//! Traversal tests against the fixture wiki in `tests/fixtures/`, driven by a
//! fake scorer: the contract from
//! [#6](https://github.com/mikekelly/s1m/issues/6).
//!
//! The fake answers from a fixed table, so what is under test is the walk —
//! priority, budgets, pruning, the visited set — and not the model. Its
//! answers can be delayed by a varying amount, which is how the determinism
//! test makes the order answers arrive in irrelevant.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::time::sleep;

use s1m::cache::{Cacheable, CachedScorer};
use s1m::cli::Uncached;
use s1m::ignore::Ignore;
use s1m::parse::{self, ParsedFile, relative_to_root};
use s1m::scorer::{FileJudgment, LinkJudgment, Scorer, ScorerError, SectionJudgment};
use s1m::trace::{Reason, Trace};
use s1m::traverse::{Admission, Config, Failure, Traversal, TraverseError, VisitedFile, traverse};

const QUERY: &str = "settlement timing for instant payouts";

/// The round size `main.rs` hands the walk: the tests about the round boundary
/// walk at the round size a caller gets.
const FANOUT: usize = 8;

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
    mixer: AtomicUsize,
    /// A trace to look at while this fake is answering: what a run that is
    /// killed mid-answer has already written about it.
    peek: Option<PathBuf>,
}

impl Fake {
    fn new(table: &[(&'static str, Entry)]) -> Fake {
        let mix = SystemTime::now()
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
            mixer: AtomicUsize::new(mix),
            peek: None,
        }
    }

    /// Watches `path` while answering: inside every call, the file's request is
    /// on disk and its answer is not, which is the state a killed run leaves.
    fn peeking(mut self, path: PathBuf) -> Fake {
        self.peek = Some(path);
        self
    }

    /// Varies each answer's latency, so answers come back in a different order
    /// from the one they were asked in.
    fn jittered(mut self) -> Fake {
        self.jitter = true;
        self
    }

    /// The same fake over another root, so a walk over a different tree has its
    /// paths spelled back the same way.
    fn over(mut self, root: PathBuf) -> Fake {
        self.root = root;
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
        self.answer(file).await
    }
}

/// The same fake as a scorer a cache can sit in front of, which is what makes
/// the requests a run makes visible to a trace: one request per file, built
/// from the query and the path, and an answer that reports how long it took.
#[async_trait::async_trait]
impl Cacheable for Fake {
    type Request = String;
    /// What the answer cost: how long the fake's own call took, which is what a
    /// trace reports as `latency_ms` and what a jittered run varies.
    type Detail = Duration;

    fn request(&self, query: &str, file: &ParsedFile) -> Result<String, ScorerError> {
        Ok(format!("{query}\n{}", file.path.display()))
    }

    fn key(&self, request: &String) -> Result<Vec<u8>, ScorerError> {
        Ok(request.as_bytes().to_vec())
    }

    async fn call(
        &self,
        _request: &String,
        file: &ParsedFile,
    ) -> Result<(FileJudgment, Duration), ScorerError> {
        let started = Instant::now();
        let judgment = self.answer(file).await?;
        Ok((judgment, started.elapsed()))
    }

    /// One request per file: nothing this fake judges is split across posts,
    /// which is a real scorer's business ([`s1m::jev::JevScorer`]).
    fn posts(&self, _request: &String) -> usize {
        1
    }

    fn latency(detail: &Duration) -> Duration {
        *detail
    }
}

impl Fake {
    /// What the table says about `file`: the answer a request for it gets, and
    /// the delay per answer the walk's determinism tests turn on.
    async fn answer(&self, file: &ParsedFile) -> Result<FileJudgment, ScorerError> {
        let path = relative_to_root(&self.root, &file.path);
        let name = path.to_string_lossy().into_owned();
        if let Some(trace) = &self.peek {
            self.peek_at(trace, &name);
        }
        let Some(entry) = self.table.get(name.as_str()) else {
            return Err(ScorerError::MissingAnswer { id: name });
        };
        entry.calls.fetch_add(1, Ordering::SeqCst);

        let in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(in_flight, Ordering::SeqCst);
        if self.jitter {
            sleep(Duration::from_millis(delay(&self.mixer))).await;
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
                    keep: true,
                })
                .collect(),
        })
    }

    /// Stops the walk under a trace file's own feet: a file's request is on
    /// disk before the answer to it is bought, which is what a run killed
    /// mid-answer leaves behind.
    ///
    /// Read line by line rather than as a whole, because a trace is JSON Lines
    /// and not one document: a reader that needs the last line to parse has a
    /// trace it cannot follow while the run that wrote it is still going.
    fn peek_at(&self, trace: &Path, file: &str) {
        let written = fs::read_to_string(trace)
            .unwrap_or_else(|error| panic!("{}: {error}", trace.display()));
        let records: Vec<Value> = written
            .lines()
            .map(|line| {
                serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}"))
            })
            .collect();
        let about = |event: &str| {
            records
                .iter()
                .filter(|record| record["event"] == event && record["path"] == file)
                .count()
        };
        assert_eq!(
            about("requested"),
            1,
            "{file}'s request should be on disk before its answer is bought: {written}"
        );
        assert_eq!(
            about("answered"),
            0,
            "{file} has no answer yet, so none should be written: {written}"
        );
    }
}

// ------------------------------------------------------------- written trees

/// A tree of pages written under the system temp directory, removed when it is
/// dropped.
///
/// The fixture wiki is nine pages. A test about a hub of thirty links, or a
/// budget of twenty-five files, needs a tree bigger than any fixture worth
/// keeping in the repository: the test that needs one writes it, so the shape
/// it asserts on is in the test itself. The name carries the process and a
/// counter, so two test binaries running at once never share one.
struct Tree {
    dir: PathBuf,
}

impl Tree {
    fn new(label: &str) -> Tree {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "s1m-traverse-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a tree under the system temp directory");
        Tree { dir }
    }

    /// Writes one page: `name` relative to the root, whose links to each of
    /// `links` — also relative to the root — sit under a heading, so the page
    /// has a section and one link per target.
    fn page(&self, name: &str, links: &[String]) -> &Tree {
        let mut body = format!("# {name}\n\n");
        for link in links {
            body.push_str(&format!("- [{link}]({link})\n"));
        }
        fs::write(self.dir.join(name), body).expect("a page in the tree");
        self
    }

    fn root(&self) -> PathBuf {
        self.dir.clone()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// The scorer for a written tree: one relevance for every page, one scent for
/// every link, and a scent of its own for a target the test names.
///
/// A table cannot hold a tree this size — thirty links is thirty rows, and the
/// tree would be written twice — so the scents are a rule and the tree's own
/// shape says the rest. What it judges is the file's own links, the way the
/// table fake does: the walk can only follow a link a page makes. It counts the
/// calls per file as the table fake does, because one call per file is the
/// walk's contract whatever the round size.
struct ByRule {
    relevance: f64,
    scent: f64,
    named: HashMap<String, f64>,
    /// The targets the scorer judges and does not keep: what a walk following
    /// the scorer's verdict
    /// ([`s1m::traverse::Admission::Scorer`]) passes over however strong the
    /// scent beside it.
    unkept: HashSet<String>,
    calls: Mutex<HashMap<String, usize>>,
}

impl ByRule {
    fn new(relevance: f64, scent: f64) -> ByRule {
        ByRule {
            relevance,
            scent,
            named: HashMap::new(),
            unkept: HashSet::new(),
            calls: Mutex::new(HashMap::new()),
        }
    }

    /// Judges a link to `target` at `scent` rather than at the default.
    fn link(mut self, target: &str, scent: f64) -> ByRule {
        self.named.insert(target.to_string(), scent);
        self
    }

    /// Judges a link to `target` and does not keep it, which is the verdict a
    /// walk following the scorer reads instead of the scent.
    fn unkept(mut self, target: &str) -> ByRule {
        self.unkept.insert(target.to_string());
        self
    }

    /// The files the scorer was asked about more than once: there must be none,
    /// however many rounds a file waits through.
    fn asked_twice(&self) -> Vec<String> {
        let calls = self.calls.lock().expect("the fake's call log");
        let mut twice: Vec<String> = calls
            .iter()
            .filter(|(_, calls)| **calls > 1)
            .map(|(name, _)| name.clone())
            .collect();
        twice.sort();
        twice
    }
}

#[async_trait::async_trait]
impl Scorer for ByRule {
    async fn score(&self, query: &str, file: &ParsedFile) -> Result<FileJudgment, ScorerError> {
        assert_eq!(query, QUERY, "the traversal's query reaches the scorer");
        *self
            .calls
            .lock()
            .expect("the fake's call log")
            .entry(file.path.display().to_string())
            .or_insert(0) += 1;
        Ok(FileJudgment {
            relevance: self.relevance,
            // The heading and the range are the fake's own and deliberately
            // not the parser's, as in the table fake.
            sections: file
                .sections
                .iter()
                .enumerate()
                .map(|(index, _)| SectionJudgment {
                    heading: None,
                    lines: [0, 0],
                    score: 0.5 + index as f64 / 100.0,
                })
                .collect(),
            links: file
                .links
                .iter()
                .map(|link| LinkJudgment {
                    target: link.target.clone(),
                    scent: self
                        .named
                        .get(&link.target.to_string_lossy().into_owned())
                        .copied()
                        .unwrap_or(self.scent),
                    keep: !self
                        .unkept
                        .contains(&link.target.to_string_lossy().into_owned()),
                })
                .collect(),
        })
    }
}

/// How many spokes the crowd's hub links.
const CROWD: usize = 30;

/// The crowd: an entry page with a hub's shape — one guide and thirty spokes —
/// where the guide reaches the leaf two hops from the entry.
///
/// The scents are the point: the guide at 0.9 from the entry, each of the
/// thirty spokes at 0.65, and the leaf at 0.9 from the guide. The leaf's path
/// score is 0.81, better than a spoke one hop down and worse than the guide, so
/// a walk that follows the path score reaches it after the guide whatever the
/// hub crowds the frontier with.
fn crowd(label: &str) -> Tree {
    let tree = Tree::new(label);
    let mut hub: Vec<String> = vec!["guide.md".to_string()];
    hub.extend((1..=CROWD).map(|n| format!("spoke-{n:02}.md")));
    tree.page("index.md", &hub);
    tree.page("guide.md", &["leaf.md".to_string()]);
    tree.page("leaf.md", &[]);
    for n in 1..=CROWD {
        tree.page(&format!("spoke-{n:02}.md"), &[]);
    }
    tree
}

/// The crowd's scents: every link 0.65, except the guide's and the leaf's.
fn crowd_scorer() -> ByRule {
    ByRule::new(0.5, 0.65)
        .link("guide.md", 0.9)
        .link("leaf.md", 0.9)
}

/// The below-threshold tree: the entry links four section hubs at 0.7, and the
/// first of them — `hub.md` — carries a crowd of thirty spokes and a leaf at
/// 0.85.
///
/// The leaf is the case the private eval was worried about: its path score is
/// 0.7 × 0.85 = 0.595, below the 0.6 threshold, while the link's own scent
/// clears it. The link is admitted on its own scent and the path score decides
/// only where it waits.
fn section_hubs(label: &str) -> Tree {
    let tree = Tree::new(label);
    let hubs: Vec<String> = (1..=3)
        .map(|n| format!("section-{n}.md"))
        .chain(std::iter::once("hub.md".to_string()))
        .collect();
    tree.page("index.md", &hubs);
    let mut hub: Vec<String> = vec!["leaf.md".to_string()];
    hub.extend((1..=CROWD).map(|n| format!("spoke-{n:02}.md")));
    tree.page("hub.md", &hub);
    tree.page("leaf.md", &[]);
    for n in 1..=3 {
        tree.page(&format!("section-{n}.md"), &[]);
    }
    for n in 1..=CROWD {
        tree.page(&format!("spoke-{n:02}.md"), &[]);
    }
    tree
}

/// The below-threshold scents: every link 0.7, except the hub's to the leaf.
fn section_hubs_scorer() -> ByRule {
    ByRule::new(0.5, 0.7).link("leaf.md", 0.85)
}

/// A different delay per call, mixed from a counter read off the clock so no
/// two runs see the same order. It only shuffles when answers become ready:
/// nothing in a result may depend on it.
fn delay(mix: &AtomicUsize) -> u64 {
    let mut value = mix.fetch_add(1, Ordering::SeqCst) as u64 | 1;
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
    max_files: usize,
    max_depth: usize,
    fanout: usize,
    /// How a link is admitted, and how many files are visited at one depth:
    /// the two rules the relative judge is followed by, and the walk that
    /// ships.
    admission: Admission,
    beam: Option<usize>,
    ignore: Ignore,
}

impl Settings {
    fn new(entries: &[&str]) -> Settings {
        Settings::over(root(), entries)
    }

    /// The same walk over another root, entry files and all: the tree the root
    /// names is the one that is walked, `.s1mignore` and all.
    fn over(root: PathBuf, entries: &[&str]) -> Settings {
        Settings {
            entries: entries.iter().map(|entry| root.join(entry)).collect(),
            ignore: Ignore::at(&root).expect("the fixture's .s1mignore should parse"),
            root,
            max_files: 8,
            max_depth: 6,
            fanout: 4,
            admission: Admission::Threshold(0.6),
            beam: None,
        }
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
        self.admission = Admission::Threshold(threshold);
        self
    }

    /// A walk that follows what the scorer kept rather than a threshold.
    fn kept(mut self) -> Self {
        self.admission = Admission::Scorer;
        self
    }

    /// A walk that visits at most `beam` files at one depth.
    fn beam(mut self, beam: usize) -> Self {
        self.beam = Some(beam);
        self
    }

    async fn run(&self, scorer: &dyn Scorer) -> Traversal {
        self.traced(scorer, None).await
    }

    /// The same walk, reporting to `trace` when the test asked for one.
    async fn traced(&self, scorer: &dyn Scorer, trace: Option<&Trace>) -> Traversal {
        let config = Config {
            query: QUERY,
            entries: &self.entries,
            root: &self.root,
            max_files: self.max_files,
            max_depth: self.max_depth,
            fanout: self.fanout,
            admission: self.admission,
            beam: self.beam,
            ignore: &self.ignore,
            trace,
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

    // A budget of one is one file beyond the entry the caller named, whichever
    // path reaches it: the ledger link was queued at 0.5 from the index and at
    // 0.9 from the README, and the walk has no room for it.
    let found = Settings::new(&["index.md"]).max_files(1).run(&scorer).await;

    assert_eq!(paths(&found), ["index.md", "payments/README.md"]);
    assert_eq!(found.calls, 2);
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
        (
            "index.md",
            Entry::new(
                0.9,
                &[("payments/README.md", 0.9), ("notes/ledger.md", 0.9)],
            ),
        ),
        ("payments/README.md", Entry::new(0.9, &[])),
        ("notes/ledger.md", Entry::new(0.9, &[])),
    ]);

    // Both of the index's links are judged at 0.9, and a budget of one file
    // beyond the entry leaves the tie to the path rather than to the order the
    // links appear in.
    let found = Settings::new(&["index.md"]).max_files(1).run(&scorer).await;

    assert_eq!(paths(&found), ["index.md", "notes/ledger.md"]);
    assert_eq!(scorer.called(), ["index.md", "notes/ledger.md"]);
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

/// A hub link at 0.7 leading to a leaf link at 0.85: the leaf's path score is
/// 0.595, below the 0.6 threshold, and it is visited within a budget of five.
///
/// The link is admitted on its own scent — 0.85 clears 0.6 — and the path score
/// only orders the frontier. Four section hubs at 0.7 outrank it and take the
/// first four of the five files the budget buys, so the leaf is reached at the
/// last one: a round that treated the hub's answer as settled, or a budget that
/// counted the entry file, would stop before it.
#[tokio::test]
async fn a_leaf_below_the_threshold_path_score_is_visited_within_the_budget() {
    let tree = section_hubs("below-threshold");
    let found = Settings::over(tree.root(), &["index.md"])
        .max_files(5)
        .fanout(FANOUT)
        .run(&section_hubs_scorer())
        .await;

    let leaf = visited(&found, "leaf.md");
    assert_eq!(leaf.depth, 2);
    assert_eq!(leaf.scent, Some(0.85));
    assert_eq!(leaf.path_score, 0.7 * 0.85);
    assert!(
        leaf.path_score < 0.6,
        "the path score the threshold would have dropped it on"
    );
    assert_eq!(
        leaf.via,
        [PathBuf::from("index.md"), PathBuf::from("hub.md")]
    );
    assert!(
        followed(visited(&found, "hub.md"), "leaf.md"),
        "the link was admitted on its own 0.85"
    );
    assert_eq!(found.results.len(), 6, "the entry file is free");
}

/// A hub of thirty links at 0.65 and one leaf at 0.9 two hops down: the leaf is
/// visited within a budget of five.
///
/// The entry links the guide and the thirty spokes, and the guide links the
/// leaf at 0.81. The spokes are popped into the guide's round at 0.65, and a
/// round that visited everything it popped would spend the whole budget on
/// them: the leaf is better than every spoke and gets its turn first.
#[tokio::test]
async fn a_leaf_two_hops_down_is_reached_ahead_of_a_hubs_crowd() {
    let tree = crowd("crowd");
    let found = Settings::over(tree.root(), &["index.md"])
        .max_files(5)
        .fanout(FANOUT)
        .run(&crowd_scorer())
        .await;

    let leaf = visited(&found, "leaf.md");
    assert_eq!(leaf.depth, 2);
    assert_eq!(leaf.scent, Some(0.9));
    assert_eq!(leaf.path_score, 0.9 * 0.9);
    assert_eq!(
        leaf.via,
        [PathBuf::from("index.md"), PathBuf::from("guide.md")]
    );
    assert_eq!(
        found.results.iter().filter(|file| file.depth > 0).count(),
        5,
        "five files beyond the entry, and no more"
    );
}

/// Fanout is the concurrency cap and nothing else: the crowd walked a file at a
/// time and eight at a time visits the same files, so the round a file was
/// scored in cannot decide whether it is in the reading list.
///
/// What the round size does change is what a round buys: the files of a round
/// are scored together, so one the budget never gets to — a file the round
/// overtook and the budget then stopped short of — was still paid for, and
/// `calls` is larger at the larger round size. It is the same file, never a
/// second judgment for a file that was already asked about.
#[tokio::test]
async fn fanout_does_not_decide_which_files_are_visited() {
    let tree = crowd("fanout");
    let one_at_a_time = Settings::over(tree.root(), &["index.md"])
        .max_files(5)
        .fanout(1)
        .run(&crowd_scorer())
        .await;
    let eight_at_a_time_scorer = crowd_scorer();
    let eight_at_a_time = Settings::over(tree.root(), &["index.md"])
        .max_files(5)
        .fanout(FANOUT)
        .run(&eight_at_a_time_scorer)
        .await;

    assert_eq!(paths(&one_at_a_time), paths(&eight_at_a_time));
    assert_eq!(visited(&one_at_a_time, "leaf.md").path_score, 0.9 * 0.9);
    assert_eq!(visited(&eight_at_a_time, "leaf.md").path_score, 0.9 * 0.9);
    assert_eq!(
        one_at_a_time.calls,
        one_at_a_time.results.len(),
        "a file at a time buys one judgment per file it visits"
    );
    assert!(eight_at_a_time.calls >= one_at_a_time.calls);
    assert!(
        eight_at_a_time_scorer.asked_twice().is_empty(),
        "asked twice: {:?}",
        eight_at_a_time_scorer.asked_twice()
    );
}

/// The budget counts the files the walk judges beyond the entry files: eight
/// entries at `--max-files 25` is twenty-five pages past them, with every entry
/// in the list besides.
#[tokio::test]
async fn the_budget_counts_files_beyond_the_entry_files() {
    let tree = Tree::new("budget");
    let entries: Vec<String> = (1..=8).map(|n| format!("entry-{n}.md")).collect();
    for (index, entry) in entries.iter().enumerate() {
        let links: Vec<String> = (1..=5)
            .map(|n| format!("page-{}-{n}.md", index + 1))
            .collect();
        tree.page(entry, &links);
        for link in &links {
            tree.page(link, &[]);
        }
    }
    let names: Vec<&str> = entries.iter().map(String::as_str).collect();

    let found = Settings::over(tree.root(), &names)
        .max_files(25)
        .run(&ByRule::new(0.5, 0.9))
        .await;

    assert_eq!(
        found.results.iter().filter(|file| file.depth > 0).count(),
        25,
        "`--max-files` is what the walk spends beyond the entry files"
    );
    assert_eq!(found.results.len(), 33, "the eight entries are free");
    for entry in &entries {
        assert_eq!(visited(&found, entry).depth, 0, "{entry} is an entry file");
    }

    // A budget of none is the entry files alone, and buys nothing.
    let found = Settings::over(tree.root(), &names)
        .max_files(0)
        .run(&ByRule::new(0.5, 0.9))
        .await;

    assert_eq!(paths(&found).len(), 8);
    assert_eq!(found.calls, 8);
}

/// With no budget the walk visits everything it admits: the guide, the leaf two
/// hops down, and each of the hub's thirty spokes, one judgment apiece.
///
/// This is the walk the private eval ran with `--max-files 100000`, in
/// miniature: nothing about the round may end it while the frontier holds a
/// file — not a round that admitted nothing, not an answer of its own round
/// left unrecorded — so the only thing that stops it is an empty frontier,
/// `--max-depth`, or a link the threshold, the root or a better path refused.
#[tokio::test]
async fn the_walk_with_no_budget_visits_everything_it_admits() {
    let tree = crowd("uncapped");
    let scorer = ByRule::new(0.5, 0.65)
        .link("guide.md", 0.85)
        .link("leaf.md", 0.85);

    let found = Settings::over(tree.root(), &["index.md"])
        .max_files(usize::MAX)
        .fanout(FANOUT)
        .run(&scorer)
        .await;

    assert_eq!(
        found.results.len(),
        3 + CROWD,
        "the entry, the guide, the leaf and every spoke"
    );
    assert_eq!(found.calls, found.results.len(), "one judgment each");
    assert_eq!(visited(&found, "leaf.md").depth, 2);
    for n in 1..=CROWD {
        visited(&found, &format!("spoke-{n:02}.md"));
    }
    assert!(found.failed.is_empty());
    assert!(
        scorer.asked_twice().is_empty(),
        "asked twice: {:?}",
        scorer.asked_twice()
    );
}

/// A file a round overtook is not visited, so the walk can end with a judgment
/// it bought and never used. One that answered is what the round spent; one that
/// failed is the hole in the ranking it would have been had the file had its
/// turn, and the walk reports it either way — it was reached, it was paid for,
/// and the caller is owed the line that says why it is not in the list.
#[tokio::test]
async fn a_judgment_that_failed_and_was_never_used_is_still_reported() {
    let scorer = Fake::new(&[
        (
            "index.md",
            Entry::new(
                0.5,
                &[("notes/ledger.md", 0.9), ("payments/cutoffs.md", 0.8)],
            ),
        ),
        (
            "notes/ledger.md",
            Entry::new(0.5, &[("payments/settlement.md", 0.95)]),
        ),
        // No judgment for the cutoffs page: the scorer fails for that one file,
        // and the settlement link the ledger queues at 0.855 outranks it, so the
        // round holds the failed answer and the budget is spent elsewhere.
        ("payments/settlement.md", Entry::new(0.6, &[])),
    ]);

    let found = Settings::new(&["index.md"])
        .max_files(2)
        .fanout(FANOUT)
        .run(&scorer)
        .await;

    assert_eq!(
        paths(&found),
        ["payments/settlement.md", "index.md", "notes/ledger.md"]
    );
    // Both files beyond the entry were visited; the third was judged, failed,
    // and did not spend a place.
    assert_eq!(found.calls, 4);
    assert_eq!(
        failures(&found)
            .into_iter()
            .map(|(path, _)| path)
            .collect::<Vec<_>>(),
        ["payments/cutoffs.md"]
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
    let ignore = Ignore::none();
    let config = Config {
        query: QUERY,
        entries: &entries,
        root: &root,
        max_files: 8,
        max_depth: 6,
        fanout: 4,
        admission: Admission::Threshold(0.6),
        beam: None,
        ignore: &ignore,
        trace: None,
    };

    assert!(matches!(
        traverse(&config, &scorer).await,
        Err(TraverseError::BaseMismatch { .. })
    ));
}

/// The root's `.s1mignore` is part of the walk and not a sieve over what it
/// found: a link whose target it matches is out of the file before the scorer
/// is asked, so the target is never scored — its path never reaches the model
/// and its text is never read for a preview — and the reading list has no link
/// to report, however sure the model would have been about it.
#[tokio::test]
async fn a_matched_link_target_is_out_of_the_file_before_it_is_scored() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ignore");
    let settings = Settings::over(root.clone(), &["entry.md"]);
    let scorer = Fake::new(&[
        (
            "entry.md",
            Entry::new(0.9, &[("public.md", 0.9), ("private/vault.md", 0.95)]),
        ),
        ("public.md", Entry::new(0.5, &[])),
        ("private/vault.md", Entry::new(1.0, &[])),
    ])
    .over(root);

    let traversal = settings.run(&scorer).await;

    assert_eq!(
        paths(&traversal),
        ["entry.md", "public.md"],
        "the matched page is not visited, whatever the walk would have scored it"
    );
    assert_eq!(
        scorer.called(),
        ["entry.md", "public.md"],
        "the matched page is never asked about"
    );
    let links: Vec<&Path> = visited(&traversal, "entry.md")
        .links
        .iter()
        .map(|link| link.target.as_path())
        .collect();
    assert_eq!(
        links,
        [Path::new("public.md")],
        "the link is gone from the judgment and from the result"
    );
}

/// Two ways on from the entry: one the scorer judges strong and does not keep,
/// and one it judges weak and keeps.
fn two_ways(label: &str) -> Tree {
    let tree = Tree::new(label);
    tree.page("index.md", &["a.md".to_string(), "b.md".to_string()]);
    tree.page("a.md", &[]);
    tree.page("b.md", &[]);
    tree
}

/// A walk whose links are admitted by the scorer's own verdict rather than by a
/// threshold follows what the scorer kept and not what it scored: a scent of 0.9
/// the scorer passed over is not followed, and a scent of 0.1 it kept is, at the
/// score the product of the scents gives it.
///
/// This is the rule the relative judge is followed by
/// ([#47](https://github.com/mikekelly/s1m/issues/47)): a Choice share means
/// something only beside the options it was weighed against, so the number
/// cannot be compared to a threshold and the scorer's answer is what the walk
/// reads.
#[tokio::test]
async fn a_link_the_scorer_kept_is_followed_whatever_its_scent() {
    let scorer = ByRule::new(0.5, 0.9).link("b.md", 0.1).unkept("a.md");

    let found = Settings::over(two_ways("kept").root(), &["index.md"])
        .kept()
        .run(&scorer)
        .await;

    assert_eq!(
        paths(&found),
        ["b.md", "index.md"],
        "the page the scorer kept, and the entry the caller named"
    );
    assert_eq!(
        visited(&found, "b.md").path_score,
        0.1,
        "queued at the product of the scents, whatever decided it was worth queueing"
    );
    assert_eq!(
        links(visited(&found, "index.md")),
        [link("a.md", 0.9, false), link("b.md", 0.1, true),],
        "the strong link the scorer passed over, and the weak one it kept"
    );
}

/// A fan: an entry page that links five leaves, each at its own scent, so a beam
/// has something to choose between at one depth.
fn fan(label: &str) -> Tree {
    let tree = Tree::new(label);
    let leaves: Vec<String> = (1..=5).map(|n| format!("leaf-{n}.md")).collect();
    tree.page("index.md", &leaves);
    for leaf in &leaves {
        tree.page(leaf, &[]);
    }
    tree
}

/// The fan's scents: every leaf above the threshold, in the leaves' own order.
fn fan_scorer() -> ByRule {
    ByRule::new(0.5, 0.9)
        .link("leaf-2.md", 0.8)
        .link("leaf-3.md", 0.7)
        .link("leaf-4.md", 0.65)
        .link("leaf-5.md", 0.61)
}

/// A beam is a budget of the same kind as `max_files`, one depth at a time:
/// every link still queues its target — the reading list says so — and only the
/// best two of them are visited at that depth, because the walk has no turn left
/// for the rest.
#[tokio::test]
async fn a_beam_visits_at_most_that_many_files_at_one_depth() {
    let scorer = fan_scorer();

    let found = Settings::over(fan("beam").root(), &["index.md"])
        .beam(2)
        .run(&scorer)
        .await;

    assert_eq!(
        paths(&found),
        ["index.md", "leaf-1.md", "leaf-2.md"],
        "the two best paths at depth one, and nothing else at that depth"
    );
    assert_eq!(
        links(visited(&found, "index.md")),
        [
            link("leaf-1.md", 0.9, true),
            link("leaf-2.md", 0.8, true),
            link("leaf-3.md", 0.7, true),
            link("leaf-4.md", 0.65, true),
            link("leaf-5.md", 0.61, true),
        ],
        "every link queues its target: the beam is what the walk visits, not what it follows"
    );
    assert_eq!(
        found.calls, 3,
        "the entry and the two the beam had a turn for"
    );
}

/// A beam does not cost the walk its determinism: what is visited is a function
/// of the walk's own decisions and not of how many files a round scores at once,
/// so the same tree walked a file at a time and four at a time gives one answer.
#[tokio::test]
async fn a_beam_does_not_depend_on_the_round_size() {
    let one = Settings::over(fan("beam-one").root(), &["index.md"])
        .beam(2)
        .fanout(1)
        .run(&fan_scorer())
        .await;
    let four = Settings::over(fan("beam-four").root(), &["index.md"])
        .beam(2)
        .fanout(4)
        .run(&fan_scorer())
        .await;

    assert_eq!(
        paths(&one),
        paths(&four),
        "the same files, whatever the round size"
    );
    assert_eq!(
        links(visited(&one, "index.md")),
        links(visited(&four, "index.md")),
        "and the same verdict on every link"
    );
    assert_eq!(one.calls, four.calls, "for the same calls");
}

/// A page the walk cannot judge still spends the turn the beam reserved for it:
/// the reservation a round makes at a depth is realized whether or not the file
/// could be judged, so which paths a depth drops is a function of the pop order
/// and not of how many files a round happened to hold.
///
/// The tree makes that visible: at a beam of one, the broken link pops first,
/// takes the depth's turn and is reported as skipped, and the good path behind
/// it is dropped. Same tree, same walk, whatever the round size.
#[tokio::test]
async fn a_file_the_walk_cannot_judge_still_spends_its_beam_turn() {
    let tree = Tree::new("beam-broken");
    tree.page(
        "index.md",
        &["broken.md".to_string(), "good.md".to_string()],
    );
    tree.page("good.md", &[]);
    let scorer = ByRule::new(0.5, 0.9).link("good.md", 0.85);

    let one = Settings::over(tree.root(), &["index.md"])
        .beam(1)
        .fanout(1)
        .run(&scorer)
        .await;
    let four = Settings::over(tree.root(), &["index.md"])
        .beam(1)
        .fanout(4)
        .run(&scorer)
        .await;

    assert_eq!(
        links(visited(&one, "index.md")),
        [link("broken.md", 0.9, true), link("good.md", 0.85, true)],
        "both links queue: the beam is what the walk visits"
    );
    assert_eq!(
        paths(&one),
        ["index.md"],
        "the depth's one turn went to a page that could not be judged"
    );
    assert!(
        failures(&one).iter().any(|(path, _)| path == "broken.md"),
        "and the page it went to is reported: {:?}",
        failures(&one)
    );
    assert_eq!(paths(&one), paths(&four), "the same at any round size");
    assert_eq!(failures(&one), failures(&four));
    assert_eq!(one.calls, four.calls);
}

// ----------------------------------------------------------------- the trace

/// The trace file a [`Traced`] writes, named inside its own directory.
const TRACE: &str = "trace.jsonl";

/// A trace file of one test's own, and what is in it so far.
///
/// The file lives in a [`Tree`]'s directory, which is a temp directory the test
/// holds: a trace is read while the walk that wrote it is still around, and one
/// test's trace must not be read by another.
///
/// One trace and not two: the walk reports its own events to it and the judge
/// reports the requests and answers to the same file, which is what `main.rs`
/// wires for `--trace`.
struct Traced {
    dir: Tree,
    trace: Arc<Trace>,
}

impl Traced {
    /// A trace for a walk over `root`, cut at the reading list's `threshold`.
    fn new(label: &str, root: &Path, threshold: f64) -> Traced {
        let dir = Tree::new(label);
        let trace = Trace::create(&dir.root().join(TRACE), root, threshold).expect("a trace file");
        Traced {
            dir,
            trace: Arc::new(trace),
        }
    }

    /// The file the records go to.
    fn path(&self) -> PathBuf {
        self.dir.root().join(TRACE)
    }

    /// What the walk reports to.
    fn walk(&self) -> Option<&Trace> {
        Some(&self.trace)
    }

    /// What a judge reports to: the same trace the walk writes.
    fn sink(&self) -> Option<Arc<Trace>> {
        Some(Arc::clone(&self.trace))
    }

    /// The scorer as the run's judge, reporting its requests and answers to this
    /// trace — what `main.rs` builds for a `--no-cache` run with `--trace`.
    fn judge<S: Cacheable>(&self, scorer: S) -> Uncached<S> {
        Uncached::new(scorer).with_trace(self.sink())
    }

    /// The records written so far, each line parsed on its own: a trace is JSON
    /// Lines, and every line of it stands alone.
    fn records(&self) -> Vec<Value> {
        fs::read_to_string(self.path())
            .expect("the trace file should be readable")
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
            .collect()
    }

    /// Every record as `event path`, or `event source -> target` for one about a
    /// link, which is the order a test holds the trace to.
    fn order(&self) -> Vec<String> {
        self.records()
            .iter()
            .map(|record| {
                let event = record["event"].as_str().expect("an event name");
                match record["path"].as_str() {
                    Some(path) => format!("{event} {path}"),
                    None => format!(
                        "{event} {} -> {}",
                        record["source"].as_str().expect("a source"),
                        record["target"].as_str().expect("a target")
                    ),
                }
            })
            .collect()
    }

    /// The one record of `event` about the file `path`.
    fn record(&self, event: &str, path: &str) -> Value {
        let records = self.records();
        records
            .iter()
            .find(|record| record["event"] == event && record["path"] == path)
            .unwrap_or_else(|| panic!("{event} {path} should be in the trace: {records:?}"))
            .clone()
    }

    /// The one record of `event` about the link from `source` to `target`.
    fn link(&self, event: &str, source: &str, target: &str) -> Value {
        let records = self.records();
        records
            .iter()
            .find(|record| {
                record["event"] == event && record["source"] == source && record["target"] == target
            })
            .unwrap_or_else(|| {
                panic!("{event} {source} -> {target} should be in the trace: {records:?}")
            })
            .clone()
    }

    /// The targets of the records of `event` whose `reason` is `reason`.
    fn targets_with(&self, event: &str, reason: &str) -> Vec<String> {
        self.records()
            .iter()
            .filter(|record| record["event"] == event && record["reason"] == reason)
            .map(|record| {
                record["target"]
                    .as_str()
                    .expect("a link record has a target")
                    .to_string()
            })
            .collect()
    }
}

/// The graph the trace tests walk: one entry, one page it links on to, a page
/// under the threshold, a page outside the root, and a page two hops down.
///
/// Small enough that the trace it produces is read line by line in the test
/// below, and shaped so that a link is passed over for each of the reasons a
/// walk has: `b.md` is under the threshold, `../outside.md` is out of the root,
/// and `index.md` — the page the walk started from — is reached again from
/// `a.md`.
fn traced_tree(label: &str) -> Tree {
    let tree = Tree::new(label);
    tree.page(
        "index.md",
        &[
            "a.md".to_string(),
            "b.md".to_string(),
            "../outside.md".to_string(),
        ],
    );
    tree.page("a.md", &["c.md".to_string(), "index.md".to_string()]);
    tree.page("b.md", &[]);
    tree.page("c.md", &["b.md".to_string()]);
    tree
}

/// What the fake knows about the traced tree: the entry is relevant to nothing,
/// `a.md` is a result, `c.md` is reached two hops down, and `b.md` is under the
/// threshold from the entry and over it from `c.md`.
fn traced_table() -> Vec<(&'static str, Entry)> {
    vec![
        (
            "index.md",
            Entry::new(
                0.4,
                &[("a.md", 0.9), ("b.md", 0.4), ("../outside.md", 0.99)],
            ),
        ),
        ("a.md", Entry::new(0.9, &[("c.md", 0.9), ("index.md", 0.9)])),
        ("b.md", Entry::new(0.3, &[])),
        ("c.md", Entry::new(0.7, &[("b.md", 0.9)])),
    ]
}

/// The trace is the walk in the order it happened: the file popped, the request
/// made for it, the answer that came back, every link it judged, and the visit
/// that closed it — a file at a time, in the order the rounds went.
///
/// The numbers are the walk's own, so the two cannot drift: `path_score` is the
/// product of the scents on the path that reached the file, `depth` and `via`
/// are that path, and every record's `t_ms` is when it happened.
#[tokio::test]
async fn the_trace_is_the_walk_in_the_order_it_happened() {
    let tree = traced_tree("trace-order");
    let settings = Settings::over(tree.root(), &["index.md"]);
    let traced = Traced::new("trace-order-file", &tree.root(), 0.6);
    let scorer = traced.judge(Fake::new(&traced_table()).over(tree.root()));

    let traversal = settings.traced(&scorer, traced.walk()).await;

    assert_eq!(
        traced.order(),
        [
            "popped index.md",
            "requested index.md",
            "answered index.md",
            "admitted index.md -> a.md",
            "pruned index.md -> b.md",
            "pruned index.md -> ../outside.md",
            "result index.md",
            "popped a.md",
            "requested a.md",
            "answered a.md",
            "admitted a.md -> c.md",
            "pruned a.md -> index.md",
            "result a.md",
            "popped c.md",
            "requested c.md",
            "answered c.md",
            "admitted c.md -> b.md",
            "result c.md",
            "popped b.md",
            "requested b.md",
            "answered b.md",
            "result b.md",
        ],
        "one file's pop, request, answer, links and visit at a time"
    );

    // The entry is popped first and is free: path score 1, depth 0, no link
    // reached it. What is two hops down carries the path that reached it.
    let entry = traced.record("popped", "index.md");
    assert_eq!(entry["path_score"], 1.0);
    assert_eq!(entry["depth"], 0);
    assert_eq!(entry["via"], json!([]));
    let deep = traced.record("popped", "c.md");
    assert_eq!(deep["depth"], 2);
    assert_eq!(deep["via"], json!(["index.md", "a.md"]));
    assert_eq!(
        deep["path_score"], 0.81,
        "0.9 from the entry, 0.9 again from a.md"
    );

    // What the scorer answered, as the walk read it: the file's relevance, its
    // sections and its links, and — with no cache in the way — no reuse.
    let answered = traced.record("answered", "a.md");
    assert_eq!(answered["relevance"], 0.9);
    assert_eq!(answered["cached"], false);
    assert_eq!(answered["latency_ms"], 0, "a fake that does not sleep");
    assert_eq!(answered["sections"][0]["score"], 0.5);
    assert_eq!(
        answered["links"],
        json!([
            {"target": "c.md", "scent": 0.9, "keep": true},
            {"target": "index.md", "scent": 0.9, "keep": true},
        ])
    );
    assert_eq!(traced.record("requested", "a.md")["post_index"], 0);

    // Each link is in the trace with the verdict the walk read it by.
    assert_eq!(traced.link("admitted", "index.md", "a.md")["scent"], 0.9);
    let under = traced.link("pruned", "index.md", "b.md");
    assert_eq!(under["reason"], "below_threshold");
    assert_eq!(under["scent"], 0.4);
    assert_eq!(
        traced.link("pruned", "index.md", "../outside.md")["reason"],
        "out_of_root"
    );
    assert_eq!(
        traced.link("pruned", "a.md", "index.md")["reason"],
        "already_reached",
        "the entry has been visited, and nothing reaches a file twice"
    );
    assert_eq!(
        traced.link("admitted", "c.md", "b.md")["scent"],
        0.9,
        "the page the entry was too weak to reach is reached from c.md"
    );

    // And the visits the trace ends with are the reading list's own files, with
    // the walk's numbers: the trace is in the order the walk visited them, and
    // the list is the same files ranked.
    let results: Vec<Value> = traced
        .records()
        .into_iter()
        .filter(|record| record["event"] == "result")
        .collect();
    assert_eq!(
        results.len(),
        traversal.results.len(),
        "every visit is a result, and no other file has one"
    );
    for file in &traversal.results {
        let path = file.path.to_string_lossy();
        let result = traced.record("result", &path);
        assert_eq!(result["relevance"], file.relevance, "{path}");
        assert_eq!(
            result["earned_a_place"],
            file.earns_a_place(0.6),
            "{path}: the reading list's own rule, asked of the reading list's cutoff"
        );
    }
}

/// A file the walk could not judge keeps the records it had: it was popped and
/// a request was made for it, and no answer came back. Nothing is held until
/// the walk ends, so what a run that is killed leaves is what had happened.
#[tokio::test]
async fn a_file_the_walk_could_not_judge_has_a_request_and_no_answer() {
    let tree = Tree::new("trace-unjudged");
    tree.page("index.md", &["ghost.md".to_string()]);
    tree.page("ghost.md", &[]);
    // The table has no row for `ghost.md`, so the walk reaches a page it cannot
    // judge — a page that is not there, or an API that refused it.
    let traced = Traced::new("trace-unjudged-file", &tree.root(), 0.6);
    let scorer = traced
        .judge(Fake::new(&[("index.md", Entry::new(0.9, &[("ghost.md", 0.9)]))]).over(tree.root()));

    let traversal = Settings::over(tree.root(), &["index.md"])
        .traced(&scorer, traced.walk())
        .await;

    assert_eq!(failures(&traversal).len(), 1, "the one page that failed");
    assert_eq!(traced.record("popped", "ghost.md")["depth"], 1);
    assert_eq!(traced.record("requested", "ghost.md")["post_index"], 0);
    assert_eq!(
        traced
            .records()
            .iter()
            .filter(|record| record["path"] == "ghost.md")
            .map(|record| record["event"].as_str().expect("an event name"))
            .collect::<Vec<_>>(),
        ["popped", "requested"],
        "and nothing else: no answer, no visit, no link of a file that was never judged"
    );
}

/// A trace is written as the walk goes and not at the end of it: while an
/// answer is being bought, the request it is being bought for is already on
/// disk. The fake reads the file under the walk's own feet to hold it to that.
#[tokio::test]
async fn a_request_is_on_disk_before_its_answer_is_bought() {
    let traced = Traced::new("trace-mid-flight", &root(), 0.6);
    let scorer = traced.judge(mixed_graph().peeking(traced.path()));
    let settings = Settings::new(&["index.md", "payments/README.md", "notes/scratch.md"]).fanout(3);

    let traversal = settings.traced(&scorer, traced.walk()).await;

    assert!(
        traversal.results.len() >= 4,
        "the walk should span several rounds: {:?}",
        paths(&traversal)
    );
    assert_eq!(
        traced
            .records()
            .iter()
            .filter(|record| record["event"] == "requested")
            .count(),
        traversal.calls,
        "one request per file that was asked about"
    );
}

/// A warm run and a cold one have the same trace: every answer a cache served
/// says `cached`, and the request that would have been made is in it either way,
/// so a player reads a replay the same way whichever it is.
#[tokio::test]
async fn the_answers_a_cache_served_are_reported_as_cached() {
    let tree = Tree::new("trace-cached-pages");
    tree.page("index.md", &["a.md".to_string()]);
    tree.page("a.md", &[]);
    let table = [
        ("index.md", Entry::new(0.9, &[("a.md", 0.9)])),
        ("a.md", Entry::new(0.7, &[])),
    ];
    let settings = Settings::over(tree.root(), &["index.md"]);
    let store = Tree::new("trace-cached-store");

    let cold = Traced::new("trace-cold", &tree.root(), 0.6);
    let bought = Fake::new(&table).over(tree.root());
    let cache = CachedScorer::new(bought, store.root())
        .expect("a cache")
        .with_trace(cold.sink());
    settings.traced(&cache, cold.walk()).await;

    let warm = Traced::new("trace-warm", &tree.root(), 0.6);
    let served = Fake::new(&table).over(tree.root());
    let cache = CachedScorer::new(served, store.root())
        .expect("a cache")
        .with_trace(warm.sink());
    settings.traced(&cache, warm.walk()).await;

    assert_eq!(cache.calls(), 0, "a warm cache asks for nothing");
    assert_eq!(cache.hits(), 2, "and serves both answers from disk");
    assert_eq!(
        cold.record("answered", "a.md")["cached"],
        false,
        "the first run bought its answer"
    );
    let served = warm.record("answered", "a.md");
    assert_eq!(served["cached"], true);
    assert_eq!(served["relevance"], 0.7, "the stored answer, not a new one");
    assert_eq!(warm.record("requested", "a.md")["post_index"], 0);
    assert_eq!(
        warm.order(),
        cold.order(),
        "the same walk, the same records in the same order"
    );
}

/// A link that queued nothing is in the trace with the reason the walk read it
/// by: the depth budget for a hop past it, and no judgment at all for a target
/// the scorer named no scent for.
#[tokio::test]
async fn a_link_the_walk_passed_over_is_reported_with_its_reason() {
    let tree = traced_tree("trace-reasons");
    // The table names the entry's link to `a.md` and nothing about `b.md`, so
    // the walk has no scent to follow for it: a link the model answered nothing
    // about is not a link the model called weak.
    let traced = Traced::new("trace-reasons-file", &tree.root(), 0.6);
    let scorer = traced.judge(
        Fake::new(&[
            ("index.md", Entry::new(0.4, &[("a.md", 0.9)])),
            ("a.md", Entry::new(0.9, &[("c.md", 0.9), ("index.md", 0.9)])),
        ])
        .over(tree.root()),
    );

    Settings::over(tree.root(), &["index.md"])
        .max_depth(1)
        .traced(&scorer, traced.walk())
        .await;

    let unjudged = traced.link("pruned", "index.md", "b.md");
    assert_eq!(unjudged["reason"], json!(Reason::Unjudged));
    assert_eq!(
        unjudged["scent"],
        Value::Null,
        "no scent was given, so none is reported"
    );
    assert_eq!(
        traced.link("pruned", "a.md", "c.md")["reason"],
        json!(Reason::MaxDepth),
        "a hop past the depth budget is never followed"
    );
    assert_eq!(
        traced.link("pruned", "a.md", "c.md")["scent"],
        0.9,
        "though the scorer called it strong"
    );
}

/// A walk that follows the scorer's own verdict reports the links it kept back
/// as the scorer's doing and not the threshold's: a Choice share means
/// something only beside the options it was weighed against
/// ([#47](https://github.com/mikekelly/s1m/issues/47)).
#[tokio::test]
async fn a_link_the_scorer_did_not_keep_is_reported_as_kept_back() {
    let tree = two_ways("trace-kept");
    let scorer = ByRule::new(0.5, 0.9).link("b.md", 0.1).unkept("a.md");
    let traced = Traced::new("trace-kept-file", &tree.root(), 0.6);

    Settings::over(tree.root(), &["index.md"])
        .kept()
        .traced(&scorer, traced.walk())
        .await;

    let kept_back = traced.link("pruned", "index.md", "a.md");
    assert_eq!(kept_back["reason"], "not_kept");
    assert_eq!(
        kept_back["scent"], 0.9,
        "the number is reported as it was answered, whatever decided the link"
    );
    assert_eq!(
        traced.link("admitted", "index.md", "b.md")["scent"],
        0.1,
        "and a share the scorer kept is followed at its own number"
    );
}

/// A link the root's `.s1mignore` matches is reported as ignored, with no
/// scent: it was never read, never sent and never judged, which is the one
/// thing the trace can say about it that the reading list cannot.
#[tokio::test]
async fn an_ignored_link_is_reported_but_never_sent() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ignore");
    let traced = Traced::new("trace-ignored", &root, 0.6);
    let scorer = traced.judge(
        Fake::new(&[
            (
                "entry.md",
                Entry::new(0.9, &[("public.md", 0.9), ("private/vault.md", 0.95)]),
            ),
            ("public.md", Entry::new(0.5, &[])),
            ("private/vault.md", Entry::new(1.0, &[])),
        ])
        .over(root.clone()),
    );

    Settings::over(root, &["entry.md"])
        .traced(&scorer, traced.walk())
        .await;

    let ignored = traced.link("pruned", "entry.md", "private/vault.md");
    assert_eq!(ignored["reason"], "ignored");
    assert_eq!(ignored["scent"], Value::Null);
    assert!(
        traced
            .records()
            .iter()
            .all(|record| record["path"] != "private/vault.md"),
        "the matched page is never popped, requested, answered or visited: {:?}",
        traced.order()
    );
    let asked: Vec<String> = traced
        .records()
        .iter()
        .filter(|record| record["event"] == "requested")
        .map(|record| record["path"].as_str().expect("a path").to_string())
        .collect();
    assert_eq!(
        asked,
        ["entry.md", "public.md"],
        "the matched page is never read, never sent, never asked about"
    );
}

/// The two budgets a queued path can run into are reported where the walk reads
/// them: a beam has no turn left for a path at its depth, and the file budget
/// runs out before a path's turn comes. A path can be admitted and pruned later
/// — a beam and a budget are read when the walk takes a path off the frontier,
/// not when a link queues it — and a replay needs both records to empty its
/// frontier.
#[tokio::test]
async fn the_paths_a_budget_dropped_are_reported() {
    let beamed = fan("trace-beam");
    let traced = Traced::new("trace-beam-file", &beamed.root(), 0.6);
    Settings::over(beamed.root(), &["index.md"])
        .beam(2)
        .traced(&fan_scorer(), traced.walk())
        .await;

    assert_eq!(
        traced.targets_with("pruned", "beam"),
        ["leaf-3.md", "leaf-4.md", "leaf-5.md"],
        "the paths the depth had no turn for, weakest first"
    );
    assert_eq!(
        traced.link("admitted", "index.md", "leaf-5.md")["scent"],
        0.61,
        "and the link that queued it was admitted all the same"
    );
    for leaf in ["leaf-1.md", "leaf-2.md"] {
        assert_eq!(traced.record("popped", leaf)["depth"], 1);
    }

    let budgeted = fan("trace-budget");
    let traced = Traced::new("trace-budget-file", &budgeted.root(), 0.6);
    Settings::over(budgeted.root(), &["index.md"])
        .max_files(0)
        .traced(&fan_scorer(), traced.walk())
        .await;

    assert_eq!(
        traced.targets_with("pruned", "max_files"),
        [
            "leaf-1.md",
            "leaf-2.md",
            "leaf-3.md",
            "leaf-4.md",
            "leaf-5.md"
        ],
        "everything the walk queued, in path order and not the heap's"
    );
    assert_eq!(
        traced
            .records()
            .iter()
            .filter(|record| record["event"] == "result")
            .count(),
        1,
        "the entry file was visited and nothing else was"
    );
}
