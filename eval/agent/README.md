# `eval-agent`: a reading list against an agent that explores

s1m hands an agent a ranked reading list. A Claude Code **Explore** agent is
handed the wiki and looks. `eval-agent` measures both on the same queries, on
any wiki, and writes two halves that are deliberately kept apart:

- **`--out`**, the raw rows: one JSONL row a run, with the query as it was
  asked, the files each run opened and the command that was run. **Never
  commit this directory.** It is where a private wiki shows through.
- **the report**, rendered from the aggregates alone: numbers, query ids and
  category labels. Nothing else reaches it, and the renderer refuses a label
  that looks like a path or reads like a question rather than printing it.

Every command takes paths, so the tooling here is generic and the wiki it
measures need not be.

## The three commands

```bash
cargo build --release          # eval-agent runs the s1m binary beside it

# 1. What the wiki's link graph looks like, as numbers.
cargo run --release --bin eval-agent -- graph-stats \
  --wiki /path/to/wiki --out /path/to/run-dir

# 2. Every query under every condition. This one costs money.
cargo run --release --bin eval-agent -- run \
  --wiki /path/to/wiki --gold /path/to/gold.json --out /path/to/run-dir \
  --repeats 3 --conditions explore,s1m,s1m-agent \
  --cache-dir /path/to/s1m-cache

# 3. The committed half.
cargo run --release --bin eval-agent -- report \
  --out /path/to/run-dir --report REPORT.md
```

`graph-stats` writes `graph_stats.json` and `graph_stats.md`. `run` writes
`runs.jsonl`, `aggregates.json` and each run's own output under `raw/`.
`report` renders `aggregates.json` plus `graph_stats.json` — it picks the
latter up from `--out` on its own, or takes `--stats PATH`.

**Start small.** `--queries id1,id2 --repeats 1` measures two queries once:
an Explore run is tens of thousands of tokens, and a gold set is twenty of
them. A pass is resumable — a `(query, condition, repeat)` already in
`runs.jsonl` is never run again — so a pass that is stopped, or that crashed,
is continued by running the same command again.

`s1m` needs `TYPESAFE_API_KEY` in the environment for anything it has not
already bought; `claude` needs whatever Claude Code is authenticated with.

## The gold set

The format the existing `eval` binary reads, plus a `category`:

```json
{"queries": [{
  "id": "node-runtime-floor",
  "category": "lookup",
  "query": "what node version must consumers run the CLI on",
  "entry": "index.md",
  "mode": "answers",
  "wanted": ["concepts/node-version-and-types.md"],
  "why": "the runtime floor against the development pin is one page"
}]}
```

- `id` and `category` are the only fields that reach a report, so they are
  names and labels — never a path, never the question.
- `wanted` may also be spelled `expected`, and `why` may be spelled `note`:
  both binaries read one file, because each ignores the fields it does not
  know. `eval/gold/llm-wiki-manager.json` carries categories and still
  reproduces the older `eval` report byte for byte.
- `entry` is per query and is the page the walk starts from. A query without
  one falls back to `--entry`, which defaults to `index.md`. A wiki with no
  page of that name must set one or the other.
- `mode` is passed to s1m as `--mode`; the agent conditions have no such flag.

## The conditions

| Condition | What runs | What is measured |
| --- | --- | --- |
| `explore` | `claude -p` in the wiki directory, asked to hand the query to the built-in Explore subagent | the **subagent's** tokens, turns, wall time and the files it opened, with the parent's tokens beside them |
| `s1m` | the `s1m` binary at its defaults from the wiki directory, against `--cache-dir` | wall time, the files and ranges it returned, and what opening those ranges would cost an agent |
| `s1m-cold` | the same, against a cache directory of its own | the same, plus the judgments it bought: Jev tokens and their cost |
| `s1m-agent` | `claude -p` handed s1m's reading list and told to open only what it needs | the agent's tokens, wall time and the files it opened |

`s1m-agent` is the comparison that puts like with like: the same agent, the
same question, one of them handed a reading list.

### Why `s1m-cold` and not `--no-cache`

s1m's JSON reports how many judgments a run bought but not what they cost, and
`--no-cache` stores nothing to read the cost back from. So a cold run is made
against an empty cache directory of its own, and the tokens are read from the
entries it wrote. Those entries are then copied into `--cache-dir`, which is
why the warm run that follows costs nothing: cache entries are keyed by the
request that produced them, so a copied answer is the same answer.

`--cold-repeats` (default 1) is how many repeats are also measured cold. Every
cold run is bought again, so this is the flag that decides what a pass costs.

### How the Explore subagent is isolated

The parent agent's own tokens would swamp the measurement, so they are kept
apart. The run is `claude -p --output-format stream-json --verbose`, and:

- the **stream** gives the session id, the spawned tasks (`task_started` names
  the agent id and the subagent type, `task_notification` its wall time), the
  parent's own turns — every `result` row's usage is a parent turn — and the
  session totals from `modelUsage`;
- the **subagent's transcript**, found under
  `$CLAUDE_CONFIG_DIR/projects/*/<session-id>/subagents/agent-<id>.jsonl`
  (`~/.claude/projects` by default, or `--transcripts DIR`), gives the
  subagent's turns with their **final** usage, its model, and every file it
  opened with `Read`.

The transcript is what the subagent figure is summed from, because the stream
writes a message once per streamed piece and only the last piece carries the
message's final output count. The two agree as an identity worth checking:
subagent tokens plus parent tokens equal the session total that `modelUsage`
reports.

The agent runs with `--safe-mode` — no `CLAUDE.md`, plugins, hooks, MCP
servers or custom agents from wherever the wiki happens to live — with
`--tools` and `--allowedTools` set to `Read,Glob,Grep,Task` (the last is what
lets it spawn the subagent), and `--permission-prompts none` so that anything
which would prompt is denied rather than hanging. A parent that hands work to
a backgrounded subagent answers twice, so the **last** result in the stream is
the answer, not the first.

## What the numbers mean

- **Recall and precision** are against the gold set's `wanted` pages. For an
  agent, they are scored on the files it *said* it relied on; the files it
  actually opened are scored separately under `read_recall` and
  `read_precision`, and the two sets are not the same.
- **Agent tokens** for an agent condition are what it was billed for — its
  system prompt, its tool definitions and every tool result included. For s1m
  they are the characters of wiki text in the ranges it returned, at four
  characters a token. Those are different quantities, and the report says so.
- **Cost** is what the CLI priced a whole run at, parent included;
  `agent_cost_share_usd` apportions it by tokens, which is an estimate and not
  a price. For `s1m-cold` the cost is Jev's, at the list price in `src/jev.rs`.
- **Wall time** is one machine on one network.
- A run that failed is written as a row with `ok: false` and left out of every
  average, so a broken harness does not read as a bad method.
