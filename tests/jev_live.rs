//! The spike's real call, against the real API.
//!
//! Every test here skips unless `TYPESAFE_API_KEY` is set, so `cargo test` and
//! CI stay offline and free. On a machine with the key these are the checks
//! behind `docs/spike-notes.md`: one request per file, one scent per link, the
//! numbers looking sensible on pages whose answer a person already knows, and —
//! since the same request does not come back bit-for-bit identical — a repeat
//! run answered from the cache instead of the API.
//!
//! The pages scored are committed — the vendored wiki in
//! `eval/wikis/llm-wiki-manager/`, this repository's own plan, and the fixture
//! page written for the cap test — so the runs the notes describe can be
//! repeated.

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
    assert_eq!(
        judgment.sections.len(),
        file.sections.len(),
        "one score per section, in the parser's own order"
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
    for (section, judged) in file.sections.iter().zip(&judgment.sections) {
        assert_eq!(
            section.heading, judged.heading,
            "answers keep the section order"
        );
        assert_eq!(
            section.lines,
            judged.lines,
            "{} is judged on the lines the parser gave it",
            section.heading.as_deref().unwrap_or("(no heading)")
        );
        assert!(
            (0.0..=1.0).contains(&judged.score),
            "{} scored {}",
            section.heading.as_deref().unwrap_or("(no heading)"),
            judged.score
        );
    }
}

/// One section's score, found by heading.
fn section(judgment: &FileJudgment, heading: &str) -> f64 {
    judgment
        .sections
        .iter()
        .find(|section| section.heading.as_deref() == Some(heading))
        .unwrap_or_else(|| panic!("no section headed {heading}"))
        .score
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

    assert_eq!(
        outcome.detail.questions,
        file.sections.len() + file.links.len() + 1
    );
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

    // #9 as a caller meets it: the page's own section is the range to read, and
    // the "See also" list under it is navigation, and the two are far apart
    // whatever wording asks about them.
    //
    // What the plan's 0.6 no longer does is separate them: [#52]'s section
    // question reads this page's own text at about 0.4 where the question it
    // replaced read it above 0.6, so this range is one of the ones the walk
    // stopped returning — the eval's recall is level without it, for a third
    // less reading, which is the trade that decision made on both gold sets.
    // The question it replaced is still reachable, as `--wording
    // section-legacy`, when the absolute height of a section score is what a
    // call is asking about.
    //
    // [#52]: https://github.com/mikekelly/s1m/issues/52
    let body = section(&outcome.judgment, "Release");
    let see_also = section(&outcome.judgment, "See also");
    assert!(
        body > see_also + 0.2,
        "the page's own text is what to read: {body} against {see_also} for the link list"
    );
    assert!(
        see_also < body,
        "the link list scored {see_also}, the page's own text {body}"
    );
    assert_eq!(outcome.detail.requests, 1, "a leaf page fits one request");
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
    assert_eq!(
        outcome.detail.questions,
        file.sections.len() + 1,
        "the file question and one per section: the plan has no local links"
    );
    assert!(
        outcome
            .judgment
            .sections
            .iter()
            .all(|section| section.heading.is_some()),
        "every section of the plan is headed: {:?}",
        outcome.judgment.sections
    );
}

/// The wall [#9] exists to stay under: a hub page whose links do not fit the
/// API's 32k state budget in one request is split, and the API answers every
/// post. Three posts of about a tenth of a million characters would be one
/// request the API refuses, and the token count that comes back — the sum over
/// the posts — is the evidence they were not one.
///
/// [#9]: https://github.com/mikekelly/s1m/issues/9
#[tokio::test]
async fn a_hub_page_too_big_for_one_request_is_split_and_answered() {
    let root = repo().join("eval/wikis/llm-wiki-manager/wiki");
    let Some(scorer) = live_scorer(&root) else {
        return;
    };

    // 300 links with a preview each: the shape the spike's synthetic hub has,
    // and well past the ~100 the notes say one request holds.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("big-hub");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("a hub directory");
    let mut source = String::from("# Hub\n\nA hub with three hundred links.\n\n");
    for index in 0..300 {
        let page = format!("page-{index:03}.md");
        fs::write(
            dir.join(&page),
            format!("# Page {index}\n\nPage {index} covers step {index} of the release runbook.\n"),
        )
        .expect("a page");
        source.push_str(&format!(
            "- [Page {index}]({page}) — step {index} of the release runbook.\n"
        ));
    }
    fs::write(dir.join("hub.md"), &source).expect("the hub");

    let file = page(&dir, "hub.md");
    let outcome = scorer
        .judge("how do I cut a release and publish the package", &file)
        .await
        .expect("the API answers every post");
    assert_judged(&file, &outcome.judgment);

    assert!(
        outcome.detail.requests > 1,
        "300 links with previews should not fit one request: {} questions in {} requests",
        outcome.detail.questions,
        outcome.detail.requests
    );
    assert_eq!(outcome.judgment.links.len(), 300);
    assert!(
        outcome.detail.input_tokens > 32_000,
        "one request could not have carried these {} tokens",
        outcome.detail.input_tokens
    );

    let _ = fs::remove_dir_all(&dir);
}

/// The page over the content cap, written for the test below and named for what
/// it is: no document that has to stay true decides whether the cap is still
/// tested. The README this used to be stopped being over the cap when [#75]
/// moved the reference out of it ([#77]).
///
/// [#75]: https://github.com/mikekelly/s1m/issues/75
/// [#77]: https://github.com/mikekelly/s1m/issues/77
const OVERSIZED: &str = "tests/fixtures/oversized-page.md";

/// A page over the content cap is judged in full ([#53]): the whole page goes
/// to the API — the tail in a post of its own — and the section written past
/// the cap is read for what it says instead of being judged from its heading.
///
/// The page is [`OVERSIZED`], which is over the cap on its own and whose last
/// section is "Development", opening past the cap, so a query about running the
/// tests is a query about the tail the cap used to cut.
///
/// [#53]: https://github.com/mikekelly/s1m/issues/53
#[tokio::test]
async fn a_page_over_the_content_cap_is_judged_in_full() {
    let root = repo().join("tests/fixtures");
    let Some(scorer) = live_scorer(&root) else {
        return;
    };
    let file = page(&root, "oversized-page.md");
    let source = fs::read_to_string(repo().join(OVERSIZED)).expect("the fixture");
    assert!(
        source.chars().count() > 40_000,
        "the page has to be over the cap the split works to for this to test it: {} characters",
        source.chars().count()
    );
    // And the section the query is about has to open past that cap, or the
    // answer could come from a page the cap never cut.
    let tail = file
        .sections
        .iter()
        .find(|section| section.heading.as_deref() == Some("Development"))
        .expect("the fixture's last section is Development");
    let before: usize = source
        .lines()
        .take(tail.lines[0] - 1)
        .map(|line| line.chars().count() + 1)
        .sum();
    assert!(
        before > 40_000,
        "the fixture's Development has to open past the cap to be the tail the cap cut: {before} characters"
    );

    let outcome = scorer
        .judge(
            "how do I run the tests, the lints and CI before opening a pull request",
            &file,
        )
        .await
        .expect("the API answers every post");
    assert_judged(&file, &outcome.judgment);

    assert!(
        outcome.detail.requests > 1,
        "{} characters should not fit one post: {} questions in {} requests",
        source.chars().count(),
        outcome.detail.questions,
        outcome.detail.requests
    );
    // The tail is judged as text: the last section of the page opens past the
    // cap, and it is the one this query is about.
    let development = section(&outcome.judgment, "Development");
    assert!(
        development > 0.5,
        "the fixture's last section is what the query asks for, and it scored {development}"
    );
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
        matches!(second, Scored::Reused { .. }),
        "the second run must not"
    );
    assert_eq!(
        first.judgment(),
        second.judgment(),
        "the answer is the same, bit for bit"
    );
    assert_eq!(
        first.detail(),
        second.detail(),
        "and so is what the call that produced it cost"
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
