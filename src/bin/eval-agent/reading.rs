//! What s1m returned, and what opening it would cost an agent.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The characters a token is counted at: the rule `docs/spike-notes.md` set the
/// caps with, and the one the existing `eval` binary counts reading in, because
/// nothing here tokenises.
pub const CHARS_PER_TOKEN: usize = 4;

/// One s1m run, as its JSON reports it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    /// The files it returned, most relevant first.
    pub files: Vec<PathBuf>,
    /// Files it scored.
    pub visited: usize,
    /// Judgments it bought: zero when every answer was already stored.
    pub calls: u64,
    /// Characters in the returned ranges, counted once where they overlap.
    pub read_chars: usize,
    /// Those characters at [`CHARS_PER_TOKEN`]: what the agent reads if it
    /// opens what it was handed and nothing else.
    pub read_tokens: usize,
}

/// Reads s1m's JSON and counts what opening the returned ranges would cost.
///
/// A section's range contains its subsections', so overlapping ranges are the
/// normal case and the union is what a reader actually reads.
pub fn summarise(json: &str, wiki: &Path) -> Result<Reading, String> {
    let list: serde_json::Value =
        serde_json::from_str(json).map_err(|error| format!("s1m's reading list: {error}"))?;
    let results = list["results"]
        .as_array()
        .ok_or_else(|| "s1m's reading list: no results".to_string())?;

    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    let mut read_chars = 0;
    for result in results {
        let Some(path) = result["path"].as_str() else {
            continue;
        };
        let path = PathBuf::from(path);
        if !seen.insert(path.clone()) {
            continue;
        }
        let ranges: Vec<[usize; 2]> = result["sections"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|section| {
                let lines = section["lines"].as_array()?;
                Some([
                    lines.first()?.as_u64()? as usize,
                    lines.get(1)?.as_u64()? as usize,
                ])
            })
            .collect();
        read_chars += chars_in(&wiki.join(&path), &ranges);
        files.push(path);
    }

    Ok(Reading {
        files,
        visited: list["visited"].as_u64().unwrap_or_default() as usize,
        calls: list["calls"].as_u64().unwrap_or_default(),
        read_chars,
        read_tokens: read_chars.div_ceil(CHARS_PER_TOKEN),
    })
}

/// The characters in one file's `ranges` — 1-based and inclusive, as s1m gives
/// them — counted once where ranges overlap. A file that cannot be read counts
/// as nothing: what was returned is the measurement, and a missing file is the
/// wiki's business.
fn chars_in(path: &Path, ranges: &[[usize; 2]]) -> usize {
    let Ok(source) = std::fs::read_to_string(path) else {
        return 0;
    };
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut counted = vec![false; lines.len()];
    for [first, last] in ranges {
        for at in first.saturating_sub(1)..*last {
            if let Some(counted) = counted.get_mut(at) {
                *counted = true;
            }
        }
    }
    lines
        .iter()
        .zip(&counted)
        .filter(|(_, counted)| **counted)
        .map(|(line, _)| line.chars().count())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempDir;

    #[test]
    fn counts_the_union_of_the_returned_ranges() {
        let wiki = TempDir::new("reading");
        // Four lines of ten characters each, newline included.
        wiki.write("a.md", "123456789\n123456789\n123456789\n123456789\n");
        wiki.write("b.md", "12345\n");

        let json = r#"{"query":"q","mode":"useful-for","visited":3,"calls":2,"results":[
            {"path":"a.md","relevance":0.9,"scent":null,"via":[],"links":[],
             "sections":[{"heading":"A","lines":[1,3],"score":0.8},
                         {"heading":"B","lines":[2,2],"score":0.7}]},
            {"path":"b.md","relevance":0.7,"scent":0.8,"via":["a.md"],"links":[],
             "sections":[{"heading":null,"lines":[1,1],"score":0.7}]}]}"#;

        let reading = summarise(json, wiki.path()).expect("a reading list");
        assert_eq!(
            reading.files,
            vec![PathBuf::from("a.md"), PathBuf::from("b.md")]
        );
        assert_eq!((reading.visited, reading.calls), (3, 2));
        // Lines 1 to 3 of `a.md` are 30 characters, and line 2 is inside them;
        // `b.md`'s one line is 6. 36 characters is 9 tokens.
        assert_eq!(reading.read_chars, 36);
        assert_eq!(reading.read_tokens, 9);
    }

    /// A file s1m returned that cannot be read counts as nothing rather than
    /// failing the run: the measurement that matters is what it returned.
    #[test]
    fn a_file_that_is_not_there_costs_nothing_to_read() {
        let wiki = TempDir::new("reading-missing");
        wiki.write("a.md", "hello\n");
        let json = r#"{"query":"q","mode":"useful-for","visited":1,"calls":0,"results":[
            {"path":"gone.md","relevance":0.9,"scent":null,"via":[],"links":[],
             "sections":[{"heading":null,"lines":[1,2],"score":0.8}]}]}"#;
        let reading = summarise(json, wiki.path()).expect("a reading list");
        assert_eq!(reading.read_chars, 0);
        assert_eq!(reading.files, vec![PathBuf::from("gone.md")]);
    }
}
