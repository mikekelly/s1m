//! The spike's real call, against the real API.
//!
//! Every test here skips unless `TYPESAFE_API_KEY` is set, so `cargo test` and
//! CI stay offline and free. On a machine with the key these are the checks
//! behind `docs/spike-notes.md`: one request per file, one scent per link, the
//! numbers looking sensible on pages whose answer a person already knows, and —
//! since the same request does not come back bit-for-bit identical — a repeat
//! run answered from the cache instead of the API.
//!
//! The pages scored are public and committed — the vendored wiki in
//! `eval/wikis/llm-wiki-manager/` and this repository's own plan — so the runs
//! the notes describe can be repeated.

use std::fs;
use std::path::{Path, PathBuf};

use s1m::cache::{CachedScorer, Scored};
use s1m::jev::JevScorer;
use s1m::parse::{ParsedFile, parse};
use s1m::scorer::{FileJudgment, ScorerError};

/// A scorer for `root`, or `None` when there is no key to call with.
fn live_scorer(root: &Path) -> Option<JevScorer> {
    match JevScorer::from_env(root) {
        Ok(scorer) => Some(scorer),
        Err(ScorerError::MissingApiKey) => {
            eprintln!("skipping: TYPESAFE_API_KEY is not set");
            None
        }
        Err(error) => panic!("{error}"),
    }
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The vendored wiki's root, as `s1m score-file --root` would be given it.
fn wiki() -> PathBuf {
    repo().join("eval/wikis/llm-wiki-manager/wiki")
}

fn page(root: &Path, name: &str) -> ParsedFile {
    parse(root.join(name), root).unwrap_or_else(|error| panic!("{name}: {error}"))
}

/// One link's scent, found by target.
fn scent(judgment: &FileJudgment, target: &str) -> f64 {
    judgment
        .links
        .iter()
        .find(|link| link.target == Path::new(target))
        .unwrap_or_else(|| panic!("no link to {target}"))
        .scent
}

fn assert_judged(file: &ParsedFile, judgment: &FileJudgment) {
    assert_eq!(
        judgment.links.len(),
        file.links.len(),
        "one scent per link, in the file's own order"
    );
    assert!(
        (0.0..=1.0).contains(&judgment.relevance),
        "relevance is a 0 to 1 fraction, got {}",
        judgment.relevance
    );
    for (link, judged) in file.links.iter().zip(&judgment.links) {
        assert_eq!(link.target, judged.target, "answers keep the link order");
        assert!(
            (0.0..=1.0).contains(&judged.scent),
            "{} scored {}",
            judged.target.display(),
            judged.scent
        );
    }
}

/// The hub page: it says nothing about releasing, and links to the page that
/// does. The file score and the link scents must disagree, and the link that
/// answers the query must stand out from the one that does not.
#[tokio::test]
async fn a_hub_page_is_not_central_but_its_release_link_stands_out() {
    let root = wiki();
    let Some(scorer) = live_scorer(&root) else {
        return;
    };
    let file = page(&root, "index.md");
    let query = "how do I cut a release and publish the package";

    let outcome = scorer.judge(query, &file).await.expect("the API answers");
    assert_judged(&file, &outcome.judgment);

    assert!(
        outcome.judgment.relevance < 1.0,
        "an index of everything is not the file about releasing, got {}",
        outcome.judgment.relevance
    );
    let release = scent(&outcome.judgment, "concepts/release.md");
    let unrelated = scent(&outcome.judgment, "concepts/unit-tests.md");
    assert!(
        release > unrelated,
        "the release page scored {release}, the unit tests page {unrelated}"
    );

    assert_eq!(outcome.detail.questions, file.links.len() + 1);
    assert!(outcome.detail.input_tokens > 0);
}

/// A leaf page whose whole content is a pointer: for the release query it is on
/// the subject, so it must clear the middle of the scale.
#[tokio::test]
async fn the_release_page_is_judged_useful_for_a_release_query() {
    let root = wiki();
    let Some(scorer) = live_scorer(&root) else {
        return;
    };
    let file = page(&root, "concepts/release.md");

    let outcome = scorer
        .judge("how do I cut a release and publish the package", &file)
        .await
        .expect("the API answers");
    assert_judged(&file, &outcome.judgment);

    assert!(
        outcome.judgment.relevance > 0.5,
        "a page about releasing is at least supporting, got {}",
        outcome.judgment.relevance
    );
}

/// The plan scores with no links at all: every link in it is an external URL,
/// which the parser drops. The file question must still stand on its own.
#[tokio::test]
async fn a_file_with_only_external_links_is_still_judged() {
    let root = repo().join("docs");
    let Some(scorer) = live_scorer(&root) else {
        return;
    };
    let file = page(&root, "initial-plan.md");

    let outcome = scorer
        .judge("how does s1m decide which links to follow", &file)
        .await
        .expect("the API answers");
    assert_judged(&file, &outcome.judgment);

    assert!(
        outcome.judgment.relevance > 0.5,
        "the plan is about the thing the query describes, got {}",
        outcome.judgment.relevance
    );
    assert_eq!(outcome.detail.questions, 1, "the file question alone");
}

/// A directory for a live test to cache in: under the target directory, so a
/// test never writes an entry into a home directory, and emptied on the way in
/// so a run does not read an earlier one's answers.
fn scratch(label: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("cache-{label}"));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// The acceptance criterion for #7 as a caller meets it: the same file and
/// query twice is one call, and the second run's numbers are the first run's.
/// The model alone does not promise that — [`s1m::jev`] moves its numbers
/// between identical requests — which is what the cache is for.
#[tokio::test]
async fn a_second_identical_run_is_answered_from_the_cache() {
    let root = wiki();
    let Some(scorer) = live_scorer(&root) else {
        return;
    };
    let dir = scratch("repeat");
    let cached = CachedScorer::new(scorer, &dir).expect("a cache");
    let file = page(&root, "index.md");
    let query = "how do I cut a release and publish the package";

    let first = cached.judge(query, &file).await.expect("the API answers");
    let second = cached.judge(query, &file).await.expect("the cache answers");

    assert!(
        matches!(first, Scored::Called { .. }),
        "the first run has to call"
    );
    assert!(
        matches!(second, Scored::Reused(_)),
        "the second run must not"
    );
    assert_eq!(
        first.judgment(),
        second.judgment(),
        "the answer is the same, bit for bit"
    );
    assert_eq!(cached.calls(), 1, "one call for two runs");
    assert_eq!(cached.hits(), 1);

    // A different query is a different question, and is not answered from the
    // entry for this one.
    let other = cached
        .judge("how are payments settled", &file)
        .await
        .expect("the API answers");
    assert!(
        matches!(other, Scored::Called { .. }),
        "a new query is a call"
    );
    assert_eq!(cached.calls(), 2);

    let _ = fs::remove_dir_all(&dir);
}
