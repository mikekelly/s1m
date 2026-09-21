//! What a wiki's link graph looks like, as numbers.
//!
//! Nothing here reports a path, a heading or a line of text: the whole point of
//! the numbers is that they can be published about a wiki that cannot be.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;

use s1m::parse::{self, LinkKind};
use serde::{Deserialize, Serialize};

/// The entry page conventions, in the order they are tried. Both are constants
/// of this harness rather than anything read from a wiki, so naming one in the
/// output leaks nothing.
pub const ENTRY_CONVENTIONS: [&str; 2] = ["index.md", "README.md"];

/// Everything the `graph-stats` command measures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphStats {
    /// Markdown and text files under the root.
    pub pages: usize,
    /// Whitespace-separated tokens of the raw markdown, markup included.
    pub words: usize,
    /// Heading sections over every page.
    pub headings: usize,
    /// Every link the parser found, by kind and by where it points.
    pub links: LinkCounts,
    /// Distinct page-to-page edges: one per source and target, self-links
    /// dropped, both link kinds counted.
    pub edges: usize,
    pub out_degree: Degrees,
    pub in_degree: Degrees,
    /// Pages no other page links to.
    pub orphans: usize,
    pub depth: Depth,
    /// The largest strongly connected component, in pages.
    pub largest_scc: usize,
}

/// Link instances, counted as they were written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkCounts {
    /// Every link instance the parser returned, duplicates included.
    pub total: usize,
    /// `[text](target.md)`.
    pub markdown: usize,
    /// `[[target]]`.
    pub wikilink: usize,
    /// Instances whose target is a page under the root.
    pub resolved: usize,
    /// Instances inside the root naming a file that is not there.
    pub broken: usize,
    /// Instances whose target escapes the root.
    pub outside: usize,
}

/// One degree distribution over every page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Degrees {
    pub min: usize,
    pub median: f64,
    pub mean: f64,
    pub max: usize,
    /// Pages per degree bucket, keyed by [`BUCKETS`]' labels.
    pub histogram: BTreeMap<String, usize>,
}

/// Which page the depths were measured from.
///
/// A convention is one of [`ENTRY_CONVENTIONS`], which are constants of this
/// harness and so safe to print. A page the caller named is not: it is a path
/// in a wiki that may be private, so it is recorded as having been given and
/// never as itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    Convention(String),
    Given,
}

/// How far each page sits from the entry page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Depth {
    /// Where the walk started, `None` when the root holds neither convention
    /// and the caller named no page.
    pub entry: Option<Entry>,
    /// Pages per hop count from the entry page, the entry page itself at 0.
    pub histogram: BTreeMap<usize, usize>,
    /// Pages no walk from the entry page reaches. Every page when there is no
    /// entry page.
    pub unreachable: usize,
}

/// The degree buckets, smallest first: `(label, first, last)` inclusive.
pub const BUCKETS: [(&str, usize, usize); 8] = [
    ("0", 0, 0),
    ("1", 1, 1),
    ("2", 2, 2),
    ("3-4", 3, 4),
    ("5-9", 5, 9),
    ("10-19", 10, 19),
    ("20-49", 20, 49),
    ("50+", 50, usize::MAX),
];

/// Reads every page under `root` and measures its link graph.
///
/// Every page is parsed once and every link looked up in a map of the pages, so
/// the cost is linear in the text and the links: a wiki of a few thousand pages
/// is one pass, not a pass per page.
pub fn collect(root: &Path, entries: &[String]) -> Result<GraphStats, String> {
    let pages = parse::pages(root);
    if pages.is_empty() {
        return Err(format!(
            "{}: no markdown or text pages under it",
            root.display()
        ));
    }
    let index: HashMap<&Path, usize> = pages
        .iter()
        .enumerate()
        .map(|(at, page)| (page.as_path(), at))
        .collect();

    let mut words = 0;
    let mut headings = 0;
    let mut links = LinkCounts {
        total: 0,
        markdown: 0,
        wikilink: 0,
        resolved: 0,
        broken: 0,
        outside: 0,
    };
    // Distinct page-to-page edges, kept as sorted adjacency so a page linked
    // twice is one edge and the degree counts agree with `edges`.
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); pages.len()];

    for (at, page) in pages.iter().enumerate() {
        let path = root.join(page);
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        words += text.split_whitespace().count();

        let parsed = parse::parse(&path, root).map_err(|error| error.to_string())?;
        headings += parsed
            .sections
            .iter()
            .filter(|section| section.heading.is_some())
            .count();

        for link in &parsed.links {
            links.total += 1;
            match link.kind {
                LinkKind::Markdown => links.markdown += 1,
                LinkKind::Wikilink => links.wikilink += 1,
            }
            if !link.in_root {
                links.outside += 1;
                continue;
            }
            match index.get(link.target.as_path()) {
                Some(&target) => {
                    links.resolved += 1;
                    if target != at {
                        out[at].push(target);
                    }
                }
                None => links.broken += 1,
            }
        }
        out[at].sort_unstable();
        out[at].dedup();
    }

    let edges = out.iter().map(Vec::len).sum();
    let incoming = in_degrees(&out);
    // The pages the caller named win over a convention, and one the wiki does
    // not hold is a mistake rather than a wiki nothing can reach.
    let entry = if entries.is_empty() {
        ENTRY_CONVENTIONS.into_iter().find_map(|convention| {
            let at = *index.get(Path::new(convention))?;
            Some((Entry::Convention(convention.to_string()), vec![at]))
        })
    } else {
        let mut starts = Vec::with_capacity(entries.len());
        for page in entries {
            let at = *index
                .get(Path::new(page))
                .ok_or_else(|| format!("{page}: not a page under {}", root.display()))?;
            starts.push(at);
        }
        Some((Entry::Given, starts))
    };
    Ok(GraphStats {
        pages: pages.len(),
        words,
        headings,
        links,
        edges,
        out_degree: degrees(out.iter().map(Vec::len)),
        in_degree: degrees(incoming.iter().copied()),
        orphans: incoming.iter().filter(|count| **count == 0).count(),
        depth: depth(&out, entry),
        largest_scc: largest_scc(&out),
    })
}

/// How far every page sits from the entry pages, breadth first: one hop count
/// per page, every entry page at zero, and a page no link chain from any of
/// them reaches counts as unreachable.
fn depth(out: &[Vec<usize>], entry: Option<(Entry, Vec<usize>)>) -> Depth {
    let Some((entry, starts)) = entry else {
        return Depth {
            entry: None,
            histogram: BTreeMap::new(),
            unreachable: out.len(),
        };
    };
    let mut hops = vec![usize::MAX; out.len()];
    let mut queue = VecDeque::new();
    for at in starts {
        if hops[at] == usize::MAX {
            hops[at] = 0;
            queue.push_back(at);
        }
    }
    let mut histogram = BTreeMap::new();
    let mut reached = 0;
    while let Some(page) = queue.pop_front() {
        reached += 1;
        *histogram.entry(hops[page]).or_insert(0) += 1;
        for &target in &out[page] {
            if hops[target] == usize::MAX {
                hops[target] = hops[page] + 1;
                queue.push_back(target);
            }
        }
    }
    Depth {
        entry: Some(entry),
        histogram,
        unreachable: out.len() - reached,
    }
}

/// The largest strongly connected component, in pages: Tarjan's algorithm with
/// its own stack rather than the call stack, so a long chain of pages is a deep
/// component and not a crash.
fn largest_scc(out: &[Vec<usize>]) -> usize {
    const UNVISITED: usize = usize::MAX;
    let mut index = vec![UNVISITED; out.len()];
    let mut low = vec![0; out.len()];
    let mut on_stack = vec![false; out.len()];
    let mut component = Vec::new();
    let mut next = 0;
    let mut largest = 0;

    for root in 0..out.len() {
        if index[root] != UNVISITED {
            continue;
        }
        // Each frame is a page and how many of its links have been walked.
        let mut frames = vec![(root, 0usize)];
        index[root] = next;
        low[root] = next;
        next += 1;
        component.push(root);
        on_stack[root] = true;

        while let Some((page, step)) = frames.pop() {
            if let Some(&target) = out[page].get(step) {
                frames.push((page, step + 1));
                if index[target] == UNVISITED {
                    index[target] = next;
                    low[target] = next;
                    next += 1;
                    component.push(target);
                    on_stack[target] = true;
                    frames.push((target, 0));
                } else if on_stack[target] {
                    low[page] = low[page].min(index[target]);
                }
                continue;
            }
            // Every link walked: this page is a component root when nothing
            // under it reached further back than the page itself.
            if low[page] == index[page] {
                let mut size = 0;
                while let Some(member) = component.pop() {
                    on_stack[member] = false;
                    size += 1;
                    if member == page {
                        break;
                    }
                }
                largest = largest.max(size);
            }
            if let Some(&(parent, _)) = frames.last() {
                low[parent] = low[parent].min(low[page]);
            }
        }
    }
    largest
}

/// How many distinct pages link to each page.
fn in_degrees(out: &[Vec<usize>]) -> Vec<usize> {
    let mut counts = vec![0; out.len()];
    for targets in out {
        for &target in targets {
            counts[target] += 1;
        }
    }
    counts
}

/// The distribution of one set of degrees. An empty set cannot happen — a wiki
/// with no pages is an error — so min and max are the first and last.
fn degrees(values: impl Iterator<Item = usize>) -> Degrees {
    let mut values: Vec<usize> = values.collect();
    values.sort_unstable();
    let mut histogram = BTreeMap::new();
    for (label, _, _) in BUCKETS {
        histogram.insert(label.to_string(), 0);
    }
    for value in &values {
        let (label, _, _) = BUCKETS
            .iter()
            .find(|(_, first, last)| value >= first && value <= last)
            .expect("the buckets cover every degree");
        *histogram.get_mut(*label).expect("a bucket per label") += 1;
    }
    Degrees {
        min: values.first().copied().unwrap_or(0),
        median: median(&values),
        mean: mean(&values),
        max: values.last().copied().unwrap_or(0),
        histogram,
    }
}

/// The middle value, or the mean of the two middle values.
fn median(sorted: &[usize]) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) as f64 / 2.0
    } else {
        sorted[middle] as f64
    }
}

fn mean(values: &[usize]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<usize>() as f64 / values.len() as f64
}

/// The statistics as a markdown table: two columns, every cell a number or a
/// word this file wrote. An entry page found by convention is named by the
/// convention, which is a constant of the harness ([`ENTRY_CONVENTIONS`]) and
/// not something the wiki said; one the caller gave is reported as given.
pub fn table(stats: &GraphStats) -> String {
    let mut out = String::from("| | |\n| --- | --- |\n");
    let mut row = |name: &str, value: String| {
        out.push_str(&format!("| {name} | {value} |\n"));
    };
    row("Pages", stats.pages.to_string());
    row("Words", stats.words.to_string());
    row("Headings", stats.headings.to_string());
    row(
        "Links",
        format!(
            "{} ({} markdown, {} wikilink)",
            stats.links.total, stats.links.markdown, stats.links.wikilink
        ),
    );
    row(
        "Link targets",
        format!(
            "{} inside the wiki, {} broken, {} outside it",
            stats.links.resolved, stats.links.broken, stats.links.outside
        ),
    );
    row("Edges", stats.edges.to_string());
    row(
        "Out-degree (min/median/mean/max)",
        spread(&stats.out_degree),
    );
    row("In-degree (min/median/mean/max)", spread(&stats.in_degree));
    row("Out-degree histogram", histogram(&stats.out_degree));
    row("In-degree histogram", histogram(&stats.in_degree));
    row("Orphans", stats.orphans.to_string());
    row(
        "Entry page",
        match &stats.depth.entry {
            Some(Entry::Convention(convention)) => format!("the `{convention}` convention"),
            Some(Entry::Given) => "given".to_string(),
            None => "none: the root holds neither convention".to_string(),
        },
    );
    row("Depth from the entry page", hops(&stats.depth));
    row(
        "Unreachable from the entry page",
        stats.depth.unreachable.to_string(),
    );
    row(
        "Largest strongly connected component",
        stats.largest_scc.to_string(),
    );
    out
}

fn spread(degrees: &Degrees) -> String {
    format!(
        "{} / {:.1} / {:.1} / {}",
        degrees.min, degrees.median, degrees.mean, degrees.max
    )
}

/// The buckets that hold a page, in order, as `label: pages`.
fn histogram(degrees: &Degrees) -> String {
    BUCKETS
        .iter()
        .filter_map(|(label, _, _)| {
            let pages = degrees.histogram.get(*label).copied().unwrap_or(0);
            (pages > 0).then(|| format!("{label}: {pages}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Pages per hop count, in order, as `hops: pages`.
fn hops(depth: &Depth) -> String {
    if depth.histogram.is_empty() {
        return "no entry page".to_string();
    }
    depth
        .histogram
        .iter()
        .map(|(hops, pages)| format!("{hops}: {pages}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempDir;

    /// A wiki small enough to count by hand: five pages, six link instances,
    /// five distinct edges, one two-page cycle and one page nothing links to.
    fn tiny_wiki() -> TempDir {
        let wiki = TempDir::new("graph");
        wiki.write(
            "index.md",
            "# Index\n\nSee [a](a.md) and [b](b.md).\n\n## More\n\nSee [[c]] and [a again](a.md).\n",
        );
        wiki.write("a.md", "# A\n\nBack to [index](index.md).\n");
        wiki.write("b.md", "# B\n\n## Sub\n\nNothing here.\n");
        wiki.write("c.md", "# C\n\nSee [b](b.md).\n");
        wiki.write("d.md", "# D\n");
        wiki
    }

    #[test]
    fn counts_pages_words_headings_and_links() {
        let wiki = tiny_wiki();
        let stats = collect(wiki.path(), &[]).expect("a wiki of five pages");

        assert_eq!(stats.pages, 5);
        // Counted by hand: 13 + 5 + 6 + 4 + 2 tokens.
        assert_eq!(stats.words, 30);
        // `# Index`, `## More`, `# A`, `# B`, `## Sub`, `# C`, `# D`.
        assert_eq!(stats.headings, 7);
        assert_eq!(
            stats.links,
            LinkCounts {
                total: 6,
                markdown: 5,
                wikilink: 1,
                resolved: 6,
                broken: 0,
                outside: 0,
            }
        );
        // `index.md` links to `a.md` twice, and an edge is a pair of pages.
        assert_eq!(stats.edges, 5);
    }

    #[test]
    fn describes_both_degree_distributions_and_counts_orphans() {
        let wiki = tiny_wiki();
        let stats = collect(wiki.path(), &[]).expect("a wiki of five pages");

        // Out-degrees, by hand: index 3, a 1, b 0, c 1, d 0.
        assert_eq!(
            (
                stats.out_degree.min,
                stats.out_degree.median,
                stats.out_degree.mean,
                stats.out_degree.max
            ),
            (0, 1.0, 1.0, 3)
        );
        assert_eq!(stats.out_degree.histogram["0"], 2);
        assert_eq!(stats.out_degree.histogram["1"], 2);
        assert_eq!(stats.out_degree.histogram["3-4"], 1);

        // In-degrees, by hand: index 1, a 1, b 2, c 1, d 0.
        assert_eq!(
            (
                stats.in_degree.min,
                stats.in_degree.median,
                stats.in_degree.mean,
                stats.in_degree.max
            ),
            (0, 1.0, 1.0, 2)
        );

        // `d.md` is the page nothing links to.
        assert_eq!(stats.orphans, 1);
    }

    #[test]
    fn measures_depth_from_the_entry_page_and_the_largest_cycle() {
        let wiki = tiny_wiki();
        let stats = collect(wiki.path(), &[]).expect("a wiki of five pages");

        assert_eq!(
            stats.depth.entry,
            Some(Entry::Convention("index.md".to_string()))
        );
        // The entry page, then `a`, `b` and `c` one hop away.
        assert_eq!(stats.depth.histogram, BTreeMap::from([(0, 1), (1, 3)]));
        // `d.md` is reachable from nothing.
        assert_eq!(stats.depth.unreachable, 1);
        // `index.md` and `a.md` link to each other; nothing else is in a cycle.
        assert_eq!(stats.largest_scc, 2);
    }

    #[test]
    fn falls_back_to_the_readme_convention_and_reports_no_entry_page() {
        let wiki = TempDir::new("graph-readme");
        wiki.write("README.md", "# Readme\n\nSee [a](a.md).\n");
        wiki.write("a.md", "# A\n");
        let stats = collect(wiki.path(), &[]).expect("a wiki of two pages");
        assert_eq!(
            stats.depth.entry,
            Some(Entry::Convention("README.md".to_string()))
        );
        assert_eq!(stats.depth.unreachable, 0);

        let neither = TempDir::new("graph-no-entry");
        neither.write("a.md", "# A\n");
        let stats = collect(neither.path(), &[]).expect("a wiki of one page");
        assert_eq!(stats.depth.entry, None);
        // With no entry page nothing is reached, and the histogram is empty.
        assert_eq!(stats.depth.unreachable, 1);
        assert!(stats.depth.histogram.is_empty());
    }

    /// The table is what gets committed about a wiki that cannot be, so it is
    /// numbers and fixed words: no page ever names itself in it.
    #[test]
    fn the_table_is_numbers_and_fixed_words() {
        let wiki = tiny_wiki();
        let stats = collect(wiki.path(), &[]).expect("a wiki of five pages");
        let table = table(&stats);

        assert!(table.contains("| Pages | 5 |"), "{table}");
        assert!(table.contains("| Words | 30 |"), "{table}");
        assert!(table.contains("| Headings | 7 |"), "{table}");
        assert!(table.contains("| Edges | 5 |"), "{table}");
        assert!(table.contains("| Orphans | 1 |"), "{table}");
        assert!(
            table.contains("| Largest strongly connected component | 2 |"),
            "{table}"
        );
        // Wikilinks against markdown links, the wiki's own link style.
        assert!(
            table.contains("| Links | 6 (5 markdown, 1 wikilink) |"),
            "{table}"
        );

        for page in ["a.md", "b.md", "c.md", "d.md", "tiny"] {
            assert!(
                !table.contains(page),
                "{page} is named in the table:\n{table}"
            );
        }
    }

    /// A wiki whose root holds neither convention still has a graph to measure
    /// depth over: the caller names the page to start from. What the report
    /// may then say about it is that it was given — never which page it was.
    #[test]
    fn an_entry_page_can_be_given_when_no_convention_is_there() {
        let wiki = TempDir::new("graph-given");
        wiki.write("docs/start.md", "# Start\n\nSee [a](../a.md).\n");
        wiki.write("a.md", "# A\n");
        wiki.write("b.md", "# B\n");

        // Without one, nothing is reachable: neither convention is there.
        let found = collect(wiki.path(), &[]).expect("a wiki of three pages");
        assert_eq!(found.depth.entry, None);
        assert_eq!(found.depth.unreachable, 3);

        let given =
            collect(wiki.path(), &["docs/start.md".to_string()]).expect("the page it was given");
        assert_eq!(given.depth.entry, Some(Entry::Given));
        assert_eq!(given.depth.histogram, BTreeMap::from([(0, 1), (1, 1)]));
        assert_eq!(given.depth.unreachable, 1);

        // The page it was given never reaches the table.
        let table = table(&given);
        assert!(table.contains("| Entry page | given |"), "{table}");
        assert!(!table.contains("docs/start.md"), "{table}");
        assert!(!table.contains("start"), "{table}");

        // A page the wiki does not hold is the caller's mistake, not an
        // unreachable wiki.
        let error =
            collect(wiki.path(), &["docs/absent.md".to_string()]).expect_err("no such page");
        assert!(error.contains("not a page"), "{error}");
    }

    /// A wiki with several ways in is measured from all of them at once: every
    /// given page is at depth zero, and what none of them reaches is what is
    /// unreachable.
    #[test]
    fn depth_can_be_measured_from_a_set_of_entry_pages() {
        let wiki = TempDir::new("graph-entries");
        wiki.write("docs/start.md", "# Start\n\nSee [a](../a.md).\n");
        wiki.write("ops/start.md", "# Ops\n");
        wiki.write("a.md", "# A\n");
        wiki.write("b.md", "# B\n");

        let given = collect(
            wiki.path(),
            &["docs/start.md".to_string(), "ops/start.md".to_string()],
        )
        .expect("two entry pages");
        assert_eq!(given.depth.entry, Some(Entry::Given));
        // Both given pages are at zero, `a.md` one hop from one of them.
        assert_eq!(given.depth.histogram, BTreeMap::from([(0, 2), (1, 1)]));
        assert_eq!(given.depth.unreachable, 1);

        // One page of the set that is not there is still the caller's mistake.
        assert!(
            collect(
                wiki.path(),
                &["docs/start.md".to_string(), "gone.md".to_string()]
            )
            .is_err()
        );
    }
}
