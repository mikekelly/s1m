//! s1m CLI entry point.
//!
//! Scaffold only: the query pipeline lands in later issues. What is fixed here
//! is the usage text and the exit codes from the plan (0 = reading list
//! returned, 1 = nothing cleared the threshold, 2 = error).

use clap::Parser;

/// Listed in the help so an agent reading `--help` today is not misled about
/// the interface #8 and later issues will implement.
const AFTER_HELP: &str = "\
Planned options, not implemented yet:
  --mode, --criteria, --max-files, --max-depth, --threshold, --fanout,
  --seed-grep, --format, --root

Exit codes:
  0  reading list returned
  1  nothing cleared the threshold
  2  error

This build is a scaffold: only -h/--help and -V/--version are implemented.";

#[derive(Debug, Parser)]
#[command(
    name = "s1m",
    version,
    about = "Ranks local files for a query so an agent reads only what matters",
    override_usage = "s1m <query> <entry-file...> [OPTIONS]",
    after_help = AFTER_HELP,
    arg_required_else_help = true
)]
struct Cli {}

fn main() {
    // Parsing is the whole stub. `-h`/`--help`, `-V`/`--version` and a bare
    // `s1m` (missing its query and entry files) all print usage; clap exits 0
    // for help and 2 for everything else.
    Cli::parse();
}
