# s1m

s1m reads local markdown files and ranks them for a query, so an LLM agent opens only what
matters.

The name is short for **System 1 memex** — after Vannevar Bush's memex, which followed
associative trails through linked documents. It is pronounced "sim", and it is not a simulator:
what it reads is a folder of files, and what it returns is a list of what to open, not a
simulation of anything.

You give it a query and one or more entry point files. It scores each file and each outgoing
link with a fast judgment model, follows the most promising links first, and returns a ranked
reading list with paths, line ranges and scores. The alternatives each miss something: grep
matches wording and ignores the link structure, embedding search needs an index kept in sync,
and letting the agent browse burns context on what is a string of quick relevance calls.

> **Status: milestones 2, 3 and 4.** The CLI walks a real wiki: `s1m "<query>" <entry>...`
> returns the plan's reading list as JSON — every result carrying the line ranges worth reading —
> with the plan's flags, defaults and exit codes, and `--mode` / `--criteria` pick what relevance
> means. What it builds on is in place too — the parser from
> [#4](https://github.com/mikekelly/s1m/issues/4) (`s1m::parse` turns one markdown file into its
> title, frontmatter, heading sections with line ranges, and outgoing links), the Jev judgment
> from [#5](https://github.com/mikekelly/s1m/issues/5) (`s1m::jev` scores one file per request,
> returning a relevance score for the file, a score per heading section and a scent for each of
> its links; a file whose sections and links do not fit the API's state budget in one request is
> split across several), the walk from
> [#6](https://github.com/mikekelly/s1m/issues/6) (`s1m::traverse` searches the link graph
> best-first against an injected `s1m::scorer::Scorer`), the cache from
> [#7](https://github.com/mikekelly/s1m/issues/7), which keeps those answers on disk so a repeat
> run is identical and free, the three relevance criteria from
> [#10](https://github.com/mikekelly/s1m/issues/10) below, and the per-section scores and line
> ranges from [#9](https://github.com/mikekelly/s1m/issues/9), where a section below
> `--threshold` is left out of the list. `s1m::cli` is the CLI's own half: the flags, the
> walk, the reading list and the exit code, with the scorer injected so tests run it without a
> key or a network. `--format` prints that reading list as the JSON above, as `md` — the same
> list to read or paste — or as `tree`, the walk's annotated link tree
> ([#13](https://github.com/mikekelly/s1m/issues/13)). Milestone 3's numbers are in
> [`eval/REPORT.md`](eval/REPORT.md), which `cargo run --release --bin eval` reproduced on a
> vendored wiki and reruns on any other. Milestone 4 is the packaging around all of it: `--help`,
> this README and [`SKILL.md`](SKILL.md) all lead with what s1m reads and ranks, and
> `.s1mignore` keeps paths out of the scoring calls entirely
> ([below](#keeping-paths-out-of-it-s1mignore)). The design lives in
> [docs/initial-plan.md](docs/initial-plan.md); what the Jev calls cost and how they read on
> real pages is in [docs/spike-notes.md](docs/spike-notes.md).

## Install

```bash
cargo install --git https://github.com/mikekelly/s1m
```

Stable Rust, edition 2024 — rustc 1.85 or newer — is the only requirement. The crate is not
published to crates.io or anywhere else, so `cargo install --git` is the install. From a
checkout, `cargo build --release` leaves the binary at `target/release/s1m`, which is what the
rest of this README spells `s1m`: run `./target/release/s1m` there, or `cargo install --path .`
to put it on your `PATH`. Either way the binary is one command, and the scoring calls need a key
from your TypeSafe account — the variable is the only thing s1m reads it from:

```bash
export TYPESAFE_API_KEY=...     # see .env.example
s1m "how do I cut a release and publish the package" wiki/index.md
```

## Your content leaves the machine

Scoring calls send the query and the content of the files visited to the TypeSafe API. The key
is read from `TYPESAFE_API_KEY`; see [`.env.example`](.env.example). Do not point s1m at a
knowledge base you are not willing to send to a third party. A root whose wiki has pages that
must not go can say so in [`.s1mignore`](#keeping-paths-out-of-it-s1mignore).

## Use

```bash
s1m "how do I cut a release and publish the package" \
  eval/wikis/llm-wiki-manager/wiki/index.md
```

A query and one or more entry files. The reading list goes to stdout as JSON: the query, the
criterion the answers were judged against, how many files were visited and how many calls that
cost, then one entry per visited file, most relevant first and ties broken by path. Each entry
carries the relevance the model gave it, the scent of the link that reached it, the path that
got there, the ranges worth reading, and its outgoing links as they were judged — `followed`
says whether a link queued its target, so the caller can see what was passed over and why.
`scent` is `null` and `via` is empty for an entry file, which no link reached; a file a link
reached carries the scent of that link and the `via` path it came along. A link whose target
resolves outside `--root` is never followed, whatever its scent.

`sections` is what to open: one entry per heading section of the file that the model called
useful, each with the heading, the `[first, last]` lines to read and the score, most useful
first. The ranges are the parser's, so the lines named are the text that was scored. A section
below `--threshold` is left out: a section is one yes-or-no question to the model, and a Noul
near 0.5 means it was unsure, so the default leaves those out. Nothing is derived from the
scores — a range is never narrowed or widened — and because a section's range contains its
subsections', a parent that is a mix of useful and useless text lands near the middle and is
dropped while the subsection that mattered stays. A caller that has read one returned range has
read everything returned inside it. A file whose sections all fell below `--threshold` carries
`"sections": []` in the JSON — it was scored, and nothing cleared the bar — and the `md` view
says the same in words.

Every path is spelled the way the entry files were given: `--root wiki` with `wiki/index.md`
gives `wiki/payments/cutoffs.md`, not `payments/cutoffs.md`, and those are the paths a caller
hands back to its editor. An absolute `--root` gives absolute paths.

This is a real run, first result and all (the other four results are elided, and the numbers are
what a cold run costs — the same query again is answered from the cache and reports
`"calls": 0`):

```json
{
  "query": "how do I cut a release and publish the package",
  "mode": "useful-for",
  "visited": 5,
  "calls": 5,
  "results": [
    {
      "path": "eval/wikis/llm-wiki-manager/wiki/concepts/release.md",
      "relevance": 0.7833333333333333,
      "scent": 0.89,
      "via": ["eval/wikis/llm-wiki-manager/wiki/index.md"],
      "sections": [
        {
          "heading": "Release",
          "lines": [12, 26],
          "score": 0.72
        }
      ],
      "links": [
        {
          "target": "eval/wikis/llm-wiki-manager/wiki/concepts/node-version-and-types.md",
          "scent": 0.37,
          "followed": false
        },
        {
          "target": "eval/wikis/llm-wiki-manager/wiki/concepts/dogfooding.md",
          "scent": 0.21,
          "followed": false
        },
        {
          "target": "eval/wikis/llm-wiki-manager/wiki/concepts/repo-layout.md",
          "scent": 0.17,
          "followed": false
        }
      ]
    }
  ]
}
```

That result is the whole answer for a caller with this task: `concepts/release.md` lines 12–26
say the runbook lives in `RELEASING.md` at the repo root and list what it covers — branching,
semver, tagging, npm Trusted Publishing, the release workflow, the post-release sync,
troubleshooting and manual fallbacks. Reading the ranges the list returned is enough; the rest
of the page is a "See also" list, which the model scored 0.14 and the threshold left out.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--mode` | `useful-for` | What relevance means: `about`, `useful-for` or `answers` |
| `--criteria` | none | A file whose content is the criterion, in place of `--mode` |
| `--max-files` | 25 | Files visited before the walk stops. The eval shows the threshold binding first on a wiki this size: 25 returns the same mean recall as 10 over four more files, and a bigger corpus is unmeasured ([numbers](eval/REPORT.md#results-at-a-fixed-file-budget)) |
| `--max-depth` | 6 | Link hops from an entry file |
| `--threshold` | 0.6 | Least link scent that queues a target and least section score the list keeps, 0 to 1. The knee of the eval's sweep: 0.5 lifts mean recall from 0.67 to 0.72 for 39% more reading, 0.7 drops it to 0.53 for 26% less ([numbers](eval/REPORT.md#the-default-threshold)) |
| `--root` | the first entry file's directory | Bounds the walk: a link resolving outside it is not followed |
| `--no-cache` | off | Call Jev for every file, ignoring the answers on disk |
| `--format` | `json` | `json` (the list above), `md` (a reading list to paste) or `tree` (the walk's link tree) |

The `--threshold` and `--max-files` defaults are backed by numbers in
[eval/REPORT.md](eval/REPORT.md), and the decisions are recorded in
[docs/initial-plan.md](docs/initial-plan.md#defaults-from-the-evaluation).

### Output formats

`--format` picks the view of that reading list. `json` is the default and the one to parse;
`md` and `tree` are for a person, or for an agent that will paste the result into its own task.

`md` is the list to act on: the same files in the same order, each with the lines worth reading
and the score that put it there, one line per section.

```bash
s1m --format md "how do I cut a release and publish the package" \
  eval/wikis/llm-wiki-manager/wiki/index.md
```

```markdown
# Reading list: how do I cut a release and publish the package

Criterion: useful-for; 5 files visited, 0 calls

## 1. `eval/wikis/llm-wiki-manager/wiki/concepts/release.md`

relevance 0.77; scent 0.86; via `eval/wikis/llm-wiki-manager/wiki/index.md`

- lines 12-26, score 0.74, Release

## 2. `eval/wikis/llm-wiki-manager/wiki/index.md`

relevance 0.74; entry file

- lines 22-35, score 0.60, Concepts

## 3. `eval/wikis/llm-wiki-manager/wiki/concepts/dogfooding.md`

relevance 0.62; scent 0.87; via `eval/wikis/llm-wiki-manager/wiki/index.md`

- lines 32-41, score 0.65, README vs wiki

## 4. `eval/wikis/llm-wiki-manager/wiki/concepts/node-version-and-types.md`

relevance 0.55; scent 0.84; via `eval/wikis/llm-wiki-manager/wiki/index.md`

- nothing above --threshold

## 5. `eval/wikis/llm-wiki-manager/wiki/concepts/init-command.md`

relevance 0.27; scent 0.63; via `eval/wikis/llm-wiki-manager/wiki/index.md`

- nothing above --threshold
```

This is that query from the cache, so it reports no calls and its digits are the stored answers
rather than the JSON sample's above; `md` is the same reading list either way. A file whose
sections all fell below `--threshold` says so in place of a section line, and a section
with no heading of its own — the text before a file's first heading — reads `(preamble)` where
the heading would be.

`tree` is the walk as it happened: every file it visited, and beneath each one every link the
model judged, in the order the frontier would have taken them — highest scent first, ties broken
by path. Each link line carries the scent it was given and what the walk did about it:
`followed` when it queued the link's target, `pruned` when it did not, which is a scent below
`--threshold`, a target outside `--root`, one past `--max-depth`, or one already reached.

```bash
s1m --format tree "how do I cut a release and publish the package" \
  eval/wikis/llm-wiki-manager/wiki/index.md
```

```text
how do I cut a release and publish the package (useful-for); 5 files visited, 0 calls

eval/wikis/llm-wiki-manager/wiki/index.md  entry file; relevance 0.74
  eval/wikis/llm-wiki-manager/wiki/concepts/dogfooding.md  followed; scent 0.87; relevance 0.62
    eval/wikis/llm-wiki-manager/wiki/concepts/release.md  pruned; scent 0.89
    eval/wikis/llm-wiki-manager/wiki/concepts/repo-layout.md  pruned; scent 0.25
    ...
  eval/wikis/llm-wiki-manager/wiki/concepts/release.md  followed; scent 0.86; relevance 0.77
    eval/wikis/llm-wiki-manager/wiki/concepts/node-version-and-types.md  pruned; scent 0.31
    eval/wikis/llm-wiki-manager/wiki/concepts/dogfooding.md  pruned; scent 0.18
  eval/wikis/llm-wiki-manager/wiki/concepts/node-version-and-types.md  followed; scent 0.84; relevance 0.55
    eval/wikis/llm-wiki-manager/wiki/concepts/release.md  pruned; scent 0.90
    ...
  eval/wikis/llm-wiki-manager/wiki/concepts/init-command.md  followed; scent 0.63; relevance 0.27
    ...
  ...
```

The `...` lines are links the walk passed over, elided here; the run prints every one of them.
The tree is where the walk's own answers show. `release.md` is the best page in the list and the
entry follows the link at 0.86; `dogfooding.md`'s stronger-looking link to it, at 0.89, is
`pruned`, because the file had already been reached and a file is visited once, along the best
path found to it. The other pruned lines are links the model scored below `--threshold`, which
is why five files are where the walk spent its calls. Roots are the files no link reached: the
entry files the caller named, marked `entry file`. A link whose target the model never judged
prints `scent unknown` — such a link can never be followed, and a 0.00 would read as a judgment
when none was made.

Both views round scores to two decimals, because they are for reading: `json` is where the
model's own number lives. Both are rendered from the reading list alone, so either can be
printed from any list a caller has — including one it assembled itself — and neither can
disagree with the JSON about a file, a range or a link.

### Relevance modes

The query goes into the request exactly as it was asked; the mode picks what the model is told
to judge it by. Three are built in, and `--mode` chooses one:

| Mode | The criterion the model judges by | Typical use |
| --- | --- | --- |
| `about` | Is the content on the subject of the query | Browsing, collecting everything on a subject |
| `useful-for` (default) | Would the content help someone doing what the query describes | Agents with a task |
| `answers` | Does the content contain the answer to the query | Question lookup |

Each mode sends its own three questions: one Score for the file as a whole on a four-level
ladder, one Noul per heading section, and one Noul per outgoing link. The reading list reports
which criterion judged the answers, so a store of results can say what they are relevant to:

```bash
s1m --mode answers "how long does an instant payout take to settle" wiki/index.md
```

The criterion is part of the cached request, so switching mode over the same files buys fresh
answers rather than reading the previous mode's.

### Custom criteria

`--criteria FILE` replaces the mode with a criterion of your own. The file is plain text, and
its whole content, trimmed, is the criterion — the sentence a mode would otherwise supply:

```text
The content states the cut-off that decides whether an instant payout can still be sent.
```

s1m puts that sentence into all three questions and scores the file on a criterion-independent
four-level ladder (unrelated, tangential, supporting, central), because a built-in ladder is
written for its own criterion. `--criteria` overrides `--mode` rather than conflicting with it,
and the reading list reports the file's path as `mode`, so a caller can tell which criterion
judged the answers:

```bash
s1m --criteria criteria/payout-cutoffs.md "when is it too late to send" wiki/index.md
```

A criteria file that cannot be read, or that holds nothing, exits 2 naming it, before anything
is bought.

### What is sent about each link

Every request carries, per outgoing link, its anchor, the sentence around it and its enclosing
heading, plus a preview of the target read from disk: its title, its frontmatter and its first
paragraph. Previews are always on for a query — the spike measured them as the signal that
separates a page which says nothing from one whose own links point at the answer, at roughly
250 input tokens a link, and a threshold tuned with previews is not valid without them
([docs/spike-notes.md](docs/spike-notes.md)) — so this is part of the request rather than a
caller's flag. `s1m score-file --no-previews` stays as the spike's control case. The frontmatter
is the part of a preview most likely to mislead, and the eval put a number on it: dropping the
frontmatter costs 0.21 of mean recall (0.67 → 0.46) for 43% of the input tokens saved, and
dropping previews altogether costs 0.28, so the frontmatter is the larger half of what a preview
buys — `related:` is why ([eval/REPORT.md](eval/REPORT.md#the-preview-experiment-frontmatter)).

### Keeping paths out of it: `.s1mignore`

A wiki that holds anything private needs to say so, because this is a run that sends what it
visits to a third party. Put a `.s1mignore` beside the pages, in the root the walk is bounded
by, and its lines are [gitignore patterns](https://git-scm.com/docs/gitignore) — the matcher is
the `ignore` crate's, the one ripgrep uses, and s1m walks a path's levels from the root down the
way git does, so the syntax and the semantics are git's and not an approximation of them:

```text
# Nothing under private/ leaves this machine.
private/

# Every key, except the one that is meant to be shared.
*.key.md
!public.key.md
```

A `!` line re-includes what a broader line took, at the level it is written for. It cannot reach
inside a directory an earlier line excluded — `private/` excludes `private/readme.md` however
loudly a later line asks for it — because git does not look inside an excluded directory either,
and neither does s1m.

A matched path is never read, on the way in and on the way out:

| Where | What happens |
| --- | --- |
| A link whose target matches | The link is out of the file before it is judged: no question is asked about it, its target is not opened for a [preview](#what-is-sent-about-each-link), its path is not among the request's links, and the walk does not queue it whatever the model would have scored it. The reading list reports no such link — not as pruned, but not at all |
| An entry file you name | Exit 2 with the path, the `.s1mignore` and one line on stderr, before anything is read or bought. Naming a path is asking for it, so silence would be the wrong answer |
| The `.s1mignore` itself | A file that cannot be read, that holds a pattern that does not parse, or that is there but is not a readable file — a directory, a symlink whose target has gone — is exit 2. Dropping a rule quietly would send exactly the files the rule was written for |

What is *not* removed is another page's prose about a matched one: a page you do link to still
says "see the vault" in its own words, and the words of a page the walk reads are what is sent.
The rules cover the target — its text, its path as a link to judge, its preview — not the
sentences other authors wrote around it.

Three consequences worth knowing:

- Only the root's `.s1mignore` is read. One in a subdirectory is not: what a wiki excludes is
  stated in one place, and `--root` is what decides which file that is.
- The rules are part of what a request is. A page judged with the file visible and the same page
  judged with it ignored are different questions, so the [cache](#cache) keys them apart and
  narrowing `.s1mignore` never serves an answer bought while the file was readable.
- `s1m score-file` runs under the same rules, and so does everything driven from the library —
  the [evaluation harness](#evaluation) included: `s1m::ignore::Ignore` is one value a run is
  built around, not a filter over its output.

Exit codes:

| Code | Meaning |
| --- | --- |
| 0 | The walk reached files beyond the entry files |
| 1 | Nothing cleared the threshold: the model judged the entry files' links and none passed, so the list is the entry files and nothing more. The JSON is still on stdout, and one line on stderr says so |
| 2 | Error: bad flags, an unknown `--mode`, a blank query, no entry file, an entry file that cannot be read, an entry file the root's `.s1mignore` covers, a `.s1mignore` that cannot be read or parsed, a criteria file that cannot be read or holds nothing, a missing `TYPESAFE_API_KEY`, or a judgment that failed. One line on stderr, nothing on stdout — a mistyped flag is the exception, where the usage message is what tells the caller what the flags are |

A file the walk *reached* but could not read is neither an error nor a silent omission: a link
to a page that is not there is the wiki's business, so it is named on stderr as skipped and the
walk carries on. A *judgment* that fails is an error, because a reading list with a hole in its
ranking is a different answer.

### Environment

| Variable | Meaning |
| --- | --- |
| `TYPESAFE_API_KEY` | Required. The key the scoring calls are made with; without it s1m exits 2 saying so |
| `S1M_CACHE_DIR` | Where stored answers live, else `$XDG_CACHE_HOME/s1m`, else `~/.cache/s1m` |
| `S1M_ENDPOINT` | Ask this endpoint instead of `https://api.typesafe.ai/v1/systemone` — a proxy, or a test's fake server. It is part of the cache key, so one endpoint's answers are never served for another's |

### Debug view of one file

`s1m score-file <query> <file>` is hidden, and it is the spike's debug view rather than the
interface. It parses one file, sends one Jev request, and prints the file's relevance, the
call's model, question count, how many requests those took, tokens, latency and cost, then one
row per section and one per link, best score first.

```bash
s1m score-file "how do I cut a release and publish the package" \
  eval/wikis/llm-wiki-manager/wiki/index.md
s1m score-file --no-previews "how does s1m decide which links to follow" \
  docs/initial-plan.md
```

`--root DIR` sets the directory links resolve against (the file's own directory by default), and
`--no-previews` leaves the target's title, frontmatter and first paragraph out of the request,
which is the control case for whether a preview earns its tokens. The root's
[`.s1mignore`](#keeping-paths-out-of-it-s1mignore) applies here too: a file it matches is an
error naming it, and a link to one is not judged at all.

Answers are cached (see [Cache](#cache)), so it also prints `cache hit`, `miss` or `off` with
the directory the entries are in, and `files` and `calls` on separate lines — one file scored,
and how many real API calls that took, which is zero on a hit. `--no-cache` calls Jev even for
a request already answered, which is what the spike's numbers come from.

## Cache

Jev is stable but not bit-for-bit deterministic: the same request sent three times returned the
same top links and moved the numbers underneath them
([docs/spike-notes.md](docs/spike-notes.md)). Repeat runs are therefore identical — and close to
free — only because s1m keeps the answers.

That is where the reading list's `calls` comes from. It counts the judgments bought from the
API, not the files the walk visited: a cold run over three files reports `3`, the same run again
reports `0` with `visited` unchanged, and `--no-cache` reports a call per file every time. A
judgment is one or more requests — a page whose sections and links did not fit one request took
several — so `calls` is what a run cost in answers, and `s1m score-file` prints the requests
behind one of them. Two runs that read the
same stored answers are byte-identical, and a warm run differs from the cold one that filled the
cache in `calls` alone. Two `--no-cache` runs of one query keep the same ranking and move the
numbers underneath it, which is the difference the cache exists to remove.

An answer is cached under a SHA-256 of the request that produced it: the endpoint, the model,
the query, the mode's questions and criteria (so a second run under `--mode` or `--criteria` is
a different question and is bought), the file's path, title and content, each section's heading,
depth and lines, and each link's target, anchor, sentence, heading and preview. Change any of
those and it is a different question; a second identical run makes no API call at all. An entry
is one file's judgment, and what that judgment cost when it was bought — the model, the request
count, the tokens and the latency — so a run whose answers all came off the disk can still say
what they cost, which is what [`eval/REPORT.md`](eval/REPORT.md) does. The
thresholds are the caller's, applied to the answers, so changing `--threshold` between runs
buys nothing.

The model in that key is the alias the request asks for — `jev-latest` — not the version that
answered it, which is only known once the call has come back. An entry therefore keeps the
answer the alias gave when it was written: when TypeSafe moves the alias to a new model, delete
the directory or run with `--no-cache` to see the new numbers.

Entries are JSON files under `$S1M_CACHE_DIR` if that is set, else `$XDG_CACHE_HOME/s1m`, else
`~/.cache/s1m`. `--no-cache` skips the cache and calls Jev every time. An entry that cannot be
read — truncated by a full disk, edited by hand, written by an older s1m — is a miss, never a
wrong answer, and an entry that cannot be written costs nothing but the recomputed call; s1m
only fails, at startup and with the path, when the cache directory itself cannot be created.

Nothing evicts entries yet, and nothing needs to: an entry is a relevance, a range and a score
per section and a scent per link, so a few kilobytes for a link-heavy page and a few tens of
megabytes for a few thousand of them. Delete the directory to reclaim the space, or point
`S1M_CACHE_DIR` at a scratch directory per run.

## Evaluation

What the ranking is worth, measured rather than asserted: [`eval/REPORT.md`](eval/REPORT.md) is
what s1m found and what it cost over 20 labelled queries on the vendored
[`llm-wiki-manager`](eval/wikis/llm-wiki-manager/SOURCE.md) vault, with the gold set in
[`eval/gold/llm-wiki-manager.json`](eval/gold/llm-wiki-manager.json).

```bash
cargo run --release --bin eval -- \
  --wiki eval/wikis/llm-wiki-manager/wiki \
  --gold eval/gold/llm-wiki-manager.json \
  --cache eval/cache \
  --out eval/REPORT.md
```

The harness walks each query at `--max-files` 10 and 25 and reports, per query and in total:
recall and precision against the gold set, the tokens an agent would read (the returned ranges,
the same files whole, and the whole corpus), what the API was asked and what it cost, the same
numbers for the keyword ranker, the calibration curve of a link's scent against what following
it reached, a `--threshold` sweep, and the preview experiment
[#10](https://github.com/mikekelly/s1m/issues/10) deferred — previews off, previews without
frontmatter, previews as they ship.

Nothing about a wiki or a gold set is in the harness: both are paths, so the same command
measures a private wiki, and the gold set's paths are relative to `--wiki`. The report is
reproducible rather than merely repeated — a cache entry stores what its call cost, and no
number in the report is a wall clock — so with `eval/cache` committed the command above prints
the same bytes with no key at all, and `--no-cache` with a key buys every judgment again.

Its headline, in three lines:

- **Recall and precision at `--max-files` 10**: mean recall 0.67, mean precision 0.27 — 23 of the
  39 wanted pages, over 83 files returned. The budget is not what binds: the walk runs out of
  links above `--threshold` first, and `--max-files 25` returns 87 files for the same mean recall.
- **What an agent reads**: 35,245 tokens for the returned ranges, against 72,795 for the same
  files whole and 346,160 for every page on every query. Reading the returned files whole costs
  21% of the corpus's text; the section scores take 52% off that, and the ranking 90% off reading
  everything.
- **What it costs**: $0.018798 for the gold set at `--max-files 10` — $0.000940 a query, at 0.19 s
  an answer. On this wiki the keyword ranker finds more and reads far more — recall 0.94 against
  s1m's 0.67, at 6.9× the tokens.

## Library

The parser and the Jev judgment later stages build on live in `src/` and are reachable without
the CLI, so they can be driven directly from tests:

| Item | What it does |
| --- | --- |
| `parse::parse(path, root)` | One file's `title`, `frontmatter`, `sections` (`heading`, `level`, `lines`) and `links` (`target` resolved against `root`, `anchor`, `sentence`, `heading`, `inRoot`) |
| `parse::preview(path)` | Title, frontmatter and first paragraph of a link target, for link previews |
| `ignore::Ignore` | The root's `.s1mignore`: `Ignore::at(root)` reads it (a root without one matches nothing, a file that cannot be read or parsed is an error, and `Ignore::none()` is the empty set), `matched(relative_path)` answers for a path or any directory above it. One value a run is built around, asked by the CLI for its entry files, by the walk for what it may read and link to |
| `scorer::Scorer` | The judgment every later stage takes as an injected dependency: `async fn score(query, &ParsedFile) -> FileJudgment`, where `FileJudgment` is `relevance` (0 to 1), one `SectionJudgment` (`heading`, `lines` as the parser gave them, `score` 0 to 1) per section and one `LinkJudgment` (`target`, `scent` 0 to 1) per link, each in the file's own order. `#[async_trait]`, so a caller can join a round's calls; tests use a fake |
| `jev::JevScorer` | That trait over the TypeSafe HTTP API: one request per file, or several when the file's sections and links would not fit the API's 32k state budget in one — the file goes in each and the answers merge — holding the query, the file, its sections (heading, depth, lines) and, per link, its anchor, sentence, heading and target preview. `from_env(root)` reads `TYPESAFE_API_KEY`, `with_mode(mode)` picks the criterion, `with_previews(false)` drops the previews, `with_preview_frontmatter(false)` drops just the frontmatter from them — the experiment [#10](https://github.com/mikekelly/s1m/issues/10) deferred, which [`eval/REPORT.md`](eval/REPORT.md) answers; `judge` also returns the model, token counts, request count and latency of the call |
| `jev::Mode` | The criterion a run judges by: its `name`, the file question and its Score levels, the section and link questions and what counts as yes and no for each. Three consts — `ABOUT`, `USEFUL_FOR` (the default) and `ANSWERS` — and `Mode::custom(name, criterion)` for a `--criteria` file, whose wording is the caller's |
| `cache::Cacheable` | What a scorer implements to be cacheable: build the request, give the cache the bytes an answer depends on, send the request and say what it cost — that accounting is stored with the answer |
| `cache::CachedScorer` | That cache in front of any scorer, same `Scorer` trait: `judge` returns `Scored::Called { judgment, detail }` or `Scored::Reused { judgment, detail }` — the detail is what the answer cost, now or when it was bought — and `calls()` and `hits()` count what reached the API and what came off the disk |
| `traverse::traverse(config, scorer)` | Async: best-first walk of the link graph over a frontier keyed by path score — the product of the link scents on the best path to a file — under the `max_files`, `max_depth`, `threshold` and `fanout` budgets, and under `config.ignore`, which drops a matched entry file before anything is parsed and takes a matched link target out of the file before it is scored. One future per file per round, joined, so a round costs one round trip. Returns the visited files with their relevance, the scent that reached them, the `via` path, their sections (the parser's ranges, in document order) and the outgoing links it judged |
| `cli::Options` | One run's flags — the query, the entry files, the root and the budgets — with no defaults of their own: the plan's defaults live on the CLI flags that carry them |
| `cli::run(options, judge)` | The whole pipeline: read the entry files, walk with the injected `Judge`, and return the plan's `ReadingList`, or an error naming what stopped it. Paths come back joined onto the root, spelled the way the entry files were |
| `cli::Judge` | What the CLI needs of a scorer beyond scoring: `scorer()` for the walk and `calls()` for the count the reading list publishes. `CachedScorer` implements it with the cache's own miss count, `cli::Uncached` counts every score for `--no-cache`, and the CLI tests' fake is a third |
| `cli::ReadingList` | The plan's shape, `exit_code()` for the 0/1 decision, and `to_json()` for the JSON view — the other two views are `format::Format::render` |
| `format::Format` | How a reading list is printed: `Json` as above, `Md` — the same files with the lines worth reading, to read or paste — and `Tree` — the walk's files with every link it judged, each link's scent and whether it was followed. `render(&ReadingList) -> String` is what stdout gets, and both reading views are drawn from the list alone, so a caller can print one it built itself |

`path` and `root` must be given against the same base: both relative to the working directory,
or both absolute. A link that resolves outside `root` keeps `inRoot: false` so it is never
followed; a target that is not `.md`/`.txt`, or an external URL, is dropped. Wikilinks
(`[[target]]`, `[[target|alias]]`) resolve by file name under `root`, preferring `.md`.

Traversal takes its `entries` against the same base as `parse`, and every path it returns —
`path`, `via` and link targets — is spelled relative to `root`, so `root.join(path)` is the
file to read. `cli::run` joins the root back on, which is why the reading list spells paths the
way the entry files were given. The walk is async because the scorer is: a round joins one
future per file, and the caller brings the runtime. Ties on path score are broken by path, the
round's answers are collected in the order they were asked for however they come back, and the
reading list is sorted by relevance: the same query on the same files gives the same result,
whatever the answers' latency. A file that cannot be parsed or scored is reported in `failed`
and does not end the walk.

`tests/fixtures/wiki/` is a small wiki covering each link form, nested headings, a link out of
the root and a broken link; `tests/parse.rs` asserts the sections' line ranges against it and
`tests/traverse.rs` walks it with a fake scorer and holds the result's sections to those same
ranges. `tests/formats.rs` walks it too, renders the `md` and `tree` views of that run and
holds each to the byte against the snapshots committed under `tests/snapshots/`, which
`S1M_UPDATE_SNAPSHOTS=1 cargo test --test formats` rewrites when a view changes on purpose.
`tests/fixtures/cli/` is a three-page chain with a broken link beside it alongside pages whose
sections nest and a page with no heading at the top, and `tests/fixtures/criteria/` is a
criterion of a caller's own; `tests/cli.rs` runs the binary over both: the API is a loopback
server the test answers itself, pointed at with `S1M_ENDPOINT`, so the exit codes, the JSON and
the two reading views on stdout, which criterion reached the request, and the cache behaviour
are checked end to end without a key or a network. `tests/fixtures/ignore/` is the `.s1mignore`
tree — an entry page, the page it links to, and two pages the root's rules cover, one behind a
directory pattern and one by name — which `tests/cli.rs` walks while holding the fake API to
the guarantee: no request names those pages and no request carries a byte of their text.
`tests/fixtures/ignore-broken/` is a root whose `.s1mignore` does not parse, and is the run
that exits 2 naming the line. `eval/wikis/llm-wiki-manager/` is a real one, vendored with its
licence and commit
([its source](eval/wikis/llm-wiki-manager/SOURCE.md)), which `tests/jev_live.rs` scores and
`docs/spike-notes.md` was measured on. `eval/gold/` labels it for the evaluation harness, and
`eval/cache/` holds the answers [`eval/REPORT.md`](eval/REPORT.md) was written from, which is
what lets that report be checked without a key. `src/jev.rs` tags each answer with the question id it
came back under, so answers land on their own section and their own link; a link whose target
cannot be read is still judged, from the text the caller wrote about it, and a section of a file
long enough to be truncated is judged from its heading and lines, the same way.

## Development

| Command | What it does |
| --- | --- |
| `cargo test` | Unit, parser, cache, Jev client, traversal, format, CLI and eval-harness tests, including the end-to-end CLI tests that answer the API themselves; the live tests skip without `TYPESAFE_API_KEY` |
| `cargo run --release --bin eval -- --wiki <dir> --gold <file>` | The evaluation harness: see [Evaluation](#evaluation), and [`eval/REPORT.md`](eval/REPORT.md) for what it last said |
| `cargo build` | Debug build |
| `cargo fmt` | Format; `cargo fmt --check` to verify |
| `cargo clippy --all-targets -- -D warnings` | Lint, warnings are errors |

CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and
`cargo build` on the stable toolchain, then a smoke test on the built binary: `--help` prints
usage, no arguments exits 2 with usage on stderr, an unknown flag exits 2
([.github/workflows/ci.yml](.github/workflows/ci.yml)). `tests/cli.rs` spawns the same binary
and covers the rest of the interface: the exit codes, the reading list on stdout in each
`--format`, the streams, and a run whose API is a loopback server the test answers.

`tests/jev_live.rs` is the only test that leaves the machine. It calls the real API for
`TYPESAFE_API_KEY`, and skips itself when the variable is unset, so CI stays offline and free;
with the key set it scores the vendored wiki's index page and release page and this
repository's plan, checks that the release page outranks an unrelated one, and checks that the
second identical run is answered from the cache rather than the API.

## License

MIT — see [LICENSE](LICENSE). Copyright Mike Kelly 2026.
