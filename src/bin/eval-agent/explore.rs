//! What an agent run cost and what it read, from what Claude Code reports.
//!
//! The parent agent is asked to hand the query to the built-in Explore
//! subagent, so the measurement that matters is the subagent's own: its tokens,
//! its wall time, its model, and the files it opened. Claude Code reports that
//! in two places, and this module reads both. The stream (`--output-format
//! stream-json --verbose`) carries the task events: which subagent was spawned,
//! under which agent id, and what the whole run cost. The subagent's transcript
//! carries its messages with their final usage, which the stream does not: the
//! stream's per-message usage is the usage *so far*, so its output token counts
//! are partial.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The tokens one agent was billed for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_create: u64,
}

impl Usage {
    /// Everything the agent was billed for: what it read, cached or not, plus
    /// what it wrote.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_create
    }

    /// What the agent read: the billed context, cache included.
    pub fn read(&self) -> u64 {
        self.input + self.cache_read + self.cache_create
    }

    pub fn add(&mut self, other: Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_create += other.cache_create;
    }
}

/// The relative paths an agent named as the ones it relied on: the last JSON
/// array of strings in its answer, fenced or not, and `None` when it wrote no
/// array at all.
///
/// An answer with no list is not an answer with an empty one. An agent that was
/// asked for a list and did not write one has not said it relied on nothing —
/// it has failed to answer the question that is being scored — and a run scored
/// zero for that reads as a bad method rather than a lost measurement.
///
/// An agent that was asked for a final JSON array sometimes writes one and then
/// says something after it, and sometimes wraps it in a code fence; and it
/// spells a path the way it opened it, which may be absolute. The last array
/// wins, absolute paths under the wiki come back relative to it, and anything
/// that is not a path under the wiki is kept as written — a file named outside
/// the wiki is a fact about the answer, not something to hide.
pub fn files_relied_on(answer: &str, wiki: &Path) -> Option<Vec<PathBuf>> {
    let bytes = answer.as_bytes();
    for (start, _) in answer.char_indices().rev().filter(|(_, c)| *c == '[') {
        // The shortest slice from this bracket that parses as an array of
        // strings: a longer one would swallow a later array.
        let Some(end) = closing_bracket(bytes, start) else {
            continue;
        };
        let Ok(values) = serde_json::from_str::<Vec<String>>(&answer[start..=end]) else {
            continue;
        };
        return Some(values.iter().map(|path| relative(path, wiki)).collect());
    }
    None
}

/// The bracket that closes the one at `start`, ignoring anything inside a JSON
/// string so that a bracket in a path does not end the array.
fn closing_bracket(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (at, byte) in bytes.iter().enumerate().skip(start) {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// A path as the wiki spells it: relative to the root, with any `./` dropped.
/// A path that is not under the wiki is left as it was written.
pub fn relative(path: &str, wiki: &Path) -> PathBuf {
    let path = Path::new(path);
    let path = path.strip_prefix(wiki).unwrap_or(path);
    let path = path.strip_prefix(".").unwrap_or(path);
    path.to_path_buf()
}

/// What one `claude -p --output-format stream-json --verbose` run reported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamSummary {
    /// The session the run wrote its transcripts under.
    pub session_id: Option<String>,
    /// The last result's text: an agent that hands off to a backgrounded
    /// subagent answers twice, and only the last answer has the findings.
    pub answer: String,
    pub is_error: bool,
    /// What the whole run cost, as the CLI priced it: parent and subagent.
    pub cost_usd: f64,
    /// Every token the run was billed for, over every model and agent.
    pub session_total: Usage,
    /// The parent agent's own tokens: the result rows are its turns.
    pub parent_total: Usage,
    /// One per subagent the parent spawned, in the order they started.
    pub tasks: Vec<Task>,
    /// The model that answered, as the run reported it.
    pub model: Option<String>,
    /// The files the parent opened itself, relative to the wiki: what a
    /// condition with no subagent read.
    pub parent_files_read: Vec<PathBuf>,
    /// Tools the parent used itself, the subagent's not counted.
    pub parent_tool_uses: usize,
    /// The same, by tool name: which tools the parent reached for, and how
    /// often. A parent that is meant to hand the work to a subagent and goes
    /// looking itself says so here.
    pub parent_tools: BTreeMap<String, usize>,
}

/// One spawned subagent, as the stream's task events describe it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// The id its transcript is named after.
    pub agent_id: String,
    /// Which agent was asked for: `Explore` is the built-in one.
    pub kind: String,
    pub status: Option<String>,
    /// How long the subagent ran, as Claude Code timed it.
    pub duration_ms: Option<u64>,
    /// How many tools it used, as Claude Code counted them.
    pub tool_uses: Option<u64>,
}

/// Reads the stream: one JSON object a line, in the order they were printed.
pub fn summarise_stream(stream: &str, wiki: &Path) -> Result<StreamSummary, String> {
    let mut summary = StreamSummary::default();
    let mut results = 0;
    let mut tasks: Vec<Task> = Vec::new();
    let mut files = BTreeSet::new();

    for line in stream.lines().filter(|line| !line.trim().is_empty()) {
        let row: serde_json::Value =
            serde_json::from_str(line).map_err(|error| format!("the stream: {error}"))?;
        if summary.session_id.is_none()
            && let Some(id) = row["session_id"].as_str()
        {
            summary.session_id = Some(id.to_string());
        }
        // A subagent's messages are the subagent's; the parent's are the ones
        // no tool call is under.
        if row["type"].as_str() == Some("assistant") && row["parent_tool_use_id"].is_null() {
            if summary.model.is_none() {
                summary.model = string(&row["message"]["model"]);
            }
            for block in row["message"]["content"].as_array().into_iter().flatten() {
                if block["type"].as_str() != Some("tool_use") {
                    continue;
                }
                summary.parent_tool_uses += 1;
                if let Some(name) = block["name"].as_str() {
                    *summary.parent_tools.entry(name.to_string()).or_insert(0) += 1;
                }
                if block["name"].as_str() == Some("Read")
                    && let Some(path) = block["input"]["file_path"].as_str()
                {
                    files.insert(relative(path, wiki));
                }
            }
        }
        match (row["type"].as_str(), row["subtype"].as_str()) {
            (Some("system"), Some("task_started")) => tasks.push(Task {
                agent_id: string(&row["task_id"]).unwrap_or_default(),
                kind: string(&row["subagent_type"]).unwrap_or_default(),
                ..Task::default()
            }),
            (Some("system"), Some("task_notification")) => {
                let id = string(&row["task_id"]).unwrap_or_default();
                if let Some(task) = tasks.iter_mut().find(|task| task.agent_id == id) {
                    task.status = string(&row["status"]);
                    task.duration_ms = row["usage"]["duration_ms"].as_u64();
                    task.tool_uses = row["usage"]["tool_uses"].as_u64();
                }
            }
            (Some("result"), _) => {
                results += 1;
                summary.answer = string(&row["result"]).unwrap_or_default();
                summary.is_error = row["is_error"].as_bool().unwrap_or(false);
                summary.cost_usd = row["total_cost_usd"].as_f64().unwrap_or_default();
                summary.parent_total.add(usage(&row["usage"]));
                // Every result carries the same session totals; the last one is
                // as good as the first, and it is the only one after a handoff.
                summary.session_total = model_usage(&row["modelUsage"]);
            }
            _ => {}
        }
    }

    if results == 0 {
        return Err("the run printed no result".to_string());
    }
    summary.tasks = tasks;
    summary.parent_files_read = files.into_iter().collect();
    Ok(summary)
}

fn string(value: &serde_json::Value) -> Option<String> {
    value.as_str().map(str::to_string)
}

/// One message's usage, as the API spells its fields.
fn usage(value: &serde_json::Value) -> Usage {
    Usage {
        input: value["input_tokens"].as_u64().unwrap_or_default(),
        output: value["output_tokens"].as_u64().unwrap_or_default(),
        cache_read: value["cache_read_input_tokens"]
            .as_u64()
            .unwrap_or_default(),
        cache_create: value["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or_default(),
    }
}

/// The run's totals, as `modelUsage` spells them: one entry per model, and the
/// whole session in each — the parent's turns and every subagent's.
fn model_usage(value: &serde_json::Value) -> Usage {
    let mut total = Usage::default();
    let Some(models) = value.as_object() else {
        return total;
    };
    for model in models.values() {
        total.add(Usage {
            input: model["inputTokens"].as_u64().unwrap_or_default(),
            output: model["outputTokens"].as_u64().unwrap_or_default(),
            cache_read: model["cacheReadInputTokens"].as_u64().unwrap_or_default(),
            cache_create: model["cacheCreationInputTokens"]
                .as_u64()
                .unwrap_or_default(),
        });
    }
    total
}

/// What one subagent's transcript says it read and was billed for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSummary {
    pub model: Option<String>,
    pub usage: Usage,
    /// Assistant messages: the subagent's turns.
    pub turns: usize,
    pub tool_uses: usize,
    /// The files it opened with `Read`, relative to the wiki, deduplicated.
    pub files_read: Vec<PathBuf>,
}

/// Reads a subagent transcript.
///
/// A message is streamed in pieces and written once per piece, so the same
/// message id appears more than once and only the last piece carries the
/// message's final output count. The piece with the most output tokens is that
/// one, so summing per message id over the largest piece is the billed usage.
pub fn summarise_transcript(transcript: &str, wiki: &Path) -> TranscriptSummary {
    // The piece with the most output tokens wins, which is the final one: the
    // pieces are not assumed to arrive in order, only to grow.
    let mut turns: std::collections::BTreeMap<String, Usage> = std::collections::BTreeMap::new();
    let mut model = None;
    let mut tool_uses = 0;
    let mut files = BTreeSet::new();

    for line in transcript.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if row["type"].as_str() != Some("assistant") {
            continue;
        }
        let message = &row["message"];
        let Some(id) = message["id"].as_str() else {
            continue;
        };
        if model.is_none() {
            model = string(&message["model"]);
        }
        let seen = usage(&message["usage"]);
        turns
            .entry(id.to_string())
            .and_modify(|billed| {
                if seen.output > billed.output {
                    *billed = seen;
                }
            })
            .or_insert(seen);

        for block in message["content"].as_array().into_iter().flatten() {
            if block["type"].as_str() != Some("tool_use") {
                continue;
            }
            tool_uses += 1;
            if block["name"].as_str() == Some("Read")
                && let Some(path) = block["input"]["file_path"].as_str()
            {
                files.insert(relative(path, wiki));
            }
        }
    }

    let mut usage = Usage::default();
    for turn in turns.values() {
        usage.add(*turn);
    }
    TranscriptSummary {
        model,
        usage,
        turns: turns.len(),
        tool_uses,
        files_read: files.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_last_json_array_in_an_answer() {
        let wiki = Path::new("/wiki");

        // A fenced array at the end, as an agent usually writes it.
        assert_eq!(
            files_relied_on(
                "The answer is 20.\n\n```json\n[\"index.md\", \"concepts/node.md\"]\n```\n",
                wiki
            ),
            Some(vec![
                PathBuf::from("index.md"),
                PathBuf::from("concepts/node.md")
            ])
        );

        // A bare array, with prose after it, and an earlier array that is not
        // the answer: the last one wins.
        assert_eq!(
            files_relied_on(
                "I considered [\"a.md\"] first.\nFiles: [\"b.md\", \"c.md\"]\nThat is all.",
                wiki
            ),
            Some(vec![PathBuf::from("b.md"), PathBuf::from("c.md")])
        );

        // Paths as the agent opened them: absolute under the wiki, or `./`.
        assert_eq!(
            files_relied_on("[\"/wiki/a.md\", \"./b.md\"]", wiki),
            Some(vec![PathBuf::from("a.md"), PathBuf::from("b.md")])
        );

        // An answer with no array at all did not answer, which is not the same
        // as answering that it relied on nothing.
        assert_eq!(files_relied_on("I could not find it.", wiki), None);
        // An array that is not of strings is not a file list.
        assert_eq!(files_relied_on("scores: [1, 2, 3]", wiki), None);
        // An empty array is an answer: it relied on nothing.
        assert_eq!(
            files_relied_on("Nothing helped: []", wiki),
            Some(Vec::new())
        );
    }

    /// Trimmed from a real run: a parent that spawned the Explore subagent in
    /// the background, answered once to say so, and answered again when the
    /// subagent reported back.
    const STREAM: &str = r#"
{"type":"system","subtype":"init","session_id":"S1","model":"claude-sonnet-5"}
{"type":"assistant","session_id":"S1","message":{"id":"m1","model":"claude-sonnet-5","usage":{"input_tokens":2,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":12610},"content":[{"type":"tool_use","id":"toolu_1","name":"Agent","input":{"subagent_type":"Explore"}}]}}
{"type":"system","subtype":"task_started","session_id":"S1","task_id":"agent7","tool_use_id":"toolu_1","subagent_type":"Explore","is_backgrounded":true}
{"type":"assistant","session_id":"S1","message":{"id":"m2","model":"claude-sonnet-5","usage":{"input_tokens":2,"output_tokens":9,"cache_read_input_tokens":0,"cache_creation_input_tokens":10},"content":[{"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"/wiki/notes/parent.md"}}]}}
{"type":"assistant","session_id":"S1","parent_tool_use_id":"toolu_1","subagent_type":"Explore","message":{"id":"s1","model":"claude-sonnet-5","usage":{"input_tokens":2,"output_tokens":3,"cache_read_input_tokens":0,"cache_creation_input_tokens":4990},"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/wiki/index.md"}}]}}
{"type":"system","subtype":"task_notification","session_id":"S1","task_id":"agent7","status":"completed","usage":{"total_tokens":8897,"tool_uses":2,"duration_ms":6334}}
{"type":"result","subtype":"success","session_id":"S1","is_error":false,"total_cost_usd":0.0963594,"duration_ms":4188,"usage":{"input_tokens":4,"output_tokens":350,"cache_read_input_tokens":12610,"cache_creation_input_tokens":13340},"modelUsage":{"claude-sonnet-5":{"inputTokens":12,"outputTokens":1058,"cacheReadInputTokens":37082,"cacheCreationInputTokens":22812,"costUSD":0.0963594}},"result":"I have launched the Explore agent."}
{"type":"result","subtype":"success","session_id":"S1","is_error":false,"total_cost_usd":0.0963594,"duration_ms":1633,"usage":{"input_tokens":2,"output_tokens":123,"cache_read_input_tokens":13340,"cache_creation_input_tokens":866},"modelUsage":{"claude-sonnet-5":{"inputTokens":12,"outputTokens":1058,"cacheReadInputTokens":37082,"cacheCreationInputTokens":22812,"costUSD":0.0963594}},"result":"Node >= 20.12.0\n\n[\"index.md\", \"concepts/node.md\"]"}
"#;

    #[test]
    fn a_stream_reports_the_last_answer_the_spawned_agents_and_both_totals() {
        let summary = summarise_stream(STREAM, Path::new("/wiki")).expect("a finished run");

        assert_eq!(summary.session_id.as_deref(), Some("S1"));
        assert!(
            summary.answer.starts_with("Node >= 20.12.0"),
            "{}",
            summary.answer
        );
        assert!(!summary.is_error);
        assert_eq!(summary.cost_usd, 0.0963594);

        // The whole session, as the CLI totalled it over every model.
        assert_eq!(
            summary.session_total,
            Usage {
                input: 12,
                output: 1058,
                cache_read: 37082,
                cache_create: 22812
            }
        );
        // The parent alone: its result rows, summed, by hand.
        assert_eq!(
            summary.parent_total,
            Usage {
                input: 6,
                output: 473,
                cache_read: 25950,
                cache_create: 14206
            }
        );

        // The parent's own tools: an agent that was handed a reading list has
        // no subagent, and these are the only files anything opened.
        assert_eq!(
            summary.parent_files_read,
            vec![PathBuf::from("notes/parent.md")]
        );
        assert_eq!(summary.parent_tool_uses, 2);
        // By name, so a parent that was meant to delegate and went looking
        // itself is visible in the row rather than only in the transcript.
        assert_eq!(
            summary.parent_tools,
            BTreeMap::from([("Agent".to_string(), 1), ("Read".to_string(), 1)])
        );
        assert_eq!(summary.model.as_deref(), Some("claude-sonnet-5"));

        assert_eq!(
            summary.tasks,
            vec![Task {
                agent_id: "agent7".to_string(),
                kind: "Explore".to_string(),
                status: Some("completed".to_string()),
                duration_ms: Some(6334),
                tool_uses: Some(2),
            }]
        );
    }

    #[test]
    fn a_run_that_printed_no_result_is_an_error() {
        assert!(
            summarise_stream(
                "{\"type\":\"system\",\"subtype\":\"init\"}\n",
                Path::new("/wiki")
            )
            .is_err()
        );
    }

    /// Trimmed from a real subagent transcript: three turns, the first two
    /// written twice because they were streamed in two pieces.
    const TRANSCRIPT: &str = r#"
{"type":"user","agentId":"agent7","message":{"role":"user","content":"go"}}
{"type":"assistant","message":{"id":"s1","model":"claude-sonnet-5","usage":{"input_tokens":2,"output_tokens":3,"cache_read_input_tokens":0,"cache_creation_input_tokens":4990},"content":[{"type":"thinking","thinking":"..."}]}}
{"type":"assistant","message":{"id":"s1","model":"claude-sonnet-5","usage":{"input_tokens":2,"output_tokens":136,"cache_read_input_tokens":0,"cache_creation_input_tokens":4990},"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/wiki/index.md"}}]}}
{"type":"assistant","message":{"id":"s2","model":"claude-sonnet-5","usage":{"input_tokens":2,"output_tokens":160,"cache_read_input_tokens":4990,"cache_creation_input_tokens":1152},"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/wiki/concepts/node.md"}}]}}
{"type":"assistant","message":{"id":"s3","model":"claude-sonnet-5","usage":{"input_tokens":2,"output_tokens":289,"cache_read_input_tokens":6142,"cache_creation_input_tokens":2464},"content":[{"type":"tool_use","id":"t3","name":"Glob","input":{"pattern":"**/*.md"}}]}}
"#;

    #[test]
    fn a_transcript_bills_each_turn_once_and_lists_what_was_read() {
        let summary = summarise_transcript(TRANSCRIPT, Path::new("/wiki"));

        assert_eq!(summary.turns, 3);
        assert_eq!(summary.model.as_deref(), Some("claude-sonnet-5"));
        // By hand, over the final piece of each turn: 2+2+2 in, 136+160+289
        // out, 0+4990+6142 read, 4990+1152+2464 created.
        assert_eq!(
            summary.usage,
            Usage {
                input: 6,
                output: 585,
                cache_read: 11132,
                cache_create: 8606
            }
        );
        assert_eq!(summary.usage.total(), 20329);
        assert_eq!(summary.tool_uses, 3);
        // Only `Read` names a file; the paths come back as the wiki spells them.
        assert_eq!(
            summary.files_read,
            vec![PathBuf::from("concepts/node.md"), PathBuf::from("index.md")]
        );
    }

    /// The whole point of reading two files instead of one: what the subagent
    /// was billed for and what the parent was billed for add up to what the
    /// run was billed for. If they ever stop adding up, one of the two is
    /// being read wrong and every comparison in the report is wrong with it.
    #[test]
    fn the_parent_and_the_subagent_account_for_the_whole_session() {
        let wiki = Path::new("/wiki");
        let stream = summarise_stream(STREAM, wiki).expect("a finished run");
        let subagent = summarise_transcript(TRANSCRIPT, wiki);

        let mut total = stream.parent_total;
        total.add(subagent.usage);
        assert_eq!(total, stream.session_total);
    }
}
