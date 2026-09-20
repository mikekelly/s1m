//! The evaluation harness as a caller meets it: the committed cache reproduces
//! the committed report, with no key and no network.
//!
//! This is the acceptance criterion for
//! [#11](https://github.com/mikekelly/s1m/issues/11) — *results reproducible
//! from the cache* — stated as the thing a reader would do: run the command the
//! report opens with, and diff. The run has to be offline for the claim to mean
//! anything, so the key is removed from the child's environment: a judgment the
//! cache cannot answer then fails the run instead of quietly buying an answer.

use std::process::Command;

/// The built harness, the way `cargo test` points at it.
const EVAL: &str = env!("CARGO_BIN_EXE_eval");

/// The command the report documents, spelled the way it does: paths relative to
/// the package root, which is where `cargo test` runs.
const WIKI: &str = "eval/wikis/llm-wiki-manager/wiki";
const GOLD: &str = "eval/gold/llm-wiki-manager.json";
const CACHE: &str = "eval/cache";
const REPORT: &str = "eval/REPORT.md";

#[test]
fn the_committed_cache_reproduces_the_committed_report() {
    let output = Command::new(EVAL)
        .args(["--wiki", WIKI, "--gold", GOLD, "--cache", CACHE])
        .env_remove("TYPESAFE_API_KEY")
        .env_remove("S1M_ENDPOINT")
        .output()
        .expect("the harness should be runnable");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "the run failed: {stderr}");
    assert!(
        stderr.contains("TYPESAFE_API_KEY is not set"),
        "the run has to be the keyless one: {stderr}"
    );

    let committed = std::fs::read_to_string(REPORT).expect("the committed report");
    assert_eq!(
        String::from_utf8(output.stdout).expect("the report is UTF-8"),
        committed,
        "a keyless rerun against the committed cache is the committed report"
    );
}
