//! The `run` command: every query, under every condition, as many times as
//! asked, with one JSONL row per run.
//!
//! Runs cost money and take minutes, so the pass is resumable — and what it
//! must not do is buy the same run twice. A run already recorded in this
//! directory is not made again, and neither is one the [ledger](crate::ledger)
//! says was bought and measured anywhere else on the machine. Before buying
//! anything, the pass prices what it owes and stops when that is more than this
//! harness spends without being told to.
//!
//! The rows are flushed one at a time for the same reason.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::explore::{self, Usage};
use crate::gold::{Gold, Query};
use crate::ledger::{self, Entry, Id, Ledger};
use crate::reading;
use crate::report::Method;
use crate::row::{Key, Row};
use crate::wiki;

/// The file every run appends to, under `--out`.
pub const ROWS: &str = "runs.jsonl";
/// Where each run's own output is kept, under `--out`.
pub const RAW: &str = "raw";
/// What the cold s1m runs buy their judgments into, under `--out`.
pub const COLD_CACHE: &str = "cold-cache";

/// The parent agent's prompt. The query and the entry pages are substituted
/// in; nothing else about the wiki is.
///
/// The parent has one tool, so it cannot answer this itself: it delegates, and
/// what it is asked for is the subagent's answer unchanged. Anything it added
/// of its own would be a second agent's work counted as the first's.
pub const EXPLORE_PROMPT: &str = "\
Hand this whole task to the Explore agent with thoroughness \"medium\". You \
may not read, glob or grep yourself: those tools are blocked for you and only \
the Explore agent may use them.
Its task: answer this query from the wiki in the current directory, starting \
at these pages: <entry>
The query: <query>
Tell it to end its report with a JSON array of the relative paths of the files \
it relied on. When it reports back, reply with its answer and that JSON array \
exactly as it wrote them, and add nothing of your own.";

/// The same, for an agent handed s1m's reading list instead of a wiki to walk.
pub const S1M_AGENT_PROMPT: &str = "\
Here is a reading list for a query, from the wiki in the current directory, \
most relevant first:
<files>
Answer this query, opening only the files you need: <query>
Reply with the answer, and then a JSON array of the relative paths of the files \
you relied on.";

/// The tools the Explore condition runs under: read-only, plus the one that
/// spawns the subagent.
///
/// This list bounds the whole session, subagents included, so it cannot be
/// used to stop the parent exploring: a parent given only `Task` spawns an
/// Explore agent that has no tools either and reports nothing. What stops the
/// parent is the hook in [`crate::hook`], which refuses the parent's own reads
/// and lets the subagent's through.
pub const EXPLORE_TOOLS: &str = "Read,Glob,Grep,Task";
/// The tools an agent handed a reading list runs under: it was given the files,
/// so it has no need to search for them.
pub const S1M_AGENT_TOOLS: &str = "Read";

#[derive(Debug, Clone)]
pub struct Options {
    pub wiki: PathBuf,
    pub gold: PathBuf,
    pub out: PathBuf,
    pub repeats: usize,
    pub conditions: Vec<String>,
    pub cache_dir: Option<PathBuf>,
    /// The pages a query with no entry of its own starts from.
    pub entry: Vec<String>,
    /// The model to run the agent on, `None` for whatever Claude Code uses.
    pub model: Option<String>,
    pub s1m: PathBuf,
    pub claude: PathBuf,
    pub transcripts: Option<PathBuf>,
    pub cold_repeats: usize,
    /// Measure only these query ids; every one when empty.
    pub queries: Vec<String>,
    /// Print what the owed runs are expected to cost, and buy nothing.
    pub estimate: bool,
    /// Run even when the estimate is above [`SPEND_LIMIT_USD`].
    pub yes: bool,
    /// Re-run the runs this directory recorded as failed, and nothing else.
    pub retry_failed: bool,
    /// The ledger of what has been bought, shared by every run directory.
    /// `None` is [`ledger::default_path`].
    pub ledger: Option<PathBuf>,
    pub timeout: Duration,
}

/// One run that has not happened yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub query: usize,
    pub condition: String,
    pub repeat: usize,
}

/// Which runs are still owed, in the order they should be made: one query at a
/// time, so a pass that is stopped early has whole queries measured.
///
/// `s1m-cold` is the s1m condition against a cache it has never seen, which is
/// the only way to see what a query costs to buy; it is measured
/// `cold_repeats` times rather than every repeat, because every cold run is
/// bought again.
pub fn pending(
    gold: &Gold,
    conditions: &[String],
    repeats: usize,
    cold_repeats: usize,
    done: &Done,
) -> Vec<Job> {
    let mut jobs = Vec::new();
    for (at, query) in gold.queries.iter().enumerate() {
        for repeat in 0..repeats {
            for condition in conditions {
                let mut wanted = Vec::new();
                if condition == "s1m" && repeat < cold_repeats {
                    wanted.push(format!("{condition}-cold"));
                }
                wanted.push(condition.clone());
                for condition in wanted {
                    let key = Key {
                        // Membership is the query, condition and repeat: the
                        // wiki revision and the model were settled when the
                        // ledger was read.
                        wiki: String::new(),
                        query: query.id.clone(),
                        condition: condition.clone(),
                        repeat,
                    };
                    if done.contains(&key) {
                        continue;
                    }
                    jobs.push(Job {
                        query: at,
                        condition,
                        repeat,
                    });
                }
            }
        }
    }
    jobs
}

/// The `(query, condition, repeat)` of a run.
type Triple = (String, String, usize);

fn triple(key: &Key) -> Triple {
    (key.query.clone(), key.condition.clone(), key.repeat)
}

/// What has already been measured: one set of runs, read from two records that
/// are held to two different rules.
///
/// This directory's rows are its own record, so a run in it has happened and is
/// not made again whatever wiki revision it was made against: a pass resumed on
/// a directory from before revisions were recorded does not pay for all of it
/// again to learn what its rows already say.
///
/// The ledger's entries are every directory's record, so they are held to the
/// key: only the ones for the wiki revision being measured now, and only the
/// ones bought at the model being asked for — where the condition takes a
/// model at all. The same query against another cut or at another tier is
/// another measurement, and a screening pass at a cheaper model has to be able
/// to make it.
///
/// `--retry-failed` puts back the runs that failed — here and in the ledger —
/// and nothing else.
pub struct Done {
    runs: BTreeSet<Triple>,
}

impl Done {
    /// Nothing measured: what a pass over a new directory reads.
    #[cfg(test)]
    pub fn nothing() -> Done {
        Done {
            runs: BTreeSet::new(),
        }
    }

    /// `model` is the model this pass is asking for, or `None` where it names
    /// none.
    pub fn read(
        rows: &[Row],
        recorded: &BTreeMap<Id, Entry>,
        wiki: &str,
        model: Option<&str>,
        retry_failed: bool,
    ) -> Done {
        let mut runs: BTreeSet<Triple> = rows
            .iter()
            .filter(|row| !(retry_failed && !row.ok))
            .map(|row| triple(&row.key()))
            .collect();
        runs.extend(
            recorded
                .values()
                .filter(|entry| match entry.ok {
                    // A purchase with no outcome is a run killed before its row
                    // was written: the money is gone and the measurement is
                    // missing, so the run is owed and is counted as bought.
                    None => false,
                    Some(false) => !retry_failed,
                    Some(true) => true,
                })
                .filter(|entry| {
                    entry.key.wiki == wiki && entry.model == asked_for(&entry.key.condition, model)
                })
                .map(|entry| triple(&entry.key)),
        );
        Done { runs }
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.runs.contains(&triple(key))
    }

    /// How many runs are behind it.
    pub fn len(&self) -> usize {
        self.runs.len()
    }
}

/// The rows already written, if any: a resumed pass reads them back rather than
/// paying for them again.
pub fn read_rows(out: &Path) -> Result<Vec<Row>, String> {
    let path = out.join(ROWS);
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Row>(line)
                .map_err(|error| format!("{}: {error}", path.display()))
        })
        .collect()
}

fn append(out: &Path, row: &Row) -> Result<(), String> {
    let path = out.join(ROWS);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let line = serde_json::to_string(row).map_err(|error| error.to_string())?;
    writeln!(file, "{line}").map_err(|error| format!("{}: {error}", path.display()))
}

/// Runs everything that is owed and writes the aggregates.
pub fn run(options: &Options) -> Result<(), String> {
    // An agent reports the paths it opened as it opened them, which is
    // absolute; the wiki has to be absolute too for those to come back
    // relative.
    let mut options = options.clone();
    options.wiki = std::fs::canonicalize(&options.wiki)
        .map_err(|error| format!("{}: {error}", options.wiki.display()))?;
    for condition in &options.conditions {
        if !crate::row::CONDITIONS.contains(&condition.as_str())
            && threshold_of(condition).is_none()
        {
            return Err(format!(
                "no condition named {condition:?}: it is one of {}, or \
                 `s1m-t<N>` for s1m at threshold N",
                crate::row::CONDITIONS.join(", ")
            ));
        }
    }
    let mut gold = Gold::load(&options.gold)?;
    gold.only(&options.queries)?;

    // A run is what it found as well as what it asked: the revision of the wiki
    // is part of its identity, so it is read once here and carried by every row
    // and every purchase this pass makes.
    let wiki = wiki::revision(&options.wiki)?;
    let ledger = Ledger::at(
        options
            .ledger
            .clone()
            .unwrap_or_else(|| ledger::default_path(&options.out)),
    );
    let recorded = ledger.read()?;
    let mut rows = read_rows(&options.out)?;
    let done = Done::read(
        &rows,
        &recorded,
        &wiki,
        options.model.as_deref(),
        options.retry_failed,
    );
    let jobs = pending(
        &gold,
        &options.conditions,
        options.repeats,
        options.cold_repeats,
        &done,
    );
    let estimate = estimate(&jobs, &wiki, &recorded);
    eprintln!(
        "eval-agent: {} runs owed, {} already measured, {}",
        jobs.len(),
        done.len(),
        estimate.summary()
    );

    // An estimate costs nothing and a pass does not: with `--estimate` it is
    // the whole job and nothing is created, and above the limit it is the
    // pass's answer unless the caller has said yes.
    if options.estimate {
        print!("{}", estimate.table());
        eprintln!(
            "eval-agent: estimate only, nothing was bought; the ledger is {}",
            ledger.path().display()
        );
        return Ok(());
    }
    if estimate.total > SPEND_LIMIT_USD && !options.yes {
        eprint!("{}", estimate.table());
        return Err(format!(
            "the {} runs owed are estimated at ${}, above the ${} this harness \
             spends without being told: pass --yes to buy them, or --estimate to \
             see what they are priced from",
            jobs.len(),
            money(estimate.total),
            money(SPEND_LIMIT_USD)
        ));
    }

    fs::create_dir_all(options.out.join(RAW))
        .map_err(|error| format!("{}: {error}", options.out.display()))?;
    let out = std::fs::canonicalize(&options.out)
        .map_err(|error| format!("{}: {error}", options.out.display()))?;
    let pass = Pass {
        options,
        out,
        wiki,
        ledger,
    };

    let mut made: BTreeSet<Key> = BTreeSet::new();
    for (at, job) in jobs.iter().enumerate() {
        let query = &gold.queries[job.query];
        // A prerequisite may have made this run already.
        let key = pass.key(query, job);
        if made.contains(&key) {
            continue;
        }
        if let Some(needed) = prerequisite(job, &query.id, &rows) {
            eprintln!(
                "eval-agent: [{}/{}] {} {} repeat {} needs {} first",
                at + 1,
                jobs.len(),
                query.id,
                job.condition,
                job.repeat,
                needed.condition
            );
            let row = pass.buy(pass.key(query, &needed), query, &needed, &rows)?;
            made.insert(row.key());
            rows.push(row);
        }
        eprintln!(
            "eval-agent: [{}/{}] {} {} repeat {}",
            at + 1,
            jobs.len(),
            query.id,
            job.condition,
            job.repeat
        );
        let row = pass.buy(key, query, job, &rows)?;
        made.insert(row.key());
        rows.push(row);
    }

    write_aggregates(&pass.options, &rows, &pass.ledger, &pass.out)
}

/// One pass as it is being made: the flags it was given, where its rows go,
/// what it records them in, and the wiki revision every one of them is measured
/// against.
struct Pass {
    options: Options,
    /// The canonical `--out`, which is what the ledger records a purchase for.
    out: PathBuf,
    wiki: String,
    ledger: Ledger,
}

impl Pass {
    /// The key of one run against this pass's wiki revision.
    fn key(&self, query: &Query, job: &Job) -> Key {
        Key {
            wiki: self.wiki.clone(),
            query: query.id.clone(),
            condition: job.condition.clone(),
            repeat: job.repeat,
        }
    }

    /// One run, bought and recorded.
    ///
    /// The purchase is written before the run is paid for and its outcome after
    /// the row has been written: a pass that dies in between has still said what
    /// it bought, and the run is owed again by whoever comes next.
    fn buy(&self, key: Key, query: &Query, job: &Job, rows: &[Row]) -> Result<Row, String> {
        let model = asked_for(&job.condition, self.options.model.as_deref());
        let buying = Entry::buying(key, model.as_deref(), &self.out);
        self.ledger.record(&buying)?;
        let row = measure(&self.options, &self.wiki, query, job, rows);
        append(&self.options.out, &row)?;
        // A failed run carries no metrics, so it has no price and none is
        // recorded: a zero would read as a run that came free.
        let cost = row.metrics.get("cost_usd").copied();
        self.ledger.record(&buying.measured(row.ok, cost))?;
        Ok(row)
    }
}

/// Above this many dollars, a pass says what it is about to spend and stops
/// unless the caller has said yes. It is the README's "start small" as a rule of
/// the harness rather than as advice.
pub const SPEND_LIMIT_USD: f64 = 5.0;

/// What the runs that are owed are expected to cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    pub lines: Vec<EstimateLine>,
    /// The sum of the lines: a floor rather than a total when one of them had
    /// nothing to price it with.
    pub total: f64,
    /// The conditions with no run of that kind to price them by.
    pub unknown: usize,
}

/// One condition's pending runs, priced by what that condition has cost before.
#[derive(Debug, Clone, PartialEq)]
pub struct EstimateLine {
    pub condition: String,
    pub jobs: usize,
    /// The mean cost of a run of this condition, and how many were averaged.
    pub mean: Option<f64>,
    pub n: usize,
    /// Whether the mean came from runs against another wiki revision: this one
    /// has none of its own, which is what a wiki that has just changed looks
    /// like.
    pub elsewhere: bool,
}

impl Estimate {
    /// What the owed runs are expected to cost, in one line for the pass's own
    /// summary.
    pub fn summary(&self) -> String {
        let floor = if self.unknown > 0 { "at least " } else { "" };
        format!("{floor}${} to buy", money(self.total))
    }

    /// The estimate as a table, with what each mean was averaged from: the
    /// ledger is the only evidence a directory with no rows of its own has, and
    /// a mean over no rows says so rather than reading as free.
    pub fn table(&self) -> String {
        // The header is written with the same columns as the rows below it, so
        // that a change to one cannot leave the table out of line.
        let mut out = format!(
            "  {:<14} {:>6} {:>14} {:>6} {:>12}\n",
            "condition", "owed", "mean $/run", "n", "subtotal"
        );
        for line in &self.lines {
            let (mean, subtotal) = match line.mean {
                Some(mean) => (money(mean), money(mean * line.jobs as f64)),
                None => ("unknown".to_string(), "?".to_string()),
            };
            out.push_str(&format!(
                "  {:<14} {:>6} {:>14} {:>6} {:>12}{}\n",
                line.condition,
                line.jobs,
                mean,
                line.n,
                subtotal,
                if line.elsewhere { " *" } else { "" }
            ));
        }
        let owed: usize = self.lines.iter().map(|line| line.jobs).sum();
        out.push_str(&format!(
            "  {:<14} {owed:>6} {:>14} {:>6} {:>12}\n",
            "total",
            "",
            "",
            money(self.total)
        ));
        if self.unknown > 0 {
            out.push_str(&format!(
                "  note: the total is a floor: {} condition(s) have no rows to \
                 price them\n",
                self.unknown
            ));
        }
        if self.lines.iter().any(|line| line.elsewhere) {
            out.push_str(
                "  note: * priced from runs against another wiki revision, \
                 which has no rows of its own yet\n",
            );
        }
        out
    }
}

/// A cost as the estimate prints it, at the precision that leaves a mean and
/// its subtotal telling the same story: four places below a dollar, where one of
/// these runs costs a fraction of one, six below a thousandth, and the report's
/// own rule above a dollar. Printing a tenth of a cent as `0.00` would say a
/// condition came free, which is the one thing this table is for.
fn money(value: f64) -> String {
    match value.abs() {
        0.0 => "0.00".to_string(),
        above if above >= 1.0 => crate::report::number(value),
        above if above >= 0.001 => format!("{value:.4}"),
        _ => format!("{value:.6}"),
    }
}

/// The runs that are owed, priced by what runs of the same condition have cost
/// before — the per-condition mean of the rows that already exist.
///
/// The same wiki revision is preferred, because how big the wiki is is most of
/// what a run costs; a revision with nothing recorded falls back to any
/// revision, since a wiki that has just changed is exactly when an estimate is
/// wanted. A condition with no rows at all is unknown, and the total says so.
fn estimate(jobs: &[Job], wiki: &str, recorded: &BTreeMap<Id, Entry>) -> Estimate {
    let mut owed: BTreeMap<String, usize> = BTreeMap::new();
    for job in jobs {
        *owed.entry(job.condition.clone()).or_default() += 1;
    }

    let mut lines = Vec::new();
    let mut total = 0.0;
    let mut unknown = 0;
    for (condition, jobs) in owed {
        let measured: Vec<&Entry> = recorded
            .values()
            .filter(|entry| {
                entry.key.condition == condition
                    && entry.ok == Some(true)
                    && entry.cost_usd.is_some()
            })
            .collect();
        let here: Vec<&Entry> = measured
            .iter()
            .copied()
            .filter(|entry| entry.key.wiki == wiki)
            .collect();
        let elsewhere = here.is_empty();
        let sample = if elsewhere { measured } else { here };
        let n = sample.len();
        let mean = (n > 0).then(|| {
            sample
                .iter()
                .filter_map(|entry| entry.cost_usd)
                .sum::<f64>()
                / n as f64
        });
        match mean {
            Some(mean) => total += mean * jobs as f64,
            None => unknown += 1,
        }
        lines.push(EstimateLine {
            condition,
            jobs,
            mean,
            n,
            elsewhere: elsewhere && n > 0,
        });
    }
    Estimate {
        lines,
        total,
        unknown,
    }
}

/// Reduces every row and writes `aggregates.json`: numbers, ids and
/// categories, and nothing else.
pub fn write_aggregates(
    options: &Options,
    rows: &[Row],
    ledger: &Ledger,
    out: &Path,
) -> Result<(), String> {
    let mut aggregates = crate::aggregate::aggregate(rows);
    // The rows describe the method, not this invocation's flags: a pass that
    // adds five runs to a directory does not get to rewrite the provenance of
    // the other three hundred.
    aggregates.method = Some(Method::from_rows(rows));
    // What was bought for this directory, read back at the end of the pass so
    // that this pass's own purchases are in it. A purchase with no row is a run
    // that was paid for and never measured, and these two counts beside each
    // other are how a report says so.
    aggregates.bought = Some(
        ledger
            .read()?
            .values()
            .filter(|entry| entry.out == out)
            .count(),
    );
    let path = options.out.join("aggregates.json");
    let text = serde_json::to_string_pretty(&aggregates).map_err(|error| error.to_string())?;
    fs::write(&path, text).map_err(|error| format!("{}: {error}", path.display()))?;
    eprintln!("eval-agent: aggregates written to {}", path.display());
    Ok(())
}

/// What the method table says when no model was asked for.
pub const DEFAULT_MODEL: &str = "default";

/// The agent's flags, as the report prints them.
pub fn method_flags() -> Vec<String> {
    vec![
        "-p".to_string(),
        "--output-format stream-json".to_string(),
        "--verbose".to_string(),
        "--setting-sources \"\"".to_string(),
        "--settings HOOK".to_string(),
        "--strict-mcp-config".to_string(),
        "--disable-slash-commands".to_string(),
        format!("--tools {EXPLORE_TOOLS}"),
        format!("--allowedTools {EXPLORE_TOOLS}"),
        "--permission-prompts none".to_string(),
    ]
}

/// The s1m flags, as the report prints them.
pub fn s1m_flags() -> Vec<String> {
    vec!["--format json".to_string(), "--root .".to_string()]
}

/// One run, as a row whatever happens: a run that failed is a row too, so that
/// a resumed pass does not try it forever.
fn measure(options: &Options, wiki: &str, query: &Query, job: &Job, rows: &[Row]) -> Row {
    let outcome = match job.condition.as_str() {
        "explore" => explore_condition(options, query, job),
        "s1m" => s1m_condition(options, query, job, false, None),
        "s1m-cold" => s1m_condition(options, query, job, true, None),
        "s1m-agent" => s1m_agent_condition(options, query, job, rows),
        other => match threshold_of(other) {
            // A threshold variant is s1m at its defaults but one, warm: the
            // report shows it beside the defaults, and nothing is bought twice
            // to get it.
            Some(threshold) => s1m_condition(options, query, job, false, Some(threshold)),
            None => Err(format!("no condition named {other:?}")),
        },
    };
    let measured = match outcome {
        Ok(measured) => measured,
        // A run that failed is a row with no metrics: nothing is averaged from
        // it, and what went wrong goes in the raw half with everything else
        // that may name a file — an API error quotes the request that caused
        // it, and a process error quotes its own stderr.
        Err(error) => {
            eprintln!("eval-agent: {} {}: {error}", query.id, job.condition);
            return Row {
                query_id: query.id.clone(),
                category: query.category().to_string(),
                condition: job.condition.clone(),
                repeat: job.repeat,
                wiki: wiki.to_string(),
                ok: false,
                metrics: BTreeMap::new(),
                model_asked_for: asked_for(&job.condition, options.model.as_deref()),
                models: Vec::new(),
                detail: Some(serde_json::json!({ "error": error })),
            };
        }
    };
    Row {
        query_id: query.id.clone(),
        category: query.category().to_string(),
        condition: job.condition.clone(),
        repeat: job.repeat,
        wiki: wiki.to_string(),
        ok: true,
        metrics: measured.metrics,
        model_asked_for: asked_for(&job.condition, options.model.as_deref()),
        models: measured.models,
        detail: Some(measured.detail),
    }
}

/// The model a run of this condition was asked for: the one named on the
/// command line, on a condition that runs an agent.
///
/// An s1m run takes a threshold rather than a model and buys judgments from an
/// API this harness does not choose the model for, so it is the same run
/// whatever `--model` says. Naming a model for a pass must not make its `s1m`
/// runs another pass's runs, on the row or in the ledger.
fn asked_for(condition: &str, model: Option<&str>) -> Option<String> {
    crate::row::agent_condition(condition)
        .then(|| model.map(str::to_string))
        .flatten()
}

/// What one run produced: the numbers a report averages, the raw half it does
/// not, and the models that answered.
struct Measured {
    metrics: BTreeMap<String, f64>,
    detail: serde_json::Value,
    /// The models Claude Code resolved for the work being measured, empty for a
    /// condition that runs no agent.
    models: Vec<String>,
}

/// The Explore condition: a parent agent that hands the query to the built-in
/// Explore subagent, measured on the subagent's own tokens.
fn explore_condition(options: &Options, query: &Query, job: &Job) -> Result<Measured, String> {
    let entries = query.entries(&options.entry);
    let prompt = EXPLORE_PROMPT
        .replace("<entry>", &entries.join(", "))
        .replace("<query>", &query.query);
    // The hook that refuses the parent's own reads, so that what is measured
    // is the subagent and not the parent wearing its name.
    let settings = crate::hook::install(&options.out)?;
    let agent = agent_run(options, query, job, &prompt, EXPLORE_TOOLS, &settings)?;

    let wanted = query.wanted();
    let relied: BTreeSet<PathBuf> = agent.files_relied.iter().cloned().collect();
    let read: BTreeSet<PathBuf> = agent.files_read.iter().cloned().collect();

    let mut metrics = BTreeMap::new();
    score(&mut metrics, "", &wanted, &relied);
    score(&mut metrics, "read_", &wanted, &read);
    metrics.insert("files_relied".to_string(), relied.len() as f64);
    metrics.insert("files_opened".to_string(), read.len() as f64);
    metrics.insert(
        "relied_parsed".to_string(),
        f64::from(u8::from(agent.relied_parsed)),
    );
    metrics.insert("tasks".to_string(), agent.tasks as f64);
    metrics.insert("explore_tasks".to_string(), agent.explore_tasks as f64);
    metrics.insert(
        "parent_tool_uses".to_string(),
        agent.parent_tool_uses as f64,
    );
    // The hook's work, counted: on this condition every read the parent tried
    // for itself was refused, and the ones it did not try are the ones it
    // delegated instead.
    metrics.insert(
        "parent_tool_denials".to_string(),
        agent.parent_tool_denials as f64,
    );
    metrics.insert("wall_ms".to_string(), agent.wall_ms as f64);
    metrics.insert("cost_usd".to_string(), agent.cost_usd);
    metrics.insert(
        "session_total_tokens".to_string(),
        agent.session.total() as f64,
    );
    metrics.insert(
        "parent_total_tokens".to_string(),
        agent.parent.total() as f64,
    );
    tokens(&mut metrics, "agent_", agent.agent);
    metrics.insert("agent_turns".to_string(), agent.turns as f64);
    metrics.insert("agent_tool_uses".to_string(), agent.tool_uses as f64);
    if let Some(ms) = agent.agent_wall_ms {
        metrics.insert("agent_wall_ms".to_string(), ms as f64);
    }
    // The CLI prices a run, not an agent. Apportioning by tokens is the closest
    // a caller gets to what the subagent alone cost.
    if agent.session.total() > 0 {
        let share = agent.agent.total() as f64 / agent.session.total() as f64;
        metrics.insert("agent_cost_share_usd".to_string(), agent.cost_usd * share);
    }

    Ok(Measured {
        metrics,
        detail: agent.detail,
        models: agent.models,
    })
}

/// Where the warm runs' answers live: the directory the caller named, else one
/// under `--out`. Both the warm runs and the cold runs' merge use this, so
/// there is one answer to where a warm run reads from.
fn warm_cache(options: &Options) -> PathBuf {
    options
        .cache_dir
        .clone()
        .unwrap_or_else(|| options.out.join("cache"))
}

/// The s1m condition: the binary, at its defaults, from the wiki directory.
fn s1m_condition(
    options: &Options,
    query: &Query,
    job: &Job,
    cold: bool,
    threshold: Option<f64>,
) -> Result<Measured, String> {
    let entries = query.entries(&options.entry);
    // A cold run buys every judgment, and buys it into a cache directory of its
    // own so that what it spent can be read back afterwards: `--no-cache`
    // stores nothing, and s1m's JSON reports the calls it made but not the
    // tokens they cost.
    let cache = if cold {
        options
            .out
            .join(COLD_CACHE)
            .join(format!("{}-{}", query.id, job.repeat))
    } else {
        warm_cache(options)
    };
    fs::create_dir_all(&cache).map_err(|error| format!("{}: {error}", cache.display()))?;
    // What the cache already holds: whatever is there afterwards and was not
    // here before is what this run bought, and the only place its tokens are
    // written down.
    let before = judgments(&cache);

    let stdout = raw_path(options, query, job, "json");
    let stderr = raw_path(options, query, job, "stderr");
    let mut arguments = vec![query.query.clone()];
    // s1m takes the entry files as its positional arguments, after the query.
    arguments.extend(entries.iter().map(|entry| entry.to_string()));
    arguments.extend([
        "--root".to_string(),
        ".".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ]);
    if let Some(threshold) = threshold {
        arguments.push("--threshold".to_string());
        arguments.push(threshold.to_string());
    }
    // The gold set says what relevance means for a query, and s1m takes it.
    if let Some(mode) = &query.mode {
        arguments.push("--mode".to_string());
        arguments.push(mode.clone());
    }
    let started = Instant::now();
    let status = spawn(
        Command::new(&options.s1m)
            .args(&arguments)
            .current_dir(&options.wiki)
            .env("S1M_CACHE_DIR", &cache),
        &stdout,
        &stderr,
        options.timeout,
    )?;
    let wall = started.elapsed();
    // Exit 1 is s1m saying nothing beyond the entry files cleared the
    // threshold. That is a reading list of the entry files, which is a
    // measurement — a bad one for the query, not a failed run.
    if !matches!(status, Some(0) | Some(1)) {
        return Err(format!(
            "s1m exited {}: {}",
            status.map_or("on a timeout".to_string(), |code| code.to_string()),
            tail(&stderr)
        ));
    }

    let json =
        fs::read_to_string(&stdout).map_err(|error| format!("{}: {error}", stdout.display()))?;
    let list = reading::summarise(&json, &options.wiki)?;
    let returned: BTreeSet<PathBuf> = list.files.iter().cloned().collect();

    let mut metrics = BTreeMap::new();
    score(&mut metrics, "", &query.wanted(), &returned);
    metrics.insert("files_opened".to_string(), returned.len() as f64);
    metrics.insert("files_visited".to_string(), list.visited as f64);
    metrics.insert("agent_read_tokens".to_string(), list.read_tokens as f64);
    metrics.insert("wall_ms".to_string(), wall.as_millis() as f64);
    metrics.insert("jev_calls".to_string(), list.calls as f64);
    // A warm run usually buys nothing, and then this is zero; a warm run at
    // another threshold walks somewhere the cache has not been, and what it
    // bought there is real money and is counted.
    let spent = jev_spent(&cache, &before);
    metrics.insert("jev_input_tokens".to_string(), spent.0 as f64);
    metrics.insert("jev_output_tokens".to_string(), spent.1 as f64);
    metrics.insert("jev_cost_usd".to_string(), spent.2);
    metrics.insert("cost_usd".to_string(), spent.2);
    if let Some(threshold) = threshold {
        metrics.insert("threshold".to_string(), threshold);
    }
    if cold {
        // A cold run is the only one that leaves the shared cache able to serve
        // the warm run that follows it — whether or not the caller named that
        // cache, because a warm run against a cache the cold run did not fill
        // buys everything again and is not warm at all.
        merge_cache(&cache, &warm_cache(options));
    }

    Ok(Measured {
        metrics,
        detail: serde_json::json!({
            "query": query.query,
            "entry": entries,
            "command": arguments,
            "cold": cold,
            "threshold": threshold,
            "files": list.files,
            "wanted": query.wanted,
            "read_chars": list.read_chars,
        }),
        // s1m's walk buys judgments from a model, but the model is the
        // judgment API's, not one this harness asked for: nothing here is a
        // measured tier.
        models: Vec::new(),
    })
}

/// The s1m-agent condition: an agent handed the reading list s1m returned for
/// this query, and told to open only what it needs.
fn s1m_agent_condition(
    options: &Options,
    query: &Query,
    job: &Job,
    rows: &[Row],
) -> Result<Measured, String> {
    let (files, s1m_cost) = reading_list(rows, &query.id, job.repeat).ok_or_else(|| {
        format!(
            "no s1m reading list for {} on repeat {}: the `s1m` run it is paired \
             with did not produce one",
            query.id, job.repeat
        )
    })?;
    let list = files
        .iter()
        .map(|file| format!("- {file}"))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = S1M_AGENT_PROMPT
        .replace("<files>", &list)
        .replace("<query>", &query.query);
    // No hook here: this agent was handed the list, and reading it is the
    // whole job. The settings file is there so that both agent conditions run
    // under the same configuration.
    let settings = crate::hook::install_plain(&options.out)?;
    let agent = agent_run(options, query, job, &prompt, S1M_AGENT_TOOLS, &settings)?;

    let wanted = query.wanted();
    let relied: BTreeSet<PathBuf> = agent.files_relied.iter().cloned().collect();
    let read: BTreeSet<PathBuf> = agent.files_read.iter().cloned().collect();

    let mut metrics = BTreeMap::new();
    score(&mut metrics, "", &wanted, &relied);
    score(&mut metrics, "read_", &wanted, &read);
    metrics.insert("files_relied".to_string(), relied.len() as f64);
    metrics.insert("files_opened".to_string(), read.len() as f64);
    metrics.insert(
        "relied_parsed".to_string(),
        f64::from(u8::from(agent.relied_parsed)),
    );
    metrics.insert("wall_ms".to_string(), agent.wall_ms as f64);
    // What this condition costs is the agent plus the reading list it was
    // handed: the list is not free, and a cost column that left it out would
    // compare an agent that was given the answer with one that had to look.
    metrics.insert("s1m_cost_usd".to_string(), s1m_cost);
    metrics.insert("agent_cost_usd".to_string(), agent.cost_usd);
    metrics.insert("cost_usd".to_string(), agent.cost_usd + s1m_cost);
    // No subagent here: the agent that was handed the list is the one measured.
    tokens(&mut metrics, "agent_", agent.session);
    metrics.insert(
        "parent_total_tokens".to_string(),
        agent.parent.total() as f64,
    );
    Ok(Measured {
        metrics,
        detail: agent.detail,
        models: agent.models,
    })
}

/// What one `claude -p` run reported, whichever condition asked for it.
struct AgentRun {
    /// The parent's own tool calls: a parent that was told to delegate and
    /// went looking itself is a measurement of the wrong thing, and this is
    /// the number that says so.
    parent_tool_uses: usize,
    /// The parent's own tool calls that were refused.
    parent_tool_denials: usize,
    files_relied: Vec<PathBuf>,
    /// Whether the answer carried a list of files at all.
    relied_parsed: bool,
    files_read: Vec<PathBuf>,
    /// Subagents the parent spawned, and how many of them were Explore.
    tasks: usize,
    explore_tasks: usize,
    /// The Explore subagents' own tokens, summed, or the parent's when nothing
    /// was spawned.
    agent: Usage,
    parent: Usage,
    session: Usage,
    turns: usize,
    tool_uses: usize,
    cost_usd: f64,
    wall_ms: u128,
    agent_wall_ms: Option<u64>,
    /// The models Claude Code resolved for the work measured: the Explore
    /// subagent's, as the `Task` result stated it, else the agent that answered
    /// when nothing was spawned. A label, and the only one here a report may
    /// print.
    models: Vec<String>,
    detail: serde_json::Value,
}

/// Runs the agent and reads back what it cost.
fn agent_run(
    options: &Options,
    query: &Query,
    job: &Job,
    prompt: &str,
    tools: &str,
    settings: &Path,
) -> Result<AgentRun, String> {
    let stdout = raw_path(options, query, job, "stream.jsonl");
    let stderr = raw_path(options, query, job, "stderr");
    let arguments = claude_arguments(prompt, tools, options.model.as_deref(), settings);

    let started = Instant::now();
    let status = spawn(
        Command::new(&options.claude)
            .args(&arguments)
            .current_dir(&options.wiki),
        &stdout,
        &stderr,
        options.timeout,
    )?;
    let wall = started.elapsed();
    if status != Some(0) {
        return Err(format!(
            "claude exited {}: {}",
            status.map_or("on a timeout".to_string(), |code| code.to_string()),
            tail(&stderr)
        ));
    }

    let stream =
        fs::read_to_string(&stdout).map_err(|error| format!("{}: {error}", stdout.display()))?;
    let summary = explore::summarise_stream(&stream, &options.wiki)?;
    if summary.is_error {
        return Err(format!("the agent reported an error: {}", summary.answer));
    }

    // Every subagent the parent spawned is measured, or the run is not a
    // measurement: a subagent's tokens are in the session total and in neither
    // the parent's nor any subagent's, so attributing the session total to the
    // agent would quietly average a parent-plus-subagent figure in with
    // subagent-only ones.
    let explore: Vec<&explore::Task> = summary
        .tasks
        .iter()
        .filter(|task| task.kind == EXPLORE_AGENT)
        .collect();
    if !summary.tasks.is_empty() && explore.is_empty() {
        return Err(format!(
            "the parent spawned {} subagent(s), none of them {EXPLORE_AGENT}: \
             only {EXPLORE_AGENT} is measured, so this run is not one",
            summary.tasks.len()
        ));
    }

    let mut transcripts = Vec::new();
    for task in &explore {
        // The subagent's own transcript is the only place its turns carry
        // their final token counts; the stream's copies are the counts so far.
        let session = summary.session_id.as_deref().unwrap_or_default();
        let path = transcript_path(options, session, &task.agent_id).ok_or_else(|| {
            format!(
                "{} ran as agent {} but its transcript is not under the project \
                 directories: its tokens cannot be told apart from the parent's",
                task.kind, task.agent_id
            )
        })?;
        let text =
            fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        transcripts.push((path, explore::summarise_transcript(&text, &options.wiki)));
    }

    let relied = explore::files_relied_on(&summary.answer, &options.wiki);
    let relied_parsed = relied.is_some();
    let files_relied = relied.unwrap_or_default();
    let (agent, files_read, turns, tool_uses, agent_model) = if transcripts.is_empty() {
        // Nothing was spawned: the agent that ran is the parent, and the
        // stream carries what it opened.
        (
            summary.session_total,
            summary.parent_files_read.clone(),
            0,
            summary.parent_tool_uses,
            None,
        )
    } else {
        let mut usage = Usage::default();
        let mut files = BTreeSet::new();
        let mut turns = 0;
        let mut tool_uses = 0;
        let mut model = None;
        for (_, subagent) in &transcripts {
            usage.add(subagent.usage);
            files.extend(subagent.files_read.iter().cloned());
            turns += subagent.turns;
            tool_uses += subagent.tool_uses;
            model = model.or_else(|| subagent.model.clone());
        }
        (usage, files.into_iter().collect(), turns, tool_uses, model)
    };

    Ok(AgentRun {
        parent_tool_uses: summary.parent_tool_uses,
        parent_tool_denials: summary.parent_tool_denials,
        files_relied: files_relied.clone(),
        relied_parsed,
        files_read: files_read.clone(),
        tasks: summary.tasks.len(),
        explore_tasks: explore.len(),
        agent,
        parent: summary.parent_total,
        session: summary.session_total,
        turns,
        tool_uses,
        cost_usd: summary.cost_usd,
        wall_ms: wall.as_millis(),
        // Summed when more than one ran, which overstates them if they ran at
        // the same time; the count is recorded beside it.
        agent_wall_ms: explore
            .iter()
            .filter_map(|task| task.duration_ms)
            .reduce(|total, ms| total + ms),
        // What the subagents resolved to, as the `Task` results stated it; a
        // run that spawned none was answered by the parent, whose model is the
        // one the stream reported.
        models: if explore.is_empty() {
            summary.model.clone().into_iter().collect()
        } else {
            explore
                .iter()
                .filter_map(|task| task.resolved_model.clone())
                .collect::<BTreeSet<String>>()
                .into_iter()
                .collect()
        },
        detail: serde_json::json!({
            "query": query.query,
            "prompt": prompt,
            "command": arguments,
            "answer": summary.answer,
            "files_relied": files_relied,
            "files_read": files_read,
            "wanted": query.wanted,
            "relied_parsed": relied_parsed,
            // What the parent and the subagent answered on, which the point of
            // the run is that they can differ. What was asked for is on the row
            // itself, where a report may read it.
            "parent_model": summary.model,
            "agent_model": agent_model,
            "parent_tools": summary.parent_tools,
            "parent_tool_denials": summary.parent_tool_denials,
            "settings": settings.display().to_string(),
            "session_id": summary.session_id,
            "tasks": summary.tasks,
            "subagent_usage_source": if transcripts.is_empty() { "stream" } else { "transcript" },
            "transcripts": transcripts
                .iter()
                .map(|(path, _)| path.display().to_string())
                .collect::<Vec<_>>(),
        }),
    })
}

/// Where Claude Code wrote one subagent's transcript: under the project
/// directory for the working directory, by session and agent id. The project
/// directory's name is derived from the path, so the session id is searched for
/// instead of spelled.
fn transcript_path(options: &Options, session: &str, agent: &str) -> Option<PathBuf> {
    let projects = options.transcripts.clone().unwrap_or_else(|| {
        let home = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude")
            });
        home.join("projects")
    });
    for project in fs::read_dir(&projects).ok()?.flatten() {
        let path = project
            .path()
            .join(session)
            .join("subagents")
            .join(format!("agent-{agent}.jsonl"));
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// The flags one `claude -p` run is made with.
pub fn claude_arguments(
    prompt: &str,
    tools: &str,
    model: Option<&str>,
    settings: &Path,
) -> Vec<String> {
    let mut arguments = vec!["-p".to_string(), prompt.to_string()];
    // A model named here is inherited by every agent in the run, the subagent
    // included, so naming one measures that model rather than the one Claude
    // Code would have used. With none named, no flag is passed and the models
    // that answered are read back from the stream and the transcripts.
    if let Some(model) = model {
        arguments.push("--model".to_string());
        arguments.push(model.to_string());
    }
    arguments.extend([
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        // Not `--safe-mode`: that disables hooks, and the hook is what makes
        // the parent delegate. `--setting-sources ""` is what keeps the wiki's
        // own configuration out — including its `CLAUDE.md`, which the default
        // flags do load — and `--settings` is the one file that is let in.
        "--setting-sources".to_string(),
        String::new(),
        "--settings".to_string(),
        settings.display().to_string(),
        // `--setting-sources ""` does not reach the account's own MCP servers
        // or skills, and those arrive as tools and prompt: on one machine, 58
        // extra tools and 18 skills, four times the parent's context before it
        // has read a word of the wiki. What is being measured is the wiki.
        "--strict-mcp-config".to_string(),
        "--disable-slash-commands".to_string(),
        "--tools".to_string(),
        tools.to_string(),
        "--allowedTools".to_string(),
        tools.to_string(),
        "--permission-prompts".to_string(),
        "none".to_string(),
    ]);
    arguments
}

/// The subagent this harness measures: Claude Code's built-in read-only
/// explorer.
pub const EXPLORE_AGENT: &str = "Explore";

/// How hard the Explore agent is told to look.
///
/// The built-in agent takes a thoroughness from whoever dispatches it, and it
/// is most of what a run measures: the same query at "quick" and at "very
/// thorough" are different experiments. It is said in the prompt and stated in
/// the report rather than left to the parent to guess.
pub const THOROUGHNESS: &str = "medium";

/// The reading list s1m returned for one query on one repeat, and what that
/// run cost.
///
/// Repeat k of an agent condition is paired with repeat k of `s1m`: the list an
/// agent was handed has to be the list that run produced, or the two rows are
/// not about the same reading list.
fn reading_list(rows: &[Row], query: &str, repeat: usize) -> Option<(Vec<String>, f64)> {
    let row = rows.iter().rev().find(|row| {
        row.query_id == query && row.condition == "s1m" && row.repeat == repeat && row.ok
    })?;
    let files = row.detail.as_ref()?["files"]
        .as_array()?
        .iter()
        .filter_map(|file| file.as_str().map(str::to_string))
        .collect();
    Some((files, row.metrics.get("cost_usd").copied().unwrap_or(0.0)))
}

/// The run that has to happen before `job` can be measured, when it has not
/// already.
///
/// `s1m-agent` is handed a reading list, so the `s1m` run for the same query
/// and repeat is its prerequisite rather than its neighbour: a pass that names
/// only `s1m-agent`, or a resume whose `s1m` row is missing, makes the run it
/// needs instead of failing.
fn prerequisite(job: &Job, query: &str, rows: &[Row]) -> Option<Job> {
    if job.condition != "s1m-agent" || reading_list(rows, query, job.repeat).is_some() {
        return None;
    }
    Some(Job {
        query: job.query,
        condition: "s1m".to_string(),
        repeat: job.repeat,
    })
}

/// The threshold a condition names, or `None` when it names none.
///
/// `s1m-t0.4` is s1m at its defaults but `--threshold 0.4`. The report shows it
/// as its own condition, which is why the threshold is in the name: one run
/// directory can hold several, and each is averaged on its own.
pub fn threshold_of(condition: &str) -> Option<f64> {
    let value: f64 = condition.strip_prefix("s1m-t")?.parse().ok()?;
    (0.0..=1.0).contains(&value).then_some(value)
}

/// Recall and precision of `returned` against `wanted`, under a name prefix.
fn score(
    metrics: &mut BTreeMap<String, f64>,
    prefix: &str,
    wanted: &BTreeSet<PathBuf>,
    returned: &BTreeSet<PathBuf>,
) {
    let found = wanted.intersection(returned).count();
    let recall = if wanted.is_empty() {
        0.0
    } else {
        found as f64 / wanted.len() as f64
    };
    let precision = if returned.is_empty() {
        0.0
    } else {
        found as f64 / returned.len() as f64
    };
    metrics.insert(format!("{prefix}recall"), recall);
    metrics.insert(format!("{prefix}precision"), precision);
    metrics.insert(format!("{prefix}found"), found as f64);
}

/// One agent's tokens, under a name prefix.
fn tokens(metrics: &mut BTreeMap<String, f64>, prefix: &str, usage: Usage) {
    metrics.insert(format!("{prefix}total_tokens"), usage.total() as f64);
    metrics.insert(format!("{prefix}read_tokens"), usage.read() as f64);
    metrics.insert(format!("{prefix}input_tokens"), usage.input as f64);
    metrics.insert(format!("{prefix}output_tokens"), usage.output as f64);
    metrics.insert(
        format!("{prefix}cache_read_tokens"),
        usage.cache_read as f64,
    );
    metrics.insert(
        format!("{prefix}cache_create_tokens"),
        usage.cache_create as f64,
    );
}

/// The judgments a cache directory holds, by name. The names are the hashes of
/// the requests that produced them, so a name that was not there before a run
/// is an answer that run bought.
fn judgments(cache: &Path) -> BTreeSet<std::ffi::OsString> {
    fs::read_dir(cache.join("judgments"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name())
        .collect()
}

/// What the judgments bought since `before` cost: input tokens, output tokens
/// and the input at the list price.
fn jev_spent(cache: &Path, before: &BTreeSet<std::ffi::OsString>) -> (u64, u64, f64) {
    let mut input = 0;
    let mut output = 0;
    let Ok(entries) = fs::read_dir(cache.join("judgments")) else {
        return (0, 0, 0.0);
    };
    for entry in entries.flatten() {
        if before.contains(&entry.file_name()) {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(stored) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        input += stored["detail"]["input_tokens"].as_u64().unwrap_or(0);
        output += stored["detail"]["output_tokens"].as_u64().unwrap_or(0);
    }
    (
        input,
        output,
        input as f64 / 1_000_000.0 * s1m::jev::PRICE_PER_MTOK,
    )
}

/// Copies judgments a cold run bought into the shared cache, so the warm run
/// after it is the cache's and not the API's. The names are the requests'
/// hashes, so a file that is already there is the same answer.
fn merge_cache(from: &Path, to: &Path) {
    let Ok(entries) = fs::read_dir(from.join("judgments")) else {
        return;
    };
    let _ = fs::create_dir_all(to.join("judgments"));
    for entry in entries.flatten() {
        let target = to.join("judgments").join(entry.file_name());
        if !target.exists() {
            let _ = fs::copy(entry.path(), target);
        }
    }
}

fn raw_path(options: &Options, query: &Query, job: &Job, suffix: &str) -> PathBuf {
    options.out.join(RAW).join(format!(
        "{}-{}-{}.{suffix}",
        query.id, job.condition, job.repeat
    ))
}

/// Runs a command with its output on disk rather than on a pipe — a pipe that
/// fills while nothing is reading it is a deadlock — and kills it if it outruns
/// the timeout. `None` is a run that was killed.
fn spawn(
    command: &mut Command,
    stdout: &Path,
    stderr: &Path,
    timeout: Duration,
) -> Result<Option<i32>, String> {
    let out = fs::File::create(stdout).map_err(|error| format!("{}: {error}", stdout.display()))?;
    let err = fs::File::create(stderr).map_err(|error| format!("{}: {error}", stderr.display()))?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .spawn()
        .map_err(|error| format!("{:?}: {error}", command.get_program()))?;

    let started = Instant::now();
    loop {
        match child.try_wait().map_err(|error| error.to_string())? {
            Some(status) => return Ok(Some(status.code().unwrap_or(-1))),
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None);
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// The last line of a file, for an error message.
fn tail(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no output")
        .chars()
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempDir;

    fn gold() -> Gold {
        serde_json::from_str(
            r#"{"queries": [
                {"id": "one", "query": "q", "wanted": ["a.md"]},
                {"id": "two", "query": "r", "wanted": ["b.md"]}]}"#,
        )
        .expect("a gold set")
    }

    /// The revision the tests measure against. Only the report checks the shape
    /// of one, and these never reach it.
    const WIKI: &str = "sha256:0f1e2d3c";

    /// A row with nothing in it but what the test it is used in is about.
    fn row(query: &str, condition: &str, repeat: usize, ok: bool) -> Row {
        Row {
            query_id: query.to_string(),
            category: "how-to".to_string(),
            condition: condition.to_string(),
            repeat,
            wiki: WIKI.to_string(),
            ok,
            metrics: BTreeMap::new(),
            model_asked_for: None,
            models: Vec::new(),
            detail: None,
        }
    }

    /// A pass costs money, so a run already in the JSONL is never made again,
    /// and the cold s1m run is made only as often as it is asked for.
    #[test]
    fn a_resumed_pass_owes_only_what_is_missing() {
        let conditions = vec!["explore".to_string(), "s1m".to_string()];
        let all = pending(&gold(), &conditions, 2, 1, &Done::nothing());
        // Two queries, two repeats, two conditions, plus one cold s1m run per
        // query on the first repeat.
        assert_eq!(all.len(), 10);
        assert_eq!(
            all.iter().filter(|job| job.condition == "s1m-cold").count(),
            2
        );
        // One query at a time, so a pass stopped early has whole queries.
        assert!(all[..5].iter().all(|job| job.query == 0));

        let rows = vec![
            row("one", "explore", 0, true),
            row("one", "s1m-cold", 0, true),
        ];
        let done = Done::read(&rows, &BTreeMap::new(), WIKI, None, false);
        let owed = pending(&gold(), &conditions, 2, 1, &done);
        assert_eq!(owed.len(), 8);
        assert!(!owed.contains(&Job {
            query: 0,
            condition: "explore".to_string(),
            repeat: 0
        }));
    }

    /// The rows are the record of what has been measured: they are read back
    /// exactly as they were written, provenance included.
    #[test]
    fn rows_are_written_and_read_back() {
        let out = TempDir::new("rows");
        let written = Row {
            metrics: BTreeMap::from([("recall".to_string(), 1.0)]),
            models: vec!["claude-sonnet-5".to_string()],
            model_asked_for: Some("sonnet".to_string()),
            detail: Some(serde_json::json!({"query": "q"})),
            ..row("one", "explore", 0, true)
        };
        assert!(read_rows(out.path()).expect("no rows yet").is_empty());
        append(out.path(), &written).expect("a row");
        append(out.path(), &written).expect("another row");
        let key = written.key();
        assert_eq!(
            read_rows(out.path()).expect("two rows"),
            vec![written.clone(), written]
        );
        assert_eq!(
            (
                key.wiki.as_str(),
                key.query.as_str(),
                key.condition.as_str(),
                key.repeat
            ),
            (WIKI, "one", "explore", 0)
        );
    }

    /// The directory's own rows are what it has made, whatever wiki revision
    /// they were made against; the ledger's are held to the revision and to the
    /// model, because the same query against another cut or at another tier is
    /// another measurement.
    #[test]
    fn what_a_pass_owes_is_the_directorys_rows_and_the_ledgers_key() {
        let out = PathBuf::from("/somewhere/run");
        let listed = |wiki: &str, condition: &str, model: Option<&str>, ok: Option<bool>| {
            let entry = Entry::buying(
                Key {
                    wiki: wiki.to_string(),
                    query: "one".to_string(),
                    condition: condition.to_string(),
                    repeat: 0,
                },
                model,
                &out,
            );
            match ok {
                Some(ok) => (entry.id(), entry.measured(ok, Some(0.1))),
                // Bought, and no outcome: the pass died before the row.
                None => (entry.id(), entry),
            }
        };
        let recorded: BTreeMap<Id, Entry> = BTreeMap::from([
            // The same run, against this revision, at this model.
            listed(WIKI, "explore", None, Some(true)),
            // Another cut of the wiki.
            listed("sha256:ff", "s1m-agent", None, Some(true)),
            // Another tier.
            listed(WIKI, "s1m", Some("haiku"), Some(true)),
        ]);

        let done = Done::read(&[], &recorded, WIKI, None, false);
        let known = |condition: &str| {
            done.contains(&Key {
                wiki: String::new(),
                query: "one".to_string(),
                condition: condition.to_string(),
                repeat: 0,
            })
        };
        assert!(known("explore"), "the ledger's own run is measured");
        assert!(!known("s1m-agent"), "another revision is not this one");
        assert!(!known("s1m"), "another model is another measurement");

        // This directory's rows are its own record, and hold whatever revision
        // they were written against: a resumed pass on an old directory does
        // not buy all of it again.
        let rows = vec![Row {
            wiki: String::new(),
            ..row("one", "explore", 0, true)
        }];
        let done = Done::read(&rows, &recorded, WIKI, None, false);
        assert!(done.contains(&Key {
            wiki: WIKI.to_string(),
            query: "one".to_string(),
            condition: "explore".to_string(),
            repeat: 0
        }));

        // An s1m run takes no model, so it is the same run whether or not the
        // pass names one: only the conditions that run an agent are another
        // measurement at another tier.
        assert_eq!(asked_for("s1m", Some("sonnet")), None);
        assert_eq!(asked_for("s1m-cold", Some("sonnet")), None);
        assert_eq!(
            asked_for("explore", Some("sonnet")).as_deref(),
            Some("sonnet")
        );
        assert_eq!(asked_for("explore", None), None);
        let plain: BTreeMap<Id, Entry> = BTreeMap::from([listed(WIKI, "s1m", None, Some(true))]);
        let agent: BTreeMap<Id, Entry> =
            BTreeMap::from([listed(WIKI, "explore", None, Some(true))]);
        let named =
            |recorded: &BTreeMap<Id, Entry>| Done::read(&[], recorded, WIKI, Some("sonnet"), false);
        assert!(
            named(&plain).contains(&Key {
                wiki: String::new(),
                query: "one".to_string(),
                condition: "s1m".to_string(),
                repeat: 0
            }),
            "an s1m run is the same run whatever the pass names"
        );
        assert!(
            !named(&agent).contains(&Key {
                wiki: String::new(),
                query: "one".to_string(),
                condition: "explore".to_string(),
                repeat: 0
            }),
            "an agent run measured at no model is not one measured at sonnet"
        );

        // A purchase with no outcome was killed before it wrote a row: it is
        // owed, and the money is already spent.
        let killed: BTreeMap<Id, Entry> = BTreeMap::from([listed(WIKI, "s1m-agent", None, None)]);
        assert!(!Done::read(&[], &killed, WIKI, None, false).contains(&Key {
            wiki: WIKI.to_string(),
            query: "one".to_string(),
            condition: "s1m-agent".to_string(),
            repeat: 0
        }));
    }

    /// `--retry-failed` puts back the runs that failed and nothing else: a run
    /// that worked is still measured however many passes ask for it.
    #[test]
    fn retrying_covers_the_failed_runs_only() {
        let out = PathBuf::from("/somewhere/run");
        let entry = |condition: &str, ok: bool| {
            let entry = Entry::buying(
                Key {
                    wiki: WIKI.to_string(),
                    query: "two".to_string(),
                    condition: condition.to_string(),
                    repeat: 0,
                },
                None,
                &out,
            );
            (entry.id(), entry.measured(ok, ok.then_some(0.1)))
        };
        let recorded: BTreeMap<Id, Entry> =
            BTreeMap::from([entry("explore", false), entry("s1m", true)]);
        let failed = vec![row("two", "explore", 0, false), row("two", "s1m", 0, true)];

        let plain = Done::read(&failed, &recorded, WIKI, None, false);
        let retried = Done::read(&failed, &recorded, WIKI, None, true);
        // Both records hold the same two runs, and a run two records hold is
        // one run.
        assert_eq!(plain.len(), 2, "both conditions, without retrying");
        assert_eq!(retried.len(), 1, "only the run that worked is measured");
        assert!(retried.contains(&Key {
            wiki: WIKI.to_string(),
            query: "two".to_string(),
            condition: "s1m".to_string(),
            repeat: 0
        }));
        assert!(!retried.contains(&Key {
            wiki: WIKI.to_string(),
            query: "two".to_string(),
            condition: "explore".to_string(),
            repeat: 0
        }));
    }

    /// `s1m-t0.4` is s1m at threshold 0.4. Anything else that starts the same
    /// way is not a condition, and saying so beats running the defaults under
    /// a name that claims otherwise.
    #[test]
    fn a_condition_can_name_a_threshold() {
        assert_eq!(threshold_of("s1m-t0.4"), Some(0.4));
        assert_eq!(threshold_of("s1m-t0"), Some(0.0));
        assert_eq!(threshold_of("s1m-t1"), Some(1.0));
        assert_eq!(threshold_of("s1m"), None);
        assert_eq!(threshold_of("s1m-cold"), None);
        assert_eq!(threshold_of("s1m-tlow"), None);
        // A scent is 0 to 1, so a threshold outside it names nothing.
        assert_eq!(threshold_of("s1m-t1.5"), None);
        assert_eq!(threshold_of("s1m-t-0.2"), None);
    }

    /// Only what a run bought is counted: a cache that already held an answer
    /// was paid for by whoever bought it first.
    #[test]
    fn only_the_judgments_a_run_bought_are_counted() {
        let cache = TempDir::new("jev");
        let judgment = |input: u64, output: u64| {
            format!(
                r#"{{"format":3,"judgment":{{}},"detail":{{"input_tokens":{input},"output_tokens":{output}}}}}"#
            )
        };
        cache.write("judgments/old.json", &judgment(1000, 10));
        let before = judgments(cache.path());
        cache.write("judgments/new.json", &judgment(5000, 300));

        let (input, output, cost) = jev_spent(cache.path(), &before);
        assert_eq!((input, output), (5000, 300));
        // 5000 input tokens at the list price in `src/jev.rs`.
        assert!((cost - 5000.0 / 1_000_000.0 * s1m::jev::PRICE_PER_MTOK).abs() < 1e-12);

        // Nothing bought is nothing owed.
        let after = judgments(cache.path());
        assert_eq!(jev_spent(cache.path(), &after), (0, 0, 0.0));
    }

    /// s1m can fail part way through a walk — a judgment the API refuses is
    /// exit 2 with the reason on stderr. That run is a row with no metrics and
    /// its reason in the raw half, never in the numbers: an API error quotes
    /// the request that caused it.
    #[cfg(unix)]
    #[test]
    fn a_failed_run_is_a_row_with_no_metrics_and_its_reason_in_detail() {
        use std::os::unix::fs::PermissionsExt;

        let wiki = TempDir::new("failed-wiki");
        wiki.write("index.md", "# Index\n");
        let out = TempDir::new("failed-out");
        let fake = TempDir::new("failed-s1m");
        fake.write(
            "s1m",
            "#!/bin/sh\necho 's1m: /a/page.md could not be judged: max_tokens_exceeded' >&2\nexit 2\n",
        );
        let binary = fake.path().join("s1m");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("an executable");

        let options = Options {
            wiki: wiki.path().to_path_buf(),
            gold: PathBuf::new(),
            out: out.path().to_path_buf(),
            repeats: 1,
            conditions: vec!["s1m".to_string()],
            cache_dir: None,
            entry: vec!["index.md".to_string()],
            model: None,
            s1m: binary,
            claude: PathBuf::from("claude"),
            transcripts: None,
            cold_repeats: 0,
            queries: Vec::new(),
            estimate: false,
            yes: false,
            retry_failed: false,
            ledger: None,
            timeout: Duration::from_secs(30),
        };
        fs::create_dir_all(out.path().join(RAW)).expect("a raw directory");
        let query = &gold().queries[0];
        let job = Job {
            query: 0,
            condition: "s1m".to_string(),
            repeat: 0,
        };

        let row = measure(&options, WIKI, query, &job, &[]);
        assert!(!row.ok);
        assert!(row.metrics.is_empty(), "{:?}", row.metrics);
        let reason = row.detail.as_ref().expect("the raw half")["error"]
            .as_str()
            .expect("a reason")
            .to_string();
        assert!(reason.contains("s1m exited 2"), "{reason}");
        assert!(reason.contains("max_tokens_exceeded"), "{reason}");

        // And it is a run that has happened: a resumed pass does not pay for
        // it again.
        append(out.path(), &row).expect("a row");
        let done = Done::read(
            &read_rows(out.path()).expect("one row"),
            &BTreeMap::new(),
            WIKI,
            None,
            false,
        );
        assert!(
            pending(&gold(), &options.conditions, 1, 0, &done)
                .iter()
                .all(|job| job.query != 0)
        );
    }

    /// Options pointing at a wiki and an out directory, with binaries that do
    /// not exist: the tests that need one replace it.
    fn options(wiki: &TempDir, out: &TempDir) -> Options {
        Options {
            wiki: wiki.path().to_path_buf(),
            gold: PathBuf::new(),
            out: out.path().to_path_buf(),
            repeats: 1,
            conditions: vec!["s1m".to_string()],
            cache_dir: None,
            estimate: false,
            yes: false,
            retry_failed: false,
            ledger: None,
            entry: vec!["index.md".to_string()],
            model: None,
            s1m: PathBuf::from("s1m"),
            claude: PathBuf::from("claude"),
            transcripts: None,
            cold_repeats: 1,
            queries: Vec::new(),
            timeout: Duration::from_secs(30),
        }
    }

    /// A shell script at `<dir>/s1m` that answers like s1m and buys one
    /// judgment into whatever cache it was pointed at. `record` names a file it
    /// appends its arguments to, so a test can count what was bought.
    #[cfg(unix)]
    fn fake_s1m(dir: &TempDir, record: Option<&Path>) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let recording = record.map_or(String::new(), |path| {
            format!("printf '%s\\n' \"$*\" >> \"{}\"\n", path.display())
        });
        dir.write(
            "s1m",
            &format!(
                "#!/bin/sh\n{recording}\
             mkdir -p \"$S1M_CACHE_DIR/judgments\"\n\
             printf '{{\"format\":3,\"judgment\":{{}},\"detail\":{{\"input_tokens\":1000,\"output_tokens\":10}}}}' \
             > \"$S1M_CACHE_DIR/judgments/bought.json\"\n\
             printf '{{\"query\":\"q\",\"mode\":\"useful-for\",\"visited\":1,\"calls\":1,\"results\":[{{\"path\":\"index.md\",\"relevance\":0.9,\"scent\":null,\"via\":[],\"links\":[],\"sections\":[{{\"heading\":null,\"lines\":[1,1],\"score\":0.9}}]}}]}}'\n"
            ),
        );
        let binary = dir.path().join("s1m");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("an executable");
        binary
    }

    /// The cold run buys into a cache of its own, and the warm run that
    /// follows has to be able to read what it bought — including when the
    /// caller named no cache directory, which is the case that silently
    /// charged every warm run cold prices.
    #[cfg(unix)]
    #[test]
    fn a_cold_run_fills_the_cache_the_warm_run_reads() {
        let wiki = TempDir::new("cold-wiki");
        wiki.write("index.md", "# Index\n");
        let out = TempDir::new("cold-out");
        let fake = TempDir::new("cold-s1m");
        let options = Options {
            s1m: fake_s1m(&fake, None),
            ..options(&wiki, &out)
        };
        fs::create_dir_all(out.path().join(RAW)).expect("a raw directory");

        let query = &gold().queries[0];
        let job = Job {
            query: 0,
            condition: "s1m-cold".to_string(),
            repeat: 0,
        };
        let measured =
            s1m_condition(&options, query, &job, true, None).expect("a cold measurement");

        // What it bought is what it is charged for: 1000 input tokens.
        assert_eq!(measured.metrics["jev_input_tokens"], 1000.0);
        assert!(
            (measured.metrics["cost_usd"] - 1000.0 / 1_000_000.0 * s1m::jev::PRICE_PER_MTOK).abs()
                < 1e-12
        );
        // Nothing about s1m's walk is a measured tier: no agent ran.
        assert!(measured.models.is_empty(), "{:?}", measured.models);

        // And the warm cache now holds it, with no `--cache-dir` in sight.
        assert!(
            warm_cache(&options).join("judgments/bought.json").is_file(),
            "the cold run's answers did not reach {}",
            warm_cache(&options).display()
        );
    }

    /// The agent conditions are handed a reading list, so the run that makes
    /// it is a prerequisite and not a neighbour — and it is the run from the
    /// same repeat.
    #[test]
    fn the_reading_list_an_agent_is_handed_is_its_own_repeats() {
        let list = |repeat: usize, file: &str, cost: f64| Row {
            metrics: BTreeMap::from([("cost_usd".to_string(), cost)]),
            detail: Some(serde_json::json!({ "files": [file] })),
            ..row("one", "s1m", repeat, true)
        };
        let rows = vec![list(0, "a.md", 0.002), list(1, "b.md", 0.0)];

        assert_eq!(
            reading_list(&rows, "one", 1),
            Some((vec!["b.md".to_string()], 0.0))
        );
        assert_eq!(
            reading_list(&rows, "one", 0),
            Some((vec!["a.md".to_string()], 0.002))
        );
        assert_eq!(reading_list(&rows, "one", 2), None);
        assert_eq!(reading_list(&rows, "two", 0), None);

        let agent = |repeat: usize| Job {
            query: 0,
            condition: "s1m-agent".to_string(),
            repeat,
        };
        // The repeat that has its list needs nothing first.
        assert_eq!(prerequisite(&agent(0), "one", &rows), None);
        // The one that does not asks for its own repeat, not any repeat.
        assert_eq!(
            prerequisite(&agent(2), "one", &rows),
            Some(Job {
                query: 0,
                condition: "s1m".to_string(),
                repeat: 2
            })
        );
        // A failed s1m row is not a reading list.
        let mut failed = list(3, "c.md", 0.0);
        failed.ok = false;
        let rows = [rows, vec![failed]].concat();
        assert!(prerequisite(&agent(3), "one", &rows).is_some());
        // Nothing else has a prerequisite.
        for condition in ["explore", "s1m", "s1m-cold"] {
            let job = Job {
                query: 0,
                condition: condition.to_string(),
                repeat: 9,
            };
            assert_eq!(prerequisite(&job, "one", &rows), None);
        }
    }

    /// A shell script at `<dir>/claude` that prints `stream`.
    #[cfg(unix)]
    fn fake_claude(dir: &TempDir, stream: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        dir.write(
            "claude",
            &format!("#!/bin/sh\ncat <<'STREAM'\n{stream}\nSTREAM\n"),
        );
        let binary = dir.path().join("claude");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("an executable");
        binary
    }

    /// A subagent ran and its transcript cannot be found. Its tokens are in the
    /// session total and in nothing else, so calling the session total the
    /// agent's would average a parent-plus-subagent figure in with
    /// subagent-only ones. That is not a measurement, and it is not recorded
    /// as one.
    #[cfg(unix)]
    #[test]
    fn a_subagent_with_no_transcript_is_not_a_measurement() {
        let wiki = TempDir::new("stream-wiki");
        wiki.write("index.md", "# Index\n");
        let out = TempDir::new("stream-out");
        let empty = TempDir::new("stream-projects");

        let stream = |kind: &str| {
            [
                r#"{"type":"system","subtype":"init","session_id":"S9"}"#.to_string(),
                format!(
                    r#"{{"type":"system","subtype":"task_started","session_id":"S9","task_id":"agent9","subagent_type":"{kind}"}}"#
                ),
                r#"{"type":"result","subtype":"success","session_id":"S9","is_error":false,"total_cost_usd":0.01,"duration_ms":10,"usage":{"input_tokens":1,"output_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":4},"modelUsage":{"claude-sonnet-5":{"inputTokens":9,"outputTokens":9,"cacheReadInputTokens":9,"cacheCreationInputTokens":9}},"result":"done [\"index.md\"]"}"#.to_string(),
            ]
            .join("\n")
        };

        let job = Job {
            query: 0,
            condition: "explore".to_string(),
            repeat: 0,
        };
        let query = &gold().queries[0];
        fs::create_dir_all(out.path().join(RAW)).expect("a raw directory");

        // An Explore subagent whose transcript is nowhere to be found.
        let fake = TempDir::new("stream-claude");
        let options = Options {
            claude: fake_claude(&fake, &stream(EXPLORE_AGENT)),
            transcripts: Some(empty.path().to_path_buf()),
            conditions: vec!["explore".to_string()],
            ..options(&wiki, &out)
        };
        let row = measure(&options, WIKI, query, &job, &[]);
        assert!(!row.ok);
        assert!(row.metrics.is_empty(), "{:?}", row.metrics);
        let reason = row.detail.expect("the raw half")["error"]
            .as_str()
            .expect("a reason")
            .to_string();
        assert!(reason.contains("transcript"), "{reason}");

        // A subagent that is not the one this harness measures is the same
        // problem by another route.
        let other = TempDir::new("stream-claude-other");
        let options = Options {
            claude: fake_claude(&other, &stream("general-purpose")),
            ..options
        };
        let row = measure(&options, WIKI, query, &job, &[]);
        assert!(!row.ok);
        let reason = row.detail.expect("the raw half")["error"]
            .as_str()
            .expect("a reason")
            .to_string();
        assert!(reason.contains(EXPLORE_AGENT), "{reason}");
    }

    /// A model named on the command line is inherited by every agent in the
    /// run, subagents included, so naming one measures that model rather than
    /// the one Claude Code would have used. With none named, no flag is passed
    /// and the resolved models are read back from the run.
    #[test]
    fn a_model_is_only_asked_for_when_one_was_named() {
        let settings = Path::new("/run/explore-settings.json");
        let named = claude_arguments("ask", "Task", Some("sonnet"), settings);
        let at = named
            .iter()
            .position(|flag| flag == "--model")
            .expect("the flag");
        assert_eq!(named[at + 1], "sonnet");

        let default = claude_arguments("ask", "Task", None, settings);
        assert!(!default.contains(&"--model".to_string()), "{default:?}");

        // Whatever else changes, the run is non-interactive, streamed, and
        // held to the tools it was given.
        for arguments in [&named, &default] {
            assert_eq!(arguments[0], "-p");
            assert_eq!(arguments[1], "ask");
            assert!(arguments.contains(&"--verbose".to_string()));
            assert!(arguments.contains(&"stream-json".to_string()));
            // The hook is what makes the parent delegate, and `--safe-mode`
            // would turn it off; `--setting-sources ""` is what keeps the
            // wiki's own configuration — its CLAUDE.md included — out.
            assert!(
                !arguments.contains(&"--safe-mode".to_string()),
                "{arguments:?}"
            );
            let at = arguments
                .iter()
                .position(|flag| flag == "--setting-sources")
                .expect("the sources flag");
            assert_eq!(arguments[at + 1], "");
            let at = arguments
                .iter()
                .position(|flag| flag == "--settings")
                .expect("the settings flag");
            assert_eq!(arguments[at + 1], settings.display().to_string());
            assert_eq!(
                arguments.iter().filter(|flag| *flag == "Task").count(),
                2,
                "--tools and --allowedTools both name them: {arguments:?}"
            );
        }
    }

    /// The agent must measure the wiki, not the machine it is run on. Without
    /// `--safe-mode` — which the hook rules out — the operator's own Claude
    /// Code installation walks in: on the machine this was written on, 18
    /// skills, 53 commands, 4 connected MCP servers and 58 extra tools, which
    /// put the parent's first turn at 57k tokens against 12.6k without them.
    /// That is the operator's context being measured as the wiki's.
    #[test]
    fn the_operators_own_installation_is_shut_out() {
        let arguments = claude_arguments("ask", "Read", None, Path::new("/run/settings.json"));
        for flag in ["--strict-mcp-config", "--disable-slash-commands"] {
            assert!(
                arguments.contains(&flag.to_string()),
                "{flag}: {arguments:?}"
            );
        }
    }

    /// The built-in Explore agent takes a thoroughness from whoever dispatches
    /// it, and how hard it looks is most of what a run measures. Left unsaid it
    /// is the subagent's guess, and two runs of the same query are not the same
    /// experiment. The prompt says it, so the report can state it.
    #[test]
    fn the_prompt_dispatches_the_explore_agent_at_a_stated_thoroughness() {
        assert!(EXPLORE_PROMPT.contains(THOROUGHNESS), "{EXPLORE_PROMPT}");
        assert!(EXPLORE_PROMPT.contains("thoroughness"), "{EXPLORE_PROMPT}");
        // It is the word the subagent is given, not one the report invented.
        assert_eq!(THOROUGHNESS, "medium");
    }
    /// The runs that are owed are priced by what runs of the same condition
    /// have cost before: the same revision first, because how big the wiki is is
    /// most of what a run costs, and a condition with nothing to price it by is
    /// unknown rather than free.
    #[test]
    fn the_estimate_prices_what_is_owed_by_what_has_been_bought() {
        let out = PathBuf::from("/somewhere/run");
        let bought = |condition: &str, wiki: &str, repeat: usize, ok: bool, cost: Option<f64>| {
            let entry = Entry::buying(
                Key {
                    wiki: wiki.to_string(),
                    query: "one".to_string(),
                    condition: condition.to_string(),
                    repeat,
                },
                None,
                &out,
            );
            (entry.id(), entry.measured(ok, cost))
        };
        let recorded: BTreeMap<Id, Entry> = BTreeMap::from([
            // Two runs of this revision, one of another, and one that failed
            // and so was never priced.
            bought("explore", WIKI, 0, true, Some(0.30)),
            bought("explore", WIKI, 1, true, Some(0.10)),
            bought("explore", "sha256:1234abcd", 0, true, Some(9.0)),
            bought("s1m-agent", WIKI, 0, false, None),
        ]);

        let jobs = vec![
            Job {
                query: 0,
                condition: "explore".to_string(),
                repeat: 0,
            },
            Job {
                query: 1,
                condition: "explore".to_string(),
                repeat: 0,
            },
            Job {
                query: 0,
                condition: "s1m-agent".to_string(),
                repeat: 0,
            },
        ];
        let priced = estimate(&jobs, WIKI, &recorded);

        let explore = &priced.lines[0];
        assert_eq!(
            (explore.condition.as_str(), explore.jobs, explore.n),
            ("explore", 2, 2)
        );
        assert!(
            (explore.mean.expect("a mean") - 0.20).abs() < 1e-12,
            "{explore:?}"
        );
        assert!(!explore.elsewhere, "this revision has its own runs");
        let agent = &priced.lines[1];
        assert_eq!(agent.mean, None, "a failed run was not priced");
        assert_eq!(priced.unknown, 1);
        assert!((priced.total - 0.40).abs() < 1e-12, "two runs at 0.20");

        let table = priced.table();
        assert!(table.contains("explore"), "{table}");
        assert!(table.contains("unknown"), "{table}");
        assert!(table.contains("the total is a floor"), "{table}");
        assert!(priced.summary().starts_with("at least $0.40"), "{priced:?}");

        // A wiki that has just changed has no rows of its own, and is priced
        // from every revision there is — saying so.
        let elsewhere = estimate(&jobs, "sha256:5678abcd", &recorded);
        let explore = &elsewhere.lines[0];
        assert!(explore.elsewhere);
        assert_eq!(explore.n, 3);
        assert!(
            (explore.mean.expect("a mean") - (0.30 + 0.10 + 9.0) / 3.0).abs() < 1e-12,
            "{explore:?}"
        );
        assert!(
            elsewhere.table().contains("another wiki revision"),
            "{elsewhere:?}"
        );
    }

    /// A run bought for one directory is not bought again for another: one
    /// ledger for the machine is what makes a second `--out` a resume rather
    /// than a second bill.
    #[cfg(unix)]
    #[test]
    fn a_second_directory_does_not_buy_what_the_ledger_already_has() {
        let wiki = TempDir::new("ledger-wiki");
        wiki.write("index.md", "# Index\n");
        let gold = TempDir::new("ledger-gold");
        gold.write(
            "gold.json",
            r#"{"queries": [{"id": "one", "query": "q", "wanted": ["index.md"]}]}"#,
        );
        let fake = TempDir::new("ledger-s1m");
        let calls = fake.path().join("calls");
        let ledger = TempDir::new("ledger-file");
        let cache = TempDir::new("ledger-cache");
        let first = TempDir::new("ledger-out-one");
        let second = TempDir::new("ledger-out-two");

        for out in [&first, &second] {
            let options = Options {
                gold: gold.path().join("gold.json"),
                cache_dir: Some(cache.path().to_path_buf()),
                ledger: Some(ledger.path().join("ledger.jsonl")),
                s1m: fake_s1m(&fake, Some(&calls)),
                cold_repeats: 0,
                ..options(&wiki, out)
            };
            run(&options).expect("a pass");
        }

        // One run was bought, and the second directory bought nothing.
        let bought: Vec<String> = fs::read_to_string(&calls)
            .expect("the calls")
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(bought.len(), 1, "{bought:?}");
        assert!(read_rows(second.path()).expect("rows").is_empty());

        // And the record is complete: what the first directory bought is what
        // it measured, and its aggregates say both.
        let aggregates: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(first.path().join("aggregates.json")).expect("aggregates"),
        )
        .expect("aggregates");
        assert_eq!(aggregates["bought"], 1);
        assert_eq!(aggregates["method"]["conditions"][0], "s1m");
        assert_eq!(aggregates["method"]["repeats"], 1);
        assert_eq!(aggregates["queries"], 1);
        // The revision every row of it was measured against, in the aggregates.
        let revision = wiki::revision(wiki.path()).expect("a revision");
        assert_eq!(aggregates["wiki"][0], revision.as_str());
        assert_eq!(
            read_rows(first.path()).expect("rows")[0].wiki,
            revision,
            "the row carries it too"
        );
    }

    /// What a directory bought and what it measured are two counts of the same
    /// ledger: a purchase with no row is money spent with nothing to show for
    /// it, and the aggregates carry both so a report can say so.
    #[cfg(unix)]
    #[test]
    fn the_aggregates_count_what_was_bought_as_well_as_what_was_measured() {
        let wiki = TempDir::new("bought-wiki");
        wiki.write("index.md", "# Index\n");
        let out = TempDir::new("bought-out");
        let ledger_file = TempDir::new("bought-ledger");
        let ledger = Ledger::at(ledger_file.path().join("ledger.jsonl"));
        let options = Options {
            ..options(&wiki, &out)
        };
        let key = Key {
            wiki: WIKI.to_string(),
            query: "one".to_string(),
            condition: "s1m".to_string(),
            repeat: 0,
        };
        // One run that was measured, and one that was bought and killed before
        // it wrote a row.
        let measured = Entry::buying(key.clone(), None, out.path());
        ledger.record(&measured).expect("a purchase");
        ledger
            .record(&measured.measured(true, Some(0.002)))
            .expect("a measurement");
        ledger
            .record(&Entry::buying(
                Key {
                    condition: "s1m-cold".to_string(),
                    ..key
                },
                None,
                out.path(),
            ))
            .expect("a purchase that wrote no row");

        let rows = vec![row("one", "s1m", 0, true)];
        write_aggregates(&options, &rows, &ledger, out.path()).expect("aggregates");

        let aggregates: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(out.path().join("aggregates.json")).expect("aggregates"),
        )
        .expect("aggregates");
        assert_eq!(
            aggregates["bought"], 2,
            "both purchases are for this directory"
        );
        assert_eq!(aggregates["conditions"]["s1m"]["runs"], 1);
        assert_eq!(aggregates["method"]["repeats"], 1);
    }
    /// A pass whose estimate is above the limit stops before it buys anything,
    /// and `--yes` is what buys it anyway.
    #[cfg(unix)]
    #[test]
    fn a_pass_over_the_limit_stops_unless_it_is_told_to_go() {
        let wiki = TempDir::new("limit-wiki");
        wiki.write("index.md", "# Index\n");
        let gold = TempDir::new("limit-gold");
        gold.write(
            "gold.json",
            r#"{"queries": [{"id": "one", "query": "q", "wanted": ["index.md"]}]}"#,
        );
        let fake = TempDir::new("limit-s1m");
        let calls = fake.path().join("calls");
        let ledger = TempDir::new("limit-ledger");
        let cache = TempDir::new("limit-cache");
        let out = TempDir::new("limit-out");
        let options = Options {
            gold: gold.path().join("gold.json"),
            cache_dir: Some(cache.path().to_path_buf()),
            ledger: Some(ledger.path().join("ledger.jsonl")),
            s1m: fake_s1m(&fake, Some(&calls)),
            cold_repeats: 0,
            ..options(&wiki, &out)
        };
        // What the ledger says one run of this condition costs against this
        // wiki: more than a pass may spend without being told to.
        let elsewhere = Entry::buying(
            Key {
                wiki: wiki::revision(wiki.path()).expect("a revision"),
                query: "elsewhere".to_string(),
                condition: "s1m".to_string(),
                repeat: 0,
            },
            None,
            out.path(),
        );
        Ledger::at(ledger.path().join("ledger.jsonl"))
            .record(&elsewhere.measured(true, Some(SPEND_LIMIT_USD + 1.0)))
            .expect("a purchase");

        let error = run(&options).expect_err("a refusal");
        assert!(error.contains("--yes"), "{error}");
        assert!(!calls.exists(), "nothing may be bought by a refused pass");
        assert!(read_rows(out.path()).expect("rows").is_empty());

        // And saying yes is what buys it.
        let options = Options {
            yes: true,
            ..options
        };
        run(&options).expect("a pass");
        assert_eq!(
            fs::read_to_string(&calls)
                .expect("the call")
                .lines()
                .count(),
            1
        );
    }
}
