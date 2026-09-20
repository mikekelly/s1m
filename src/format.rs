//! The shapes the reading list is printed in: the plan's `--format` flag.
//!
//! One reading list, three views of it. `json` is the plan's `Output` section
//! and the one a caller parses. `md` is the same list as something to read or
//! paste — the files, the lines worth reading and the scores that say why each
//! one is there. `tree` is the walk's link tree: every file it visited with
//! every link it judged under it, each with the scent the model gave it and
//! whether the walk followed it, so a person can see what was passed over and
//! how narrowly.
//!
//! Both reading views are rendered from [`ReadingList`] alone. They ask nothing
//! of the walk and nothing of the model, which is what makes them a view rather
//! than a second pipeline, and what lets a caller's own reading list be printed
//! the same way ([`ReadingList`]'s fields are public).
//!
//! # What the reading views keep, and what they drop
//!
//! - Scores are rounded to two decimals. The JSON is where the model's own
//!   number lives; these two are to read, and the second decimal is the last
//!   one a person acts on. A threshold sits at 0.60, so it is the last one the
//!   views need to keep the scores honest about.
//! - Lines are the parser's, printed `first-last` inclusive, and a section
//!   whose heading is `None` is the text before the file's first heading.
//! - A link the model named no scent for prints `scent unknown` rather than a
//!   number it never gave: such a link can never be followed, so it is always
//!   `pruned`, and inventing a 0.00 would read as a judgment.
//!
//! # Order
//!
//! `md` is the reading list in its own order: most relevant first, ties broken
//! by path, which is what a caller reading top-down wants.
//!
//! `tree` is the walk's own order, as far as the reading list can carry it. The
//! files no link reached — the entry files and the `--seed-grep` seeds — are the
//! roots, by path, which is the order the frontier holds them at path score 1.
//! Under a file, its judged links come in the order the frontier would have
//! taken them: highest scent first, ties broken by path, which is [`crate`]'s
//! own tie-break and total, so the tree never depends on the order answers
//! arrived in. A link the model gave no scent sorts last, because a link with no
//! scent cannot be queued at all.
//!
//! Every judged link is one line, and a link the walk followed whose target was
//! visited *and* is this file's own child is the line its subtree hangs from —
//! so a file is printed once, under the link that reached it, and a link to a
//! file reached better by another file is still `followed` with no subtree
//! under it. A file is visited once, along the best path found to it, and the
//! mark says what the link did, not where the walk ended up.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Write as _;

use crate::cli::{RankedFile, RankedLink, RankedSection, ReadingList};

/// Spaces of indentation per hop in the link tree.
const INDENT: usize = 2;

/// The shape the reading list is printed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The plan's `Output` section, verbatim: what a caller parses.
    Json,
    /// A reading list to read or paste: each file with the lines worth reading.
    Md,
    /// The walk's link tree: every judged link, and whether it was followed.
    Tree,
}

impl Format {
    /// `list` as this format prints it, newline at the end: the bytes that go
    /// to stdout.
    pub fn render(&self, list: &ReadingList) -> String {
        match self {
            Format::Json => format!("{}\n", list.to_json()),
            Format::Md => md(list),
            Format::Tree => tree(list),
        }
    }
}

/// The reading list as markdown: the files to read, most relevant first, each
/// with the lines worth reading inside it.
///
/// What a caller does with this is paste it — into a task, an issue, a note —
/// so it carries what the caller acts on: the path, the line ranges and the
/// heading to read from, in the order the caller should read them. The links
/// the walk judged are [`tree`]'s, and the model's own numbers are the JSON's.
///
/// A file with nothing above the section threshold says so rather than printing
/// no sections at all, which would read as a file with no sections in it.
fn md(list: &ReadingList) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Reading list: {}", list.query);
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Criterion: {}; {} visited, {}",
        list.mode,
        count(list.visited, "file"),
        count(list.calls as usize, "call"),
    );

    for (rank, file) in list.results.iter().enumerate() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## {}. `{}`", rank + 1, file.path);
        let _ = writeln!(out);
        let _ = writeln!(out, "{}", reached(file));
        let _ = writeln!(out);
        if file.sections.is_empty() {
            let _ = writeln!(out, "- nothing above --section-threshold");
        }
        for section in &file.sections {
            let _ = writeln!(out, "- {}", section_line(section));
        }
    }

    out
}

/// The walk's link tree, annotated: every file it visited, and under it every
/// link it judged, with that link's scent and whether the walk followed it.
///
/// The first line names the run, so a tree pasted on its own still says what it
/// is a tree of. Everything under it is one file or one judged link per line,
/// indented by hop, so a reader can see at a glance which links a page offered
/// and which of them the walk took — and, where a link refused the walk, what
/// it thought of the link anyway.
fn tree(list: &ReadingList) -> String {
    let index: HashMap<&str, &RankedFile> = list
        .results
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} ({}); {} visited, {}\n",
        list.query,
        list.mode,
        count(list.visited, "file"),
        count(list.calls as usize, "call"),
    );

    let mut roots: Vec<&RankedFile> = list
        .results
        .iter()
        .filter(|file| parent(file, &index).is_none())
        .collect();
    roots.sort_by(|a, b| a.path.cmp(&b.path));

    for root in roots {
        node(&mut out, root, 0, &root_line(root), &index);
    }

    out
}

/// What a root's line says about it: an entry file the caller named or a
/// `--seed-grep` keyword seed, for the files no link reached.
///
/// A result whose `via` names a file this list does not have is a root too —
/// [`parent`] has no link to nest it under — and saying it was an entry file
/// would be a provenance it does not have. It gets the line [`md`] prints for
/// the same file instead: the relevance, the scent of the link that reached it
/// and the path it came along.
fn root_line(root: &RankedFile) -> String {
    if !root.via.is_empty() {
        return reached(root);
    }
    let entry = if root.seeded {
        "keyword seed"
    } else {
        "entry file"
    };
    format!("{entry}; relevance {:.2}", root.relevance)
}

/// One file's line, then the links it judged: a link this file followed to a
/// target it reached nests that target's own subtree beneath it, and every
/// other link is a line with nothing under it.
///
/// `annotation` is the file's own line — its entry or seed or the scent of the
/// link that reached it, plus its relevance — because the line a reached file
/// is printed on *is* the link that reached it: one line per judged link, and
/// one per visited file, as the plan asks.
fn node(
    out: &mut String,
    file: &RankedFile,
    depth: usize,
    annotation: &str,
    index: &HashMap<&str, &RankedFile>,
) {
    let _ = writeln!(
        out,
        "{:indent$}{}  {}",
        "",
        file.path,
        annotation,
        indent = depth * INDENT
    );

    for link in queued(file) {
        match child(link, file, index) {
            Some(child) => {
                let annotation = format!(
                    "followed; {}; relevance {:.2}",
                    scent(link.scent),
                    child.relevance
                );
                node(out, child, depth + 1, &annotation, index);
            }
            None => {
                let mark = if link.followed { "followed" } else { "pruned" };
                let _ = writeln!(
                    out,
                    "{:indent$}{}  {}; {}",
                    "",
                    link.target,
                    mark,
                    scent(link.scent),
                    indent = (depth + 1) * INDENT,
                );
            }
        }
    }
}

/// The visited file a followed link stands for, when this file is the one that
/// reached it.
///
/// The walk keeps the best path to each file and visits it once, so a link to a
/// file another file reached at a higher score queued its target and is still
/// `followed`, but the subtree belongs to the link that reached the file — and
/// the same test is what stops a file being printed twice.
fn child<'a>(
    link: &RankedLink,
    file: &RankedFile,
    index: &HashMap<&str, &'a RankedFile>,
) -> Option<&'a RankedFile> {
    if !link.followed {
        return None;
    }
    let child = *index.get(link.target.as_str())?;
    (parent(child, index)?.path == file.path).then_some(child)
}

/// The file a result's `via` path says reached it, when that file was visited
/// and judges a followed link to it.
///
/// A result with no such parent is a root of the tree: an entry file or a seed,
/// which no link reached, and — for a reading list a caller built itself, or one
/// whose walk reported a path it did not visit — a result that would otherwise
/// be dropped from the tree rather than printed as a line of its own.
fn parent<'a>(file: &RankedFile, index: &HashMap<&str, &'a RankedFile>) -> Option<&'a RankedFile> {
    let parent = *index.get(file.via.last()?.as_str())?;
    parent
        .links
        .iter()
        .any(|link| link.followed && link.target == file.path)
        .then_some(parent)
}

/// A file's judged links in the order the walk would queue them: highest scent
/// first, ties broken by path.
///
/// Every link of one file multiplies into the same path score, so ordering by
/// scent is the frontier's own order. A link the model named no scent for cannot
/// be queued at all and sorts last.
fn queued(file: &RankedFile) -> Vec<&RankedLink> {
    let mut links: Vec<&RankedLink> = file.links.iter().collect();
    links.sort_by(|left, right| match (left.scent, right.scent) {
        (Some(left_scent), Some(right_scent)) => right_scent
            .total_cmp(&left_scent)
            .then_with(|| left.target.cmp(&right.target)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => left.target.cmp(&right.target),
    });
    links
}

/// What put a file in the reading list: the relevance the model gave it, and
/// the link path that reached it — or, for a file no link reached, whether it
/// was an entry file the caller named or a `--seed-grep` keyword seed.
fn reached(file: &RankedFile) -> String {
    let mut line = format!("relevance {:.2}", file.relevance);
    if file.via.is_empty() {
        let _ = write!(
            line,
            "; {}",
            if file.seeded {
                "keyword seed"
            } else {
                "entry file"
            }
        );
        return line;
    }
    if let Some(scent) = file.scent {
        let _ = write!(line, "; scent {scent:.2}");
    }
    let via = file
        .via
        .iter()
        .map(|path| format!("`{path}`"))
        .collect::<Vec<_>>()
        .join(" -> ");
    let _ = write!(line, "; via {via}");
    line
}

/// One section as a caller reads it: the lines to read, the score it was judged
/// at and the heading to look for. A section with no heading is the text before
/// the file's first heading, which the parser reports as a section in its own
/// right.
fn section_line(section: &RankedSection) -> String {
    format!(
        "lines {}-{}, score {:.2}, {}",
        section.lines[0],
        section.lines[1],
        section.score,
        section.heading.as_deref().unwrap_or("(preamble)"),
    )
}

/// A link's scent as the tree prints it.
fn scent(scent: Option<f64>) -> String {
    match scent {
        Some(scent) => format!("scent {scent:.2}"),
        None => "scent unknown".to_string(),
    }
}

/// `1 file` and `2 files`, so the header reads as a sentence.
fn count(number: usize, noun: &str) -> String {
    if number == 1 {
        format!("{number} {noun}")
    } else {
        format!("{number} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------- building

    /// A reading list of one query and the results a test built, with `calls`
    /// equal to what a cold run of one call per file would cost.
    fn list(results: Vec<RankedFile>) -> ReadingList {
        ReadingList {
            query: "settlement timing".to_string(),
            mode: "useful-for".to_string(),
            visited: results.len(),
            calls: results.len() as u64,
            results,
        }
    }

    /// An entry file: no `via`, nothing reaching it.
    fn entry(path: &str, relevance: f64) -> RankedFile {
        RankedFile {
            path: path.to_string(),
            relevance,
            scent: None,
            via: Vec::new(),
            seeded: false,
            sections: Vec::new(),
            links: Vec::new(),
        }
    }

    /// A file a link reached: the scent of that link, and the path it came
    /// along.
    fn reached(path: &str, relevance: f64, scent: f64, reached_via: &[&str]) -> RankedFile {
        RankedFile {
            path: path.to_string(),
            relevance,
            scent: Some(scent),
            via: reached_via.iter().map(|path| path.to_string()).collect(),
            seeded: false,
            sections: Vec::new(),
            links: Vec::new(),
        }
    }

    /// The same file as a `--seed-grep` keyword seed.
    fn seed(mut file: RankedFile) -> RankedFile {
        file.seeded = true;
        file
    }

    /// The same file with the sections a caller reads from.
    fn sectioned(mut file: RankedFile, sections: &[(Option<&str>, [usize; 2], f64)]) -> RankedFile {
        file.sections = sections
            .iter()
            .map(|(heading, lines, score)| RankedSection {
                heading: heading.map(str::to_string),
                lines: *lines,
                score: *score,
            })
            .collect();
        file
    }

    /// The same file with the links the walk judged, in the order the file
    /// lists them.
    fn linking(mut file: RankedFile, links: &[(&str, Option<f64>, bool)]) -> RankedFile {
        file.links = links
            .iter()
            .map(|(target, scent, followed)| RankedLink {
                target: target.to_string(),
                scent: *scent,
                followed: *followed,
            })
            .collect();
        file
    }

    // ------------------------------------------------------------------ md

    #[test]
    fn md_carries_the_ranked_files_and_the_lines_to_read() {
        let list = list(vec![
            sectioned(
                reached(
                    "wiki/payments/settlement.md",
                    0.94,
                    0.88,
                    &["wiki/index.md"],
                ),
                &[(Some("Settlement"), [12, 26], 0.93), (None, [1, 6], 0.71)],
            ),
            sectioned(
                entry("wiki/index.md", 0.62),
                &[(Some("Home"), [5, 9], 0.68)],
            ),
        ]);

        assert_eq!(
            Format::Md.render(&list),
            "\
# Reading list: settlement timing

Criterion: useful-for; 2 files visited, 2 calls

## 1. `wiki/payments/settlement.md`

relevance 0.94; scent 0.88; via `wiki/index.md`

- lines 12-26, score 0.93, Settlement
- lines 1-6, score 0.71, (preamble)

## 2. `wiki/index.md`

relevance 0.62; entry file

- lines 5-9, score 0.68, Home
"
        );
    }

    /// The list is what an agent reads and acts on, so it is titled with the
    /// query and says which criterion judged it and what the run cost — and a
    /// file with nothing above the section threshold says that, rather than
    /// printing as a file with no sections in it.
    #[test]
    fn md_says_when_no_section_cleared_the_threshold() {
        let list = list(vec![entry("wiki/index.md", 0.42)]);

        assert_eq!(
            Format::Md.render(&list),
            "\
# Reading list: settlement timing

Criterion: useful-for; 1 file visited, 1 call

## 1. `wiki/index.md`

relevance 0.42; entry file

- nothing above --section-threshold
"
        );
    }

    /// The reading views are for a person, so the counts read as a sentence
    /// rather than as the JSON's bare numbers.
    #[test]
    fn md_counts_read_as_a_sentence() {
        let list = list(vec![sectioned(entry("wiki/index.md", 0.42), &[])]);

        assert!(
            Format::Md
                .render(&list)
                .contains("1 file visited, 1 call\n")
        );
    }

    /// A run that visited nothing — `--max-files 0` — still prints its header,
    /// so a caller can tell an empty reading list from a failed run.
    #[test]
    fn an_empty_list_prints_its_header_alone() {
        let empty = list(Vec::new());

        assert_eq!(
            Format::Md.render(&empty),
            "\
# Reading list: settlement timing

Criterion: useful-for; 0 files visited, 0 calls
"
        );
        assert_eq!(
            Format::Tree.render(&empty),
            "settlement timing (useful-for); 0 files visited, 0 calls\n\n"
        );
    }

    // ---------------------------------------------------------------- tree

    /// The plan's tree: every judged link on its own line with its scent and
    /// whether the walk followed it, the files that were reached nested under
    /// the link that reached them, and the entry file every link came from.
    #[test]
    fn tree_nests_a_file_under_the_link_that_reached_it() {
        let list = list(vec![
            linking(
                entry("wiki/index.md", 0.62),
                &[
                    ("wiki/payments/README.md", Some(0.88), true),
                    ("wiki/notes/ledger.md", Some(0.31), false),
                ],
            ),
            sectioned(
                linking(
                    reached("wiki/payments/README.md", 0.81, 0.88, &["wiki/index.md"]),
                    &[("wiki/payments/settlement.md", Some(0.94), true)],
                ),
                &[(Some("Payments"), [1, 9], 0.85)],
            ),
            reached(
                "wiki/payments/settlement.md",
                0.94,
                0.94,
                &["wiki/index.md", "wiki/payments/README.md"],
            ),
        ]);

        assert_eq!(
            Format::Tree.render(&list),
            "\
settlement timing (useful-for); 3 files visited, 3 calls

wiki/index.md  entry file; relevance 0.62
  wiki/payments/README.md  followed; scent 0.88; relevance 0.81
    wiki/payments/settlement.md  followed; scent 0.94; relevance 0.94
  wiki/notes/ledger.md  pruned; scent 0.31
"
        );
    }

    /// A link the model named no scent for is not a link at zero scent: the
    /// tree says it has none, and such a link can never be followed.
    #[test]
    fn tree_prints_a_link_with_no_scent_as_unknown_and_pruned() {
        let list = list(vec![linking(
            entry("wiki/index.md", 0.62),
            &[("wiki/notes/ledger.md", None, false)],
        )]);

        assert_eq!(
            Format::Tree.render(&list),
            "\
settlement timing (useful-for); 1 file visited, 1 call

wiki/index.md  entry file; relevance 0.62
  wiki/notes/ledger.md  pruned; scent unknown
"
        );
    }

    /// The roots are what no link reached: the caller's entry files and the
    /// `--seed-grep` hits, by path — the order the frontier holds them, all at
    /// path score 1. A result whose `via` names no visited file is printed as a
    /// root rather than dropped, which is what keeps the tree total for a
    /// reading list a caller assembled itself — and it keeps the `via` and the
    /// scent it does have, the way `md` prints them, rather than claiming a
    /// provenance it does not.
    #[test]
    fn tree_starts_from_every_file_no_link_reached() {
        let list = list(vec![
            linking(
                reached("wiki/notes/scratch.md", 0.44, 0.9, &["wiki/absent.md"]),
                &[("wiki/notes/ledger.md", Some(0.31), false)],
            ),
            // Not in path order in the list, so the sort is what puts it first.
            entry("wiki/payments/README.md", 0.81),
            seed(entry("wiki/index.md", 0.62)),
        ]);

        assert_eq!(
            Format::Tree.render(&list),
            "\
settlement timing (useful-for); 3 files visited, 3 calls

wiki/index.md  keyword seed; relevance 0.62
wiki/notes/scratch.md  relevance 0.44; scent 0.90; via `wiki/absent.md`
  wiki/notes/ledger.md  pruned; scent 0.31
wiki/payments/README.md  entry file; relevance 0.81
"
        );
    }

    /// A file is visited once, along the best path found to it, so a link that
    /// queued a target another file reached at a higher score is still
    /// `followed` — the mark is what the link did — but the subtree belongs to
    /// the link that reached the file, and the file is printed once.
    #[test]
    fn tree_nests_a_file_once_under_the_link_that_reached_it() {
        let list = list(vec![
            linking(
                entry("wiki/index.md", 0.62),
                &[("wiki/payments/settlement.md", Some(0.55), true)],
            ),
            linking(
                entry("wiki/payments/README.md", 0.81),
                &[("wiki/payments/settlement.md", Some(0.94), true)],
            ),
            reached(
                "wiki/payments/settlement.md",
                0.94,
                0.94,
                &["wiki/payments/README.md"],
            ),
        ]);

        assert_eq!(
            Format::Tree.render(&list),
            "\
settlement timing (useful-for); 3 files visited, 3 calls

wiki/index.md  entry file; relevance 0.62
  wiki/payments/settlement.md  followed; scent 0.55
wiki/payments/README.md  entry file; relevance 0.81
  wiki/payments/settlement.md  followed; scent 0.94; relevance 0.94
"
        );
    }

    /// Links are listed in the order the frontier would take them, which is
    /// what puts the link the walk followed above the ones it passed over, and
    /// what breaks a tie between two scents by path — never by the order the
    /// answers arrived in.
    #[test]
    fn tree_lists_links_in_the_frontiers_order() {
        let list = list(vec![linking(
            entry("wiki/index.md", 0.62),
            &[
                ("wiki/notes/ledger.md", Some(0.31), false),
                ("wiki/payments/README.md", Some(0.88), true),
                ("wiki/payments/cutoffs.md", Some(0.88), true),
                ("wiki/notes/scratch.md", None, false),
            ],
        )]);

        assert_eq!(
            Format::Tree.render(&list),
            "\
settlement timing (useful-for); 1 file visited, 1 call

wiki/index.md  entry file; relevance 0.62
  wiki/payments/README.md  followed; scent 0.88
  wiki/payments/cutoffs.md  followed; scent 0.88
  wiki/notes/ledger.md  pruned; scent 0.31
  wiki/notes/scratch.md  pruned; scent unknown
"
        );
    }

    /// The JSON is the one format that keeps the model's own numbers: a caller
    /// diffing two runs of the same wiki needs the digits, and the reading
    /// views round them for the person reading.
    #[test]
    fn json_keeps_the_models_own_numbers_and_ends_in_a_newline() {
        let list = list(vec![sectioned(
            entry("wiki/index.md", 0.783_333_333_333_333_3),
            &[(Some("Home"), [1, 9], 0.7)],
        )]);

        let json = Format::Json.render(&list);

        assert!(json.ends_with("}\n"), "{json}");
        assert!(json.contains("0.7833333333333333"), "{json}");
    }
}
