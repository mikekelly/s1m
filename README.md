# s1m

s1m reads local files and ranks them for a query, so an LLM agent opens only what matters.

You give it a query and one or more entry point files. It scores each file and each outgoing
link with a fast judgment model, follows the most promising links first, and returns a ranked
reading list with paths, line ranges and scores. The alternatives each miss something: grep
matches wording and ignores the link structure, embedding search needs an index kept in sync,
and letting the agent browse burns context on what is a string of quick relevance calls.

> **Status: scaffold.** The CLI prints usage and nothing else; the scoring client and the output
> formats are still to come. What exists is the project skeleton from
> [#3](https://github.com/mikekelly/s1m/issues/3), the parser from
> [#4](https://github.com/mikekelly/s1m/issues/4) — `s1m::parse` turns one markdown file into
> its title, frontmatter, heading sections with line ranges, and outgoing links — and the walk
> from [#6](https://github.com/mikekelly/s1m/issues/6): `s1m::traverse` searches the link graph
> best-first against an injected `s1m::scorer::Scorer`. The design lives in
> [docs/initial-plan.md](docs/initial-plan.md).

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

The plan's interface, once implemented:

```bash
s1m "how do we handle settlement timing for instant payouts" wiki/index.md
s1m --mode about --max-files 40 --format tree "chargebacks" wiki/index.md
```

Exit codes: `0` reading list returned, `1` nothing cleared the threshold, `2` error.

## Library

The parser later stages build on lives in `src/parse.rs` and is reachable without the CLI, so
it can be driven directly from tests:

| Item | What it does |
| --- | --- |
| `parse::parse(path, root)` | One file's `title`, `frontmatter`, `sections` (`heading`, `level`, `lines`) and `links` (`target` resolved against `root`, `anchor`, `sentence`, `heading`, `inRoot`) |
| `parse::preview(path)` | Title, frontmatter and first paragraph of a link target, for link previews |
| `traverse::traverse(config, scorer)` | Async: best-first walk of the link graph over a frontier keyed by path score — the product of the link scents on the best path to a file — under the `max_files`, `max_depth`, `threshold` and `fanout` budgets. One future per file per round, joined, so a round costs one round trip. Returns the visited files with their relevance, the scent that reached them, the `via` path and the outgoing links it judged |
| `scorer::Scorer` | The judgment seam, called once per visited file: a file's relevance plus a scent per outgoing link. `async fn` behind `#[async_trait]`, so a caller can join a round's calls; [#5](https://github.com/mikekelly/s1m/issues/5) implements it against Jev, tests use a fake |

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

## Development

| Command | What it does |
| --- | --- |
| `cargo test` | Unit, parser, traversal and CLI tests |
| `cargo build` | Debug build |
| `cargo fmt` | Format; `cargo fmt --check` to verify |
| `cargo clippy --all-targets -- -D warnings` | Lint, warnings are errors |

CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and
`cargo build` on the stable toolchain, then a smoke test on the built binary: `--help` prints
usage, no arguments exits 2 with usage on stderr, an unknown flag exits 2
([.github/workflows/ci.yml](.github/workflows/ci.yml)). `tests/cli.rs` spawns the same binary,
so the usage text, the streams and the exit codes above are what CI exercises.
