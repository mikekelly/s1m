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

/// The parent agent's prompt. The query and the entry page are substituted in;
/// nothing else about the wiki is.
pub const EXPLORE_PROMPT: &str = "\
Use the Explore agent to answer this query from the wiki in the current \
directory, starting at its entry page <entry>: <query>
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
    pub entry: String,
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
        if !crate::row::CONDITIONS.contains(&condition.as_str()) {
            return Err(format!(
                "no condition named {condition:?}: it is one of {}",
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

/// How the runs were made, for the report's method table. Every value here is
/// a flag or a constant of this harness: no path, no query, no page.
pub fn method(options: &Options) -> Method {
    Method {
        repeats: options.repeats,
        conditions: options.conditions.clone(),
        claude_model: options.model.clone(),
        claude_flags: vec![
            "-p".to_string(),
            "--output-format stream-json".to_string(),
            "--verbose".to_string(),
            "--safe-mode".to_string(),
            format!("--tools {EXPLORE_TOOLS}"),
            format!("--allowedTools {EXPLORE_TOOLS}"),
            "--permission-prompts none".to_string(),
        ],
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
        "s1m" => s1m_condition(options, query, job, false),
        "s1m-cold" => s1m_condition(options, query, job, true),
        "s1m-agent" => s1m_agent_condition(options, query, job, rows),
        other => Err(format!("no condition named {other:?}")),
    };
    match outcome {
        Ok((metrics, detail)) => Row {
            query_id: query.id.clone(),
            category: query.category().to_string(),
            condition: job.condition.clone(),
            repeat: job.repeat,
            ok: true,
            error: None,
            metrics,
            detail: Some(detail),
        },
        Err(error) => {
            eprintln!("eval-agent: {} {}: {error}", query.id, job.condition);
            Row {
                query_id: query.id.clone(),
                category: query.category().to_string(),
                condition: job.condition.clone(),
                repeat: job.repeat,
                ok: false,
                error: Some(error),
                metrics: BTreeMap::new(),
                detail: None,
            }
        }
    }
}

type Measured = (BTreeMap<String, f64>, serde_json::Value);

/// The Explore condition: a parent agent that hands the query to the built-in
/// Explore subagent, measured on the subagent's own tokens.
fn explore_condition(options: &Options, query: &Query, job: &Job) -> Result<Measured, String> {
    let entry = query.entry(&options.entry);
    let prompt = EXPLORE_PROMPT
        .replace("<entry>", entry)
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

/// The s1m condition: the binary, at its defaults, from the wiki directory.
fn s1m_condition(
    options: &Options,
    query: &Query,
    job: &Job,
    cold: bool,
) -> Result<Measured, String> {
    let entry = query.entry(&options.entry);
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
        options
            .cache_dir
            .clone()
            .unwrap_or_else(|| options.out.join("cache"))
    };
    fs::create_dir_all(&cache).map_err(|error| format!("{}: {error}", cache.display()))?;

    let stdout = raw_path(options, query, job, "json");
    let stderr = raw_path(options, query, job, "stderr");
    let mut arguments = vec![
        query.query.clone(),
        entry.to_string(),
        "--root".to_string(),
        ".".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
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
    if cold {
        let spent = jev_spent(&cache);
        metrics.insert("jev_input_tokens".to_string(), spent.0 as f64);
        metrics.insert("jev_output_tokens".to_string(), spent.1 as f64);
        metrics.insert("jev_cost_usd".to_string(), spent.2);
        metrics.insert("cost_usd".to_string(), spent.2);
        // A cold run is the only one that leaves the shared cache able to serve
        // the warm run that follows it.
        if let Some(shared) = &options.cache_dir {
            merge_cache(&cache, shared);
        }
    } else {
        metrics.insert("cost_usd".to_string(), 0.0);
    }

    Ok((
        metrics,
        serde_json::json!({
            "query": query.query,
            "entry": entry,
            "command": arguments,
            "cold": cold,
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
    let files = rows
        .iter()
        .rev()
        .find(|row| row.query_id == query.id && row.condition == "s1m" && row.ok)
        .and_then(|row| row.detail.as_ref())
        .and_then(|detail| detail["files"].as_array().cloned())
        .ok_or_else(|| {
            "no s1m reading list for this query yet: run the `s1m` condition first".to_string()
        })?;
    let list = files
        .iter()
        .filter_map(|file| file.as_str())
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
    metrics.insert("wall_ms".to_string(), agent.wall_ms as f64);
    metrics.insert("cost_usd".to_string(), agent.cost_usd);
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
    files_read: Vec<PathBuf>,
    /// The subagent's own tokens, or the parent's when nothing was spawned.
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

    // The subagent's own transcript is the only place its turns carry their
    // final token counts; the stream's copies are the counts so far.
    let task = summary.tasks.first().cloned();
    let transcript = task.as_ref().and_then(|task| {
        let path = transcript_path(options, summary.session_id.as_deref()?, &task.agent_id)?;
        let text = fs::read_to_string(&path).ok()?;
        Some((path, explore::summarise_transcript(&text, &options.wiki)))
    });

    let files_relied = explore::files_relied_on(&summary.answer, &options.wiki);
    let (agent, files_read, turns, tool_uses, model) = match &transcript {
        Some((_, subagent)) => (
            subagent.usage,
            subagent.files_read.clone(),
            subagent.turns,
            subagent.tool_uses,
            subagent.model.clone(),
        ),
        // Nothing was spawned, or its transcript could not be found: the agent
        // that ran is the parent, and the stream carries what it opened.
        None => (
            summary.session_total,
            summary.parent_files_read.clone(),
            0,
            summary.parent_tool_uses,
            summary.model.clone(),
        ),
    };

    Ok(AgentRun {
        files_relied: files_relied.clone(),
        files_read: files_read.clone(),
        agent,
        parent: summary.parent_total,
        session: summary.session_total,
        turns,
        tool_uses,
        cost_usd: summary.cost_usd,
        wall_ms: wall.as_millis(),
        agent_wall_ms: task.as_ref().and_then(|task| task.duration_ms),
        detail: serde_json::json!({
            "query": query.query,
            "prompt": prompt,
            "command": arguments,
            "answer": summary.answer,
            "files_relied": files_relied,
            "files_read": files_read,
            "wanted": query.wanted,
            "model": model.unwrap_or_else(|| options.model.clone()),
            "session_id": summary.session_id,
            "task": task,
            "subagent_usage_source": if transcript.is_some() { "transcript" } else { "stream" },
            "transcript": transcript.as_ref().map(|(path, _)| path.display().to_string()),
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

/// What the judgments in one cache directory cost: input tokens, output tokens
/// and the input at the list price.
fn jev_spent(cache: &Path) -> (u64, u64, f64) {
    let mut input = 0;
    let mut output = 0;
    let Ok(entries) = fs::read_dir(cache.join("judgments")) else {
        return (0, 0, 0.0);
    };
    for entry in entries.flatten() {
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
            error: None,
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
}
