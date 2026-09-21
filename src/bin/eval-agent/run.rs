//! The `run` command: every query, under every condition, as many times as
//! asked, with one JSONL row per run.
//!
//! Runs cost money and take minutes, so the pass is resumable: a row already in
//! the JSONL is a run that has happened, and it is not run again. The rows are
//! flushed one at a time for the same reason.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::explore::{self, Usage};
use crate::gold::{Gold, Query};
use crate::reading;
use crate::report::Method;
use crate::row::Row;

/// The file every run appends to, under `--out`.
pub const ROWS: &str = "runs.jsonl";
/// Where each run's own output is kept, under `--out`.
pub const RAW: &str = "raw";
/// What the cold s1m runs buy their judgments into, under `--out`.
pub const COLD_CACHE: &str = "cold-cache";

/// The parent agent's prompt. The query and the entry pages are substituted
/// in; nothing else about the wiki is.
pub const EXPLORE_PROMPT: &str = "\
Use the Explore agent to answer this query from the wiki in the current \
directory, starting at these pages: <entry>
The query: <query>
Reply with the answer, and then a JSON array of the relative paths of the files \
the Explore agent relied on.";

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
    pub model: String,
    pub s1m: PathBuf,
    pub claude: PathBuf,
    pub transcripts: Option<PathBuf>,
    pub cold_repeats: usize,
    /// Measure only these query ids; every one when empty.
    pub queries: Vec<String>,
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
    done: &BTreeSet<(String, String, usize)>,
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
                    if done.contains(&(query.id.clone(), condition.clone(), repeat)) {
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
    let options = &Options {
        wiki: std::fs::canonicalize(&options.wiki)
            .map_err(|error| format!("{}: {error}", options.wiki.display()))?,
        ..options.clone()
    };
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
    fs::create_dir_all(options.out.join(RAW))
        .map_err(|error| format!("{}: {error}", options.out.display()))?;

    let mut rows = read_rows(&options.out)?;
    let done: BTreeSet<(String, String, usize)> = rows.iter().map(Row::key).collect();
    let jobs = pending(
        &gold,
        &options.conditions,
        options.repeats,
        options.cold_repeats,
        &done,
    );
    eprintln!(
        "eval-agent: {} runs owed, {} already measured",
        jobs.len(),
        rows.len()
    );

    for (at, job) in jobs.iter().enumerate() {
        let query = &gold.queries[job.query];
        // A prerequisite may have made this run already.
        let key = (query.id.clone(), job.condition.clone(), job.repeat);
        if rows.iter().any(|row| row.key() == key) {
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
            let row = measure(options, query, &needed, &rows);
            append(&options.out, &row)?;
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
        let row = measure(options, query, job, &rows);
        append(&options.out, &row)?;
        rows.push(row);
    }

    write_aggregates(options, &rows)
}

/// Reduces every row and writes `aggregates.json`: numbers, ids and
/// categories, and nothing else.
pub fn write_aggregates(options: &Options, rows: &[Row]) -> Result<(), String> {
    let mut aggregates = crate::aggregate::aggregate(rows);
    aggregates.method = Some(method(options));
    let path = options.out.join("aggregates.json");
    let text = serde_json::to_string_pretty(&aggregates).map_err(|error| error.to_string())?;
    fs::write(&path, text).map_err(|error| format!("{}: {error}", path.display()))?;
    eprintln!("eval-agent: aggregates written to {}", path.display());
    Ok(())
}

/// The agent's flags, as the report prints them.
pub fn method_flags() -> Vec<String> {
    vec![
        "-p".to_string(),
        "--output-format stream-json".to_string(),
        "--verbose".to_string(),
        "--safe-mode".to_string(),
        format!("--tools {EXPLORE_TOOLS}"),
        format!("--allowedTools {EXPLORE_TOOLS}"),
        "--permission-prompts none".to_string(),
    ]
}

/// How the runs were made, for the report's method table. Every value here is
/// a flag or a constant of this harness: no path, no query, no page.
pub fn method(options: &Options) -> Method {
    Method {
        repeats: options.repeats,
        conditions: options.conditions.clone(),
        claude_model: options.model.clone(),
        claude_flags: method_flags(),
        s1m_flags: vec!["--format json".to_string(), "--root .".to_string()],
        explore_prompt: EXPLORE_PROMPT.to_string(),
        s1m_agent_prompt: S1M_AGENT_PROMPT.to_string(),
        chars_per_token: reading::CHARS_PER_TOKEN,
    }
}

/// One run, as a row whatever happens: a run that failed is a row too, so that
/// a resumed pass does not try it forever.
fn measure(options: &Options, query: &Query, job: &Job, rows: &[Row]) -> Row {
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
    match outcome {
        Ok((metrics, detail)) => Row {
            query_id: query.id.clone(),
            category: query.category().to_string(),
            condition: job.condition.clone(),
            repeat: job.repeat,
            ok: true,
            metrics,
            detail: Some(detail),
        },
        // A run that failed is a row with no metrics: nothing is averaged from
        // it, and what went wrong goes in the raw half with everything else
        // that may name a file — an API error quotes the request that caused
        // it, and a process error quotes its own stderr.
        Err(error) => {
            eprintln!("eval-agent: {} {}: {error}", query.id, job.condition);
            Row {
                query_id: query.id.clone(),
                category: query.category().to_string(),
                condition: job.condition.clone(),
                repeat: job.repeat,
                ok: false,
                metrics: BTreeMap::new(),
                detail: Some(serde_json::json!({ "error": error })),
            }
        }
    }
}

type Measured = (BTreeMap<String, f64>, serde_json::Value);

/// The Explore condition: a parent agent that hands the query to the built-in
/// Explore subagent, measured on the subagent's own tokens.
fn explore_condition(options: &Options, query: &Query, job: &Job) -> Result<Measured, String> {
    let entries = query.entries(&options.entry);
    let prompt = EXPLORE_PROMPT
        .replace("<entry>", &entries.join(", "))
        .replace("<query>", &query.query);
    let agent = agent_run(options, query, job, &prompt, EXPLORE_TOOLS)?;

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

    Ok((metrics, agent.detail))
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

    Ok((
        metrics,
        serde_json::json!({
            "query": query.query,
            "entry": entries,
            "command": arguments,
            "cold": cold,
            "threshold": threshold,
            "files": list.files,
            "wanted": query.wanted,
            "read_chars": list.read_chars,
        }),
    ))
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
    let agent = agent_run(options, query, job, &prompt, S1M_AGENT_TOOLS)?;

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
    Ok((metrics, agent.detail))
}

/// What one `claude -p` run reported, whichever condition asked for it.
struct AgentRun {
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
    detail: serde_json::Value,
}

/// Runs the agent and reads back what it cost.
fn agent_run(
    options: &Options,
    query: &Query,
    job: &Job,
    prompt: &str,
    tools: &str,
) -> Result<AgentRun, String> {
    let stdout = raw_path(options, query, job, "stream.jsonl");
    let stderr = raw_path(options, query, job, "stderr");
    let arguments = vec![
        "-p".to_string(),
        prompt.to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--model".to_string(),
        options.model.clone(),
        "--safe-mode".to_string(),
        "--tools".to_string(),
        tools.to_string(),
        "--allowedTools".to_string(),
        tools.to_string(),
        "--permission-prompts".to_string(),
        "none".to_string(),
    ];

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
    let (agent, files_read, turns, tool_uses, model) = if transcripts.is_empty() {
        // Nothing was spawned: the agent that ran is the parent, and the
        // stream carries what it opened.
        (
            summary.session_total,
            summary.parent_files_read.clone(),
            0,
            summary.parent_tool_uses,
            summary.model.clone(),
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
        detail: serde_json::json!({
            "query": query.query,
            "prompt": prompt,
            "command": arguments,
            "answer": summary.answer,
            "files_relied": files_relied,
            "files_read": files_read,
            "wanted": query.wanted,
            "relied_parsed": relied_parsed,
            "model": model.unwrap_or_else(|| options.model.clone()),
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

/// The subagent this harness measures: Claude Code's built-in read-only
/// explorer.
pub const EXPLORE_AGENT: &str = "Explore";

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

    /// A pass costs money, so a run already in the JSONL is never made again,
    /// and the cold s1m run is made only as often as it is asked for.
    #[test]
    fn a_resumed_pass_owes_only_what_is_missing() {
        let conditions = vec!["explore".to_string(), "s1m".to_string()];
        let all = pending(&gold(), &conditions, 2, 1, &BTreeSet::new());
        // Two queries, two repeats, two conditions, plus one cold s1m run per
        // query on the first repeat.
        assert_eq!(all.len(), 10);
        assert_eq!(
            all.iter().filter(|job| job.condition == "s1m-cold").count(),
            2
        );
        // One query at a time, so a pass stopped early has whole queries.
        assert!(all[..5].iter().all(|job| job.query == 0));

        let done = BTreeSet::from([
            ("one".to_string(), "explore".to_string(), 0),
            ("one".to_string(), "s1m-cold".to_string(), 0),
        ]);
        let owed = pending(&gold(), &conditions, 2, 1, &done);
        assert_eq!(owed.len(), 8);
        assert!(!owed.contains(&Job {
            query: 0,
            condition: "explore".to_string(),
            repeat: 0
        }));
    }

    /// The rows are the record of what has been measured: they are read back
    /// exactly as they were written.
    #[test]
    fn rows_are_written_and_read_back() {
        let out = TempDir::new("rows");
        let row = Row {
            query_id: "one".to_string(),
            category: "how-to".to_string(),
            condition: "explore".to_string(),
            repeat: 0,
            ok: true,
            metrics: BTreeMap::from([("recall".to_string(), 1.0)]),
            detail: Some(serde_json::json!({"query": "q"})),
        };
        assert!(read_rows(out.path()).expect("no rows yet").is_empty());
        append(out.path(), &row).expect("a row");
        append(out.path(), &row).expect("another row");
        assert_eq!(
            read_rows(out.path()).expect("two rows"),
            vec![row.clone(), row]
        );
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
            model: "sonnet".to_string(),
            s1m: binary,
            claude: PathBuf::from("claude"),
            transcripts: None,
            cold_repeats: 0,
            queries: Vec::new(),
            timeout: Duration::from_secs(30),
        };
        fs::create_dir_all(out.path().join(RAW)).expect("a raw directory");
        let query = &gold().queries[0];
        let job = Job {
            query: 0,
            condition: "s1m".to_string(),
            repeat: 0,
        };

        let row = measure(&options, query, &job, &[]);
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
        let done: BTreeSet<(String, String, usize)> = read_rows(out.path())
            .expect("one row")
            .iter()
            .map(Row::key)
            .collect();
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
            entry: vec!["index.md".to_string()],
            model: "sonnet".to_string(),
            s1m: PathBuf::from("s1m"),
            claude: PathBuf::from("claude"),
            transcripts: None,
            cold_repeats: 1,
            queries: Vec::new(),
            timeout: Duration::from_secs(30),
        }
    }

    /// A shell script at `<dir>/s1m` that answers like s1m and buys one
    /// judgment into whatever cache it was pointed at.
    #[cfg(unix)]
    fn fake_s1m(dir: &TempDir) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        dir.write(
            "s1m",
            "#!/bin/sh\n\
             mkdir -p \"$S1M_CACHE_DIR/judgments\"\n\
             printf '{\"format\":3,\"judgment\":{},\"detail\":{\"input_tokens\":1000,\"output_tokens\":10}}' \
             > \"$S1M_CACHE_DIR/judgments/bought.json\"\n\
             printf '{\"query\":\"q\",\"mode\":\"useful-for\",\"visited\":1,\"calls\":1,\"results\":[{\"path\":\"index.md\",\"relevance\":0.9,\"scent\":null,\"via\":[],\"links\":[],\"sections\":[{\"heading\":null,\"lines\":[1,1],\"score\":0.9}]}]}'\n",
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
            s1m: fake_s1m(&fake),
            ..options(&wiki, &out)
        };
        fs::create_dir_all(out.path().join(RAW)).expect("a raw directory");

        let query = &gold().queries[0];
        let job = Job {
            query: 0,
            condition: "s1m-cold".to_string(),
            repeat: 0,
        };
        let (metrics, _) =
            s1m_condition(&options, query, &job, true, None).expect("a cold measurement");

        // What it bought is what it is charged for: 1000 input tokens.
        assert_eq!(metrics["jev_input_tokens"], 1000.0);
        assert!(
            (metrics["cost_usd"] - 1000.0 / 1_000_000.0 * s1m::jev::PRICE_PER_MTOK).abs() < 1e-12
        );

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
            query_id: "one".to_string(),
            category: "how-to".to_string(),
            condition: "s1m".to_string(),
            repeat,
            ok: true,
            metrics: BTreeMap::from([("cost_usd".to_string(), cost)]),
            detail: Some(serde_json::json!({ "files": [file] })),
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
        let row = measure(&options, query, &job, &[]);
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
        let row = measure(&options, query, &job, &[]);
        assert!(!row.ok);
        let reason = row.detail.expect("the raw half")["error"]
            .as_str()
            .expect("a reason")
            .to_string();
        assert!(reason.contains(EXPLORE_AGENT), "{reason}");
    }
}
