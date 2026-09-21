//! The committed half: a markdown report of numbers.
//!
//! The wiki this measures may be private, so the report is rendered from
//! [`Aggregates`] and [`GraphStats`] and from nothing else. Neither holds a
//! query, a path, a heading or a line of text — the raw JSONL keeps those, and
//! the raw JSONL is never committed. What is left that a wiki could still speak
//! through is the labels: a query id and a category come from the gold set,
//! whoever wrote it. So every label is checked before it is printed, and a
//! label that looks like a path or reads like a sentence stops the report
//! rather than appearing in it.

use crate::aggregate::{Aggregates, Group};
use crate::graph::{self, GraphStats};
use serde::{Deserialize, Serialize};

/// How a run was made: flags and constants, never a path or a query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Method {
    pub repeats: usize,
    pub conditions: Vec<String>,
    pub claude_model: String,
    /// The flags the agent was run under, values included where they are not
    /// paths: what a reader needs to repeat the run.
    pub claude_flags: Vec<String>,
    pub s1m_flags: Vec<String>,
    /// The prompt template, which is a constant of this harness: the query and
    /// the entry page are substituted into it at run time and are not here.
    pub explore_prompt: String,
    pub s1m_agent_prompt: String,
    pub chars_per_token: usize,
}

/// The longest a label may be before it is treated as prose.
const LABEL_LIMIT: usize = 64;

/// The metrics the results table shows, in order, with the heading each one
/// prints under. A metric no condition measured is left out of the table.
const COLUMNS: [(&str, &str); 8] = [
    ("recall", "Recall"),
    ("precision", "Precision"),
    ("agent_read_tokens", "Agent tokens"),
    ("agent_total_tokens", "Billed tokens"),
    ("files_opened", "Files opened"),
    ("cost_usd", "Cost (USD)"),
    ("wall_ms", "Wall (ms)"),
    ("jev_cost_usd", "Jev cost (USD)"),
];

/// Renders the report, or says which label stopped it.
pub fn render(
    aggregates: &Aggregates,
    stats: Option<&GraphStats>,
    method: &Method,
) -> Result<String, String> {
    let mut out = String::from("# Evaluation: a reading list against an agent that explores\n\n");
    out.push_str(
        "s1m hands an agent a ranked reading list; a Claude Code Explore agent \
         opens the wiki and looks. This is what each is worth on one wiki, in \
         numbers only: the wiki, the queries and the pages they name stay out \
         of this file, so it can be published about a wiki that cannot be.\n\n",
    );

    if let Some(stats) = stats {
        out.push_str("## The wiki\n\n");
        out.push_str(&graph::table(stats));
        out.push('\n');
    }

    method_section(&mut out, method, aggregates);
    results_section(&mut out, aggregates)?;
    per_query_section(&mut out, aggregates)?;
    caveats_section(&mut out, method);
    Ok(out)
}

fn method_section(out: &mut String, method: &Method, aggregates: &Aggregates) {
    out.push_str("## Method\n\n");
    out.push_str(&format!(
        "{} queries, {} repeat(s) per condition. A run is one query under one \
         condition; recall and precision are against the gold set's wanted \
         pages, and a run that errored is counted and left out of every \
         average.\n\n",
        aggregates.queries, method.repeats
    ));
    out.push_str("| | |\n| --- | --- |\n");
    out.push_str(&format!(
        "| Conditions | {} |\n",
        method.conditions.join(", ")
    ));
    out.push_str(&format!("| Agent model | `{}` |\n", method.claude_model));
    out.push_str(&format!(
        "| Agent flags | {} |\n",
        flags(&method.claude_flags)
    ));
    out.push_str(&format!("| s1m flags | {} |\n", flags(&method.s1m_flags)));
    out.push_str(&format!(
        "| Agent tokens | returned characters at {} a token |\n",
        method.chars_per_token
    ));
    out.push_str(
        "\nThe agent was asked, with the query and the entry page \
                  substituted in:\n\n",
    );
    out.push_str(&quote(&method.explore_prompt));
    if !method.s1m_agent_prompt.is_empty() {
        out.push_str(
            "\nThe `s1m-agent` condition was asked, with the reading \
                      list substituted in:\n\n",
        );
        out.push_str(&quote(&method.s1m_agent_prompt));
    }
    out.push('\n');
}

/// The flags as a reader would type them, back-quoted one by one.
fn flags(flags: &[String]) -> String {
    if flags.is_empty() {
        return "the defaults".to_string();
    }
    flags
        .iter()
        .map(|flag| format!("`{flag}`"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(text: &str) -> String {
    text.lines()
        .map(|line| format!("> {line}\n"))
        .collect::<String>()
}

/// One row per condition, one column per metric anything measured.
fn results_section(out: &mut String, aggregates: &Aggregates) -> Result<(), String> {
    out.push_str("## Results\n\n");
    let columns: Vec<(&str, &str)> = COLUMNS
        .into_iter()
        .filter(|(metric, _)| {
            aggregates
                .conditions
                .values()
                .any(|condition| condition.overall.metrics.contains_key(*metric))
        })
        .collect();

    let mut head = vec!["Condition".to_string(), "Runs".to_string()];
    head.extend(columns.iter().map(|(_, title)| (*title).to_string()));
    table_head(out, &head);
    for (name, condition) in &aggregates.conditions {
        let mut cells = vec![format!("`{}`", safe(name)?), runs(&condition.overall)];
        cells.extend(
            columns
                .iter()
                .map(|(metric, _)| cell(&condition.overall, metric)),
        );
        table_row(out, &cells);
    }
    out.push_str(
        "\nEvery cell is the mean over the runs, with the sample standard \
         deviation after it where there was more than one run.\n\n",
    );
    Ok(())
}

/// One row per query and condition: the id, the category and the numbers.
fn per_query_section(out: &mut String, aggregates: &Aggregates) -> Result<(), String> {
    out.push_str("## Per query\n\n");
    let columns: Vec<(&str, &str)> = COLUMNS
        .into_iter()
        .filter(|(metric, _)| {
            aggregates.conditions.values().any(|condition| {
                condition
                    .by_query
                    .values()
                    .any(|query| query.group.metrics.contains_key(*metric))
            })
        })
        .collect();

    let mut head = vec![
        "Query".to_string(),
        "Category".to_string(),
        "Condition".to_string(),
    ];
    head.extend(columns.iter().map(|(_, title)| (*title).to_string()));
    table_head(out, &head);

    let mut ids: Vec<&String> = aggregates
        .conditions
        .values()
        .flat_map(|condition| condition.by_query.keys())
        .collect();
    ids.sort();
    ids.dedup();

    for id in ids {
        for (name, condition) in &aggregates.conditions {
            let Some(query) = condition.by_query.get(id) else {
                continue;
            };
            let mut cells = vec![
                format!("`{}`", safe(id)?),
                safe(&query.category)?.to_string(),
                format!("`{}`", safe(name)?),
            ];
            cells.extend(columns.iter().map(|(metric, _)| cell(&query.group, metric)));
            table_row(out, &cells);
        }
    }
    out.push_str(
        "\nThe query column is the gold set's id for the query, and \
                  the category its label: the question itself, the pages it \
                  wanted and the files any run opened are in the raw rows under \
                  the run directory, which is not committed.\n\n",
    );
    Ok(())
}

fn caveats_section(out: &mut String, method: &Method) {
    out.push_str("## Caveats\n\n");
    for caveat in [
        "Recall and precision are against one gold set, written by reading the \
         wiki. A page that is useful and unlisted costs precision, so precision \
         is a lower bound.",
        "The Explore condition is scored on the files the agent said it relied \
         on. The files it actually opened are counted separately, and the two \
         are not the same set: an agent reads more than it cites.",
        "The agent's tokens are what Claude Code billed the subagent for, \
         summed over its turns from its own transcript, cache reads and cache \
         writes included. The parent agent's tokens are reported beside them \
         and are not part of the subagent's figure.",
        "Cost is what the CLI priced the whole run at, parent included. A \
         subagent's share of it is apportioned by tokens, which is an estimate \
         and not a price.",
        "s1m's tokens are the characters in the ranges it returned, counted at \
         the rate in the method table. Nothing here tokenises, and an agent \
         that opens a returned file whole reads more than that.",
        "The agent token columns and s1m's are not the same quantity. An \
         agent's are what it was billed for, which includes its system prompt, \
         its tool definitions and every tool result it read; s1m's are the \
         characters of wiki text it asked for. The comparison that puts them \
         on one footing is `s1m-agent` against `explore`.",
        "Wall time is one machine on one network, and the agent condition \
         depends on a service whose latency is not ours.",
        "The two conditions are not the same shape of work: s1m returns a \
         reading list and answers nothing, the agent reads until it can answer. \
         The `s1m-agent` condition is the one that compares like with like.",
    ] {
        out.push_str(&format!("- {caveat}\n"));
    }
    let _ = method;
}

fn runs(group: &Group) -> String {
    match group.failed {
        0 => group.runs.to_string(),
        failed => format!("{} ({failed} failed)", group.runs),
    }
}

/// One metric's mean, with its spread where there is one.
fn cell(group: &Group, metric: &str) -> String {
    let Some(stat) = group.metrics.get(metric) else {
        return "—".to_string();
    };
    match stat.sd {
        Some(sd) => format!("{} ± {}", number(stat.mean), number(sd)),
        None => number(stat.mean),
    }
}

/// A number at the precision it means something: money to six places, rates to
/// two, and anything counted in tokens or milliseconds whole. A cost of a tenth
/// of a cent is a real number and must not print as zero.
fn number(value: f64) -> String {
    if value != 0.0 && value.abs() < 0.01 {
        return format!("{value:.6}");
    }
    if value.abs() < 100.0 {
        return format!("{value:.2}");
    }
    format!("{}", value.round() as i64)
}

fn table_head(out: &mut String, cells: &[String]) {
    table_row(out, cells);
    out.push('|');
    for _ in cells {
        out.push_str(" --- |");
    }
    out.push('\n');
}

fn table_row(out: &mut String, cells: &[String]) {
    out.push('|');
    for cell in cells {
        out.push_str(&format!(" {cell} |"));
    }
    out.push('\n');
}

/// A label may be printed when it cannot be mistaken for a path and does not
/// read like a sentence. This is the last gate between a private gold set and
/// a committed file, so it errs towards refusing.
fn safe(label: &str) -> Result<&str, String> {
    let lower = label.to_ascii_lowercase();
    let path_like = ['/', '\\'].iter().any(|bad| label.contains(*bad))
        || [".md", ".txt", ".markdown"]
            .iter()
            .any(|bad| lower.contains(bad));
    if path_like {
        return Err(format!(
            "{label:?} looks like a path: a report never names a file, so \
             give the query an id that is a name"
        ));
    }
    if label.chars().count() > LABEL_LIMIT {
        return Err(format!(
            "{label:?} is longer than {LABEL_LIMIT} characters: a report prints \
             ids and categories, never a query"
        ));
    }
    if label.split_whitespace().count() > 4 {
        return Err(format!(
            "{label:?} reads like a sentence: a report prints ids and \
             categories, never a query"
        ));
    }
    Ok(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::aggregate;
    use crate::row::Row;
    use std::collections::BTreeMap;

    fn row(id: &str, category: &str, condition: &str, recall: f64) -> Row {
        Row {
            query_id: id.to_string(),
            category: category.to_string(),
            condition: condition.to_string(),
            repeat: 0,
            ok: true,
            error: None,
            metrics: BTreeMap::from([
                ("recall".to_string(), recall),
                ("precision".to_string(), 0.5),
                ("cost_usd".to_string(), 0.03),
                ("wall_ms".to_string(), 1200.0),
                ("files_opened".to_string(), 2.0),
                ("agent_read_tokens".to_string(), 1000.0),
            ]),
            detail: Some(serde_json::json!({
                "query": "how do I cut a release and publish the package",
                "entry": "index.md",
                "files_read": ["concepts/release.md"],
                "command": ["claude", "-p", "/home/someone/private-wiki"],
            })),
        }
    }

    fn method() -> Method {
        Method {
            repeats: 1,
            conditions: vec!["explore".to_string(), "s1m".to_string()],
            claude_model: "claude-sonnet-5".to_string(),
            claude_flags: vec!["--output-format stream-json".to_string()],
            s1m_flags: vec!["--format json".to_string()],
            explore_prompt: "Answer <query> starting at <entry>.".to_string(),
            s1m_agent_prompt: "Open only what you need.".to_string(),
            chars_per_token: 4,
        }
    }

    /// The renderer is fed rows whose raw half is full of things that must
    /// never be published. None of it may come out the other end.
    #[test]
    fn nothing_from_the_raw_half_of_a_row_reaches_the_report() {
        let rows = vec![
            row("release-and-publish", "how-to", "explore", 1.0),
            row("release-and-publish", "how-to", "s1m", 0.5),
        ];
        let report = render(&aggregate(&rows), None, &method()).expect("a report");

        for leak in [
            "how do I cut a release",
            "index.md",
            "concepts/release.md",
            "private-wiki",
            "/home/someone",
        ] {
            assert!(
                !report.contains(leak),
                "{leak:?} reached the report:\n{report}"
            );
        }
        // What it may print: the id, the category and the numbers.
        assert!(report.contains("release-and-publish"), "{report}");
        assert!(report.contains("how-to"), "{report}");
        assert!(report.contains("## Results"), "{report}");
        assert!(report.contains("## Method"), "{report}");
        assert!(report.contains("## Caveats"), "{report}");
        assert!(report.contains("## Per query"), "{report}");
    }

    /// A gold set whose ids are paths, or whole questions, would publish the
    /// wiki through the one column the report prints. It is refused instead.
    #[test]
    fn a_label_that_could_be_a_path_or_a_question_stops_the_report() {
        let paths = vec![row("concepts/release.md", "how-to", "explore", 1.0)];
        let error = render(&aggregate(&paths), None, &method()).expect_err("a path-like id");
        assert!(error.contains("looks like a path"), "{error}");

        let sentence = vec![row(
            "how do I cut a release and publish",
            "how-to",
            "explore",
            1.0,
        )];
        let error = render(&aggregate(&sentence), None, &method()).expect_err("a query as an id");
        assert!(
            error.contains("sentence") || error.contains("longer than"),
            "{error}"
        );

        let category = vec![row("fine-id", "pages/about the wiki", "explore", 1.0)];
        assert!(render(&aggregate(&category), None, &method()).is_err());
    }

    /// A tenth of a cent is what one of these runs costs, and a report that
    /// rounded it to zero would say the wrong thing.
    #[test]
    fn small_money_keeps_its_digits() {
        assert_eq!(number(0.000_523), "0.000523");
        assert_eq!(number(0.001_172), "0.001172");
        assert_eq!(number(0.0), "0.00");
        assert_eq!(number(0.67), "0.67");
        assert_eq!(number(20737.0), "20737");
    }

    #[test]
    fn the_graph_statistics_are_the_first_table() {
        let rows = vec![row("one", "how-to", "explore", 1.0)];
        let stats = GraphStats {
            pages: 2000,
            words: 900_000,
            headings: 9000,
            links: crate::graph::LinkCounts {
                total: 20_000,
                markdown: 20_000,
                wikilink: 0,
                resolved: 19_000,
                broken: 900,
                outside: 100,
            },
            edges: 15_000,
            out_degree: crate::graph::Degrees {
                min: 0,
                median: 6.0,
                mean: 7.5,
                max: 300,
                histogram: BTreeMap::from([("0".to_string(), 120)]),
            },
            in_degree: crate::graph::Degrees {
                min: 0,
                median: 5.0,
                mean: 7.5,
                max: 400,
                histogram: BTreeMap::from([("0".to_string(), 200)]),
            },
            orphans: 200,
            depth: crate::graph::Depth {
                entry: Some(crate::graph::Entry::Convention("index.md".to_string())),
                histogram: BTreeMap::from([(0, 1), (1, 40)]),
                unreachable: 12,
            },
            largest_scc: 800,
        };
        let report = render(&aggregate(&rows), Some(&stats), &method()).expect("a report");
        assert!(report.contains("## The wiki"), "{report}");
        assert!(report.contains("| Pages | 2000 |"), "{report}");
        assert!(
            report.contains("| Largest strongly connected component | 800 |"),
            "{report}"
        );
    }
}
