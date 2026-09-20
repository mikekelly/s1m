# AGENTS.md

Rules for any coding session on this repository, whoever starts it.

- Rust, edition 2024 — rustc 1.85 or newer.
- Before a PR: `cargo fmt --all --check`,
  `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`.
  CI runs those plus `cargo build --locked`.
- Write the test first; add a dependency only with the issue that needs it.
- Plan and design goals live in [`docs/initial-plan.md`](docs/initial-plan.md).
- Never reference a private wiki or repository in code, fixtures, docs, PRs or
  comments: this repository is open source.
