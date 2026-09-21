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

use std::collections::BTreeSet;

use crate::aggregate::{Aggregates, Condition, Group};
use crate::graph::{self, GraphStats};
use crate::row::Row;
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
    /// How hard the Explore agent was told to look. Empty in a run made
    /// before it was said, which is a run that did not say.
    #[serde(default)]
    pub thoroughness: String,
    pub chars_per_token: usize,
}

impl Method {
    /// The method the rows describe.
    ///
    /// A resumed pass writes aggregates for every row in its directory, the
    /// ones an earlier pass made included, so what the table says about them
    /// has to be read back from them: the conditions, the repeats and the model
    /// asked for are the rows' own, and a pass that adds five runs to a
    /// directory does not get to rewrite the provenance of the other three
    /// hundred. The flags, the prompts and the thoroughness are constants of
    /// this harness rather than anything a row holds, and stay constants.
    pub fn from_rows(rows: &[Row]) -> Method {
        let conditions: Vec<String> = rows
            .iter()
            .map(|row| row.condition.clone())
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect();
        // The design the rows came from: a pass interrupted after two of its
        // three repeats says two, and a directory two passes wrote into says
        // the longer of them.
        let repeats = rows.iter().map(|row| row.repeat + 1).max().unwrap_or(0);
        // Rows that agree on the model asked for are the method; rows that
        // disagree are a directory two passes measured into, and naming both
        // beats naming one of them.
        let asked: BTreeSet<&String> = rows
            .iter()
            .filter_map(|row| row.model_asked_for.as_ref())
            .collect();
        let claude_model = match asked.len() {
            0 => crate::run::DEFAULT_MODEL.to_string(),
            _ => asked
                .iter()
                .map(|model| model.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        };
        Method {
            repeats,
            conditions,
            claude_model,
            claude_flags: crate::run::method_flags(),
            s1m_flags: crate::run::s1m_flags(),
            explore_prompt: crate::run::EXPLORE_PROMPT.to_string(),
            s1m_agent_prompt: crate::run::S1M_AGENT_PROMPT.to_string(),
            thoroughness: crate::run::THOROUGHNESS.to_string(),
            chars_per_token: crate::reading::CHARS_PER_TOKEN,
        }
    }
}

/// The longest a label may be before it is treated as prose.
const LABEL_LIMIT: usize = 64;

/// The metrics the results table shows, in order, with the heading each one
/// prints under. A metric no condition measured is left out of the table.
const COLUMNS: [(&str, &str); 11] = [
    ("recall", "Recall"),
    ("precision", "Precision"),
    ("agent_read_tokens", "Agent tokens"),
    ("agent_total_tokens", "Billed tokens"),
    ("files_opened", "Files opened"),
    ("parent_tool_uses", "Parent tools"),
    ("parent_tool_denials", "Parent blocked"),
    ("cost_usd", "Cost (USD)"),
    ("wall_ms", "Wall (ms)"),
    ("jev_input_tokens", "Jev input tokens"),
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
    check_aggregates(aggregates)?;
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
    // The tier the numbers were measured at, which is not the same question as
    // what was asked for: the Explore agent inherits the session's model, and
    // the rows are where that is written down.
    let models = models(aggregates);
    if agents_ran(aggregates) {
        // A run that named no model is a run whose models Claude Code chose,
        // and the subagent's may not be the parent's; the rows record what
        // answered.
        out.push_str(&match method.claude_model.as_str() {
            crate::run::DEFAULT_MODEL => "| Agent model | Claude Code's own: no \
                 `--model` flag was passed, so the parent and its subagent each \
                 took their default |\n"
                .to_string(),
            model => format!("| Agent model | `{model}` |\n"),
        });
        out.push_str(&format!("| Models measured | {models} |\n"));
    } else {
        // No agent ran, so there is no model to name and nothing answered:
        // saying what Claude Code would have used would be saying it about a
        // run that was never made.
        out.push_str(&format!("| Agent model | {models} |\n"));
    }
    out.push_str(&format!(
        "| Agent flags | {} |\n",
        flags(&method.claude_flags)
    ));
    out.push_str(&format!("| s1m flags | {} |\n", flags(&method.s1m_flags)));
    if !method.thoroughness.is_empty() {
        out.push_str(&format!(
            "| Explore thoroughness | `{}` |\n",
            method.thoroughness
        ));
    }
    out.push_str(&format!(
        "| Agent tokens | returned characters at {} a token |\n",
        method.chars_per_token
    ));
    out.push_str(&format!(
        "| Wiki revision | {} |\n",
        revisions(&aggregates.wiki)
    ));
    if let Some(bought) = aggregates.bought {
        let measured = measured_runs(aggregates);
        out.push_str(&format!(
            "| Runs | {bought} bought, {measured} measured |\n"
        ));
        // Only one direction of difference means a missing measurement: a
        // retried run appends a row of its own for a run the ledger holds one
        // entry for, so more rows than purchases says the opposite, and is left
        // to the caveats rather than called a missing run here.
        if bought > measured {
            out.push_str(&format!(
                "\n{bought} runs were bought for this directory and {measured} rows \
                 are in it. A run is recorded before it is paid for and its row \
                 after it is measured, so a purchase with no row is a run that was \
                 bought and left unmeasured.\n"
            ));
        }
    }
    out.push_str(
        "\nThe Explore agent is dispatched by the parent, and on this build it \
         inherits the session's model rather than declaring one of its own: \
         with no model named it takes whatever Claude Code would have used, and \
         with one named it takes that. What answered is recorded per run.\n",
    );
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

/// The tier the numbers were measured at, by how many runs each: read back from
/// the rows rather than from the flag, so it is what answered and not what was
/// asked for. A condition that runs an agent whose rows name no model says so,
/// instead of leaving the reader to guess from the flag.
fn models(aggregates: &Aggregates) -> String {
    if !aggregates.models.is_empty() {
        return aggregates
            .models
            .iter()
            .map(|(model, runs)| {
                format!(
                    "`{model}` ({runs} run{})",
                    if *runs == 1 { "" } else { "s" }
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
    }
    if agents_ran(aggregates) {
        "not recorded".to_string()
    } else {
        "none: no agent condition ran".to_string()
    }
}

/// Whether any measured group runs an agent: the two whose runs are on a model,
/// and so the only ones an agent model row is about. It is asked of the group
/// rather than of the key it is filed under, which for a tiered condition names
/// the tier too.
fn agents_ran(aggregates: &Aggregates) -> bool {
    aggregates
        .conditions
        .iter()
        .any(|(key, group)| crate::row::agent_condition(condition_of(key, group)))
}

/// The condition a group is: its own name, or — on an aggregates file written
/// before the pair was recorded — the condition it is filed under.
fn condition_of<'a>(key: &'a str, group: &'a Condition) -> &'a str {
    match group.condition.is_empty() {
        true => key,
        false => &group.condition,
    }
}

/// The cut of the wiki the rows were measured against. Rows from more than one
/// cut are the thing this cannot unpick, and it says so rather than naming one
/// of them as though it were all of them.
fn revisions(revisions: &[String]) -> String {
    match revisions {
        [] => "not recorded".to_string(),
        [one] => format!("`{one}`"),
        many => format!(
            "more than one: the rows were measured against {} cuts",
            many.len()
        ),
    }
}

/// How many runs produced a row, over every condition.
fn measured_runs(aggregates: &Aggregates) -> usize {
    aggregates
        .conditions
        .values()
        .map(|condition| condition.overall.runs)
        .sum()
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
        let mut cells = vec![condition_cell(name, condition)?, runs(&condition.overall)];
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
                condition_cell(name, condition)?,
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
        "The Explore condition's parent is stopped from exploring by a hook \
         that refuses its own reads, so that what is measured is the subagent. \
         *Parent tools* counts the parent's tool calls and *Parent blocked* the \
         ones the hook refused; a parent that read anything the hook let \
         through would be work counted as the Explore agent's.",
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
        "Jev's input tokens are of the same order as an exploring agent's: it \
         reads every page the walk visits whole, plus a preview of each of that \
         page's links. What differs is the price of a token and the cache — a \
         judgment already bought is not bought again, and a warm run reads \
         nothing at all.",
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
        "The method table is read back from the rows and not from the command \
         that wrote them, so a pass that resumes a directory describes the rows \
         in it. The wiki revision it names is a hash of the pages the walk \
         reads; rows measured against more than one cut are named as more than \
         one, and which row came from which cut is in the raw rows.",
        "The results table is a row per condition and tier, so a directory \
         holding one condition measured at two models is two rows and not one \
         average of both. The tier a row names is the model its pass asked for; \
         what actually answered is counted in the method table, and the two are \
         not always the same.",
        "A run is recorded when it is bought and its row when it has been \
         measured, so *Runs* counts both: a directory with more runs bought than \
         rows holds a run that was paid for and never measured, and one with more \
         rows than runs holds a retried run, whose second attempt is a row of its \
         own and not a new purchase.",
    ] {
        out.push_str(&format!("- {caveat}\n"));
    }
}

/// The condition as a table names it: the condition, and the model it was asked
/// for where a pass named one. One directory can hold one condition measured at
/// two tiers, and each is a row of its own, so a row has to say which it is.
fn condition_cell(key: &str, group: &Condition) -> Result<String, String> {
    let name = safe(condition_of(key, group))?;
    match &group.model {
        Some(model) => Ok(format!("`{name}` (asked for `{}`)", safe(model)?)),
        None => Ok(format!("`{name}`")),
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
    if nothing_to_report(group, metric) {
        return "—".to_string();
    }
    let mut cell = match stat.sd {
        Some(sd) => format!("{} ± {}", number(stat.mean), number(sd)),
        None => number(stat.mean),
    };
    if stat.n != group.runs - group.failed {
        cell.push_str(&format!(" (n={})", stat.n));
    }
    cell
}

/// Whether a metric is a number the run never went and got. A warm s1m run
/// bought no judgment, so it read no tokens; that zero is the cache's doing
/// and not the walk's, and printing it beside a cold run's tokens would
/// compare a price with an absence.
fn nothing_to_report(group: &Group, metric: &str) -> bool {
    metric == "jev_input_tokens"
        && group
            .metrics
            .get("jev_calls")
            .is_some_and(|calls| calls.max == 0.0)
}

/// A number at the precision it means something: money to six places, rates to
/// two, and anything counted in tokens or milliseconds whole. A cost of a tenth
/// of a cent is a real number and must not print as zero — which is every cost
/// this harness has, the free conditions included.
///
/// Shared with the estimate the runner prints, so that what a report calls a
/// cost and what a pass calls one are written the same way.
pub fn number(value: f64) -> String {
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
    if !method.thoroughness.is_empty() {
        safe(&method.thoroughness)?;
    }
    template(&method.explore_prompt, &["<query>", "<entry>"])?;
    if !method.s1m_agent_prompt.is_empty() {
        template(&method.s1m_agent_prompt, &["<query>", "<files>"])?;
    }
    Ok(())
}

/// The aggregates, as a file gave them. The models and the wiki revisions are
/// the two strings in them a report may print, and they go through the same
/// gate as every other label: they come from a file this process did not write.
fn check_aggregates(aggregates: &Aggregates) -> Result<(), String> {
    for revision in &aggregates.wiki {
        revision_label(revision)?;
    }
    for model in aggregates.models.keys() {
        safe(model)?;
    }
    Ok(())
}

/// A wiki revision is something this harness writes and nobody else's file
/// chooses the shape of: a prefix and a hex digest. Anything else is refused
/// rather than printed.
fn revision_label(revision: &str) -> Result<(), String> {
    let refused = || {
        format!(
            "{revision:?} is not a wiki revision: a report names one as this \
             harness writes it, `sha256:<hex>` or `git:<hex>`"
        )
    };
    let Some((prefix, digest)) = revision.split_once(':') else {
        return Err(refused());
    };
    if !["sha256", "git"].contains(&prefix)
        || !(6..=64).contains(&digest.len())
        || !digest
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(refused());
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

    /// The revision these rows were measured against, in the shape this
    /// harness writes one.
    const WIKI: &str = "sha256:0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

    fn row(id: &str, category: &str, condition: &str, recall: f64) -> Row {
        Row {
            query_id: id.to_string(),
            category: category.to_string(),
            condition: condition.to_string(),
            repeat: 0,
            wiki: WIKI.to_string(),
            ok: true,
            model_asked_for: None,
            models: Vec::new(),
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
            thoroughness: "medium".to_string(),
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

    /// Naming a model measures that model; naming none measures the one
    /// Claude Code would have used, subagent included. The method table has to
    /// say which of the two this was, because the numbers mean different
    /// things.
    #[test]
    fn the_method_says_whether_a_model_was_asked_for() {
        let rows = vec![row("one", "how-to", "explore", 1.0)];
        let aggregates = aggregate(&rows);

        let named = render(&aggregates, None, &method()).expect("a named model");
        assert!(
            named.contains("| Agent model | `claude-sonnet-5` |"),
            "{named}"
        );

        let default = Method {
            claude_model: crate::run::DEFAULT_MODEL.to_string(),
            ..method()
        };
        let report = render(&aggregates, None, &default).expect("no model named");
        assert!(
            report.contains("no `--model` flag"),
            "the table does not say the models were Claude Code's own:\n{report}"
        );
    }

    /// How hard the Explore agent was told to look is part of the method, and
    /// so is the fact that it answered on the session's model rather than one
    /// of its own.
    #[test]
    fn the_method_states_the_thoroughness_and_the_inherited_model() {
        let rows = vec![row("one", "how-to", "explore", 1.0)];
        let report = render(&aggregate(&rows), None, &method()).expect("a report");
        assert!(
            report.contains("| Explore thoroughness | `medium` |"),
            "{report}"
        );
        assert!(report.contains("inherits the session's model"), "{report}");

        // A run made before the thoroughness was said does not claim one.
        let silent = Method {
            thoroughness: String::new(),
            ..method()
        };
        let report = render(&aggregate(&rows), None, &silent).expect("a report");
        assert!(!report.contains("Explore thoroughness"), "{report}");

        // And it is a label like any other: a file cannot talk through it.
        let crafted = Method {
            thoroughness: "notes/private.md".to_string(),
            ..method()
        };
        assert!(render(&aggregate(&rows), None, &crafted).is_err());
    }

    /// What Jev read is the number s1m's cost is made of, so it belongs beside
    /// the cost. A warm run bought nothing and read nothing: printing its zero
    /// would read as a walk that was judged for free rather than one whose
    /// answers were already paid for.
    #[test]
    fn jev_input_tokens_are_shown_where_something_was_bought() {
        let bought = |id: &str, calls: f64, input: f64| {
            let mut row = row(id, "how-to", "s1m", 1.0);
            row.metrics.insert("jev_calls".to_string(), calls);
            row.metrics.insert("jev_input_tokens".to_string(), input);
            row.metrics
                .insert("jev_cost_usd".to_string(), input / 1e6 * 0.042);
            row
        };
        let rows = vec![bought("cold-one", 6.0, 30000.0)];
        let report = render(&aggregate(&rows), None, &method()).expect("a report");
        assert!(report.contains("Jev input tokens"), "{report}");
        assert!(report.contains("| 30000 |"), "{report}");

        // A warm run: no call, so nothing to report but the zero it did spend.
        let rows = vec![bought("warm-one", 0.0, 0.0)];
        let report = render(&aggregate(&rows), None, &method()).expect("a report");
        let line = report
            .lines()
            .find(|line| line.contains("warm-one"))
            .expect("the per-query row");
        assert!(line.contains("—"), "{line}");
        assert!(!line.contains("| 0.00 | 0.00 |"), "{line}");

        // An agent condition never calls Jev at all, and says so the same way.
        let report = render(
            &aggregate(&[row("one", "how-to", "explore", 1.0)]),
            None,
            &method(),
        )
        .expect("a report");
        assert!(report.contains("—"), "{report}");
    }
    /// One directory can hold one condition measured at two tiers, and the
    /// results table has a row per tier: the mix the method table names has to
    /// be separable, or the average is of two experiments.
    #[test]
    fn one_condition_at_two_tiers_is_two_rows_in_the_results_table() {
        let at = |model: &str, measured: &str, repeat: usize, recall: f64| Row {
            repeat,
            model_asked_for: Some(model.to_string()),
            models: vec![measured.to_string()],
            ..row("one", "how-to", "explore", recall)
        };
        let rows = vec![
            at("sonnet", "claude-sonnet-5", 0, 1.0),
            at("sonnet", "claude-sonnet-5", 1, 0.0),
            at("haiku", "claude-haiku-4-5", 0, 1.0),
        ];
        let report = render(&aggregate(&rows), None, &Method::from_rows(&rows)).expect("a report");

        let line = |tier: &str| {
            report
                .lines()
                .find(|line| line.starts_with(&format!("| `explore` (asked for `{tier}`)")))
                .unwrap_or_else(|| panic!("no `{tier}` row:\n{report}"))
                .to_string()
        };
        // By hand: sonnet's two runs average 0.50, haiku's one is 1.00.
        assert!(line("sonnet").starts_with("| `explore` (asked for `sonnet`) | 2 | 0.50 ± 0.71 |"));
        assert!(line("haiku").starts_with("| `explore` (asked for `haiku`) | 1 | 1.00 |"));

        // And the per-query table splits the same way.
        assert!(
            report.contains("| `one` | how-to | `explore` (asked for `haiku`) |"),
            "{report}"
        );
        assert!(
            report.contains("| `one` | how-to | `explore` (asked for `sonnet`) |"),
            "{report}"
        );

        // The method table still knows an agent ran: a tiered group is an agent
        // condition asked for a model, and not a condition no one has heard of.
        assert!(
            report.contains("| Agent model | `haiku, sonnet` |"),
            "{report}"
        );
        assert!(
            report.contains(
                "| Models measured | `claude-haiku-4-5` (1 run), `claude-sonnet-5` (2 runs) |"
            ),
            "{report}"
        );

        // A directory whose rows were all asked for the same tier is one row
        // with the counts it always had, and that row names the tier: a row is
        // a (condition, tier) pair, and this is which.
        let single = vec![at("sonnet", "claude-sonnet-5", 0, 1.0)];
        let report =
            render(&aggregate(&single), None, &Method::from_rows(&single)).expect("a report");
        assert!(
            report.contains("| `explore` (asked for `sonnet`) | 1 |"),
            "{report}"
        );

        // A directory whose pass named no model is the bare condition, as it
        // has always been.
        let silent = vec![row("one", "how-to", "explore", 1.0)];
        let report = render(&aggregate(&silent), None, &method()).expect("a report");
        assert!(report.contains("| `explore` | 1 |"), "{report}");
        assert!(!report.contains("(asked for"), "{report}");
    }

    /// The aggregates of a directory an earlier pass wrote hold the condition
    /// alone, as the key: that directory's table is the one it always rendered.
    #[test]
    fn aggregates_from_before_the_tier_pair_render_the_row_they_always_did() {
        let rows = vec![
            row("one", "how-to", "explore", 1.0),
            row("one", "how-to", "s1m", 0.5),
        ];
        let mut json = serde_json::to_value(aggregate(&rows)).expect("json");
        for group in json["conditions"]
            .as_object_mut()
            .expect("a map of groups")
            .values_mut()
        {
            let group = group.as_object_mut().expect("a group");
            group.remove("condition");
            group.remove("model");
        }
        let aggregates: crate::aggregate::Aggregates =
            serde_json::from_value(json).expect("an aggregates file from before the pair");
        let report = render(&aggregates, None, &method()).expect("a report");
        assert!(report.contains("| `explore` | 1 |"), "{report}");
        assert!(report.contains("| `s1m` | 1 |"), "{report}");
        assert!(!report.contains("(asked for"), "{report}");
    }

    /// A tier is a label out of a file this process did not write, and it is
    /// printed, so it goes through the same gate as every other label.
    #[test]
    fn a_crafted_aggregates_file_cannot_name_a_tier() {
        let at = |model: &str| {
            aggregate(&[Row {
                model_asked_for: Some(model.to_string()),
                ..row("one", "how-to", "explore", 1.0)
            }])
        };
        let error = render(&at("notes/private.md"), None, &method()).expect_err("a tier as a path");
        assert!(error.contains("looks like a path"), "{error}");
        assert!(render(&at("haiku"), None, &method()).is_ok());
    }

    /// The method is read back from the rows, not from the command that wrote
    /// them: a pass that resumes a directory describes the rows in it.
    #[test]
    fn the_method_is_read_back_from_the_rows() {
        let rows = vec![
            Row {
                model_asked_for: Some("sonnet".to_string()),
                ..row("one", "how-to", "explore", 1.0)
            },
            Row {
                repeat: 1,
                model_asked_for: Some("sonnet".to_string()),
                models: vec!["claude-sonnet-5".to_string()],
                ..row("one", "how-to", "explore", 0.0)
            },
        ];
        let method = Method::from_rows(&rows);
        assert_eq!(method.conditions, vec!["explore".to_string()]);
        assert_eq!(method.repeats, 2, "two repeats were made");
        assert_eq!(method.claude_model, "sonnet");
        // The flags and the prompts are the harness's, not the rows'.
        assert_eq!(method.claude_flags, crate::run::method_flags());
        assert_eq!(method.explore_prompt, crate::run::EXPLORE_PROMPT);

        // Rows that disagree about the model name both rather than one.
        let rows = vec![
            Row {
                model_asked_for: Some("sonnet".to_string()),
                ..row("one", "how-to", "explore", 1.0)
            },
            Row {
                model_asked_for: Some("haiku".to_string()),
                ..row("one", "how-to", "explore", 1.0)
            },
        ];
        assert_eq!(Method::from_rows(&rows).claude_model, "haiku, sonnet");

        // Nothing measured is nothing described, and no model was asked for.
        let empty = Method::from_rows(&[]);
        assert_eq!((empty.repeats, empty.conditions.len()), (0, 0));
        assert_eq!(empty.claude_model, crate::run::DEFAULT_MODEL);
    }

    /// The method table names the tier that answered and the cut of the wiki the
    /// rows were measured against, both read back from the rows.
    #[test]
    fn the_method_table_names_the_tier_and_the_revision_measured() {
        let rows = vec![
            Row {
                models: vec!["claude-sonnet-5".to_string()],
                ..row("one", "how-to", "explore", 1.0)
            },
            Row {
                models: vec!["claude-sonnet-5".to_string()],
                ..row("one", "how-to", "s1m-agent", 0.5)
            },
        ];
        let report = render(&aggregate(&rows), None, &Method::from_rows(&rows)).expect("a report");
        assert!(
            report.contains("| Models measured | `claude-sonnet-5` (2 runs) |"),
            "{report}"
        );
        assert!(
            report.contains(&format!("| Wiki revision | `{WIKI}` |")),
            "{report}"
        );

        // No agent ran: there is no tier to name, and saying what Claude Code
        // would have used would be saying it about a run that was never made.
        let rows = vec![row("one", "how-to", "s1m", 1.0)];
        let report = render(&aggregate(&rows), None, &Method::from_rows(&rows)).expect("a report");
        assert!(
            report.contains("| Agent model | none: no agent condition ran |"),
            "{report}"
        );
        assert!(!report.contains("Models measured"), "{report}");

        // An agent ran and its rows named no model: not recorded, rather than
        // a claim that none was used.
        let rows = vec![row("one", "how-to", "explore", 1.0)];
        let report = render(&aggregate(&rows), None, &Method::from_rows(&rows)).expect("a report");
        assert!(
            report.contains("| Models measured | not recorded |"),
            "{report}"
        );
    }

    /// Rows measured against two cuts of a wiki are the one thing a report
    /// cannot unpick, and it says so rather than naming one of them.
    #[test]
    fn a_directory_that_mixed_two_cuts_says_so() {
        let rows = vec![
            row("one", "how-to", "explore", 1.0),
            Row {
                wiki: "sha256:1111222233334444555566667777888899990000aaaabbbbccccddddeeeeffff"
                    .to_string(),
                ..row("one", "how-to", "explore", 0.0)
            },
        ];
        let report = render(&aggregate(&rows), None, &Method::from_rows(&rows)).expect("a report");
        assert!(
            report.contains(
                "| Wiki revision | more than one: the rows were measured against 2 cuts |"
            ),
            "{report}"
        );

        // A row from before revisions were recorded says that, too.
        let rows = vec![Row {
            wiki: String::new(),
            ..row("one", "how-to", "explore", 1.0)
        }];
        let report = render(&aggregate(&rows), None, &Method::from_rows(&rows)).expect("a report");
        assert!(
            report.contains("| Wiki revision | not recorded |"),
            "{report}"
        );
    }

    /// Runs bought and rows measured are two counts of the same ledger, and a
    /// directory with more of the first is one whose record is incomplete.
    #[test]
    fn the_report_says_what_was_bought_beside_what_was_measured() {
        let rows = vec![row("one", "how-to", "s1m", 1.0)];
        let mut aggregates = aggregate(&rows);
        aggregates.bought = Some(3);
        let report = render(&aggregates, None, &Method::from_rows(&rows)).expect("a report");
        assert!(
            report.contains("| Runs | 3 bought, 1 measured |"),
            "{report}"
        );
        assert!(report.contains("left unmeasured"), "{report}");

        // In step with each other, there is nothing to explain.
        aggregates.bought = Some(1);
        let report = render(&aggregates, None, &Method::from_rows(&rows)).expect("a report");
        assert!(
            report.contains("| Runs | 1 bought, 1 measured |"),
            "{report}"
        );
        assert!(!report.contains("left unmeasured"), "{report}");

        // More rows than purchases is the other direction: a run retried after
        // it failed is a second row and not a second purchase, and calling that
        // a missing measurement would say the opposite of what happened.
        let retried = vec![
            row("one", "how-to", "s1m", 0.0),
            row("one", "how-to", "s1m", 1.0),
        ];
        let mut twice = aggregate(&retried);
        twice.bought = Some(1);
        let report = render(&twice, None, &Method::from_rows(&retried)).expect("a report");
        assert!(
            report.contains("| Runs | 1 bought, 2 measured |"),
            "{report}"
        );
        assert!(!report.contains("left unmeasured"), "{report}");

        // A directory whose aggregates predate the ledger does not claim one.
        let report = render(&aggregate(&rows), None, &Method::from_rows(&rows)).expect("a report");
        assert!(!report.contains(" bought, "), "{report}");
    }

    /// The models and the revisions in the aggregates are strings from a file
    /// this process did not write, and go through the same gate as everything
    /// else a report prints.
    #[test]
    fn a_crafted_aggregates_file_cannot_name_a_model_or_a_revision() {
        let rows = vec![row("one", "how-to", "explore", 1.0)];
        let mut aggregates = aggregate(&rows);

        aggregates.models = BTreeMap::from([("notes/private.md".to_string(), 1)]);
        assert!(render(&aggregates, None, &method()).is_err());
        aggregates.models = BTreeMap::from([("claude-sonnet-5".to_string(), 1)]);
        assert!(render(&aggregates, None, &method()).is_ok());

        for bad in [
            "/home/someone/wiki",
            "the wiki as of yesterday",
            "sha256:zzzz",
            "sha256:abc",
            "md5:0f1e2d3c",
        ] {
            aggregates.wiki = vec![bad.to_string()];
            assert!(
                render(&aggregates, None, &method()).is_err(),
                "{bad:?} was printed"
            );
        }
        aggregates.wiki = vec!["sha256:0f1e2d3c".to_string(), "git:abcdef1".to_string()];
        assert!(render(&aggregates, None, &method()).is_ok());
    }
}
