//! Parser tests against the fixture wiki in `tests/fixtures/`.
//!
//! These expectations are the contract from
//! [#4](https://github.com/mikekelly/s1m/issues/4): every link form, the line
//! ranges of nested headings, out-of-root and broken links, determinism.

use std::path::{Path, PathBuf};

use s1m::parse::{
    FrontmatterField, Link, LinkKind, ParseError, ParsedFile, Section, parse, preview,
};

/// Every page in the fixture wiki. The line-range invariant runs over all of
/// them, so a page missing from here is a page whose ranges stop being checked.
const PAGES: &[&str] = &[
    "index.md",
    "payments/README.md",
    "payments/cutoffs.md",
    "payments/settlement.md",
    "notes/ledger.md",
    "notes/scratch.md",
    "notes/weekly review.md",
    "notes/reading.md",
    "notes/reading.txt",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wiki")
}

fn doc(page: &str) -> ParsedFile {
    parse(root().join(page), root()).unwrap_or_else(|err| panic!("{page}: {err}"))
}

fn section(heading: Option<&str>, level: u8, lines: [usize; 2]) -> Section {
    Section {
        heading: heading.map(str::to_string),
        level,
        lines,
    }
}

fn link_to<'a>(parsed: &'a ParsedFile, target: &str) -> &'a Link {
    parsed
        .links
        .iter()
        .find(|link| link.target == Path::new(target))
        .unwrap_or_else(|| panic!("no link to {target} in {:?}", targets(parsed)))
}

fn targets(parsed: &ParsedFile) -> Vec<String> {
    parsed
        .links
        .iter()
        .map(|link| link.target.display().to_string())
        .collect()
}

/// The part of a link these tests compare: everything the source text decides,
/// with the sentence checked separately where it matters.
fn shape(link: &Link) -> (String, String, Option<String>, bool) {
    (
        link.target.display().to_string(),
        link.anchor.clone(),
        link.heading.clone(),
        link.in_root,
    )
}

/// First line of the body: just after the frontmatter fence, or line 1.
fn body_start_line(source: &str) -> usize {
    let mut lines = source.split_inclusive('\n');
    if lines.next().map(|line| line.trim_end()) != Some("---") {
        return 1;
    }
    let mut number = 1;
    for line in lines {
        number += 1;
        if line.trim_end() == "---" {
            return number + 1;
        }
    }
    1
}

#[test]
fn title_prefers_frontmatter_then_h1_then_the_file_name() {
    // Frontmatter, quotes stripped.
    assert_eq!(
        doc("payments/settlement.md").title,
        "Instant payout settlement"
    );
    // Frontmatter wins over the page's own H1.
    assert_eq!(doc("notes/ledger.md").title, "Ledger");
    assert_eq!(
        doc("notes/ledger.md")
            .sections
            .iter()
            .filter_map(|section| section.heading.clone())
            .collect::<Vec<_>>(),
        ["The ledger", "Accounts"]
    );
    // No frontmatter, so the first H1.
    assert_eq!(doc("payments/README.md").title, "Payments");
    assert_eq!(doc("index.md").title, "Home");
    // No frontmatter and no H1, so the file name.
    assert_eq!(doc("payments/cutoffs.md").title, "cutoffs");
}

#[test]
fn frontmatter_is_flat_key_value_pairs() {
    assert_eq!(
        doc("index.md").frontmatter,
        [
            FrontmatterField {
                key: "title".to_string(),
                value: "Home".to_string(),
            },
            FrontmatterField {
                key: "tags".to_string(),
                value: "wiki, fixtures".to_string(),
            },
        ]
    );
    assert_eq!(
        doc("payments/settlement.md").frontmatter,
        [FrontmatterField {
            key: "title".to_string(),
            value: "Instant payout settlement".to_string(),
        }]
    );
    assert!(doc("payments/README.md").frontmatter.is_empty());
}

#[test]
fn a_parent_section_reaches_its_subsections() {
    // `## Instant payouts` owns `### Windows`; `## Settlement` closes both.
    assert_eq!(
        doc("payments/README.md").sections,
        [
            section(Some("Payments"), 1, [1, 25]),
            section(Some("Instant payouts"), 2, [5, 12]),
            section(Some("Windows"), 3, [9, 12]),
            section(Some("Settlement"), 2, [13, 25]),
        ]
    );
}

#[test]
fn content_before_the_first_heading_is_a_section_with_no_heading() {
    assert_eq!(
        doc("notes/scratch.md").sections,
        [
            section(None, 0, [1, 2]),
            section(Some("Scratch"), 1, [3, 5]),
        ]
    );
}

#[test]
fn frontmatter_is_not_part_of_a_section() {
    // settlement.md: frontmatter on 1-3, so the body starts at 4 and the blank
    // line before the H1 leaves no empty section behind.
    assert_eq!(
        doc("payments/settlement.md").sections,
        [
            section(Some("Settlement"), 1, [5, 11]),
            section(Some("Windows"), 2, [9, 11]),
        ]
    );
    assert_eq!(
        doc("index.md").sections,
        [section(Some("Home"), 1, [6, 20])]
    );
    assert_eq!(
        doc("notes/ledger.md").sections,
        [
            section(Some("The ledger"), 1, [5, 11]),
            section(Some("Accounts"), 2, [9, 11]),
        ]
    );
    assert_eq!(
        doc("payments/cutoffs.md").sections,
        [section(Some("Cutoff times"), 2, [1, 7])]
    );
}

#[test]
fn sections_cover_every_non_blank_body_line() {
    for page in PAGES {
        let source = std::fs::read_to_string(root().join(page)).unwrap();
        let parsed = doc(page);
        let body_start = body_start_line(&source);
        let last_line = source.lines().count();

        assert!(
            parsed
                .sections
                .iter()
                .all(|section| section.lines[0] <= section.lines[1]),
            "{page}: a section ends before it starts: {:?}",
            parsed.sections
        );
        assert!(
            parsed
                .sections
                .windows(2)
                .all(|pair| pair[0].lines[0] < pair[1].lines[0]),
            "{page}: sections are not in document order: {:?}",
            parsed.sections
        );
        assert_eq!(
            parsed.sections.last().unwrap().lines[1],
            last_line,
            "{page}: the last section does not reach the end of the file"
        );

        for (index, line) in source.lines().enumerate() {
            let number = index + 1;
            if number < body_start || line.trim().is_empty() {
                continue;
            }
            assert!(
                parsed
                    .sections
                    .iter()
                    .any(|section| section.lines[0] <= number && number <= section.lines[1]),
                "{page}: line {number} is in no section: {:?}",
                parsed.sections
            );
        }
    }
}

#[test]
fn every_link_form_is_reported_in_document_order() {
    let parsed = doc("index.md");

    assert_eq!(
        parsed.links.iter().map(shape).collect::<Vec<_>>(),
        [
            // [text](relative.md), wrapped over two source lines
            (
                "payments/README.md".to_string(),
                "payments".to_string(),
                Some("Home".to_string()),
                true
            ),
            // [text](relative.md#anchor): the fragment is not part of the path
            (
                "payments/cutoffs.md".to_string(),
                "cutoffs".to_string(),
                Some("Home".to_string()),
                true
            ),
            // [[wikilink]] resolved by file name from the root, not from index.md
            (
                "payments/settlement.md".to_string(),
                "settlement".to_string(),
                Some("Home".to_string()),
                true
            ),
            // [[wikilink|alias]]: alias is the anchor text
            (
                "notes/ledger.md".to_string(),
                "the ledger".to_string(),
                Some("Home".to_string()),
                true
            ),
            // out of root: reported, never followed
            (
                "../outside.md".to_string(),
                "outside the root".to_string(),
                Some("Home".to_string()),
                false
            ),
            // broken: the file is absent but the link is still reported
            (
                "payments/missing.md".to_string(),
                "missing page".to_string(),
                Some("Home".to_string()),
                true
            ),
            // [[name.md]]: an explicit extension matches the file name
            (
                "payments/cutoffs.md".to_string(),
                "cutoffs.md".to_string(),
                Some("Home".to_string()),
                true
            ),
            // a wikilink that names a path can point out of the root too
            (
                "../outside.md".to_string(),
                "../outside.md".to_string(),
                Some("Home".to_string()),
                false
            ),
        ]
    );

    // Dropped: external URL, mailto, non-markdown target, anchor-only link, a
    // wikilink that matches nothing, and a wikilink matching only a non-markdown
    // file (`assets/logo.png`).
    let reported = targets(&parsed);
    for dropped in [
        "https://example.com/page.md",
        "mailto:a@b.test",
        "assets/logo.png",
        "does-not-exist",
        "logo",
    ] {
        assert!(
            !reported.iter().any(|target| target == dropped),
            "{dropped} should have been dropped, got {reported:?}"
        );
    }
}

#[test]
fn the_sentence_is_the_one_the_link_sits_in() {
    assert_eq!(
        link_to(&doc("index.md"), "payments/README.md").sentence,
        "It links to payments and to cutoffs too."
    );
    assert_eq!(
        link_to(&doc("index.md"), "payments/settlement.md").sentence,
        "See also settlement and the ledger."
    );
    assert_eq!(
        link_to(&doc("payments/README.md"), "payments/cutoffs.md").sentence,
        "See the cutoffs page."
    );
    assert_eq!(
        link_to(&doc("notes/scratch.md"), "notes/ledger.md").sentence,
        "See ledger."
    );
}

#[test]
fn each_link_carries_its_enclosing_heading() {
    let readme = doc("payments/README.md");
    assert_eq!(
        readme.links.iter().map(shape).collect::<Vec<_>>(),
        [
            (
                "payments/settlement.md".to_string(),
                "payouts".to_string(),
                Some("Payments".to_string()),
                true
            ),
            (
                "notes/ledger.md".to_string(),
                "ledger".to_string(),
                Some("Payments".to_string()),
                true
            ),
            (
                "payments/cutoffs.md".to_string(),
                "cutoffs".to_string(),
                Some("Instant payouts".to_string()),
                true
            ),
            (
                "notes/ledger.md".to_string(),
                "ten minute".to_string(),
                Some("Windows".to_string()),
                true
            ),
            (
                "payments/cutoffs.md".to_string(),
                "cutoffs".to_string(),
                Some("Settlement".to_string()),
                true
            ),
        ]
    );
}

#[test]
fn fenced_code_is_not_prose() {
    // README.md has a fence holding a heading, a link and a wikilink. None of
    // the three may be reported.
    let readme = doc("payments/README.md");
    assert_eq!(readme.sections.len(), 4);
    assert!(
        !targets(&readme)
            .iter()
            .any(|target| target == "whatever.md")
    );

    let scratch = doc("notes/scratch.md");
    assert_eq!(scratch.title, "Scratch");
}

#[test]
fn a_relative_link_that_stays_inside_the_root_is_in_root() {
    // notes/ledger.md walks up into payments/ without leaving the root.
    assert!(link_to(&doc("notes/ledger.md"), "payments/settlement.md").in_root);
    // and the file it points at is really outside the wiki.
    assert!(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/outside.md")
            .is_file()
    );
}

#[test]
fn percent_encoded_targets_resolve_to_the_file_that_exists() {
    let ledger = doc("notes/ledger.md");

    // `[weekly review](weekly%20review.md)` names a page with a space in it.
    assert_eq!(
        targets(&ledger),
        [
            "payments/settlement.md",
            "notes/weekly review.md",
            "notes/reading.txt",
            "notes/reading.md",
        ]
    );
    assert_eq!(
        link_to(&ledger, "notes/weekly review.md").anchor,
        "weekly review"
    );
    assert!(link_to(&ledger, "notes/weekly review.md").in_root);
}

#[test]
fn text_targets_are_kept_and_markdown_wins_a_name_match() {
    let ledger = doc("notes/ledger.md");

    // `[reading](../notes/reading.txt)`: a `.txt` target is a target.
    assert!(link_to(&ledger, "notes/reading.txt").in_root);
    // `[[reading]]` matches both files by name; the `.md` one wins.
    assert_eq!(link_to(&ledger, "notes/reading.md").anchor, "reading");
    assert_eq!(
        link_to(&ledger, "notes/reading.md").sentence,
        link_to(&ledger, "notes/reading.txt").sentence
    );
}

#[test]
fn output_is_deterministic() {
    let first = doc("index.md");
    let encoded = serde_json::to_string(&first).unwrap();

    for _ in 0..10 {
        let again = doc("index.md");
        assert_eq!(again, first, "parse is not deterministic");
        assert_eq!(
            serde_json::to_string(&again).unwrap(),
            encoded,
            "serialised output is not deterministic"
        );
    }

    assert_eq!(
        targets(&first),
        [
            "payments/README.md",
            "payments/cutoffs.md",
            "payments/settlement.md",
            "notes/ledger.md",
            "../outside.md",
            "payments/missing.md",
            "payments/cutoffs.md",
            "../outside.md",
        ]
    );
}

#[test]
fn preview_returns_the_title_frontmatter_and_first_paragraph() {
    let settlement = preview(root().join("payments/settlement.md"), root()).unwrap();
    assert_eq!(settlement.title, "Instant payout settlement");
    assert_eq!(
        settlement.first_paragraph.as_deref(),
        Some("Settlement timing depends on the cutoff table.")
    );
    assert_eq!(
        settlement.frontmatter,
        [FrontmatterField {
            key: "title".to_string(),
            value: "Instant payout settlement".to_string(),
        }]
    );

    // The frontmatter and the H1 are not the first paragraph.
    let ledger = preview(root().join("notes/ledger.md"), root()).unwrap();
    assert_eq!(ledger.title, "Ledger");
    assert_eq!(
        ledger.first_paragraph.as_deref(),
        Some("The ledger records every movement.")
    );

    // Preview falls back to the file name for the title too.
    let cutoffs = preview(root().join("payments/cutoffs.md"), root()).unwrap();
    assert_eq!(cutoffs.title, "cutoffs");
    assert_eq!(
        cutoffs.first_paragraph.as_deref(),
        Some("Cutoffs are 16:00 UTC on business days. See settlement.")
    );
}

#[test]
fn preview_carries_the_headings_and_the_in_root_link_text() {
    let readme = preview(root().join("payments/README.md"), root()).unwrap();

    // H2s and the H3 under one of them, in the file's own order. The H1 is the
    // title the preview already carries.
    assert_eq!(readme.title, "Payments");
    assert_eq!(
        readme.headings,
        ["Instant payouts", "Windows", "Settlement"],
        "the page's own parts, in order"
    );

    // Every link's anchor text, in document order. The fenced block's
    // `[not a link](whatever.md)` was never a link, and the anchor the page
    // repeats is left here for the caller to drop: this is what the page says,
    // not what a request can afford.
    assert_eq!(
        readme.leads,
        ["payouts", "ledger", "cutoffs", "ten minute", "cutoffs"]
    );

    // A link that leaves the root is not part of what the page leads on with; a
    // broken one that stays inside it is, because the text names a target.
    let index = preview(root().join("index.md"), root()).unwrap();
    assert!(
        index.leads.contains(&"missing page".to_string()),
        "an in-root link to a page that is not there is still a lead"
    );
    assert!(
        !index.leads.contains(&"outside the root".to_string()),
        "and a link out of the root is not: {:?}",
        index.leads
    );
}

#[test]
fn unreadable_and_mixed_base_inputs_are_errors() {
    assert!(matches!(
        parse(root().join("absent.md"), root()),
        Err(ParseError::Read { .. })
    ));
    assert!(matches!(
        parse(Path::new("index.md"), Path::new("/tmp/wiki")),
        Err(ParseError::BaseMismatch { .. })
    ));
}

/// A link carries the syntax it was written in, because a wiki's link style is
/// a fact about the wiki: the graph statistics count `[[wikilinks]]` against
/// `[markdown](links.md)`, and only the parser knows which a target came from.
#[test]
fn a_link_reports_the_syntax_it_was_written_in() {
    let index = doc("index.md");

    // `[payments](payments/README.md)`.
    assert_eq!(
        link_to(&index, "payments/README.md").kind,
        LinkKind::Markdown
    );
    // `[[settlement]]`, resolved by file name.
    assert_eq!(
        link_to(&index, "payments/settlement.md").kind,
        LinkKind::Wikilink
    );
    // `[[ledger|the ledger]]`: an alias does not change the syntax.
    assert_eq!(link_to(&index, "notes/ledger.md").kind, LinkKind::Wikilink);
    // `[outside the root](../outside.md)`: an escaping target is still markdown.
    assert_eq!(link_to(&index, "../outside.md").kind, LinkKind::Markdown);
}
