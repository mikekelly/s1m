//! The gold set: the queries, and the pages a person would want for each.
//!
//! This is the existing `eval` binary's format, with two fields it does not
//! read: `category`, which the report groups by, and `wanted` as another
//! spelling of `expected`. Both binaries ignore fields they do not know, so one
//! file feeds both.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The category a query with none is counted under.
pub const UNCATEGORISED: &str = "uncategorised";

#[derive(Debug, Clone, Deserialize)]
pub struct Gold {
    pub queries: Vec<Query>,
}

/// One labelled query. Only `id` and `category` ever reach a report.
///
/// A gold set's `why`/`note` — the sentence explaining the labels — is read by
/// nothing here and is not held: it is prose about the wiki, and this harness
/// has no use for it.
#[derive(Debug, Clone, Deserialize)]
pub struct Query {
    /// A short name for the query, unique in the set: what the tables key on.
    pub id: String,
    /// What kind of question this is, for grouping the results.
    #[serde(default)]
    pub category: Option<String>,
    /// The query, in the words the caller would use.
    pub query: String,
    /// The page a caller would start from, relative to the wiki root.
    #[serde(default)]
    pub entry: Option<String>,
    /// What relevance means for it: `about`, `useful-for` or `answers`. s1m is
    /// run under it; the agent conditions have no such flag.
    #[serde(default)]
    pub mode: Option<String>,
    /// The pages a person would want, relative to the wiki root. `expected` is
    /// the same field under the name the committed gold set uses.
    #[serde(alias = "expected")]
    pub wanted: Vec<String>,
}

impl Query {
    /// The category this query is grouped under.
    pub fn category(&self) -> &str {
        self.category.as_deref().unwrap_or(UNCATEGORISED)
    }

    /// The page the walk starts from: the query's own, else the default the
    /// caller passed.
    pub fn entry<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.entry.as_deref().unwrap_or(fallback)
    }

    /// The pages this query wants, deduplicated.
    pub fn wanted(&self) -> BTreeSet<PathBuf> {
        self.wanted.iter().map(PathBuf::from).collect()
    }
}

impl Gold {
    /// Keeps only the queries named, in the gold set's own order: a pass over a
    /// whole gold set costs real money, so a caller can measure two queries
    /// first and see what a run is worth. An id that is in no gold set is an
    /// error rather than an empty run.
    pub fn only(&mut self, ids: &[String]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let wanted: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
        let known: BTreeSet<&str> = self.queries.iter().map(|query| query.id.as_str()).collect();
        if let Some(missing) = wanted.difference(&known).next() {
            return Err(format!("{missing:?}: no query in the gold set has this id"));
        }
        self.queries
            .retain(|query| wanted.contains(query.id.as_str()));
        Ok(())
    }

    /// Reads a gold set, checking only what this harness needs: a query needs
    /// an id no other query has, text to ask, and at least one wanted page.
    pub fn load(file: &Path) -> Result<Gold, String> {
        let text = std::fs::read_to_string(file)
            .map_err(|error| format!("{}: {error}", file.display()))?;
        let gold: Gold =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", file.display()))?;
        if gold.queries.is_empty() {
            return Err(format!("{}: no queries", file.display()));
        }
        let mut ids = BTreeSet::new();
        for query in &gold.queries {
            if query.query.trim().is_empty() {
                return Err(format!("{}: the query is blank", query.id));
            }
            if !ids.insert(query.id.clone()) {
                return Err(format!("{}: two queries share this id", query.id));
            }
            if query.wanted.is_empty() {
                return Err(format!("{}: no wanted pages", query.id));
            }
        }
        Ok(gold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempDir;

    fn load(text: &str) -> Result<Gold, String> {
        let dir = TempDir::new("gold");
        dir.write("gold.json", text);
        Gold::load(&dir.path().join("gold.json"))
    }

    /// The committed gold set spells the wanted pages `expected` and has no
    /// categories; a private one spells them `wanted` and has them. One loader
    /// reads both, because the same harness measures both.
    #[test]
    fn reads_both_spellings_of_a_gold_set() {
        let old = load(
            r#"{"queries": [{"id": "one", "query": "q", "mode": "answers",
                "entry": "index.md", "expected": ["a.md"], "note": "the reason"}]}"#,
        )
        .expect("the committed spelling");
        assert_eq!(
            old.queries[0].wanted(),
            BTreeSet::from([PathBuf::from("a.md")])
        );
        assert_eq!(old.queries[0].category(), UNCATEGORISED);
        assert_eq!(old.queries[0].entry("fallback.md"), "index.md");

        let new = load(
            r#"{"queries": [{"id": "one", "category": "how-to", "query": "q",
                "entry": "docs/start.md", "mode": "useful-for",
                "wanted": ["a.md", "b.md"], "why": "the reason"}]}"#,
        )
        .expect("the private spelling");
        assert_eq!(new.queries[0].category(), "how-to");
        assert_eq!(new.queries[0].entry("fallback.md"), "docs/start.md");
        assert_eq!(new.queries[0].wanted().len(), 2);

        // A query with no entry of its own takes the caller's default.
        let bare = load(r#"{"queries": [{"id": "one", "query": "q", "wanted": ["a.md"]}]}"#)
            .expect("the smallest gold set that means anything");
        assert_eq!(bare.queries[0].entry("fallback.md"), "fallback.md");
    }

    #[test]
    fn a_gold_set_that_cannot_be_measured_is_an_error() {
        assert!(load(r#"{"queries": []}"#).is_err());
        assert!(load(r#"{"queries": [{"id": "a", "query": " ", "wanted": ["a.md"]}]}"#).is_err());
        assert!(load(r#"{"queries": [{"id": "a", "query": "q", "wanted": []}]}"#).is_err());
        assert!(
            load(
                r#"{"queries": [{"id": "a", "query": "q", "wanted": ["a.md"]},
                    {"id": "a", "query": "r", "wanted": ["b.md"]}]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn a_pass_can_be_narrowed_to_named_queries() {
        let mut gold = load(
            r#"{"queries": [{"id": "one", "query": "q", "wanted": ["a.md"]},
                {"id": "two", "query": "r", "wanted": ["b.md"]},
                {"id": "three", "query": "s", "wanted": ["c.md"]}]}"#,
        )
        .expect("three queries");

        gold.only(&["three".to_string(), "one".to_string()])
            .expect("two of them");
        let ids: Vec<&str> = gold.queries.iter().map(|query| query.id.as_str()).collect();
        assert_eq!(ids, vec!["one", "three"]);

        assert!(gold.only(&["four".to_string()]).is_err());
        // Naming nothing keeps everything.
        gold.only(&[]).expect("no filter");
        assert_eq!(gold.queries.len(), 2);
    }
}
