# s1m

s1m reads local files and ranks them for a query, so an LLM agent opens only what matters.

You give it a query and one or more entry point files. It scores each file and each outgoing
link with a fast judgment model, follows the most promising links first, and returns a ranked
reading list with paths, line ranges and scores. The alternatives each miss something: grep
matches wording and ignores the link structure, embedding search needs an index kept in sync,
and letting the agent browse burns context on what is a string of quick relevance calls.

> **Status: milestone 2, without the section line ranges yet.** The CLI walks a real wiki:
> `s1m "<query>" <entry>...` returns the plan's reading list as JSON, with the plan's flags,
> defaults and exit codes, and `--mode` / `--criteria` pick what relevance means. What it builds
> on is in place too — the parser from
> [#4](https://github.com/mikekelly/s1m/issues/4) (`s1m::parse` turns one markdown file into its
> title, frontmatter, heading sections with line ranges, and outgoing links), the Jev judgment
> from [#5](https://github.com/mikekelly/s1m/issues/5) (`s1m::jev` scores one file per request,
> returning a relevance score for the file and a scent for each of its links), the walk from
> [#6](https://github.com/mikekelly/s1m/issues/6) (`s1m::traverse` searches the link graph
> best-first against an injected `s1m::scorer::Scorer`), the cache from
> [#7](https://github.com/mikekelly/s1m/issues/7), which keeps those answers on disk so a repeat
> run is identical and free, and the three relevance criteria from
> [#10](https://github.com/mikekelly/s1m/issues/10) below. `s1m::cli` is the CLI's own half: the
> flags, the walk, the reading list and the exit code, with the scorer injected so tests run it
> without a key or a network. `s1m::seed` is `--seed-grep`
> ([#14](https://github.com/mikekelly/s1m/issues/14)): the query's keywords matched against the
> pages under the root and put on the frontier as extra entry files, so a page nothing links to
> is still reached. Section line ranges are [#9](https://github.com/mikekelly/s1m/issues/9); the
> `md` and `tree` formats are later. The design lives in
> [docs/initial-plan.md](docs/initial-plan.md); what the Jev calls cost and how they read on
> real pages is in [docs/spike-notes.md](docs/spike-notes.md).

## Your content leaves the machine

Scoring calls send the query and the content of the files visited to the TypeSafe API. The key
is read from `TYPESAFE_API_KEY`; see [`.env.example`](.env.example). Do not point s1m at a
knowledge base you are not willing to send to a third party.

## Requirements

Stable Rust, edition 2024 — rustc 1.85 or newer.

## Build

```bash
cargo build --release     # target/release/s1m
```

## Use

```bash
cargo build --release     # target/release/s1m

s1m "how do I cut a release and publish the package" \
  eval/wikis/llm-wiki-manager/wiki/index.md
```

A query and one or more entry files. The reading list goes to stdout as JSON: the query, the
criterion the answers were judged against, how many files were visited and how many calls that
cost, then one entry per visited file, most relevant first and ties broken by path. Each entry
carries the relevance the model gave it, the scent of the link that reached it, the path that
got there, and its outgoing links as they were judged — `followed` says whether a link queued
its target, so the caller can see what was passed over and why. `scent` is `null` and `via` is
empty for an entry file, which no link reached, and `seeded` says whether the entry file was one
the caller named or one `--seed-grep` found. A link whose target resolves outside `--root` is
never followed, whatever its scent. Line ranges and per-section scores are
[#9](https://github.com/mikekelly/s1m/issues/9).

Every path is spelled the way the entry files were given: `--root wiki` with `wiki/index.md`
gives `wiki/payments/cutoffs.md`, not `payments/cutoffs.md`, and those are the paths a caller
hands back to its editor. An absolute `--root` gives absolute paths.

This is a real run, first result and all (the other two results are elided, and the numbers are
what a cold run costs — the same query again is answered from the cache and reports
`"calls": 0`):

```json
{
  "query": "how do I cut a release and publish the package",
  "mode": "useful-for",
  "visited": 3,
  "calls": 3,
  "results": [
    {
      "path": "eval/wikis/llm-wiki-manager/wiki/concepts/release.md",
      "relevance": 0.8133333333333334,
      "scent": 0.87,
      "via": ["eval/wikis/llm-wiki-manager/wiki/index.md"],
      "seeded": false,
      "links": [
        {
          "target": "eval/wikis/llm-wiki-manager/wiki/concepts/node-version-and-types.md",
          "scent": 0.38,
          "followed": false
        },
        {
          "target": "eval/wikis/llm-wiki-manager/wiki/concepts/dogfooding.md",
          "scent": 0.17,
          "followed": false
        },
        {
          "target": "eval/wikis/llm-wiki-manager/wiki/concepts/repo-layout.md",
          "scent": 0.15,
          "followed": false
        }
      ]
    }
  ]
}
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--mode` | `useful-for` | What relevance means: `about`, `useful-for` or `answers` |
| `--criteria` | none | A file whose content is the criterion, in place of `--mode` |
| `--max-files` | 25 | Files visited before the walk stops |
| `--max-depth` | 6 | Link hops from an entry file |
| `--threshold` | 0.6 | Least link scent that queues a target, 0 to 1 |
| `--fanout` | 8 | Frontier files expanded per round |
| `--root` | the first entry file's directory | Bounds the walk: a link resolving outside it is not followed |
| `--no-cache` | off | Call Jev for every file, ignoring the answers on disk |
| `--format` | `json` | Only `json` exists so far; any other value exits 2 |
| `--seed-grep` | off | Add the top keyword hits under the root as extra entry files |
| `--seed-count` | 5 | How many hits `--seed-grep` adds; needs `--seed-grep` |

Not implemented yet: the `md` and `tree` formats.

### Relevance modes

The query goes into the request exactly as it was asked; the mode picks what the model is told
to judge it by. Three are built in, and `--mode` chooses one:

| Mode | The criterion the model judges by | Typical use |
| --- | --- | --- |
| `about` | Is the content on the subject of the query | Browsing, collecting everything on a subject |
| `useful-for` (default) | Would the content help someone doing what the query describes | Agents with a task |
| `answers` | Does the content contain the answer to the query | Question lookup |

Each mode sends its own two questions: one Score for the file as a whole on a four-level ladder,
and one Noul per outgoing link. The reading list reports which one judged the answers, so a
store of results can say what they are relevant to:

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

s1m puts that sentence into both questions and scores the file on a criterion-independent
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
is the part of a preview most likely to mislead; splitting it out is an experiment for the
evaluation milestone ([#11](https://github.com/mikekelly/s1m/issues/11)), not a decision to make
here.

Exit codes:

| Code | Meaning |
| --- | --- |
| 0 | The walk reached files beyond the entry files |
| 1 | Nothing cleared the threshold: the model judged the entry files' links and none passed, so the list is the entry files and nothing more. The JSON is still on stdout, and one line on stderr says so |
| 2 | Error: bad flags, an unknown `--mode`, a blank query, no entry file, an entry file that cannot be read, a criteria file that cannot be read or holds nothing, a missing `TYPESAFE_API_KEY`, or a judgment that failed. One line on stderr, nothing on stdout — a mistyped flag is the exception, where the usage message is what tells the caller what the flags are |

A file the walk *reached* but could not read is neither an error nor a silent omission: a link
to a page that is not there is the wiki's business, so it is named on stderr as skipped and the
walk carries on. A *judgment* that fails is an error, because a reading list with a hole in its
ranking is a different answer.

### Seeding

The walk follows links, so a page nothing links to is never reached however relevant it is.
`--seed-grep` is the other way in: before the walk starts, s1m reads the pages under `--root`,
counts the query's keywords in each, and puts the best `--seed-count` of them on the frontier as
extra entry files — path score 1, no `via`, and `seeded: true` in the reading list.

```bash
s1m --seed-grep "how do I cut a release and publish the package" \
  eval/wikis/llm-wiki-manager/wiki/index.md
```

A keyword is a whole word of three characters or more: `we`, `do` and `a` name too little of a
query to be worth a hit, and matching a term anywhere in the text would count `for` inside
`before` and `note` inside `notes`. Files rank by how many of the query's terms they match, then
by how many hits they have, then by path — the page covering more of the query first, and never
the order a directory happened to list its files in. The candidates are the same `.md`/`.txt`
files the parser resolves wikilinks against, so a hidden directory is not searched, and the
entry files the caller named are left out of the hits: they are on the frontier already.

This is a keyword match, not a second opinion: it recovers pages that are orphaned or weakly
linked, and the ranking still comes from the model. A seed that the walk would have reached
anyway costs no extra call — a seed enters at path score 1 and a file is judged once — and a
seeded run whose links all fell below the threshold still exits 1, with its seeds in the list.

### Environment

| Variable | Meaning |
| --- | --- |
| `TYPESAFE_API_KEY` | Required. The key the scoring calls are made with; without it s1m exits 2 saying so |
| `S1M_CACHE_DIR` | Where stored answers live, else `$XDG_CACHE_HOME/s1m`, else `~/.cache/s1m` |
| `S1M_ENDPOINT` | Ask this endpoint instead of `https://api.typesafe.ai/v1/systemone` — a proxy, or a test's fake server. It is part of the cache key, so one endpoint's answers are never served for another's |

### Debug view of one file

`s1m score-file <query> <file>` is hidden, and it is the spike's debug view rather than the
interface. It parses one file, sends one Jev request, and prints the file's relevance, the
call's model, token count, latency and cost, then one row per link, best scent first.

```bash
s1m score-file "how do I cut a release and publish the package" \
  eval/wikis/llm-wiki-manager/wiki/index.md
s1m score-file --no-previews "how does s1m decide which links to follow" \
  docs/initial-plan.md
```

`--root DIR` sets the directory links resolve against (the file's own directory by default), and
`--no-previews` leaves the target's title, frontmatter and first paragraph out of the request,
which is the control case for whether a preview earns its tokens.

Answers are cached (see [Cache](#cache)), so it also prints `cache hit`, `miss` or `off` with
the directory the entries are in, and `files` and `calls` on separate lines — one file scored,
and how many real API calls that took, which is zero on a hit. `--no-cache` calls Jev even for
a request already answered, which is what the spike's numbers come from.

## Cache

Jev is stable but not bit-for-bit deterministic: the same request sent three times returned the
same top links and moved the numbers underneath them
([docs/spike-notes.md](docs/spike-notes.md)). Repeat runs are therefore identical — and close to
free — only because s1m keeps the answers.

That is where the reading list's `calls` comes from. It counts what the API was asked, not what
the walk visited: a cold run over three files reports `3`, the same run again reports `0` with
`visited` unchanged, and `--no-cache` reports a call per file every time. Two runs that read the
same stored answers are byte-identical, and a warm run differs from the cold one that filled the
cache in `calls` alone. Two `--no-cache` runs of one query keep the same ranking and move the
numbers underneath it, which is the difference the cache exists to remove.

An answer is cached under a SHA-256 of the request that produced it: the endpoint, the model,
the query, the mode's questions and criteria (so a second run under `--mode` or `--criteria` is
a different question and is bought), the file's path, title and content, and each
link's target, anchor, sentence, heading and preview. Change any of those and it is a different
question; a second identical run makes no API call at all.

The model in that key is the alias the request asks for — `jev-latest` — not the version that
answered it, which is only known once the call has come back. An entry therefore keeps the
answer the alias gave when it was written: when TypeSafe moves the alias to a new model, delete
the directory or run with `--no-cache` to see the new numbers.

Entries are JSON files under `$S1M_CACHE_DIR` if that is set, else `$XDG_CACHE_HOME/s1m`, else
`~/.cache/s1m`. `--no-cache` skips the cache and calls Jev every time. An entry that cannot be
read — truncated by a full disk, edited by hand, written by an older s1m — is a miss, never a
wrong answer, and an entry that cannot be written costs nothing but the recomputed call; s1m
only fails, at startup and with the path, when the cache directory itself cannot be created.

Nothing evicts entries yet, and nothing needs to: an entry is a relevance and a scent per link,
so a few kilobytes for a link-heavy page and a few tens of megabytes for a few thousand of them.
Delete the directory to reclaim the space, or point `S1M_CACHE_DIR` at a scratch directory per
run.

## Library

The parser and the Jev judgment later stages build on live in `src/` and are reachable without
the CLI, so they can be driven directly from tests:

| Item | What it does |
| --- | --- |
| `parse::parse(path, root)` | One file's `title`, `frontmatter`, `sections` (`heading`, `level`, `lines`) and `links` (`target` resolved against `root`, `anchor`, `sentence`, `heading`, `inRoot`) |
| `parse::preview(path)` | Title, frontmatter and first paragraph of a link target, for link previews |
| `scorer::Scorer` | The judgment every later stage takes as an injected dependency: `async fn score(query, &ParsedFile) -> FileJudgment`, where `FileJudgment` is `relevance` (0 to 1) and one `LinkJudgment` (`target`, `scent` 0 to 1) per link, in the file's own order. `#[async_trait]`, so a caller can join a round's calls; tests use a fake |
| `jev::JevScorer` | That trait over the TypeSafe HTTP API: one request per file, holding the query, the file and, per link, its anchor, sentence, heading and target preview. `from_env(root)` reads `TYPESAFE_API_KEY`, `with_mode(mode)` picks the criterion, `with_previews(false)` drops the previews; `judge` also returns the model, token counts and latency of the call |
| `jev::Mode` | The criterion a run judges by: its `name`, the file question and its Score levels, the link question and what counts as yes and no. Three consts — `ABOUT`, `USEFUL_FOR` (the default) and `ANSWERS` — and `Mode::custom(name, criterion)` for a `--criteria` file, whose wording is the caller's |
| `cache::Cacheable` | What a scorer implements to be cacheable: build the request, give the cache the bytes an answer depends on, send the request |
| `cache::CachedScorer` | That cache in front of any scorer, same `Scorer` trait: `judge` returns `Scored::Called { judgment, detail }` or `Scored::Reused(judgment)`, and `calls()` and `hits()` count what reached the API and what came off the disk |
| `traverse::traverse(config, scorer)` | Async: best-first walk of the link graph over a frontier keyed by path score — the product of the link scents on the best path to a file — under the `max_files`, `max_depth`, `threshold` and `fanout` budgets. One future per file per round, joined, so a round costs one round trip. Returns the visited files with their relevance, the scent that reached them, the `via` path, whether they were a keyword seed, and the outgoing links it judged |
| `seed::seed(root, query, count, skip)` | In-process keyword match, no ripgrep: the best `count` pages under `root` for `query`'s keywords, spelled the way `traverse` wants its entry files, with `skip` — the caller's entry files — left out. Whole words of three characters or more, ranked by terms matched, then hits, then path |
| `cli::Options` | One run's flags — the query, the entry files, the root, the budgets and how many keyword hits to seed — with no defaults of their own: the plan's defaults live on the CLI flags that carry them |
| `cli::run(options, judge)` | The whole pipeline: read the entry files, walk with the injected `Judge`, and return the plan's `ReadingList`, or an error naming what stopped it. Paths come back joined onto the root, spelled the way the entry files were |
| `cli::Judge` | What the CLI needs of a scorer beyond scoring: `scorer()` for the walk and `calls()` for the count the reading list publishes. `CachedScorer` implements it with the cache's own miss count, `cli::Uncached` counts every score for `--no-cache`, and the CLI tests' fake is a third |
| `cli::ReadingList` | The plan's JSON shape, `exit_code()` for the 0/1 decision, and `to_json()` for stdout |

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
`tests/traverse.rs` walks it with a fake scorer. `tests/fixtures/cli/` is a three-page chain
with a broken link beside it, and `tests/fixtures/criteria/` is a criterion of a caller's own;
`tests/cli.rs` runs the binary over both: the API is a loopback server the test answers itself,
pointed at with `S1M_ENDPOINT`, so the exit codes, the JSON on stdout, which criterion reached
the request, and the cache behaviour are checked end to end without a key or a network.
`tests/fixtures/seed/` is the `--seed-grep` tree — an entry page, the chain it links to, a page
in a hidden directory, and a page nothing links to — which `src/seed.rs` measures the keyword
match against and `tests/cli.rs` walks both ways.
`eval/wikis/llm-wiki-manager/` is a real one, vendored with its licence and commit
([its source](eval/wikis/llm-wiki-manager/SOURCE.md)), which `tests/jev_live.rs` scores and
`docs/spike-notes.md` was measured on. `src/jev.rs` tags each answer with the question id it
came back under, so answers land on their own link; a link whose target cannot be read is
still judged, from the text the caller wrote about it.

## Development

| Command | What it does |
| --- | --- |
| `cargo test` | Unit, parser, cache, Jev client, traversal and CLI tests, including the end-to-end CLI tests that answer the API themselves; the live tests skip without `TYPESAFE_API_KEY` |
| `cargo build` | Debug build |
| `cargo fmt` | Format; `cargo fmt --check` to verify |
| `cargo clippy --all-targets -- -D warnings` | Lint, warnings are errors |

CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and
`cargo build` on the stable toolchain, then a smoke test on the built binary: `--help` prints
usage, no arguments exits 2 with usage on stderr, an unknown flag exits 2
([.github/workflows/ci.yml](.github/workflows/ci.yml)). `tests/cli.rs` spawns the same binary
and covers the rest of the interface: the exit codes, the JSON on stdout, the streams, and a
run whose API is a loopback server the test answers.

`tests/jev_live.rs` is the only test that leaves the machine. It calls the real API for
`TYPESAFE_API_KEY`, and skips itself when the variable is unset, so CI stays offline and free;
with the key set it scores the vendored wiki's index page and release page and this
repository's plan, checks that the release page outranks an unrelated one, and checks that the
second identical run is answered from the cache rather than the API.
