//! Tests the built binary, so the exit codes and streams are the ones a caller
//! actually sees.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
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

/// An entry page that links on to `next.md` and to a page that is not there:
/// the wiki shape a walk meets when one page of it cannot be judged, and one
/// hop further than `entry.md`.
const BROKEN_LINK_ENTRY: &str = "tests/fixtures/cli/broken.md";

/// A criterion of the caller's own, for the run that replaces a mode with one.
const CRITERIA: &str = "tests/fixtures/criteria/payouts.md";

/// What that file says, and so what a request under it must judge by.
const CRITERION: &str =
    "The content states the cut-off that decides whether an instant payout can still be sent.";

/// The `.s1mignore` fixture: an entry page, a page it links to, and two pages
/// the root's `.s1mignore` covers — one behind a matched directory, which the
/// entry links to, and one matched by name, which is a keyword hit and nothing
/// else.
const IGNORE_ROOT: &str = "tests/fixtures/ignore";
const IGNORE_ENTRY: &str = "tests/fixtures/ignore/entry.md";
const IGNORE_PUBLIC: &str = "tests/fixtures/ignore/public.md";
const IGNORE_VAULT: &str = "tests/fixtures/ignore/private/vault.md";
const IGNORE_DRAFTS: &str = "tests/fixtures/ignore/drafts.md";

/// The query the ignore fixture's entry page and its links are written for.
const IGNORE_QUERY: &str = "settlement timing for instant payouts";

/// A line from the matched page that must never leave the machine, so a test
/// can look for it in every request the binary sent.
const IGNORE_SECRET: &str = "Combination 4471 opens the safe";

/// A root whose `.s1mignore` does not parse, and the page beside it.
const BROKEN_ENTRY: &str = "tests/fixtures/ignore-broken/entry.md";

/// What the API answers when a state is over its budget: the 400 a page of 17k
/// characters with 92 previewed links met ([#37]).
///
/// The body is written the way a gateway in front of the API may write one —
/// with newlines in it — because the notice the CLI prints about it goes to a
/// caller who reads stderr by line.
///
/// [#37]: https://github.com/mikekelly/s1m/issues/37
const OVER_BUDGET: &str =
    "{\"detail\":\n  {\"error_type\": \"max_tokens_exceeded\",\n   \"state\": \"too long\"}\n}";

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

/// The name reads as "sim", and the first guess it invites is a simulator. The
/// help text is where that is answered: both the one-line description and the
/// paragraph `--help` opens with lead with what s1m reads and ranks, and say
/// what it is not, before a flag is explained.
#[test]
fn help_leads_with_what_s1m_reads_and_says_what_it_is_not() {
    let described = "reads local markdown files and ranks them for a query";

    let long = run(&["--help"]);
    let long = stdout(&long);
    assert!(long.contains(described), "{long}");
    assert!(long.contains("not a simulator"), "{long}");
    assert!(long.contains(".s1mignore"), "{long}");
    assert!(
        long.find(described).expect("the phrase") < long.find("Options:").expect("the flags"),
        "what s1m is comes before its flags: {long}"
    );

    let short = stdout(&run(&["-h"]));
    assert!(
        short.contains("Reads local markdown files and ranks them for a query"),
        "{short}"
    );
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
    let api = FakeApi::new(3.0, 0.9, 0.7);
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
    let api = FakeApi::new(3.0, 0.9, 0.7);
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
    let api = FakeApi::new(3.0, 0.9, 0.7);
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

/// The hidden opt-out flags of [#46] are a run's state: the run that names none
/// of them carries the whole of it, and each flag takes exactly its own part
/// away and leaves the rest.
///
/// This is the flag-to-field wiring the unit tests cannot see: the switches are
/// [`s1m::jev::Context`]'s, and nothing but this checks that the flag a person
/// types is the field the shipped state is built from.
///
/// [#46]: https://github.com/mikekelly/s1m/issues/46
#[test]
fn the_hidden_ablation_flags_take_the_state_away() {
    let api = FakeApi::new(3.0, 0.9, 0.7);
    let cache = Cache::new();
    let output = run_with(&[QUERY, ENTRY], &api, &cache);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    // What ships: the target's own headings and the anchor text of its own
    // links. `next.md` is an H1, a sentence and one link, so it has no headings
    // to carry and one lead, and the question reaches past the link it names.
    let requests = api.requests();
    let entry = request_for(&requests, ENTRY);
    assert_eq!(
        entry["state"]["links"][0]["target_preview"]["headings"],
        json!([])
    );
    assert_eq!(
        entry["state"]["links"][0]["target_preview"]["leads_to"],
        json!(["Deep"])
    );
    let two_hop = instructions(entry, "link_0");
    assert!(
        two_hop.contains("directly or through the pages it links to"),
        "the two-hop question is what ships: {two_hop}"
    );

    // Each state flag takes its own field away and leaves the other one.
    for (flag, gone) in [
        ("--no-preview-headings", "headings"),
        ("--no-preview-leads", "leads_to"),
    ] {
        let api = FakeApi::new(3.0, 0.9, 0.7);
        let cache = Cache::new();
        let output = run_with(&[QUERY, ENTRY, flag], &api, &cache);
        assert_eq!(output.status.code(), Some(0), "{flag}: {}", stderr(&output));

        let requests = api.requests();
        let entry = request_for(&requests, ENTRY);
        let preview = &entry["state"]["links"][0]["target_preview"];
        assert!(
            preview.get(gone).is_none(),
            "{flag} left {gone} in: {preview}"
        );
        for other in ["headings", "leads_to"] {
            if other != gone {
                assert!(
                    preview.get(other).is_some(),
                    "{flag} took {other} with it: {preview}"
                );
            }
        }
        assert_eq!(
            instructions(entry, "link_0"),
            two_hop,
            "{flag} changed the question"
        );
    }

    // And the question flag is the question, not the state.
    let api = FakeApi::new(3.0, 0.9, 0.7);
    let cache = Cache::new();
    let output = run_with(&[QUERY, ENTRY, "--one-hop-links"], &api, &cache);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    let requests = api.requests();
    let entry = request_for(&requests, ENTRY);
    let one_hop = instructions(entry, "link_0");
    assert!(
        !one_hop.contains("directly or through") && one_hop.contains("`links[0]`"),
        "the one-hop question is asked: {one_hop}"
    );
    assert!(
        entry["state"]["links"][0]["target_preview"]
            .get("headings")
            .is_some(),
        "and the state is not what it changed"
    );
}

/// The hidden `--wording` flag of [#52] is how a register reaches a run: the
/// run that names none sends the questions that ship, and every name re-words
/// the part of the request its register is about, leaving the criterion — and
/// the state, but for the one register that defines a reader — where they were.
///
/// Every name is asked for by name, because nothing else can catch a wrong arm:
/// `src/jev.rs`'s tests call [`s1m::jev::Wording::word`] directly and the eval
/// harness walks every register at once, so `--wording necessity` reaching the
/// wrong register would leave all of them green and only show up in what a run
/// buys. The flag's own vocabulary is this test's subject; the sentences are
/// `src/jev.rs`'s.
///
/// [#52]: https://github.com/mikekelly/s1m/issues/52
#[test]
fn the_hidden_wording_flag_asks_every_register_by_name() {
    // One run of one register, and the request the entry page was judged in.
    let sent = |wording: Option<&str>| -> Value {
        let api = FakeApi::new(3.0, 0.9, 0.7);
        let cache = Cache::new();
        let mut args = vec![QUERY, ENTRY];
        if let Some(wording) = wording {
            args.extend_from_slice(&["--wording", wording]);
        }
        let output = run_with(&args, &api, &cache);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{wording:?}: {}",
            stderr(&output)
        );
        request_for(&api.requests(), ENTRY).clone()
    };

    // What ships: the questions every other run is read against.
    let shipped = sent(None);
    let shipped_link = instructions(&shipped, "link_0");
    let shipped_section = instructions(&shipped, "section_0");
    assert!(
        shipped["state"].get("reader").is_none(),
        "the shipped state defines no reader: {}",
        shipped["state"]
    );
    assert_eq!(
        shipped_section,
        "Does `sections[0]` — the part of `file` under that heading, at the lines given — hold \
         something someone doing `query` would use: a step, a rule, a value, a decision?",
        "the section question #52 decided on"
    );

    // The click register: the link question, and nothing else.
    let navigator = sent(Some("navigator"));
    assert_eq!(
        instructions(&navigator, "link_0"),
        "A person looking for `query` is reading `file`. Would they click `links[0]` next?"
    );
    assert_eq!(instructions(&navigator, "section_0"), shipped_section);
    assert_eq!(
        navigator["questions"][FILE_QUESTION]["criteria"],
        shipped["questions"][FILE_QUESTION]["criteria"]
    );
    assert_eq!(
        navigator["state"], shipped["state"],
        "the wording moved the state"
    );

    // The position register: the question, and the destination the mode owns.
    for (mode, wanted) in [
        (None, "the pages that answer what `query` describes"),
        (Some("about"), "the pages on the subject of `query`"),
        (Some("answers"), "the pages that answer `query`"),
    ] {
        let api = FakeApi::new(3.0, 0.9, 0.7);
        let cache = Cache::new();
        let mut args = vec![QUERY, ENTRY, "--wording", "path"];
        if let Some(mode) = mode {
            args.extend_from_slice(&["--mode", mode]);
        }
        let output = run_with(&args, &api, &cache);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{mode:?}: {}",
            stderr(&output)
        );

        let requests = api.requests();
        let entry = request_for(&requests, ENTRY);
        assert_eq!(
            instructions(entry, "link_0"),
            format!("Is `links[0]` on the way from `file` to {wanted}?"),
            "the wording and the mode compose: {mode:?}"
        );
    }

    // The sharper no: the shipped question, and a no that has to lead nowhere.
    let sharp = sent(Some("sharp-no"));
    assert_eq!(instructions(&sharp, "link_0"), shipped_link);
    assert_eq!(
        sharp["questions"]["link_0"]["criteria"]["true"],
        shipped["questions"]["link_0"]["criteria"]["true"]
    );
    assert_eq!(
        sharp["questions"]["link_0"]["criteria"]["false"],
        "The target is about something else, and nothing it links to is about `query`."
    );

    // The rules: the shipped question, inside the structured instructions.
    let rules = sent(Some("rules"));
    assert_eq!(
        rules["questions"]["link_0"]["instructions"]["question"],
        json!(shipped_link)
    );
    assert_eq!(
        rules["questions"]["link_0"]["instructions"]["rules"]
            .as_array()
            .expect("a rule list")
            .len(),
        3
    );

    // The section registers: one that asks a sharper question, and the one that
    // asks what shipped before the decision.
    let necessity = sent(Some("necessity"));
    assert_eq!(
        instructions(&necessity, "section_0"),
        "Would someone doing `query` be worse off for skipping `sections[0]` — the part of \
         `file` under that heading, at the lines given?"
    );
    assert_eq!(instructions(&necessity, "link_0"), shipped_link);

    let legacy = sent(Some("section-legacy"));
    assert_eq!(
        instructions(&legacy, "section_0"),
        "Is `sections[0]` — the part of `file` under that heading, at the lines given — useful \
         for someone doing what `query` describes?"
    );
    assert_eq!(instructions(&legacy, "link_0"), shipped_link);

    // The file registers: the ladder, and the question above it.
    for (name, level) in [
        (
            "reader-action",
            "skim and leave — a glance, and nothing `query` needs.",
        ),
        (
            "answer-bearing",
            "part of it — `file` itself holds part of what `query` needs.",
        ),
    ] {
        let entry = sent(Some(name));
        assert!(
            entry["questions"][FILE_QUESTION]["criteria"]
                .as_array()
                .expect("a ladder")
                .contains(&json!(level)),
            "{name}'s ladder: {}",
            entry["questions"][FILE_QUESTION]["criteria"]
        );
        assert_eq!(instructions(&entry, "section_0"), shipped_section);
        assert_eq!(instructions(&entry, "link_0"), shipped_link);
    }

    // The cross-cutting register: the definition in the state, and every
    // question about it.
    let reader = sent(Some("reader"));
    assert_eq!(
        reader["state"]["reader"],
        json!("an agent that must complete `query` by reading pages")
    );
    for id in [FILE_QUESTION, "section_0", "link_0"] {
        assert!(
            instructions(&reader, id).contains("`reader`"),
            "{id} is asked about the reader: {}",
            instructions(&reader, id)
        );
    }
    let mut without_reader = reader["state"].clone();
    without_reader
        .as_object_mut()
        .expect("a state object")
        .remove("reader");
    assert_eq!(
        without_reader, shipped["state"],
        "the reader is all this register adds to the state"
    );
}

/// The request one file was judged in, found by the fixture path it ends with.
fn request_for<'a>(requests: &'a [Value], page: &str) -> &'a Value {
    requests
        .iter()
        .find(|request| {
            request["state"]["file"]["path"]
                .as_str()
                .is_some_and(|path| path.ends_with(page))
        })
        .unwrap_or_else(|| panic!("{page} was never judged: {requests:?}"))
}

/// The one-shot HTTP reply the fake server sends, and the request bodies it
/// saw, are enough to answer the questions above; these read the wording out of
/// one.
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
    /// A server that answers every file question with `score`, every section
    /// question with `section` and every link question with `noul`.
    ///
    /// `score` is the file's own Score, on the mode's scale: 3 is the top of the
    /// four levels every mode has, which the scorer reports as a relevance of
    /// 1.0. `noul` is what every link's scent comes back as, and it is what a
    /// test varies to put links above or below `--threshold`; `section` does
    /// the same for the sections `--threshold` keeps.
    fn new(score: f64, noul: f64, section: f64) -> FakeApi {
        FakeApi::answering(score, noul, section, None)
    }

    /// The same server, refusing the page whose path ends in `page`: the API's
    /// own answer when a state is over its budget — the `max_tokens_exceeded`
    /// of [#37] — which is what a page that cannot be judged looks like from
    /// the CLI.
    ///
    /// [#37]: https://github.com/mikekelly/s1m/issues/37
    fn refusing(page: &str, score: f64, noul: f64, section: f64) -> FakeApi {
        FakeApi::answering(score, noul, section, Some((page.to_string(), false)))
    }

    /// The same server, refusing `page`'s Choice question alone: the file's own
    /// Score and its sections are answered, and the post that asks about the
    /// page's links is the one the API rejects — the 422 an empty question map
    /// earns ([#47]).
    ///
    /// [#47]: https://github.com/mikekelly/s1m/issues/47
    fn refusing_choice(page: &str, score: f64, noul: f64, section: f64) -> FakeApi {
        FakeApi::answering(score, noul, section, Some((page.to_string(), true)))
    }

    fn answering(score: f64, noul: f64, section: f64, refuse: Option<(String, bool)>) -> FakeApi {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let address = listener.local_addr().expect("the bound address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let recorded = Arc::clone(&requests);
        let flag = Arc::clone(&stop);
        let refused = refuse;
        let thread = thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { break };
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let refused = refused.as_ref().is_some_and(|(page, choice_only)| {
                    let on_page = request["state"]["file"]["path"]
                        .as_str()
                        .is_some_and(|path| path.ends_with(page.as_str()));
                    let choice = request["questions"].get(CHOICE_QUESTION).is_some();
                    on_page && (!*choice_only || choice)
                });
                recorded
                    .lock()
                    .expect("the lock is not poisoned")
                    .push(request.clone());
                let body = if refused {
                    OVER_BUDGET.to_string()
                } else {
                    reply(&request, score, noul, section)
                };
                let status = if refused { 400 } else { 200 };
                let _ = stream.write_all(response(status, &body).as_bytes());
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

/// The id the relative judge asks its one question under, and the option that
/// says none of the page's links is worth following.
const CHOICE_QUESTION: &str = "link_choice";
const NONE_OPTION: &str = "none";

/// The API's answer to one request: `score` for the file question, `noul` for
/// every link question, and a Choice over the options a Choice question
/// defines.
///
/// The ids are the request's own keys, so a file with any number of sections
/// and links is answered whole — the scorer treats one unanswered question as a
/// failed judgment, which would be the test's bug and not the binary's.
fn reply(request: &Value, score: f64, noul: f64, section: f64) -> String {
    let questions = request["questions"]
        .as_object()
        .expect("a request should carry questions");
    let answers = questions
        .keys()
        .map(|id| {
            let answer = if id == FILE_QUESTION {
                json!({"type": "score", "score": score, "confidence": 0.9})
            } else if id == CHOICE_QUESTION {
                choice_answer(&questions[id])
            } else if id.starts_with("section_") {
                json!({"type": "noul", "noul": section})
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

/// The answer to one Choice question: one option holding almost all of the
/// mass, the way the model answers a hub's links, and `none` under it.
///
/// The keys are the question's own options, so the answer is read back onto the
/// links the question defined whichever ones they are.
fn choice_answer(question: &Value) -> Value {
    let options = question["criteria"]
        .as_object()
        .expect("a Choice question defines its options");
    let links: Vec<&String> = options.keys().filter(|key| *key != NONE_OPTION).collect();
    let mut probabilities = Map::new();
    for (position, key) in links.iter().enumerate() {
        // Most of the mass on the first option, a sliver on the rest: the shape
        // the walk has to keep one link of.
        probabilities.insert(
            (*key).clone(),
            json!(if position == 0 { 0.9 } else { 0.002 }),
        );
    }
    probabilities.insert(NONE_OPTION.to_string(), json!(0.01));
    json!({
        "type": "choice",
        "choice": links.first().map(|key| (*key).clone()).unwrap_or_else(|| NONE_OPTION.to_string()),
        "probabilities": Value::Object(probabilities),
        "confidence": 0.8,
    })
}

/// A response as a one-shot HTTP/1.1 reply: the length is the body's, and the
/// connection closes after it.
fn response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        529 => "Overloaded",
        _ => "Bad Request",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\n\
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

/// Everything the child wrote on stdout, as text: what the `md` and `tree`
/// formats print, where `json` is read as a [`Value`].
fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
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

/// The records of a trace file, each line parsed on its own: a trace is JSON
/// Lines, and every line of it stands alone.
fn records(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}")))
        .collect()
}

/// Those records as `(event, what it is about)`: the path for a record about a
/// file, and `source -> target` for one about a link.
fn events(records: &[Value]) -> Vec<(String, String)> {
    records
        .iter()
        .map(|record| {
            let event = record["event"].as_str().expect("an event name").to_string();
            let what = match record["path"].as_str() {
                Some(path) => path.to_string(),
                None => format!(
                    "{} -> {}",
                    record["source"].as_str().expect("a source"),
                    record["target"].as_str().expect("a target")
                ),
            };
            (event, what)
        })
        .collect()
}

// ------------------------------------------------------------- the tests

/// The whole point of the command: one query and an entry file come back as the
/// reading list on stdout, in the plan's field order, with the walk's paths
/// spelled the caller's way and the entry file marked as the one no link
/// reached.
#[test]
fn a_run_returns_the_reading_list_as_json_and_exits_0() {
    let api = FakeApi::new(3.0, 0.9, 0.7);
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
        entry["sections"],
        json!([{"heading": "Entry", "lines": [1, 4], "score": 0.7}]),
        "the sections the caller reads from, on the parser's lines"
    );
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
    let api = FakeApi::new(3.0, 0.9, 0.7);
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

/// `--trace` writes what the walk did as it did it, and changes nothing else:
/// the file is one JSON object per line, and stdout is the reading list the
/// same run prints without the flag.
///
/// The traces come off one cache, so the cold run and the warm one are the same
/// walk: a replay has to be able to tell them apart, which is what `cached` is
/// for. The traces are written under this test's cache directory, which is a
/// temp directory the test removes.
#[test]
fn a_trace_is_written_as_the_walk_goes() {
    let api = FakeApi::new(3.0, 0.9, 0.7);
    let cache = Cache::new();
    let cold_path = cache.dir.join("cold.jsonl");
    let warm_path = cache.dir.join("warm.jsonl");

    let cold = run_with(
        &[QUERY, ENTRY, "--trace", cold_path.to_str().expect("a path")],
        &api,
        &cache,
    );
    // The same run from the now warm cache, and the same run with no flag at
    // all: what `--trace` may not change.
    let warm = run_with(
        &[QUERY, ENTRY, "--trace", warm_path.to_str().expect("a path")],
        &api,
        &cache,
    );
    let plain = run_with(&[QUERY, ENTRY], &api, &cache);

    assert_eq!(cold.status.code(), Some(0), "{}", stderr(&cold));
    assert_eq!(warm.status.code(), Some(0), "{}", stderr(&warm));
    assert_eq!(cold.stderr, b"", "{}", stderr(&cold));
    assert_eq!(
        stdout(&plain),
        stdout(&warm),
        "the list is the same with and without --trace"
    );

    let cold = records(&cold_path);
    assert_eq!(
        events(&cold),
        [
            ("popped", "entry.md"),
            ("requested", "entry.md"),
            ("answered", "entry.md"),
            ("admitted", "entry.md -> next.md"),
            ("result", "entry.md"),
            ("popped", "next.md"),
            ("requested", "next.md"),
            ("answered", "next.md"),
            ("admitted", "next.md -> deep.md"),
            ("result", "next.md"),
            ("popped", "deep.md"),
            ("requested", "deep.md"),
            ("answered", "deep.md"),
            ("result", "deep.md"),
        ]
        .map(|(event, what)| (event.to_string(), what.to_string())),
        "the walk's own order: one visit at a time, entries first"
    );

    let answered: Vec<&Value> = cold
        .iter()
        .filter(|record| record["event"] == "answered")
        .collect();
    assert_eq!(answered.len(), 3, "one answer per file judged");
    for record in &answered {
        assert_eq!(record["cached"], false, "a cold run bought this: {record}");
        assert!(
            record["latency_ms"].as_u64().is_some(),
            "and reports what the call took: {record}"
        );
    }

    let warm = records(&warm_path);
    assert_eq!(
        events(&warm),
        events(&cold),
        "a warm run walks the same walk, and `cached` is what tells them apart"
    );
    for record in warm.iter().filter(|record| record["event"] == "answered") {
        assert_eq!(record["cached"], true, "a warm run: {record}");
    }
    assert_eq!(
        api.answered(),
        3,
        "only the cold run asked the API anything"
    );
}

/// A trace that cannot be written is the caller's mistake and the run's error,
/// and it is caught before anything is read or bought: nothing is asked of the
/// API for a run whose trace is not the trace the caller asked for.
#[test]
fn a_trace_that_cannot_be_written_exits_2_without_a_call() {
    let api = FakeApi::new(3.0, 0.9, 0.7);
    let cache = Cache::new();

    let output = run_with(
        &[QUERY, ENTRY, "--trace", "tests/fixtures/cli"],
        &api,
        &cache,
    );

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(error.contains("trace"), "{error}");
    assert_eq!(api.answered(), 0, "the run stopped before its first call");
}

/// A section the model scored below `--threshold` is dropped from the file's
/// `sections`: the same section comes back under a `--threshold` of 0.5 and is
/// dropped under the default 0.6, with the cache answering both runs.
#[test]
fn a_section_below_the_threshold_is_dropped() {
    let api = FakeApi::new(3.0, 0.9, 0.55);
    let cache = Cache::new();

    let kept = json(&run_with(
        &[QUERY, ENTRY, "--threshold", "0.5"],
        &api,
        &cache,
    ));
    assert_eq!(
        result(&kept, ENTRY)["sections"],
        json!([{"heading": "Entry", "lines": [1, 4], "score": 0.55}])
    );

    let dropped = json(&run_with(&[QUERY, ENTRY], &api, &cache));
    assert_eq!(
        result(&dropped, ENTRY)["sections"],
        json!([]),
        "0.55 is under the default threshold of 0.6"
    );
    assert_eq!(api.answered(), 3, "the cache answers both runs");
}

/// A run whose links all fell below the threshold is not an answer: the list is
/// the entry files and nothing more, so the code says so on the way out and
/// says why on stderr, without taking the list away from a caller that wants
/// it.
#[test]
fn nothing_above_the_threshold_exits_1_with_the_entry_file() {
    let api = FakeApi::new(3.0, 0.1, 0.7);
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

/// A page the API will not judge — a state over its budget, the
/// `max_tokens_exceeded` of [#37] — is skipped, not fatal: the walk keeps the
/// pages it judged, says which page it dropped in one line on stderr, and exits
/// with the code its reading list earned.
///
/// [#37]: https://github.com/mikekelly/s1m/issues/37
#[test]
fn a_page_that_cannot_be_judged_is_skipped_and_the_walk_carries_on() {
    // The page that cannot be judged is two hops on, where `broken.md` links to
    // `next.md` and `next.md` links to `deep.md`: the walk still reaches beyond
    // the entry file, so the run earns a 0 and prints a list.
    let api = FakeApi::refusing(DEEP, 3.0, 0.9, 0.7);
    let cache = Cache::new();

    let output = run_with(&[QUERY, BROKEN_LINK_ENTRY], &api, &cache);

    assert_eq!(
        output.status.code(),
        Some(0),
        "a walked page was judged, so the run is a reading list: {}",
        stderr(&output)
    );
    let list = json(&output);
    assert_eq!(
        paths(&list),
        [BROKEN_LINK_ENTRY, NEXT],
        "the page that could not be judged is not in the list, and the pages around it are"
    );
    let error = stderr(&output);
    assert!(
        error.contains(DEEP) && error.contains("max_tokens_exceeded"),
        "the page and the reason are on stderr: {error}"
    );
    assert_eq!(
        error.lines().count(),
        2,
        "one line for each page the walk dropped — the link that is not there, \
         and the judgment whose reason carries newlines of its own — and nothing \
         else: {error}"
    );
}

/// A Choice the API refuses is a page that cannot be judged, not a page with no
/// links: the link that reached it is still reported as followed, the page is
/// named on stderr, and the walk keeps the pages it judged.
///
/// The refusal is aimed at the Choice request alone — the 422 the API answers a
/// post whose questions are empty with ([#47]) — so what is under test is that a
/// link judgment that fails is a hole in the walk, and never a page silently
/// judged as one with nothing to follow.
///
/// [#47]: https://github.com/mikekelly/s1m/issues/47
#[test]
fn a_refused_choice_is_skipped_and_not_a_page_without_links() {
    let api = FakeApi::refusing_choice(NEXT, 3.0, 0.9, 0.7);
    let cache = Cache::new();

    let output = run_with(&[QUERY, ENTRY, "--scorer", "choice"], &api, &cache);

    let list = json(&output);
    assert_eq!(list["scorer"], "choice");
    assert_eq!(
        paths(&list),
        [ENTRY],
        "the page whose links could not be judged is not in the list"
    );
    assert_eq!(
        list["results"][0]["links"][0]["followed"], true,
        "the link queued its target all the same: what failed is the page, not the link"
    );
    assert_eq!(list["visited"], 1);
    assert_eq!(
        output.status.code(),
        Some(1),
        "and the list is the entry alone: {}",
        stderr(&output)
    );

    let error = stderr(&output);
    assert!(
        error.contains(NEXT) && error.contains("max_tokens_exceeded"),
        "the page and the reason are on stderr: {error}"
    );
    assert_eq!(error.lines().count(), 2, "{error}");
}

/// A page whose judgment fails is not retried and not revisited: the walk asks
/// about it once, and it is not a visited file.
#[test]
fn a_page_that_cannot_be_judged_is_asked_once() {
    let api = FakeApi::refusing(DEEP, 3.0, 0.9, 0.7);
    let cache = Cache::new();

    let output = run_with(&[QUERY, BROKEN_LINK_ENTRY], &api, &cache);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        api.answered(),
        3,
        "the entry, the page on from it, and the one that failed — once each"
    );
    let list = json(&output);
    assert_eq!(
        list["visited"], 2,
        "the page that failed is not one of the files that were judged"
    );
}

/// Nothing judged at all: an entry file whose judgment failed, with no other
/// page reached, has no reading list to print. Exit 2, one line naming it, and
/// nothing on stdout — the same shape as an entry file that cannot be read
/// ([#37]).
///
/// [#37]: https://github.com/mikekelly/s1m/issues/37
#[test]
fn an_entry_file_that_cannot_be_judged_exits_2() {
    let api = FakeApi::refusing(ENTRY, 3.0, 0.9, 0.7);
    let cache = Cache::new();

    let output = run_with(&[QUERY, ENTRY], &api, &cache);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(
        error.contains(ENTRY) && error.contains("max_tokens_exceeded"),
        "{error}"
    );
    assert_eq!(error.lines().count(), 1, "{error}");
    assert_eq!(
        api.answered(),
        1,
        "nothing else was reached, so nothing else was asked"
    );
}

/// An entry file the caller named and cannot be read is the caller's mistake,
/// and it is named before anything is bought: no API call, nothing on stdout,
/// one line naming the file.
#[test]
fn a_missing_entry_file_exits_2_naming_it() {
    let api = FakeApi::new(3.0, 0.9, 0.7);
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
    let api = FakeApi::new(3.0, 0.9, 0.7);
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
    let api = FakeApi::new(3.0, 0.9, 0.7);
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
    let api = FakeApi::new(3.0, 0.9, 0.7);
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

// ----------------------------------------------------------- the .s1mignore

/// The whole guarantee, seen from the other end of the wire: a page the root's
/// `.s1mignore` covers is not merely absent from the reading list, it is absent
/// from every request the run sent — its path, its text and its link are not in
/// the state the model was asked about, and so not in the reading list either.
#[test]
fn an_ignored_page_is_not_in_a_request_or_in_the_reading_list() {
    let api = FakeApi::new(3.0, 0.9, 0.7);
    let cache = Cache::new();

    let output = run_with(&[IGNORE_QUERY, IGNORE_ENTRY], &api, &cache);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let list = json(&output);
    assert_eq!(
        paths(&list),
        [IGNORE_ENTRY, IGNORE_PUBLIC],
        "the matched page is not visited, and the page beside it is"
    );
    assert_eq!(list["calls"], 2, "nothing was bought for the matched page");
    assert_eq!(api.answered(), 2);

    for request in api.requests() {
        let links = request["state"]["links"]
            .as_array()
            .expect("a request carries the links it judges");
        assert!(
            links
                .iter()
                .all(|link| link["target"] != json!("private/vault.md")),
            "no question is asked about the matched page: {request}"
        );
        assert!(
            !request.to_string().contains(IGNORE_SECRET),
            "not a byte of the matched page's text is sent: {request}"
        );
    }

    let entry = result(&list, IGNORE_ENTRY);
    assert_eq!(
        entry["links"].as_array().map(Vec::len),
        Some(1),
        "the link to the matched page is not reported at all: {}",
        entry["links"]
    );
    assert_eq!(entry["links"][0]["target"], json!(IGNORE_PUBLIC));

    // The tree prints every link the walk judged, so a link that was never sent
    // is a link that is not here either.
    let tree = run_with(
        &[IGNORE_QUERY, IGNORE_ENTRY, "--format", "tree"],
        &api,
        &cache,
    );
    let tree = stdout(&tree);
    assert!(tree.contains(IGNORE_PUBLIC), "{tree}");
    assert!(!tree.contains("vault"), "{tree}");
}

/// An entry file the caller named is the one case where a matched path is not
/// quietly dropped: the names on the command line are what the caller asked
/// for, so s1m says which rule covers it and exits 2 without reading a byte of
/// it or buying a call — for a page behind a matched directory and for a page
/// matched by its own name.
#[test]
fn an_ignored_entry_file_exits_2_rather_than_reading_it() {
    let api = FakeApi::new(3.0, 0.9, 0.7);
    let cache = Cache::new();

    for entry in [IGNORE_VAULT, IGNORE_DRAFTS] {
        let output = run_with(&[IGNORE_QUERY, entry, "--root", IGNORE_ROOT], &api, &cache);

        assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
        assert!(output.stdout.is_empty());
        let error = stderr(&output);
        assert!(error.contains(entry), "{error}");
        assert!(error.contains(".s1mignore"), "{error}");
        assert!(error.contains("never reads an ignored file"), "{error}");
        assert_eq!(error.lines().count(), 1, "{error}");
    }
    assert_eq!(api.answered(), 0);
}

/// A `.s1mignore` s1m cannot use is exit 2 naming the line, before the entry
/// file is read: dropping the rule quietly would send the paths the rule was
/// written for, which is the one failure the file exists to prevent.
#[test]
fn a_s1mignore_that_does_not_parse_exits_2_naming_its_line() {
    let api = FakeApi::new(3.0, 0.9, 0.7);
    let cache = Cache::new();

    let output = run_with(&[IGNORE_QUERY, BROKEN_ENTRY], &api, &cache);

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(error.contains(".s1mignore"), "{error}");
    assert!(error.contains("line 2"), "{error}");
    assert_eq!(error.lines().count(), 1, "{error}");
    assert_eq!(api.answered(), 0);
}

/// The hidden debug view runs under the same rules as a query: one file the
/// root's `.s1mignore` matches is exit 2 naming it, and the API is not asked —
/// the view reads the file and previews its links, and a matched path is not
/// read. A *link* to a matched page is the other half of that: it is out of the
/// file before the request is built, so the view is asked about the page the
/// file does link to and never about the one it does not.
#[test]
fn score_file_will_not_read_an_ignored_file() {
    let api = FakeApi::new(3.0, 0.9, 0.7);
    let cache = Cache::new();

    let output = run_with(
        &[
            "score-file",
            IGNORE_QUERY,
            IGNORE_VAULT,
            "--root",
            IGNORE_ROOT,
        ],
        &api,
        &cache,
    );

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(error.contains(IGNORE_VAULT), "{error}");
    assert!(error.contains(".s1mignore"), "{error}");
    assert_eq!(api.answered(), 0);

    let output = run_with(
        &[
            "score-file",
            IGNORE_QUERY,
            IGNORE_ENTRY,
            "--root",
            IGNORE_ROOT,
        ],
        &api,
        &cache,
    );

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let report = stdout(&output);
    assert!(
        report.contains("public.md"),
        "the link the file does link to is judged: {report}"
    );
    assert!(!report.contains("vault"), "{report}");
    for request in api.requests() {
        let links = request["state"]["links"]
            .as_array()
            .expect("a request carries the links it judges");
        assert!(
            links
                .iter()
                .all(|link| link["target"] != json!("private/vault.md")),
            "the matched target is not judged: {request}"
        );
        assert!(
            !request.to_string().contains(IGNORE_SECRET),
            "not a byte of the matched page's text is sent: {request}"
        );
    }
}

/// An entry file and a root spelled against different bases are the caller's
/// mistake, and the run says so in one line: the ignore check reads the path
/// before `parse` rejects the pair, so it has to answer for the pair rather than
/// bring the process down.
#[test]
fn an_entry_file_on_another_base_than_the_root_exits_2() {
    let api = FakeApi::new(3.0, 0.9, 0.7);
    let cache = Cache::new();
    let absolute = PathBuf::from(MANIFEST_DIR).join(IGNORE_ENTRY);

    let output = run_with(
        &[
            IGNORE_QUERY,
            &absolute.display().to_string(),
            "--root",
            IGNORE_ROOT,
        ],
        &api,
        &cache,
    );

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(
        error.contains("must both be relative or both absolute"),
        "{error}"
    );
    assert_eq!(error.lines().count(), 1, "{error}");
    assert_eq!(api.answered(), 0);
}

// ------------------------------------------------------------- the formats

/// `--format md` prints the same reading list as something to read: the files in
/// order, each with the lines worth reading inside it. The numbers and the shape
/// are the whole contract, so the test holds the run to every byte of it.
#[test]
fn md_prints_the_reading_list_as_markdown() {
    let api = FakeApi::new(3.0, 0.9, 0.7);
    let cache = Cache::new();

    let output = run_with(&[QUERY, ENTRY, "--format", "md"], &api, &cache);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(output.stderr.is_empty(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "\
# Reading list: what is there to read

Criterion: useful-for; 3 files visited, 3 calls

## 1. `tests/fixtures/cli/deep.md`

relevance 1.00; scent 0.90; via `tests/fixtures/cli/entry.md` -> `tests/fixtures/cli/next.md`

- lines 1-3, score 0.70, Deep

## 2. `tests/fixtures/cli/entry.md`

relevance 1.00; entry file

- lines 1-4, score 0.70, Entry

## 3. `tests/fixtures/cli/next.md`

relevance 1.00; scent 0.90; via `tests/fixtures/cli/entry.md`

- lines 1-3, score 0.70, Next
"
    );
}

/// `--format tree` prints the walk: the entry file at the root — no link reached
/// it — and under it each link it judged, at the scent the model gave it and
/// marked with what the walk did about it. Every link here cleared the
/// threshold, so every one of them was followed.
#[test]
fn tree_prints_the_walks_link_tree() {
    let api = FakeApi::new(3.0, 0.9, 0.7);
    let cache = Cache::new();

    let output = run_with(&[QUERY, ENTRY, "--format", "tree"], &api, &cache);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(output.stderr.is_empty(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "\
what is there to read (useful-for); 3 files visited, 3 calls

tests/fixtures/cli/entry.md  entry file; relevance 1.00
  tests/fixtures/cli/next.md  followed; scent 0.90; relevance 1.00
    tests/fixtures/cli/deep.md  followed; scent 0.90; relevance 1.00
"
    );
}

/// A scent below the threshold queues nothing, and the tree says so on the link
/// itself: `pruned`, with the scent that was not enough, beside the file the
/// walk reached instead.
#[test]
fn tree_marks_a_link_below_the_threshold_pruned() {
    let api = FakeApi::new(3.0, 0.1, 0.7);
    let cache = Cache::new();

    let output = run_with(&[QUERY, ENTRY, "--format", "tree"], &api, &cache);

    assert_eq!(
        output.status.code(),
        Some(1),
        "nothing cleared the threshold: {}",
        stderr(&output)
    );
    assert_eq!(
        stdout(&output),
        "\
what is there to read (useful-for); 1 file visited, 1 call

tests/fixtures/cli/entry.md  entry file; relevance 1.00
  tests/fixtures/cli/next.md  pruned; scent 0.10
"
    );
}

/// A file has to earn its place: relevance at or above the threshold, or a
/// section at or above it. Here nothing does — every page comes back at 0.5 —
/// so `results` is empty, the walk is reported under `walked`, and the exit
/// code says the list holds nothing the caller did not already have.
///
/// The two reading views disagree on purpose: `md` is what to read, so it has
/// nothing to print, and `tree` is the walk, so it prints all of it.
#[test]
fn nothing_earns_a_place_so_the_walk_is_reported_and_results_is_empty() {
    let api = FakeApi::new(1.5, 0.9, 0.5);
    let cache = Cache::new();

    let output = run_with(&[QUERY, ENTRY], &api, &cache);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("nothing beyond the entry files earned a place"),
        "{}",
        stderr(&output)
    );
    let list = json(&output);
    assert_eq!(list["visited"], 3, "the walk visits all three pages");
    assert!(paths(&list).is_empty(), "nothing earned a place: {list}");
    assert_eq!(
        list["walked"]
            .as_array()
            .expect("walked should be an array")
            .iter()
            .map(|file| file["path"].as_str().expect("a path"))
            .collect::<Vec<_>>(),
        [DEEP, ENTRY, NEXT],
        "the walk's own files, ranked the way a result would be"
    );
    assert_eq!(list["walked"][0]["relevance"], 0.5);
    assert!(
        list["walked"][0].get("sections").is_none(),
        "a file that earned no place has no ranges to return: {list}"
    );

    let markdown = run_with(&[QUERY, ENTRY, "--format", "md"], &api, &cache);
    assert_eq!(
        stdout(&markdown),
        "\
# Reading list: what is there to read

Criterion: useful-for; 3 files visited, 0 calls
"
    );
    let tree = run_with(&[QUERY, ENTRY, "--format", "tree"], &api, &cache);
    assert_eq!(
        stdout(&tree),
        "\
what is there to read (useful-for); 3 files visited, 0 calls

tests/fixtures/cli/entry.md  entry file; relevance 0.50
  tests/fixtures/cli/next.md  followed; scent 0.90; relevance 0.50
    tests/fixtures/cli/deep.md  followed; scent 0.90; relevance 0.50
"
    );
}

/// A format that is not one of the three is the flag's vocabulary, not a run:
/// clap names the value it did not recognise and exits 2, the way it does for a
/// flag that does not exist.
#[test]
fn an_unknown_format_exits_2_naming_it() {
    let output = run(&["--format", "yaml", QUERY, ENTRY]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("--format"), "{error}");
    assert!(error.contains("yaml"), "{error}");
}
