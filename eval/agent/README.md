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
# --model MODEL pins every agent in the run to MODEL; leave it off to measure
# whatever Claude Code would have used.

# 3. The committed half.
cargo run --release --bin eval-agent -- report \
  --out /path/to/run-dir --report REPORT.md
```

`graph-stats` writes `graph_stats.json` and `graph_stats.md`. Depth is
measured from `index.md`, else `README.md`; a wiki whose root holds neither
takes `--entry <relative path>`, and the report then says the entry page was
`given` rather than which one it was. `run` writes
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
- `entry` is per query and is the page the walk starts from. It may be one
  path or an array of them — a wiki need not have one way in — and a query
  without it falls back to `--entry`, which defaults to `index.md` and may be
  repeated. A wiki with no page of that name must set one or the other. s1m
  takes the whole set as its entry files, and the Explore prompt names them
  all as the pages to start from.
- `mode` is passed to s1m as `--mode`; the agent conditions have no such flag.

## The conditions

| Condition | What runs | What is measured |
| --- | --- | --- |
| `explore` | `claude -p` in the wiki directory with `Task` as its only tool, so its one way to answer is the built-in Explore subagent | the **subagent's** tokens, turns, wall time and the files it opened, with the parent's tokens beside them |
| `s1m` | the `s1m` binary at its defaults from the wiki directory, against `--cache-dir` | wall time, the files and ranges it returned, and what opening those ranges would cost an agent |
| `s1m-cold` | the same, against a cache directory of its own | the same, plus the judgments it bought: Jev tokens and their cost |
| `s1m-t<N>` | s1m at its defaults but `--threshold N`, warm only — `s1m-t0.4` is one | the same as `s1m`, so the report can show one threshold variant beside the defaults |
| `s1m-agent` | `claude -p` handed s1m's reading list and told to open only what it needs | the agent's tokens, wall time and the files it opened, plus what the reading list cost to buy |

`s1m-agent` is the comparison that puts like with like: the same agent, the
same question, one of them handed a reading list. Repeat *k* of it is paired
with repeat *k* of `s1m`: that is the run whose list it was handed, and whose
`cost_usd` is added to its own so the Cost column compares like with like. A
warm s1m run has bought nothing and adds nothing; the cold run is where a
list's price shows. If the `s1m` row it needs is missing — a pass that names
only `s1m-agent`, or a resume whose `s1m` row failed — that run is made first
and recorded, rather than the agent run failing.

### Why `s1m-cold` and not `--no-cache`

s1m's JSON reports how many judgments a run bought but not what they cost, and
`--no-cache` stores nothing to read the cost back from. So a cold run is made
against an empty cache directory of its own, and the tokens are read from the
entries it wrote. Those entries are then copied into `--cache-dir`, which is
why the warm run that follows costs nothing: cache entries are keyed by the
request that produced them, so a copied answer is the same answer.

`--cold-repeats` (default 1) is how many repeats of `s1m` are also measured
cold. Every cold run is bought again, so this is the flag that decides what a
pass costs. A `s1m-t<N>` condition is never measured cold: it is there to show
what the threshold does, not what a first walk costs.

Every s1m run is priced by what it bought: the judgments in the cache
directory afterwards that were not there before, at the list price in
`src/jev.rs`. A warm run at the defaults has usually bought nothing, so it
costs nothing; a warm run at another threshold walks where the cache has not
been, and what it bought there is counted.

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
message's final output count. The two agree as an identity the tests hold to:
subagent tokens plus parent tokens equal the session total that `modelUsage`
reports.

Every Explore subagent the parent spawned is measured and their tokens summed;
the count is recorded as `tasks` and `explore_tasks`. That identity is also why
a run whose subagent left no findable transcript is **not** a measurement: its
tokens are in the session total and in neither the parent's nor any
subagent's, so the run is recorded as failed rather than averaged in as though
the parent had done the work. A parent that spawned some other kind of
subagent is failed for the same reason.

The agent runs with `--setting-sources ""`, `--settings <the file above>` and
`--permission-prompts none`, so anything that would prompt is denied rather
than hanging. **Not `--safe-mode`**: it disables hooks, and the hook is the
mechanism. `--strict-mcp-config` and `--disable-slash-commands` go with it, because
`--setting-sources ""` does **not** reach the account's own MCP servers or
skills and `--safe-mode` used to: on one machine they arrived as 58 extra
tools, 18 skills and 53 commands, putting the parent's first turn at 57k
tokens against 12.6k — the operator's installation being measured as the
wiki. With all three flags the session reports 4 tools, 0 skills, 0 MCP
servers, and the Explore agent is still there.

`--setting-sources ""` is what keeps the wiki's own configuration
out, and on 2.1.278 that includes its `CLAUDE.md` — verified three ways on a
throwaway wiki holding a `CLAUDE.md` with a distinctive instruction: with the
default flags the agent quoted the instruction and obeyed it; with
`--setting-sources ""` it answered `NONE` and the debug log mentions no memory
file at all. A parent that hands work to a backgrounded subagent answers
twice, so the **last** result in the stream is the answer, not the first.

**A hook makes the parent delegate.** Given the read-only tools, the parent
does not hand the task over: it explores — around twenty reads and greps a run
— and what gets measured is a parent wearing a subagent's name. Taking the
tools away does not work either: `--tools` bounds the **whole session**, so a
parent given only `Task` spawns an Explore agent that has no tools and reports
nothing (verified: 0 tool uses, empty answer).

What works is a `PreToolUse` hook. Claude Code 2.1.278 puts `agent_type` and
`agent_id` in the hook's input for a **subagent's** tool call and leaves them
out for the **parent's**. `run` writes two files into `--out`:
`delegate-only.sh`, a POSIX-shell hook that exits 2 with the reason *"the
parent must delegate to the Explore agent"* when the input carries no
`agent_type`, and `explore-settings.json`, which installs it on the matcher
`Read|Glob|Grep`. Exit 2 blocks the call and hands the reason back to the
model, which then delegates. `Task` is not matched, so delegating is always
allowed.

`parent_tool_uses` and `parent_tool_denials` are metrics on every agent run,
printed as *Parent tools* and *Parent blocked*, with the per-name breakdown in
`detail.parent_tools`. On the Explore condition a parent that read anything
the hook let through is work counted as the Explore agent's.

`s1m-agent` keeps `--tools Read` and gets a hookless `agent-settings.json`: it
was handed the list, and reading it is the whole job.

**What model the Explore agent runs on.** 2.1.278 states no declared model for
its built-in agents anywhere reachable: `claude agents --json` lists *running
background sessions*, not agent definitions, and the stream's `init` event
lists agent names only (`["claude", "Explore", "general-purpose", "Plan"]`).
What is observable is the model Claude Code resolved for the subagent, which
the `Task` tool's own result carries as `resolvedModel`; it is recorded per run
as `detail.tasks[].resolved_model`, beside the model of every turn in the
subagent's transcript. With `--model` passed, the observed behaviour is
inheritance — the subagent resolves to the model the parent was given.

**No `--model` unless you name one.** A model named on the command line is
inherited by every agent in the run, so `--model sonnet` measures Sonnet
exploring rather than what Claude Code would have sent. With `--model` left
off, no flag is passed, each agent takes its own default, and the method table
says so. What actually answered is read back per run either way, and the
parent's model and the subagent's are recorded separately:
`detail.model_asked_for`, `detail.parent_model`, `detail.agent_model`.

## What the numbers mean

- **Recall and precision** are against the gold set's `wanted` pages. For an
  agent, they are scored on the files it *said* it relied on; the files it
  actually opened are scored separately under `read_recall` and
  `read_precision`, and the two sets are not the same.
- **`relied_parsed`** is 0 when the agent answered without the list of files it
  was asked for. That run is still scored, and it scores zero, so the report
  prints the count of unparsed answers beside the failure count: a condition
  with unparsed answers is reading lower than it looks.
- **Agent tokens** for an agent condition are what it was billed for — its
  system prompt, its tool definitions and every tool result included. For s1m
  they are the characters of wiki text in the ranges it returned, at four
  characters a token. Those are different quantities, and the report says so.
- **Cost** is what the CLI priced a whole run at, parent included;
  `agent_cost_share_usd` apportions it by tokens, which is an estimate and not
  a price. For an s1m run the cost is Jev's, at the list price in
  `src/jev.rs`; for `s1m-agent` it is the agent's plus the paired s1m run's,
  kept separately as `agent_cost_usd` and `s1m_cost_usd`.
- **Wall time** is one machine on one network.
- **A cell says `n=` when fewer runs are behind it than the condition
  measured**, so a metric only some runs carried does not read as an average
  over all of them.
- A run that failed is written as a row with `ok: false` and **no metrics**,
  and is left out of every average, so a broken harness does not read as a bad
  method. The report counts it in the condition's `Runs` cell — `3 (1 failed)`
  — and nothing more: what went wrong is under `detail.error` in the raw row,
  because an error message quotes requests, paths and stderr. s1m failing part
  way through a walk is the case this exists for: a page the judgment API
  refuses is exit 2, and the rest of the pass carries on.
