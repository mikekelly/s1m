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
    // Everything below is rendered from files this process did not write, so
    // every string in them is checked before any of it is printed.
    check(method)?;
    if let Some(stats) = stats {
        check_stats(stats)?;
    }

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
    caveats_section(&mut out);
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

fn caveats_section(out: &mut String) {
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
        "The `s1m-agent` condition's cost is the agent's plus what the reading \
         list it was handed cost to buy, so the two agent conditions are \
         priced on the same footing. A warm s1m run has bought nothing and adds \
         nothing; the cold run is where a list's price shows.",
        "An agent that answered without the list of files it was asked for is \
         counted as unparsed beside the failure count. Those runs are still \
         scored, and they score zero, so a condition with unparsed answers is \
         reading lower than it looked.",
        "Wall time is one machine on one network, and the agent condition \
         depends on a service whose latency is not ours.",
        "The two conditions are not the same shape of work: s1m returns a \
         reading list and answers nothing, the agent reads until it can answer. \
         The `s1m-agent` condition is the one that compares like with like.",
    ] {
        out.push_str(&format!("- {caveat}\n"));
    }
}

/// How many runs are behind a row, and how many of them said nothing: a run
/// that failed, and a run whose agent wrote no list of files to score.
fn runs(group: &Group) -> String {
    let mut notes = Vec::new();
    if group.failed > 0 {
        notes.push(format!("{} failed", group.failed));
    }
    if unparsed(group) > 0 {
        notes.push(format!("{} unparsed", unparsed(group)));
    }
    match notes.is_empty() {
        true => group.runs.to_string(),
        false => format!("{} ({})", group.runs, notes.join(", ")),
    }
}

/// Runs whose agent answered without the list of files it was asked for.
/// Those runs are scored — an agent that names nothing found nothing — but a
/// column of zeroes from unanswered questions is not the same measurement as
/// one from wrong answers, and the difference belongs beside the count.
fn unparsed(group: &Group) -> usize {
    group.metrics.get("relied_parsed").map_or(0, |stat| {
        (stat.n as f64 * (1.0 - stat.mean)).round() as usize
    })
}

/// One metric's mean, with its spread where there is one, and how many runs it
/// came from where that is not every run that produced a measurement. A metric
/// only some runs carry — a subagent's wall time, say — would otherwise read as
/// an average over all of them.
fn cell(group: &Group, metric: &str) -> String {
    let Some(stat) = group.metrics.get(metric) else {
        return "—".to_string();
    };
    let mut cell = match stat.sd {
        Some(sd) => format!("{} ± {}", number(stat.mean), number(sd)),
        None => number(stat.mean),
    };
    if stat.n != group.runs - group.failed {
        cell.push_str(&format!(" (n={})", stat.n));
    }
    cell
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

/// The method, as a file gave it. A model, a flag or a condition is a label,
/// and a prompt is a template: it has to still have its placeholders in it,
/// because a prompt with the query substituted into it is a copy of the query.
fn check(method: &Method) -> Result<(), String> {
    safe(&method.claude_model)?;
    for condition in &method.conditions {
        safe(condition)?;
    }
    for flag in method.claude_flags.iter().chain(&method.s1m_flags) {
        safe(flag)?;
    }
    template(&method.explore_prompt, &["<query>", "<entry>"])?;
    if !method.s1m_agent_prompt.is_empty() {
        template(&method.s1m_agent_prompt, &["<query>", "<files>"])?;
    }
    Ok(())
}

/// The graph statistics, as a file gave them. The only string in them is the
/// entry page, and the only entry pages that may be named are the conventions
/// this harness looks for — which are its own constants. A page the caller
/// gave is reported as [`crate::graph::Entry::Given`] and never as itself.
fn check_stats(stats: &GraphStats) -> Result<(), String> {
    if let Some(crate::graph::Entry::Convention(convention)) = &stats.depth.entry
        && !graph::ENTRY_CONVENTIONS.contains(&convention.as_str())
    {
        return Err(format!(
            "{convention:?} is not an entry page convention: the report names \
             the conventions this harness looks for, and nothing else a wiki holds"
        ));
    }
    Ok(())
}

/// A prompt may be printed when it is still a template: every placeholder in
/// place, and nothing in it that looks like a path.
fn template(text: &str, placeholders: &[&str]) -> Result<(), String> {
    for placeholder in placeholders {
        if !text.contains(placeholder) {
            return Err(format!(
                "the prompt in the aggregates has no literal {placeholder}: a \
                 prompt with the query or the pages substituted into it is not \
                 a template, and the report prints the template"
            ));
        }
    }
    let lower = text.to_ascii_lowercase();
    if text.contains('/')
        || [".md", ".txt", ".markdown"]
            .iter()
            .any(|bad| lower.contains(bad))
    {
        return Err("the prompt in the aggregates names a path".to_string());
    }
    Ok(())
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
            s1m_agent_prompt: "Open what you need of <files> and answer <query>.".to_string(),
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

    fn graph_stats() -> GraphStats {
        GraphStats {
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
        }
    }

    #[test]
    fn the_graph_statistics_are_the_first_table() {
        let rows = vec![row("one", "how-to", "explore", 1.0)];
        let stats = graph_stats();
        let report = render(&aggregate(&rows), Some(&stats), &method()).expect("a report");
        assert!(report.contains("## The wiki"), "{report}");
        assert!(report.contains("| Pages | 2000 |"), "{report}");
        assert!(
            report.contains("| Largest strongly connected component | 800 |"),
            "{report}"
        );
    }

    /// A condition that failed part way through a pass must say so: a mean
    /// over the runs that worked, beside the count of the ones that did not.
    #[test]
    fn a_condition_that_failed_says_how_often() {
        let mut failed = row("one", "how-to", "s1m", 0.0);
        failed.ok = false;
        failed.metrics.clear();
        failed.detail = Some(serde_json::json!({"error": "s1m exited 2: /a/page.md"}));
        let rows = vec![row("one", "how-to", "s1m", 1.0), failed];

        let report = render(&aggregate(&rows), None, &method()).expect("a report");
        assert!(report.contains("| `s1m` | 2 (1 failed) |"), "{report}");
        // The run that failed is counted, not averaged: one good run of 1.00.
        assert!(report.contains("| 2 (1 failed) | 1.00 |"), "{report}");
        // And nothing from the failed row's raw half is in the report.
        assert!(!report.contains("page.md"), "{report}");
    }

    /// The aggregates and the graph statistics are files. A report rendered
    /// from a file someone else wrote must not print what that file says:
    /// every string in it goes through the same gate as a query id.
    #[test]
    fn a_crafted_aggregates_file_cannot_talk_through_the_method_table() {
        let rows = vec![row("one", "how-to", "explore", 1.0)];
        let aggregates = aggregate(&rows);

        let crafted = |change: fn(&mut Method)| {
            let mut method = method();
            change(&mut method);
            render(&aggregates, None, &method)
        };

        // A model, a flag or a condition that is really a path.
        assert!(crafted(|m| m.claude_model = "/home/someone/private-wiki".to_string()).is_err());
        assert!(
            crafted(|m| m.claude_flags = vec!["--add-dir /home/someone/wiki".to_string()]).is_err()
        );
        assert!(crafted(|m| m.s1m_flags = vec!["--root concepts/release.md".to_string()]).is_err());
        assert!(crafted(|m| m.conditions = vec!["notes/private.md".to_string()]).is_err());

        // A prompt with the query substituted into it is not a template.
        let filled = crafted(|m| {
            m.explore_prompt = "Answer how do I cut a release starting at index.md.".to_string()
        });
        assert!(filled.is_err(), "a filled-in prompt was printed");
        // A template is one because the placeholders are still in it.
        assert!(crafted(|m| m.explore_prompt = "Answer <query> from <entry>.".to_string()).is_ok());
        assert!(crafted(|m| m.explore_prompt = "Answer <query>.".to_string()).is_err());

        // The prompts this harness actually sends are templates by that test.
        let shipped = Method {
            explore_prompt: crate::run::EXPLORE_PROMPT.to_string(),
            s1m_agent_prompt: crate::run::S1M_AGENT_PROMPT.to_string(),
            claude_flags: crate::run::method_flags(),
            ..method()
        };
        let report = render(&aggregates, None, &shipped).expect("the shipped method");
        assert!(report.contains("<query>"), "{report}");
        assert!(!report.contains("how do I cut a release"), "{report}");
    }

    /// The same for the graph statistics: the entry page is a convention this
    /// harness knows, or the report does not print it.
    #[test]
    fn a_crafted_graph_stats_file_cannot_name_a_page() {
        let rows = vec![row("one", "how-to", "explore", 1.0)];
        let mut stats = graph_stats();

        stats.depth.entry = Some(crate::graph::Entry::Convention(
            "private/index.md".to_string(),
        ));
        let error = render(&aggregate(&rows), Some(&stats), &method())
            .expect_err("a convention no one has");
        assert!(error.contains("convention"), "{error}");

        // The two this harness looks for are fine, and so is a given page.
        for entry in [
            crate::graph::Entry::Convention("index.md".to_string()),
            crate::graph::Entry::Convention("README.md".to_string()),
            crate::graph::Entry::Given,
        ] {
            stats.depth.entry = Some(entry);
            assert!(render(&aggregate(&rows), Some(&stats), &method()).is_ok());
        }
    }

    /// A metric no run carried is not a metric every run carried, and an
    /// answer with no file list is not an answer that relied on nothing.
    #[test]
    fn the_cells_say_how_many_runs_are_behind_them() {
        let mut full = row("one", "how-to", "explore", 1.0);
        full.metrics.insert("relied_parsed".to_string(), 1.0);
        // A metric only one of the two runs carried.
        full.metrics.insert("jev_cost_usd".to_string(), 0.002);
        let mut thin = row("one", "how-to", "explore", 0.0);
        thin.metrics.insert("relied_parsed".to_string(), 0.0);

        let report = render(&aggregate(&[full, thin]), None, &method()).expect("a report");
        // Two runs, one of which wrote no list of files.
        assert!(
            report.contains("| `explore` | 2 (1 unparsed) |"),
            "{report}"
        );
        // Recall came from both runs and says nothing about how many; a metric
        // only one run carried says which.
        assert!(report.contains("| 0.50 ± 0.71 |"), "{report}");
        assert!(report.contains("0.002000 (n=1)"), "{report}");
    }
}
