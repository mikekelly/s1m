# s1m

s1m reads local files and ranks them for a query, so an LLM agent opens only what matters.

You give it a query and one or more entry point files. It scores each file and each outgoing
link with a fast judgment model, follows the most promising links first, and returns a ranked
reading list with paths, line ranges and scores. The alternatives each miss something: grep
matches wording and ignores the link structure, embedding search needs an index kept in sync,
and letting the agent browse burns context on what is a string of quick relevance calls.

> **Status: scaffold.** The CLI does not run a query yet; budgets, caching and the output
> formats are still to come. What exists is the project skeleton from
> [#3](https://github.com/mikekelly/s1m/issues/3), the parser from
> [#4](https://github.com/mikekelly/s1m/issues/4) — `s1m::parse` turns one markdown file into
> its title, frontmatter, heading sections with line ranges, and outgoing links — the Jev
> judgment from [#5](https://github.com/mikekelly/s1m/issues/5) — `s1m::jev` scores one file
> per request, returning a relevance score for the file and a scent for each of its links —
> and the walk from [#6](https://github.com/mikekelly/s1m/issues/6): `s1m::traverse` searches
> the link graph best-first against an injected `s1m::scorer::Scorer`. The design lives in
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
cargo run -- --help
```

The stub implements `-h`/`--help` and `-V`/`--version` only: usage or version on stdout, exit 0.
No arguments, an unknown flag or one of the planned flags prints usage on stderr and exits 2 —
running `s1m` with no arguments is missing its query and entry files, not a reading list.

One hidden command works, and it is the spike's debug view rather than the interface:

```bash
cargo run -- score-file "how do I cut a release and publish the package" \
  eval/wikis/llm-wiki-manager/wiki/index.md
cargo run -- score-file --no-previews "how does s1m decide which links to follow" \
  docs/initial-plan.md
```

It parses one file, sends one Jev request, and prints the file's relevance, the call's model,
token count, latency and cost, then one row per link, best scent first. `--root DIR` sets the
directory links resolve against (the file's own directory by default). It needs
`TYPESAFE_API_KEY` and exits 2, with the reason on stderr, when the key is missing or the API
refuses the request.

The plan's interface, once implemented:

```bash
s1m "how do we handle settlement timing for instant payouts" wiki/index.md
s1m --mode about --max-files 40 --format tree "chargebacks" wiki/index.md
```

Exit codes: `0` reading list returned, `1` nothing cleared the threshold, `2` error.

## Library

The parser and the Jev judgment later stages build on live in `src/` and are reachable without
the CLI, so they can be driven directly from tests:

| Item | What it does |
| --- | --- |
| `parse::parse(path, root)` | One file's `title`, `frontmatter`, `sections` (`heading`, `level`, `lines`) and `links` (`target` resolved against `root`, `anchor`, `sentence`, `heading`, `inRoot`) |
| `parse::preview(path)` | Title, frontmatter and first paragraph of a link target, for link previews |
| `scorer::Scorer` | The judgment every later stage takes as an injected dependency: `async fn score(query, &ParsedFile) -> FileJudgment`, where `FileJudgment` is `relevance` (0 to 1) and one `LinkJudgment` (`target`, `scent` 0 to 1) per link, in the file's own order. `#[async_trait]`, so a caller can join a round's calls; tests use a fake |
| `jev::JevScorer` | That trait over the TypeSafe HTTP API: one request per file, holding the query, the file and, per link, its anchor, sentence, heading and target preview. `from_env(root)` reads `TYPESAFE_API_KEY`; `judge` also returns the model, token counts and latency of the call |
| `traverse::traverse(config, scorer)` | Async: best-first walk of the link graph over a frontier keyed by path score — the product of the link scents on the best path to a file — under the `max_files`, `max_depth`, `threshold` and `fanout` budgets. One future per file per round, joined, so a round costs one round trip. Returns the visited files with their relevance, the scent that reached them, the `via` path and the outgoing links it judged |

`path` and `root` must be given against the same base: both relative to the working directory,
or both absolute. A link that resolves outside `root` keeps `inRoot: false` so it is never
followed; a target that is not `.md`/`.txt`, or an external URL, is dropped. Wikilinks
(`[[target]]`, `[[target|alias]]`) resolve by file name under `root`, preferring `.md`.

Traversal takes its `entries` against the same base as `parse`, and every path it returns —
`path`, `via` and link targets — is spelled relative to `root`, so `root.join(path)` is the
file to read. The walk is async because the scorer is: a round joins one future per file, and
the caller brings the runtime. Ties on path score are broken by path, the round's answers are
collected in the order they were asked for however they come back, and the reading list is
sorted by relevance: the same query on the same files gives the same result, whatever the
answers' latency. A file that cannot be parsed or scored is reported in `failed` and does not
end the walk.

`tests/fixtures/wiki/` is a small wiki covering each link form, nested headings, a link out of
the root and a broken link; `tests/parse.rs` asserts the sections' line ranges against it and
`tests/traverse.rs` walks it with a fake scorer.
`eval/wikis/llm-wiki-manager/` is a real one, vendored with its licence and commit
([its source](eval/wikis/llm-wiki-manager/SOURCE.md)), which `tests/jev_live.rs` scores and
`docs/spike-notes.md` was measured on. `src/jev.rs` tags each answer with the question id it
came back under, so answers land on their own link; a link whose target cannot be read is
still judged, from the text the caller wrote about it.

## Development

| Command | What it does |
| --- | --- |
| `cargo test` | Unit, parser, Jev client, traversal and CLI tests; the live tests skip without `TYPESAFE_API_KEY` |
| `cargo build` | Debug build |
| `cargo fmt` | Format; `cargo fmt --check` to verify |
| `cargo clippy --all-targets -- -D warnings` | Lint, warnings are errors |

CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and
`cargo build` on the stable toolchain, then a smoke test on the built binary: `--help` prints
usage, no arguments exits 2 with usage on stderr, an unknown flag exits 2
([.github/workflows/ci.yml](.github/workflows/ci.yml)). `tests/cli.rs` spawns the same binary,
so the usage text, the streams and the exit codes above are what CI exercises.

`tests/jev_live.rs` is the only test that leaves the machine. It calls the real API for
`TYPESAFE_API_KEY`, and skips itself when the variable is unset, so CI stays offline and free;
with the key set it scores the vendored wiki's index page and release page and this
repository's plan, and checks that the release page outranks an unrelated one.
