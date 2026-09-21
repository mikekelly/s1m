//! The gold set: the queries, and the pages a person would want for each.
//!
//! This is the existing `eval` binary's format, with two fields it does not
//! read: `category`, which the report groups by, and `wanted` as another
//! spelling of `expected`. Both binaries ignore fields they do not know, so one
//! file feeds both.
//!
//! A wanted page may also be spelled as an object naming the part of it that
//! answers — a `heading` or `lines` the `eval` harness counts section recall
//! over ([#58]) — and this harness reads the page and scores it at file level,
//! which is the question it asks: which pages a run should have returned.
//!
//! [#58]: https://github.com/mikekelly/s1m/issues/58

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
    /// The page or pages a caller would start from, relative to the wiki
    /// root. A wiki need not have one way in, and a walk takes a set.
    #[serde(default)]
    pub entry: Option<Entries>,
    /// What relevance means for it: `about`, `useful-for` or `answers`. s1m is
    /// run under it; the agent conditions have no such flag.
    #[serde(default)]
    pub mode: Option<String>,
    /// The pages a person would want, relative to the wiki root. `expected` is
    /// the same field under the name the committed gold set uses.
    #[serde(alias = "expected")]
    pub wanted: Vec<Entry>,
}

/// One wanted page: its path, or an object naming the part of it that answers.
///
/// The object is the `eval` harness's label for where in the page the answer
/// is — its `heading`, its `lines`, or neither — and it is a page here: this
/// harness measures whole files, so the label is read and the page is scored.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Entry {
    Page(String),
    Part { path: String },
}

impl Entry {
    /// The page this entry wants, as the gold set spells it.
    pub fn path(&self) -> &str {
        match self {
            Entry::Page(path) => path,
            Entry::Part { path } => path,
        }
    }
}

/// One entry page or several: a gold set may spell either, because most wikis
/// have one way in and some have several.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Entries {
    One(String),
    Many(Vec<String>),
}

impl Entries {
    fn pages(&self) -> Vec<&str> {
        match self {
            Entries::One(page) => vec![page.as_str()],
            Entries::Many(pages) => pages.iter().map(String::as_str).collect(),
        }
    }
}

impl Query {
    /// The category this query is grouped under.
    pub fn category(&self) -> &str {
        self.category.as_deref().unwrap_or(UNCATEGORISED)
    }

    /// The pages the walk starts from: the query's own, else the ones the
    /// caller passed.
    pub fn entries<'a>(&'a self, fallback: &'a [String]) -> Vec<&'a str> {
        let named = self.entry.as_ref().map(Entries::pages).unwrap_or_default();
        if named.is_empty() {
            return fallback.iter().map(String::as_str).collect();
        }
        named
    }

    /// The pages this query wants, deduplicated.
    pub fn wanted(&self) -> BTreeSet<PathBuf> {
        self.wanted
            .iter()
            .map(|entry| PathBuf::from(entry.path()))
            .collect()
    }

    /// The pages this query wants, in the gold set's own order: what a raw row
    /// records of the labels it was measured against.
    pub fn pages(&self) -> Vec<&str> {
        self.wanted.iter().map(Entry::path).collect()
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
        assert_eq!(
            old.queries[0].entries(&["fallback.md".to_string()]),
            vec!["index.md"]
        );

        let new = load(
            r#"{"queries": [{"id": "one", "category": "how-to", "query": "q",
                "entry": "docs/start.md", "mode": "useful-for",
                "wanted": ["a.md", "b.md"], "why": "the reason"}]}"#,
        )
        .expect("the private spelling");
        assert_eq!(new.queries[0].category(), "how-to");
        assert_eq!(
            new.queries[0].entries(&["fallback.md".to_string()]),
            vec!["docs/start.md"]
        );
        assert_eq!(new.queries[0].wanted().len(), 2);

        // A query with no entry of its own takes the caller's default.
        let bare = load(r#"{"queries": [{"id": "one", "query": "q", "wanted": ["a.md"]}]}"#)
            .expect("the smallest gold set that means anything");
        assert_eq!(
            bare.queries[0].entries(&["fallback.md".to_string()]),
            vec!["fallback.md"]
        );
    }

    /// A wanted page may name the part of it that answers — the `eval`
    /// harness's section label ([#58]) — and this harness reads the page: the
    /// question it asks is which files a run should have returned.
    #[test]
    fn a_wanted_page_may_name_the_part_of_it_that_answers() {
        let labelled = load(
            r#"{"queries": [{"id": "one", "query": "q", "expected": [
                "a.md",
                {"path": "b.md", "heading": "Current policy"},
                {"path": "c.md", "lines": [12, 24]}]}]}"#,
        )
        .expect("the labelled spelling");
        assert_eq!(
            labelled.queries[0].wanted(),
            BTreeSet::from([
                PathBuf::from("a.md"),
                PathBuf::from("b.md"),
                PathBuf::from("c.md"),
            ])
        );
        assert_eq!(
            labelled.queries[0].pages(),
            vec!["a.md", "b.md", "c.md"],
            "a raw row records the pages, in the gold set's own order"
        );

        // A part that names no page names nothing to measure.
        assert!(
            load(r#"{"queries": [{"id": "a", "query": "q", "wanted": [{"heading": "x"}]}]}"#)
                .is_err()
        );
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

    /// A wiki need not have one way in. A query may name several entry pages,
    /// as one string or as an array, and a query that names none takes the
    /// caller's.
    #[test]
    fn a_query_may_name_a_set_of_entry_pages() {
        let one = load(
            r#"{"queries": [{"id": "a", "query": "q", "entry": "index.md",
            "wanted": ["a.md"]}]}"#,
        )
        .expect("one entry page");
        assert_eq!(one.queries[0].entries(&fallback()), vec!["index.md"]);

        let many = load(
            r#"{"queries": [{"id": "a", "query": "q",
            "entry": ["docs/start.md", "ops/start.md"], "wanted": ["a.md"]}]}"#,
        )
        .expect("two entry pages");
        assert_eq!(
            many.queries[0].entries(&fallback()),
            vec!["docs/start.md", "ops/start.md"]
        );

        let none = load(r#"{"queries": [{"id": "a", "query": "q", "wanted": ["a.md"]}]}"#)
            .expect("no entry page of its own");
        assert_eq!(
            none.queries[0].entries(&fallback()),
            vec!["one.md", "two.md"]
        );

        // A query whose entry is an empty array names none of its own.
        let empty = load(
            r#"{"queries": [{"id": "a", "query": "q", "entry": [],
            "wanted": ["a.md"]}]}"#,
        )
        .expect("an empty set");
        assert_eq!(
            empty.queries[0].entries(&fallback()),
            vec!["one.md", "two.md"]
        );
    }

    fn fallback() -> Vec<String> {
        vec!["one.md".to_string(), "two.md".to_string()]
    }
}
