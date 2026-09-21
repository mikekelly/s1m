//! The evaluation harness and the walk it measures, as a caller meets them: the
//! committed cache reproduces the committed report, and it answers every
//! judgment the walk makes over the same wiki, so both run with no key and no
//! network.
//!
//! The first test is the acceptance criterion for
//! [#11](https://github.com/mikekelly/s1m/issues/11) — *results reproducible
//! from the cache* — stated as the thing a reader would do: run the command the
//! report opens with, and diff. The run has to be offline for the claim to mean
//! anything, so the key is removed from the child's environment: a judgment the
//! cache cannot answer then fails the run instead of quietly buying an answer.
//!
//! The second walks the same gold set at no budget and counts what every judged
//! link came to. It runs keyless out of the same cache, and it is where a round
//! loop that ended with work still on the heap — a walk that lost a link it had
//! admitted — would show.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The built harness, the way `cargo test` points at it.
const EVAL: &str = env!("CARGO_BIN_EXE_eval");

/// The command the report documents, spelled the way it does: paths relative to
/// the package root, which is where `cargo test` runs.
const WIKI: &str = "eval/wikis/llm-wiki-manager/wiki";
const GOLD: &str = "eval/gold/llm-wiki-manager.json";
const CACHE: &str = "eval/cache";
const REPORT: &str = "eval/REPORT.md";

#[test]
fn the_committed_cache_reproduces_the_committed_report() {
    // `--relative-judge` and `--wordings` are part of the command the report
    // documents: the committed report carries the relative judge's rows and the
    // wording table, and the answers both were bought with are in the committed
    // cache like any other.
    let output = Command::new(EVAL)
        .args([
            "--wiki",
            WIKI,
            "--gold",
            GOLD,
            "--cache",
            CACHE,
            "--relative-judge",
            "--wordings",
        ])
        .env_remove("TYPESAFE_API_KEY")
        .env_remove("S1M_ENDPOINT")
        .output()
        .expect("the harness should be runnable");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "the run failed: {stderr}");
    assert!(
        stderr.contains("TYPESAFE_API_KEY is not set"),
        "the run has to be the keyless one: {stderr}"
    );

    let committed = std::fs::read_to_string(REPORT).expect("the committed report");
    assert_eq!(
        String::from_utf8(output.stdout).expect("the report is UTF-8"),
        committed,
        "a keyless rerun against the committed cache is the committed report"
    );
}

/// What every judged link of a walk came to.
///
/// A link the walk did not follow is one of five things, and only the last is
/// the walk's own doing: below `--threshold`, a scent that is not a probability,
/// out of the root, past `--max-depth`, or a target some other link already
/// reached — `followed: false` is about the *path*, not the file, so a target
/// reached from another parent prints it under every parent but the first.
#[derive(Debug, Default)]
struct Dispositions {
    judged: usize,
    followed: usize,
    below_threshold: usize,
    not_a_probability: usize,
    out_of_root: usize,
    past_depth: usize,
    reached_elsewhere: usize,
    /// Cleared the threshold, in the root, within depth, and not followed, with
    /// a target no path reached: a link the walk refused without a reason.
    refused: usize,
    /// Queued, and its target never reached or failed: a walk that stopped with
    /// work still on its frontier.
    stranded: usize,
}

impl Dispositions {
    fn count(walk: &s1m::traverse::Traversal, threshold: f64, max_depth: usize) -> Dispositions {
        let reached: Vec<&PathBuf> = walk
            .results
            .iter()
            .map(|file| &file.path)
            .chain(walk.failed.iter().map(|file| &file.path))
            .collect();
        let mut census = Dispositions::default();
        for file in &walk.results {
            for link in &file.links {
                let Some(scent) = link.scent else {
                    continue;
                };
                census.judged += 1;
                if link.followed && !reached.contains(&&link.target) {
                    census.stranded += 1;
                }
                if !link.in_root {
                    census.out_of_root += 1;
                } else if !(0.0..=1.0).contains(&scent) {
                    census.not_a_probability += 1;
                } else if scent < threshold {
                    census.below_threshold += 1;
                } else if file.depth + 1 > max_depth {
                    census.past_depth += 1;
                } else if link.followed {
                    census.followed += 1;
                } else if reached.contains(&&link.target) {
                    census.reached_elsewhere += 1;
                } else {
                    census.refused += 1;
                }
            }
        }
        census
    }
}

/// The walk over the public gold set with no budget: the frontier emptying is
/// the only thing that may end it, so every link it queues is reached, and
/// nothing it admits is refused.
///
/// This is where a round loop that ends with work still on the heap would show,
/// and it is the check behind a `tree` line that reads `pruned; scent 0.85`: on
/// this wiki every one of those is a target another parent's link reached
/// first, not a file the walk dropped.
#[tokio::test]
async fn the_walk_with_no_budget_leaves_nothing_queued_or_admitted() {
    const THRESHOLD: f64 = 0.6;
    const MAX_DEPTH: usize = 6;
    const FANOUT: usize = 8;

    let root = Path::new(WIKI);
    let gold: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(GOLD).expect("the gold set")).expect("json");
    let ignore = s1m::ignore::Ignore::at(root).expect("the wiki's .s1mignore");
    // The cache answers every judgment this walk makes, which is what lets the
    // test run keyless in CI: a miss would fail on the call rather than buy it.
    let key = std::env::var("TYPESAFE_API_KEY").unwrap_or_else(|_| "no key needed".to_string());
    let mut walked = 0;
    let mut census = Dispositions::default();

    for query in gold["queries"].as_array().expect("queries") {
        let mode = match query["mode"].as_str().expect("mode") {
            "about" => s1m::jev::ABOUT.clone(),
            "useful-for" => s1m::jev::USEFUL_FOR.clone(),
            other => {
                assert_eq!(other, "answers", "a mode the harness knows");
                s1m::jev::ANSWERS.clone()
            }
        };
        let jev = s1m::jev::JevScorer::new(key.clone(), root)
            .expect("a scorer")
            .with_mode(mode);
        let scorer = s1m::cache::CachedScorer::new(jev, CACHE).expect("the committed cache");
        let entries = [root.join(query["entry"].as_str().expect("entry"))];
        let config = s1m::traverse::Config {
            query: query["query"].as_str().expect("query"),
            entries: &entries,
            root,
            max_files: usize::MAX,
            max_depth: MAX_DEPTH,
            fanout: FANOUT,
            admission: s1m::traverse::Admission::Threshold(THRESHOLD),
            beam: None,
            ignore: &ignore,
        };
        let traversal = s1m::traverse::traverse(&config, &scorer)
            .await
            .expect("a walk");
        assert!(
            traversal.failed.is_empty(),
            "the committed cache answers every judgment this walk asks for: {:?}",
            traversal.failed
        );
        walked += traversal.results.len();
        let query = Dispositions::count(&traversal, THRESHOLD, MAX_DEPTH);
        census.judged += query.judged;
        census.followed += query.followed;
        census.below_threshold += query.below_threshold;
        census.not_a_probability += query.not_a_probability;
        census.out_of_root += query.out_of_root;
        census.past_depth += query.past_depth;
        census.reached_elsewhere += query.reached_elsewhere;
        census.refused += query.refused;
        census.stranded += query.stranded;
    }

    assert!(walked > 0, "the gold set should walk {census:?}");
    assert_eq!(
        (census.refused, census.stranded),
        (0, 0),
        "no admitted link is refused and nothing queued is left behind: {census:?}"
    );
}
