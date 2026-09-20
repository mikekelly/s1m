//! Tests the built binary, so the exit codes and streams are the ones a caller
//! actually sees.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use serde_json::{Map, Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_s1m");

/// The directory the child runs in, so every fixture path resolves.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// The query every test asks.
const QUERY: &str = "what is there to read";

/// The fixture wiki every test walks: an entry page, one hop on, and one beyond
/// that.
const ENTRY: &str = "tests/fixtures/cli/entry.md";
const NEXT: &str = "tests/fixtures/cli/next.md";
const DEEP: &str = "tests/fixtures/cli/deep.md";

/// An entry file that is not there, for the run that must name it.
const GONE: &str = "tests/fixtures/cli/gone.md";

/// A criterion of the caller's own, for the run that replaces a mode with one.
const CRITERIA: &str = "tests/fixtures/criteria/payouts.md";

/// What that file says, and so what a request under it must judge by.
const CRITERION: &str =
    "The content states the cut-off that decides whether an instant payout can still be sent.";

/// The `--seed-grep` fixture: an entry page, the two pages it links to in a
/// chain, and a page nothing links to.
const SEED_ENTRY: &str = "tests/fixtures/seed/index.md";
const SEED_LINKED: &str = "tests/fixtures/seed/notes/checklist.md";
const SEED_ARCHIVE: &str = "tests/fixtures/seed/notes/archive.md";
const SEED_ORPHAN: &str = "tests/fixtures/seed/orphan.md";

/// A query whose keywords are the orphan page's own: the pages the entry file
/// links to are the ones it meets the terms on least.
const SEED_QUERY: &str = "release checklist";

/// The id the scorer asks the file's own question under; every other question
/// in a request is a link, `link_0`, `link_1`, and so on.
const FILE_QUESTION: &str = "file_relevance";

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

/// A mode that is not one of the plan's three is the flag's vocabulary, not a
/// run: clap names the value it did not recognise and exits 2, the way it does
/// for a flag that does not exist.
#[test]
fn an_unknown_mode_exits_2_naming_it() {
    let output = run(&["--mode", "everything", "chargebacks", "wiki/index.md"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("--mode"), "{error}");
    assert!(error.contains("everything"), "{error}");
}

/// The plan's Relevance modes table, end to end: each mode sends its own
/// questions to the API, the reading list names the one that judged the
/// answers, and — the questions being part of what the cache keys on — a change
/// of mode buys fresh answers instead of reading the previous mode's.
#[test]
fn every_mode_sends_its_own_instructions_and_the_list_reports_it() {
    let api = FakeApi::new(3.0, 0.9);
    let cache = Cache::new();

    let mut reported = Vec::new();
    let mut file_questions = Vec::new();
    let mut link_questions = Vec::new();
    for mode in ["about", "useful-for", "answers"] {
        let before = api.requests().len();
        let output = run_with(&[QUERY, ENTRY, "--mode", mode], &api, &cache);

        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        let list = json(&output);
        reported.push(list["mode"].as_str().expect("a mode").to_string());

        let sent = api.requests();
        assert_eq!(
            sent.len() - before,
            3,
            "{mode} is a different question on the same three files, so it is bought, not read: {sent:?}"
        );
        file_questions.push(instructions(&sent[before], FILE_QUESTION));
        link_questions.push(instructions(&sent[before], "link_0"));
    }

    assert_eq!(reported, ["about", "useful-for", "answers"]);
    assert_eq!(
        distinct(file_questions),
        3,
        "every mode asks its own file question"
    );
    assert_eq!(distinct(link_questions), 3, "and its own link question");
}

/// `--criteria` replaces the mode, and the list says which file did: the
/// criterion is what the request judges by, even though `--mode` was given too,
/// because the plan's flag table says the file overrides it.
#[test]
fn a_criteria_file_sends_its_own_criterion_and_is_reported_as_the_mode() {
    let api = FakeApi::new(3.0, 0.9);
    let cache = Cache::new();

    let output = run_with(
        &[QUERY, ENTRY, "--mode", "about", "--criteria", CRITERIA],
        &api,
        &cache,
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let list = json(&output);
    assert_eq!(
        list["mode"], CRITERIA,
        "the criterion's own name, not --mode's"
    );

    let sent = api.requests();
    let entry = sent
        .iter()
        .find(|request| request["questions"].get("link_0").is_some())
        .expect("the entry page's request");
    let file = instructions(entry, FILE_QUESTION);
    assert!(file.contains(CRITERION), "{file}");
    let link = instructions(entry, "link_0");
    assert!(link.contains(CRITERION), "{link}");
    assert!(
        !file.contains("on the subject"),
        "the mode that was overridden is not asked as well: {file}"
    );
}

/// A criteria file that cannot be read, or that holds nothing, is the caller's
/// mistake, and it is named before anything is bought: no API call, nothing on
/// stdout, one line on stderr naming the file.
#[test]
fn an_unreadable_or_empty_criteria_file_exits_2_naming_it() {
    let api = FakeApi::new(3.0, 0.9);
    let cache = Cache::new();

    let missing = run_with(
        &[
            QUERY,
            ENTRY,
            "--criteria",
            "tests/fixtures/criteria/gone.md",
        ],
        &api,
        &cache,
    );
    assert_eq!(missing.status.code(), Some(2));
    assert!(missing.stdout.is_empty());
    let error = stderr(&missing);
    assert!(error.contains("gone.md"), "{error}");
    assert_eq!(error.lines().count(), 1, "{error}");

    let blank = cache.dir.join("blank.md");
    fs::write(&blank, "  \n").expect("a criteria file holding nothing");
    let output = run_with(
        &[QUERY, ENTRY, "--criteria", blank.to_str().expect("a path")],
        &api,
        &cache,
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(error.contains("blank.md"), "{error}");
    assert_eq!(error.lines().count(), 1, "{error}");

    assert_eq!(
        api.answered(),
        0,
        "the criteria file is read before anything is bought"
    );
}

/// The one-shot HTTP reply the fake server sends, and the request bodies it
/// saw, are enough to answer the two questions above; these read the wording
/// out of one.
fn instructions(request: &Value, id: &str) -> String {
    request["questions"][id]["instructions"]
        .as_str()
        .unwrap_or_else(|| panic!("{id} should carry instructions: {request}"))
        .to_string()
}

/// How many different strings a list holds.
fn distinct(mut values: Vec<String>) -> usize {
    values.sort();
    values.dedup();
    values.len()
}

// -------------------------------------------------------------- the fixtures

/// A cache directory of its own under the system temp directory, deleted when
/// the test ends.
///
/// Every run that queries must set `S1M_CACHE_DIR`: without it a test reads and
/// writes the caller's real cache, and answers stored by one test then decide
/// another test's `calls`. The name carries the process id while a counter
/// carries the test, so two test binaries running at once never share one.
struct Cache {
    dir: PathBuf,
}

impl Cache {
    fn new() -> Cache {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "s1m-cli-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("a cache directory under the system temp directory");
        Cache { dir }
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// The binary as a test runs it: from the crate root, with the key the CLI
/// demands, at `api`'s endpoint instead of the real one, and with `cache` to
/// read and write.
fn command(api: &FakeApi, cache: &Cache) -> Command {
    let mut command = Command::new(BIN);
    command
        .env("TYPESAFE_API_KEY", "test-key")
        .env("S1M_ENDPOINT", api.url())
        .env("S1M_CACHE_DIR", &cache.dir)
        .current_dir(MANIFEST_DIR);
    command
}

/// Runs the binary as a caller would, with the environment a test controls.
fn run_with(args: &[&str], api: &FakeApi, cache: &Cache) -> std::process::Output {
    command(api, cache)
        .args(args)
        .output()
        .expect("s1m should be runnable")
}

// ---------------------------------------------------------- the fake server

/// A stand-in for the Jev API on a loopback port: one request per connection,
/// answered with the `score` and `noul` the test was built with.
///
/// A real server rather than a mocked scorer, because what a query test needs
/// to defend is the whole way out to the network and back: the request bytes,
/// the serde types on both sides, the `S1M_ENDPOINT` the binary chooses, and
/// the scorer's retry loop. `src/jev.rs`'s own `FakeApi` defends that boundary
/// one layer down, at the client's API; this one defends it from the process,
/// which is where a caller stands.
struct FakeApi {
    url: String,
    address: SocketAddr,
    /// Every request body the binary sent, in order: what a test asserts the
    /// criterion's wording on, and the count of what the API was asked.
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeApi {
    /// A server that answers every file question with `score` and every link
    /// question with `noul`.
    ///
    /// `score` is the file's own Score, on the mode's scale: 3 is the top of the
    /// four levels every mode has, which the scorer reports as a relevance of
    /// 1.0. `noul` is what every link's scent comes back as, and it is what a
    /// test varies to put links above or below `--threshold`.
    fn new(score: f64, noul: f64) -> FakeApi {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let address = listener.local_addr().expect("the bound address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let recorded = Arc::clone(&requests);
        let flag = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { break };
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                recorded
                    .lock()
                    .expect("the lock is not poisoned")
                    .push(request.clone());
                let body = reply(&request, score, noul);
                let _ = stream.write_all(response(&body).as_bytes());
            }
        });
        FakeApi {
            url: format!("http://{address}/"),
            address,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    /// The URL to hand the binary as `S1M_ENDPOINT`.
    fn url(&self) -> &str {
        &self.url
    }

    /// Every request body the binary sent, in order.
    fn requests(&self) -> Vec<Value> {
        self.requests
            .lock()
            .expect("the lock is not poisoned")
            .clone()
    }

    /// Requests answered: what the binary reports as `calls`, seen from the
    /// other end of the wire.
    fn answered(&self) -> usize {
        self.requests().len()
    }
}

impl Drop for FakeApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so the thread notices and returns.
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One request body, read off the wire: the head until its `content-length`
/// bytes of body have arrived, so a body that arrives in pieces still parses.
fn read_request(stream: &mut TcpStream) -> Option<Value> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(head_end) = head_end(&buffer) {
            let length = content_length(&buffer[..head_end]);
            if buffer.len() >= head_end + length {
                let body = String::from_utf8_lossy(&buffer[head_end..head_end + length]);
                return serde_json::from_str(&body).ok();
            }
        }
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// Where the head ends in `buffer`, once both blank lines have arrived.
fn head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
}

/// The body length the head promised.
fn content_length(head: &[u8]) -> usize {
    String::from_utf8_lossy(head)
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(name, value)| {
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0)
}

/// The API's answer to one request: `score` for the file question and `noul`
/// for every link question.
///
/// The ids are the request's own keys, so a file with any number of links is
/// answered whole — the scorer treats one unanswered question as a failed
/// judgment, which would be the test's bug and not the binary's.
fn reply(request: &Value, score: f64, noul: f64) -> String {
    let questions = request["questions"]
        .as_object()
        .expect("a request should carry questions");
    let answers = questions
        .keys()
        .map(|id| {
            let answer = if id == FILE_QUESTION {
                json!({"type": "score", "score": score, "confidence": 0.9})
            } else {
                json!({"type": "noul", "noul": noul})
            };
            (id.clone(), answer)
        })
        .collect::<Map<String, Value>>();
    json!({
        "model": "jev-test",
        "answers": Value::Object(answers),
        "usage": {"input_tokens": 100, "output_tokens": 10},
    })
    .to_string()
}

/// A response as a one-shot HTTP/1.1 reply: the length is the body's, and the
/// connection closes after it.
fn response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

// --------------------------------------------------------------- the answers

/// The reading list a run printed.
fn json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout should be the reading list as JSON: {error}\n{}",
            stderr(output)
        )
    })
}

/// Everything the child wrote on stderr.
fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// The result paths, in the order the reading list gives them.
fn paths(list: &Value) -> Vec<&str> {
    list["results"]
        .as_array()
        .expect("results should be an array")
        .iter()
        .map(|result| result["path"].as_str().expect("a path"))
        .collect()
}

/// The one result for `path`.
fn result<'a>(list: &'a Value, path: &str) -> &'a Value {
    list["results"]
        .as_array()
        .expect("results should be an array")
        .iter()
        .find(|result| result["path"] == path)
        .unwrap_or_else(|| panic!("{path} should be in the reading list"))
}

// ------------------------------------------------------------- the tests

/// The whole point of the command: one query and an entry file come back as the
/// reading list on stdout, in the plan's field order, with the walk's paths
/// spelled the caller's way and the entry file marked as the one no link
/// reached.
#[test]
fn a_run_returns_the_reading_list_as_json_and_exits_0() {
    let api = FakeApi::new(3.0, 0.9);
    let cache = Cache::new();

    let output = run_with(&[QUERY, ENTRY], &api, &cache);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(output.stderr.is_empty(), "{}", stderr(&output));
    let list = json(&output);
    assert_eq!(list["query"], QUERY);
    assert_eq!(list["mode"], "useful-for");
    assert_eq!(list["visited"], 3);
    assert_eq!(list["calls"], 3);
    assert_eq!(
        paths(&list),
        [DEEP, ENTRY, NEXT],
        "one relevance for every file ties, so the list is by path"
    );

    let entry = result(&list, ENTRY);
    assert_eq!(
        entry["scent"],
        Value::Null,
        "no link reached the entry file"
    );
    assert_eq!(entry["via"], json!([]));
    assert_eq!(entry["relevance"].as_f64(), Some(1.0));
    assert_eq!(
        entry["links"],
        json!([{"target": NEXT, "scent": 0.9, "followed": true}])
    );

    let deep = result(&list, DEEP);
    assert_eq!(deep["relevance"].as_f64(), Some(1.0));
    assert_eq!(deep["scent"].as_f64(), Some(0.9));
    assert_eq!(deep["via"], json!([ENTRY, NEXT]), "two hops from the entry");
    assert_eq!(deep["links"], json!([]), "nothing links on from here");
    assert_eq!(api.answered(), 3, "one call per file judged");
}

/// `calls` is what the API was asked, not what the walk visited: the same
/// command over the same wiki a second time is answered from the cache, so the
/// list is the same and the bill is nothing.
#[test]
fn a_second_run_with_a_warm_cache_reports_no_calls() {
    let api = FakeApi::new(3.0, 0.9);
    let cache = Cache::new();
    let args = [QUERY, ENTRY];

    let cold = run_with(&args, &api, &cache);
    let warm = run_with(&args, &api, &cache);

    assert_eq!(cold.status.code(), Some(0), "{}", stderr(&cold));
    assert_eq!(warm.status.code(), Some(0), "{}", stderr(&warm));
    let cold = json(&cold);
    let warm = json(&warm);
    assert_eq!(cold["visited"], 3);
    assert_eq!(warm["visited"], 3, "the walk still visits every file");
    assert_eq!(cold["calls"], 3, "a cold cache buys every answer");
    assert_eq!(warm["calls"], 0, "a warm cache buys none of them");
    assert_eq!(paths(&warm), [DEEP, ENTRY, NEXT], "and ranks the same list");
    assert_eq!(api.answered(), 3, "the second run asked the API nothing");
}

/// A run whose links all fell below the threshold is not an answer: the list is
/// the entry files and nothing more, so the code says so on the way out and
/// says why on stderr, without taking the list away from a caller that wants
/// it.
#[test]
fn nothing_above_the_threshold_exits_1_with_the_entry_file() {
    let api = FakeApi::new(3.0, 0.1);
    let cache = Cache::new();

    let output = run_with(&[QUERY, ENTRY], &api, &cache);

    assert_eq!(output.status.code(), Some(1));
    let list = json(&output);
    assert_eq!(list["visited"], 1, "only the entry file was judged");
    assert_eq!(list["calls"], 1);
    assert_eq!(paths(&list), [ENTRY]);
    let error = stderr(&output);
    assert!(error.contains("nothing cleared the threshold"), "{error}");
    assert_eq!(error.lines().count(), 1, "{error}");
}

/// An entry file the caller named and cannot be read is the caller's mistake,
/// and it is named before anything is bought: no API call, nothing on stdout,
/// one line naming the file.
#[test]
fn a_missing_entry_file_exits_2_naming_it() {
    let api = FakeApi::new(3.0, 0.9);
    let cache = Cache::new();

    let output = run_with(&[QUERY, GONE], &api, &cache);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(error.contains("gone.md"), "{error}");
    assert_eq!(error.lines().count(), 1, "{error}");
    assert_eq!(
        api.answered(),
        0,
        "the entry files are read before anything is bought"
    );
}

/// Without `TYPESAFE_API_KEY` there is no scorer, so no run: the variable is
/// named on stderr and nothing is printed on stdout, because a caller whose
/// environment is wrong has no reading list to parse.
#[test]
fn a_missing_api_key_exits_2_saying_so() {
    let api = FakeApi::new(3.0, 0.9);
    let cache = Cache::new();

    let output = command(&api, &cache)
        .args([QUERY, ENTRY])
        .env_remove("TYPESAFE_API_KEY")
        .output()
        .expect("s1m should be runnable");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(error.contains("TYPESAFE_API_KEY"), "{error}");
    assert_eq!(error.lines().count(), 1, "{error}");
    assert_eq!(api.answered(), 0);
}

/// A threshold outside 0 to 1 follows nothing or everything, which is never
/// what a caller meant, so the flag is rejected where it is parsed and the
/// message says both the value and the range it is outside of.
#[test]
fn an_out_of_range_threshold_exits_2() {
    let api = FakeApi::new(3.0, 0.9);
    let cache = Cache::new();

    let output = run_with(&[QUERY, ENTRY, "--threshold", "2"], &api, &cache);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(
        error.contains("2 is not between 0 and 1"),
        "the message should name the value and the range it is outside of: {error}"
    );
    assert_eq!(api.answered(), 0);
}

/// `--root` with no entry file is a command line with nothing to walk, not an
/// empty reading list: it exits 2 saying so, and it says so before the scorer
/// is built — the key is missing here and the arguments are what get reported.
#[test]
fn a_root_without_an_entry_file_exits_2_saying_so() {
    let api = FakeApi::new(3.0, 0.9);
    let cache = Cache::new();

    let output = command(&api, &cache)
        .args([QUERY, "--root", "tests/fixtures/cli"])
        .env_remove("TYPESAFE_API_KEY")
        .output()
        .expect("s1m should be runnable");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(
        error.contains("a query and at least one entry file are required"),
        "{error}"
    );
    assert_eq!(error.lines().count(), 1, "{error}");
    assert_eq!(api.answered(), 0);
}

/// The point of `--seed-grep`: a page nothing links to is not reachable at all
/// without the flag, and is in the reading list with it — on the frontier like
/// an entry file, marked as a seed, beside the pages a link reached.
#[test]
fn seed_grep_reaches_a_page_no_link_points_to() {
    let api = FakeApi::new(3.0, 0.9);
    let cold = Cache::new();

    let without = run_with(&[SEED_QUERY, SEED_ENTRY], &api, &cold);

    assert_eq!(without.status.code(), Some(0), "{}", stderr(&without));
    assert_eq!(
        paths(&json(&without)),
        [SEED_ENTRY, SEED_ARCHIVE, SEED_LINKED],
        "the walk follows the entry file's links and stops"
    );

    // A cache of its own, so the seeded run buys every answer: seeding is one
    // more file judged.
    let fresh = Cache::new();
    let seeded = run_with(&[SEED_QUERY, SEED_ENTRY, "--seed-grep"], &api, &fresh);

    assert_eq!(seeded.status.code(), Some(0), "{}", stderr(&seeded));
    assert!(seeded.stderr.is_empty(), "{}", stderr(&seeded));
    let list = json(&seeded);
    assert_eq!(
        paths(&list),
        [SEED_ENTRY, SEED_ARCHIVE, SEED_LINKED, SEED_ORPHAN],
        "the keyword hit is on the frontier with the entry file"
    );
    assert_eq!(list["visited"], 4);
    assert_eq!(list["calls"], 4);

    let orphan = result(&list, SEED_ORPHAN);
    assert_eq!(orphan["seeded"], json!(true), "a seed says so");
    assert_eq!(orphan["via"], json!([]), "no path reached it");
    assert_eq!(orphan["scent"], Value::Null);

    let entry = result(&list, SEED_ENTRY);
    assert_eq!(entry["seeded"], json!(false));
    assert_eq!(entry["via"], json!([]));

    let archive = result(&list, SEED_ARCHIVE);
    assert_eq!(archive["seeded"], json!(false), "a link reached this one");
    assert_eq!(
        archive["via"].as_array().map(Vec::len),
        Some(1),
        "one hop from the page the entry links to"
    );
}

/// `--seed-count` is not a flag of its own: a count with no `--seed-grep` is a
/// usage error rather than a silent no-op, and it is caught before anything is
/// bought.
#[test]
fn a_seed_count_without_seed_grep_exits_2() {
    let api = FakeApi::new(3.0, 0.9);
    let cache = Cache::new();

    let output = run_with(&[SEED_QUERY, SEED_ENTRY, "--seed-count", "1"], &api, &cache);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(error.contains("--seed-grep"), "{error}");
    assert_eq!(api.answered(), 0);
}

/// `--seed-count` is how many of the hits are used, and the entry file the
/// caller named is left out before the count is applied rather than after: one
/// takes the best hit, which is not the entry file, and two takes the orphan as
/// well.
#[test]
fn seed_count_says_how_many_hits_are_used() {
    let api = FakeApi::new(3.0, 0.9);
    let cache = Cache::new();

    let one = run_with(
        &[SEED_QUERY, SEED_ENTRY, "--seed-grep", "--seed-count", "1"],
        &api,
        &cache,
    );

    assert_eq!(one.status.code(), Some(0), "{}", stderr(&one));
    let one = json(&one);
    assert_eq!(
        paths(&one),
        [SEED_ENTRY, SEED_ARCHIVE, SEED_LINKED],
        "one hit, and the orphan is not it"
    );
    assert_eq!(
        result(&one, SEED_LINKED)["seeded"],
        json!(true),
        "the hit used is the best of them, not the entry file the caller named"
    );

    let two = run_with(
        &[SEED_QUERY, SEED_ENTRY, "--seed-grep", "--seed-count", "2"],
        &api,
        &cache,
    );

    assert_eq!(two.status.code(), Some(0), "{}", stderr(&two));
    let two = json(&two);
    assert_eq!(
        paths(&two),
        [SEED_ENTRY, SEED_ARCHIVE, SEED_LINKED, SEED_ORPHAN],
        "the second hit is the orphan"
    );
    assert_eq!(result(&two, SEED_ORPHAN)["seeded"], json!(true));
}
