//! Snapshot tests for the `md` and `tree` views of one run: the contract from
//! [#13](https://github.com/mikekelly/s1m/issues/13).
//!
//! The wiki is `tests/fixtures/wiki/`, the one the parser and the traversal
//! tests share; the model is a table. Everything else is the real pipeline —
//! [`cli::run`] reads the entry file, walks the fixture's links, ranks the
//! results and hands [`s1m::format`] the reading list a caller would get — so
//! what these tests defend is the view of a real run and not a hand-built list.
//! The edges a run like this cannot show (no result at all, a link the model
//! named no scent for, a file no link reached) are pinned by the unit tests in
//! `src/format.rs`.
//!
//! The expected outputs are committed under `tests/snapshots/` and compared
//! byte for byte, so a change to either view shows up as a diff of a file a
//! reviewer can read. `S1M_UPDATE_SNAPSHOTS=1 cargo test --test formats`
//! rewrites them, and the diff is what the change then is.
//!
//! [#13]: https://github.com/mikekelly/s1m/issues/13

use std::fs;
use std::path::{Path, PathBuf};

use s1m::cache::Cacheable;
use s1m::cli::{self, Options, Uncached};
use s1m::format::Format;
use s1m::parse::{ParsedFile, relative_to_root};
use s1m::scorer::{FileJudgment, LinkJudgment, Scorer, ScorerError, SectionJudgment};

/// The fixture wiki, spelled as a caller running s1m from the crate root would
/// spell it: relative, so no snapshot depends on where the checkout is.
const ROOT: &str = "tests/fixtures/wiki";
const ENTRY: &str = "tests/fixtures/wiki/index.md";

/// The query this wiki is about, and the one the fake asserts it is asked.
const QUERY: &str = "settlement timing for instant payouts";

/// What a file's sections score: the first one this, and every one after it a
/// step lower. The fourth lands under the section threshold below, which is why
/// a snapshot shows a file whose last section is missing rather than a file with
/// a section scored at zero.
const SECTION_TOP: f64 = 0.95;
const SECTION_STEP: f64 = 0.15;

/// The budgets the run walks with: the plan's, except for `max_files`, which is
/// room for every page of this wiki rather than a limit worth testing here.
const MAX_FILES: usize = 12;
const MAX_DEPTH: usize = 6;
const THRESHOLD: f64 = 0.6;
const FANOUT: usize = 4;

/// Where the expected outputs live, and the environment variable that rewrites
/// them.
const SNAPSHOTS: &str = "tests/snapshots";
const UPDATE: &str = "S1M_UPDATE_SNAPSHOTS";

/// What the fake answers for one page: the relevance the model gives the file,
/// and the scent it gives each of the file's links, by the target the parser
/// spells.
///
/// By target rather than by position, because a page that links to the same
/// place twice is judged once — the walk keeps the first scent it was given —
/// and a table that had to repeat itself would hide which page agreed with
/// which.
struct Answer {
    relevance: f64,
    scents: &'static [(&'static str, f64)],
}

/// The fixture wiki's pages, and what the model says about each.
///
/// The scents are chosen so the walk has something to show: the entry's own
/// links are mostly weak, so the pages worth reading are two and three hops out;
/// one link leaving the root has a high scent and is still never followed; and
/// every page below the first is reached by exactly one link the walk followed,
/// so the tree's nesting is the walk's own and not a tie between two paths.
///
/// A page missing from the table is a page the walk should not have visited: the
/// fake refuses the judgment, and the run fails rather than quietly scoring
/// nothing.
const ANSWERS: &[(&str, Answer)] = &[
    // The entry: one link worth following, and five the walk passes over.
    (
        "index.md",
        Answer {
            relevance: 0.62,
            scents: &[
                ("payments/README.md", 0.86),
                ("payments/cutoffs.md", 0.41),
                ("payments/settlement.md", 0.33),
                ("notes/ledger.md", 0.28),
                // Out of the root, so the walk never follows it however strong
                // the scent: the highest-scoring line in the tree with no file
                // under it.
                ("../outside.md", 0.74),
                // A link to a page that is not there. The walk judges it like
                // any other and follows nothing.
                ("payments/missing.md", 0.19),
            ],
        },
    ),
    // Two hops out, and the page the walk's spine runs through.
    (
        "payments/README.md",
        Answer {
            relevance: 0.81,
            scents: &[
                ("payments/settlement.md", 0.94),
                ("notes/ledger.md", 0.72),
                ("payments/cutoffs.md", 0.40),
            ],
        },
    ),
    (
        "payments/settlement.md",
        Answer {
            relevance: 0.94,
            scents: &[("payments/cutoffs.md", 0.86)],
        },
    ),
    // The bottom of the walk: its one link points back at a page already
    // visited, and the walk has nothing left to follow.
    (
        "payments/cutoffs.md",
        Answer {
            relevance: 0.71,
            scents: &[("payments/settlement.md", 0.58)],
        },
    ),
    // Three hops out, and the busiest page: three links worth following and one
    // that is not.
    (
        "notes/ledger.md",
        Answer {
            relevance: 0.55,
            scents: &[
                ("payments/settlement.md", 0.44),
                ("notes/weekly review.md", 0.83),
                ("notes/reading.txt", 0.77),
                ("notes/reading.md", 0.71),
            ],
        },
    ),
    (
        "notes/weekly review.md",
        Answer {
            relevance: 0.52,
            scents: &[],
        },
    ),
    (
        "notes/reading.md",
        Answer {
            relevance: 0.48,
            scents: &[],
        },
    ),
    // A text file, reached by a link, whose one link points back.
    (
        "notes/reading.txt",
        Answer {
            relevance: 0.30,
            scents: &[("notes/ledger.md", 0.35)],
        },
    ),
    // Nothing links here, so the walk never scores it.
    (
        "notes/scratch.md",
        Answer {
            relevance: 0.44,
            scents: &[("notes/ledger.md", 0.31)],
        },
    ),
];

/// The model, in place of Jev: the table above, and nothing else.
///
/// It is [`Cacheable`] as well as a [`Scorer`] because the run below is the
/// CLI's, and the judge the CLI takes is one of its two: a request the cache
/// would key, and the call that answers it. Nothing here is stored — the fake
/// keeps no state and answers from its table either way — and nothing is asked
/// of the request beyond the page it names.
struct Fake {
    root: PathBuf,
}

impl Fake {
    fn new() -> Fake {
        Fake {
            root: PathBuf::from(ROOT),
        }
    }

    /// What the table says about one page: the relevance, a score per section
    /// and a scent per link.
    fn answer(&self, file: &ParsedFile) -> Result<FileJudgment, ScorerError> {
        let page = relative_to_root(&self.root, &file.path);
        let page = page.to_string_lossy().into_owned();
        let Some((_, answer)) = ANSWERS.iter().find(|(name, _)| *name == page) else {
            return Err(ScorerError::MissingAnswer { id: page });
        };

        let links = file
            .links
            .iter()
            .map(|link| {
                let target = link.target.to_string_lossy().into_owned();
                let scent = answer
                    .scents
                    .iter()
                    .find(|(name, _)| *name == target)
                    .ok_or_else(|| ScorerError::MissingAnswer {
                        id: format!("{page}: the scent of its link to {target}"),
                    })?;
                Ok(LinkJudgment {
                    target: link.target.clone(),
                    scent: scent.1,
                    keep: true,
                })
            })
            .collect::<Result<Vec<_>, ScorerError>>()?;

        Ok(FileJudgment {
            relevance: answer.relevance,
            // The parser's own sections, one score each: headings and ranges
            // belong to the file, and only the score is the model's.
            sections: file
                .sections
                .iter()
                .enumerate()
                .map(|(index, section)| SectionJudgment {
                    heading: section.heading.clone(),
                    lines: section.lines,
                    score: SECTION_TOP - SECTION_STEP * index as f64,
                })
                .collect(),
            links,
        })
    }
}

#[async_trait::async_trait]
impl Scorer for Fake {
    async fn score(&self, query: &str, file: &ParsedFile) -> Result<FileJudgment, ScorerError> {
        assert_eq!(query, QUERY, "the run's query reaches the scorer");
        self.answer(file)
    }
}

/// The request the CLI's judge builds for a page: the page itself, which is all
/// the fake's answer depends on.
#[async_trait::async_trait]
impl Cacheable for Fake {
    type Request = PathBuf;
    /// What a call cost: nothing the tests read, so nothing is reported.
    type Detail = ();

    fn request(&self, _query: &str, file: &ParsedFile) -> Result<PathBuf, ScorerError> {
        Ok(file.path.clone())
    }

    fn key(&self, request: &PathBuf) -> Result<Vec<u8>, ScorerError> {
        Ok(request.as_os_str().as_encoded_bytes().to_vec())
    }

    async fn call(
        &self,
        _request: &PathBuf,
        file: &ParsedFile,
    ) -> Result<(FileJudgment, ()), ScorerError> {
        Ok((self.answer(file)?, ()))
    }

    /// One request per page: this fake never splits one.
    fn posts(&self, _request: &PathBuf) -> usize {
        1
    }

    /// Nothing in the fake's accounting is a duration.
    fn latency(_detail: &()) -> std::time::Duration {
        std::time::Duration::ZERO
    }
}

/// One run over the fixture wiki: the CLI's own options and the fake in place of
/// Jev.
///
/// `Uncached` is the library's, so the run counts what it bought the way
/// `--no-cache` does — one answer per file visited — which is what `calls` in
/// both views reports.
async fn walk() -> cli::ReadingList {
    let options = Options {
        query: QUERY.to_string(),
        entries: vec![PathBuf::from(ENTRY)],
        // The first entry file's directory, which is the fixture wiki: the
        // default a caller gets without `--root`.
        root: None,
        max_files: MAX_FILES,
        max_depth: MAX_DEPTH,
        threshold: THRESHOLD,
        admission: s1m::traverse::Admission::Threshold(THRESHOLD),
        beam: None,
        fanout: FANOUT,
        mode: "useful-for".to_string(),
        scorer: "noul".to_string(),
    };

    cli::run(&options, &Uncached::new(Fake::new()))
        .await
        .expect("the fixture wiki walks")
}

/// Compares `printed` with the committed snapshot of that name, or rewrites it
/// when `S1M_UPDATE_SNAPSHOTS` is set.
///
/// A whole-view comparison rather than an assertion per line: what the two views
/// are is their shape — which lines, in which order, at which indent — and that
/// is what a diff shows and an assertion cannot.
fn snapshot(name: &str, printed: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(SNAPSHOTS)
        .join(name);
    if std::env::var_os(UPDATE).is_some() {
        fs::write(&path, printed).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        return;
    }
    let expected =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert_eq!(
        printed,
        expected,
        "{} is not what this run prints; if the change is deliberate, rewrite it with {UPDATE}=1 cargo test --test formats",
        path.display(),
    );
}

/// The reading list as markdown: the files a person or an agent should open, in
/// the order to open them, each with the lines worth reading inside it.
#[tokio::test]
async fn md_lists_the_fixture_wikis_files_with_the_lines_to_read() {
    snapshot("reading-list.md", &Format::Md.render(&walk().await));
}

/// The walk's link tree: every file it visited with every link it judged
/// beneath it, each link with the scent the model gave it and whether the walk
/// followed it — so a reader sees both the link the entry follows at 0.86 and
/// the one it passes over at 0.28, and that a link out of the root is passed
/// over however strong it is.
#[tokio::test]
async fn tree_marks_every_judged_link_followed_or_pruned() {
    snapshot("link-tree.txt", &Format::Tree.render(&walk().await));
}
