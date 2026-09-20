//! `--seed-grep`: the query's keywords, matched against the pages under the
//! root, as extra entry points for the walk.
//!
//! The walk follows links, so a page nothing links to is never reached however
//! relevant it is. [`seed`] is the other way in: it reads the pages under the
//! root, counts the query's keywords in each, and returns the best few, spelled
//! the way the walk spells its entry files, for the caller to put on the
//! frontier at path score 1.
//!
//! What a keyword is, and what a hit is:
//!
//! - A query breaks into whole words of [`MIN_TERM_LEN`] characters or more,
//!   lowercased and deduplicated. `we`, `do` and `a` name too little of a query
//!   to be worth a hit, and matching a term anywhere in the text would count
//!   `for` inside `before` and `note` inside `notes`, which is not what the
//!   query asked for.
//! - A hit is one whole-word, case-insensitive occurrence of a term.
//! - Files rank by how many of the query's terms they match, then by how many
//!   hits they have, then by path: a page covering more of the query comes
//!   first, and the answer never depends on the order a directory happened to
//!   list its files in.
//!
//! The candidates are [`crate::parse::pages`], the same `.md`/`.txt` listing
//! the parser resolves wikilinks against: hidden directories are not descended,
//! and unreadable directories and files that are not UTF-8 text are skipped. It
//! is in-process string work — no ripgrep, no dependency, no index.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::parse::{from_root, pages, relative_to_root};

/// Shortest query word that counts as a keyword.
const MIN_TERM_LEN: usize = 3;

/// The best `count` pages under `root` for `query`'s keywords, best first,
/// spelled the way [`crate::traverse::Config::entries`] wants its entry files
/// spelled: against the same base as `root`, so a caller can add them to the
/// frontier as extra entry files.
///
/// `skip` is left out of the search and so out of the count: the caller's entry
/// files are already on the frontier, and a hit on one of them would spend a
/// place on a file the walk already had.
///
/// Only pages with a hit come back, so fewer than `count` is normal. There is no
/// failure to report: a root that cannot be read has no hits, the way
/// [`crate::parse::pages`] treats a directory it cannot open.
pub fn seed(root: &Path, query: &str, count: usize, skip: &[PathBuf]) -> Vec<PathBuf> {
    let terms = terms(query);
    if count == 0 || terms.is_empty() {
        return Vec::new();
    }
    let skipped: HashSet<PathBuf> = skip
        .iter()
        .map(|path| relative_to_root(root, path))
        .collect();

    let mut hits: Vec<Hit> = Vec::new();
    for page in pages(root) {
        if skipped.contains(&page) {
            continue;
        }
        let Ok(bytes) = fs::read(root.join(&page)) else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let text = text.to_lowercase();
        let mut terms_matched = 0;
        let mut occurrences = 0;
        for term in &terms {
            let found = occurrences_in(&text, term);
            terms_matched += usize::from(found > 0);
            occurrences += found;
        }
        if terms_matched > 0 {
            hits.push(Hit {
                path: page,
                terms_matched,
                occurrences,
            });
        }
    }

    hits.sort_by(|a, b| {
        b.terms_matched
            .cmp(&a.terms_matched)
            .then_with(|| b.occurrences.cmp(&a.occurrences))
            .then_with(|| a.path.cmp(&b.path))
    });
    hits.truncate(count);
    hits.into_iter()
        .map(|hit| from_root(root, hit.path))
        .collect()
}

/// One page's score: how many of the query's terms it matches, and how many
/// times those terms occur in it.
#[derive(Debug)]
struct Hit {
    /// The page, relative to the root it was found under.
    path: PathBuf,
    terms_matched: usize,
    occurrences: usize,
}

/// The query's keywords: its words of at least [`MIN_TERM_LEN`] characters,
/// lowercased, in the order they were asked, each one once.
fn terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for word in query.split(|character: char| !character.is_alphanumeric()) {
        let word = word.to_lowercase();
        if word.chars().count() < MIN_TERM_LEN || terms.contains(&word) {
            continue;
        }
        terms.push(word);
    }
    terms
}

/// How many whole-word occurrences of `term` are in `text`, ignoring case.
///
/// `text` is already lowercased and `term` is a word of alphanumerics, so a
/// match is a whole word when neither the character before it nor the one after
/// it is alphanumeric.
fn occurrences_in(text: &str, term: &str) -> usize {
    text.match_indices(term)
        .filter(|(at, _)| {
            let before = text[..*at].chars().next_back();
            let after = text[at + term.len()..].chars().next();
            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture tree: an entry page, the two pages it links to in a chain,
    /// a page nothing links to, and a page in a hidden directory.
    fn root() -> PathBuf {
        PathBuf::from("tests/fixtures/seed")
    }

    /// The hits for a query, as the paths a caller would get back.
    fn hits(query: &str) -> Vec<String> {
        seed(&root(), query, 5, &[])
            .iter()
            .map(|path| path.display().to_string())
            .collect()
    }

    /// The whole contract of the ranking in one query: the entry page matches
    /// both terms and so comes first, the page it links to matches both with
    /// fewer hits, and the page nothing links to matches one term — so a page
    /// covering more of the query beats one repeating a single word.
    #[test]
    fn pages_rank_by_terms_matched_then_by_hits() {
        assert_eq!(
            hits("release checklist"),
            [
                "tests/fixtures/seed/index.md",
                "tests/fixtures/seed/notes/checklist.md",
                "tests/fixtures/seed/orphan.md",
            ]
        );
    }

    /// One term in common: the pages with more hits come first even though the
    /// entry page's path sorts before them, and two pages with the same hits
    /// fall back on the path — so the same tree always answers in the same
    /// order.
    #[test]
    fn ties_break_on_hits_then_path() {
        assert_eq!(
            hits("release"),
            [
                "tests/fixtures/seed/notes/checklist.md",
                "tests/fixtures/seed/orphan.md",
                "tests/fixtures/seed/index.md",
            ]
        );
    }

    /// The count is how many hits come back, and an entry file the caller named
    /// is not one of them: it is on the frontier already.
    #[test]
    fn the_count_limits_the_hits_and_entries_are_left_out() {
        let all = seed(&root(), "release checklist", 5, &[]);
        assert_eq!(all.len(), 3);

        let top = seed(&root(), "release checklist", 1, &[]);
        assert_eq!(top, all[..1].to_vec());

        let entries = [root().join("index.md")];
        assert_eq!(
            seed(&root(), "release checklist", 5, &entries),
            all[1..].to_vec()
        );

        assert!(seed(&root(), "release checklist", 0, &[]).is_empty());
    }

    /// A query is words, not letters: terms shorter than three characters are
    /// dropped, so a query of them is no query at all.
    #[test]
    fn terms_shorter_than_three_characters_are_dropped() {
        assert!(hits("a an of we do").is_empty());
        assert!(hits("").is_empty());
    }

    /// A term repeated in a query is one term, so asking twice does not double
    /// a page's score.
    #[test]
    fn repeated_terms_count_once() {
        assert_eq!(hits("release release"), hits("release"));
    }

    /// Matching is case-insensitive.
    #[test]
    fn matching_ignores_case() {
        assert_eq!(hits("RELEASE Checklist"), hits("release checklist"));
    }

    /// Whole words only: `note` does not match `notes`, so a term does not hit
    /// longer words that happen to contain it.
    #[test]
    fn only_whole_words_match() {
        assert!(hits("note").is_empty());
        assert_eq!(
            hits("notes"),
            [
                "tests/fixtures/seed/index.md",
                "tests/fixtures/seed/orphan.md",
            ]
        );
    }

    /// The candidates are the pages under the root: a hidden directory is not
    /// descended, so the page in it that would otherwise rank first is not a
    /// hit at all.
    #[test]
    fn hidden_directories_are_not_searched() {
        assert!(hits("hidden directories").is_empty());
    }
}
