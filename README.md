# s1m

s1m reads local files and ranks them for a query, so an LLM agent opens only what matters.

You give it a query and one or more entry point files. It scores each file and each outgoing
link with a fast judgment model, follows the most promising links first, and returns a ranked
reading list with paths, line ranges and scores. The alternatives each miss something: grep
matches wording and ignores the link structure, embedding search needs an index kept in sync,
and letting the agent browse burns context on what is a string of quick relevance calls.

> **Status: scaffold.** This repository currently contains only the project skeleton from
> [#3](https://github.com/mikekelly/s1m/issues/3). The CLI prints usage and nothing else;
> linking, traversal and scoring are still to come. The design lives in
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

## Development

| Command | What it does |
| --- | --- |
| `cargo test` | Unit and CLI tests |
| `cargo build` | Debug build |
| `cargo fmt` | Format; `cargo fmt --check` to verify |
| `cargo clippy --all-targets -- -D warnings` | Lint, warnings are errors |

CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and
`cargo build` on the stable toolchain, then a smoke test on the built binary: `--help` prints
usage, no arguments exits 2 with usage on stderr, an unknown flag exits 2
([.github/workflows/ci.yml](.github/workflows/ci.yml)). `tests/cli.rs` spawns the same binary,
so the usage text, the streams and the exit codes above are what CI exercises.
