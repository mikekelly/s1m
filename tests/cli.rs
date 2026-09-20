//! Tests the built binary, so the exit codes and streams are the ones a caller
//! actually sees.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_s1m");

fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("s1m should be runnable")
}

#[test]
fn no_arguments_prints_usage_and_exits_2() {
    let output = run(&[]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
}

#[test]
fn help_prints_usage() {
    let output = run(&["--help"]);

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
}

#[test]
fn unknown_flag_exits_2_and_names_itself() {
    let output = run(&["--nope"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--nope"));
}

#[test]
fn planned_arguments_are_rejected() {
    let output = run(&["--mode", "about", "chargebacks", "wiki/index.md"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--mode"));
}
