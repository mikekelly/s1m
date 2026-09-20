//! One Jev request per file.
//!
//! The query, the file, its heading sections and everything known about its
//! outgoing links go in as one `state`; the answers come back as one Score for
//! the file, one Noul per section and one Noul per link. Jev evaluates every
//! question against the state in parallel, so a file costs one round trip
//! however many links it has — the premise of the spike in
//! [#5](https://github.com/mikekelly/s1m/issues/5). What real runs produced is
//! written up in `docs/spike-notes.md`.
//!
//! A file whose sections and links would not fit the API's state budget in one
//! request is split across posts instead: every post carries the same file and
//! its own share of the questions, and the answers merge into one judgment. See
//! [`JevScorer::pack`].
//!
//! There is no Rust SDK, so this calls the HTTP API directly:
//! <https://docs.typesafe.ai/api.md>. The wording of every question lives in
//! [`Mode`], so adding a relevance mode (#10) means adding a table entry, not
//! changing the builder.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};

use crate::cache::Cacheable;
use crate::parse::{self, FrontmatterField, Link, ParsedFile};
use crate::scorer::{FileJudgment, LinkJudgment, Scorer, ScorerError, SectionJudgment};

/// The evaluation endpoint. One call, one shape; the SDKs wrap this.
pub const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

/// The variable that points the scorer at another endpoint — a proxy, or a
/// test's fake server — the way [`crate::cache::DIR_VAR`] points the cache
/// elsewhere. Set but blank counts as unset.
///
/// The endpoint is part of the cache key ([`Cacheable::key`]), so a run against
/// another endpoint never reads the answers a run against the API stored.
pub const ENDPOINT_VAR: &str = "S1M_ENDPOINT";

/// The model alias the issue names. The response reports the versioned id that
/// answered, which is what [`JevDetail::model`] carries.
pub const MODEL: &str = "jev-latest";

/// Dollars per million input tokens, output free: the list price for `jev-1.13`
/// as of 2026-09, from <https://docs.typesafe.ai/models>.
pub const PRICE_PER_MTOK: f64 = 0.042;

/// The id the file relevance Score comes back under. Question ids are for this
/// code: the model sees only `instructions` and `criteria`.
const FILE_QUESTION: &str = "file_relevance";

/// How much of the file's text is sent. The API allows 32k tokens for the state
/// plus the longest question and 64k for the whole request; four characters per
/// token is the rough rule for English, so this is about a fifth of the state
/// budget and leaves the link table most of the rest.
const CONTENT_LIMIT: usize = 40_000;

/// The tokens the API allows for `state` plus the longest question, from
/// <https://docs.typesafe.ai/models> as read for `docs/spike-notes.md`. Past
/// this the request is rejected, so a file whose sections and links do not fit
/// alongside its content is split across posts ([`JevScorer::pack`]).
const STATE_TOKENS: usize = 32_000;

/// Four characters per token: the rule of thumb the caps in this file are set
/// with, because nothing here tokenises. [`JevScorer::pack`] measures the same
/// way and rounds the same way, up.
const CHARS_PER_TOKEN: usize = 4;

/// Room left over in a post for what holds it together — the braces, the
/// commas between items, the `model` field, the escaping of a quote in the
/// file's own text. A post is only split when the estimate crosses the budget
/// with this margin, so the split errs towards an early one.
const POST_MARGIN: usize = 1_024;

/// A first paragraph cut to this many characters: the preview is a hint for the
/// scent judgment, not the page.
const PREVIEW_LIMIT: usize = 600;

/// A round trip here is a second or two, so anything near this is a hang.
const TIMEOUT: Duration = Duration::from_secs(120);

/// The docs tell callers using the HTTP API directly to back off and retry on
/// `429` and `529` and to honour `retry-after`. Two retries is what a CLI run
/// can afford; a test's fake server makes them instant.
const ATTEMPTS: u32 = 3;

/// A `retry-after` longer than this is a reason to fail rather than to sit.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

// ------------------------------------------------------------------- modes

/// Everything that changes between relevance criteria: the name, and the
/// wording of the three questions.
///
/// The three modes the plan's Relevance modes table names are consts below, and
/// [`Mode::custom`] builds one from a criterion of the caller's own. All of them
/// leave the builder alone: what a run asks is this table, and every field of it
/// is in the request's bytes, which is what makes one criterion's stored answers
/// unusable for another's.
///
/// Only the name and the three questions are [`Cow`]s, because a criteria file
/// supplies those in the caller's own words: its path names the mode and its
/// criterion goes into all three questions. The ladder and the yes/no wording
/// are this module's and are static, which is why the criteria file borrows
/// them.
#[derive(Debug, Clone)]
pub struct Mode {
    /// The mode's name, as `--mode` spells it; a criteria file's path, as
    /// `--criteria` was given it, when the criterion came from there.
    pub name: Cow<'static, str>,
    /// The question about the file as a whole.
    pub file_question: Cow<'static, str>,
    /// The Score levels, least useful first. The order is load-bearing: a Score
    /// answer is the probability-weighted position over these levels, numbered
    /// from zero, so the last level is the top of the scale.
    pub file_levels: &'static [&'static str],
    /// The question about one heading section. `{index}` is replaced with that
    /// section's position in `state.sections`, which is how the instructions
    /// point at it.
    pub section_question: Cow<'static, str>,
    /// What a yes means for that section.
    pub section_true: &'static str,
    /// What a no means for that section.
    pub section_false: &'static str,
    /// The question about one link. `{index}` is replaced with that link's
    /// position in `state.links`, which is how the instructions point at it.
    pub link_question: Cow<'static, str>,
    /// What a yes means for that link.
    pub link_true: &'static str,
    /// What a no means for that link.
    pub link_false: &'static str,
}

impl Mode {
    /// The number of Score levels, as the API requires: at least two.
    pub fn levels(&self) -> usize {
        self.file_levels.len()
    }

    /// The score the top level is worth, and so the divisor that turns a Score
    /// answer into a 0 to 1 relevance.
    pub fn top_level(&self) -> f64 {
        (self.levels() - 1) as f64
    }

    /// A criterion from a file of the caller's own, in place of a mode.
    ///
    /// The criterion sentence is the caller's and goes into all three
    /// questions; the wording around it is this module's, because each built-in
    /// ladder is written for its own criterion and a caller's sentence has no
    /// ladder to match. The scale is therefore the criterion-independent one
    /// below, and `name` is what the reading list reports as `mode` —
    /// `--criteria` passes the path it was given.
    ///
    /// `criterion` is expected trimmed and non-empty; the CLI is where a file
    /// that holds nothing is the caller's mistake.
    pub fn custom(name: impl Into<Cow<'static, str>>, criterion: &str) -> Mode {
        let mut file_question =
            String::from("How relevant is `file` for `query`, judged by this criterion: ");
        file_question.push_str(criterion);
        let mut section_question = String::from(
            "Is `sections[{index}]` — the part of `file` under that heading, at the lines given — worth reading, judged by this criterion: ",
        );
        section_question.push_str(criterion);
        let mut link_question = String::from(
            "Is following `links[{index}]` likely to lead to content that meets this criterion: ",
        );
        link_question.push_str(criterion);
        Mode {
            name: name.into(),
            file_question: Cow::Owned(file_question),
            file_levels: CRITERION_LEVELS,
            section_question: Cow::Owned(section_question),
            section_true: CRITERION_SECTION_TRUE,
            section_false: CRITERION_SECTION_FALSE,
            link_question: Cow::Owned(link_question),
            link_true: CRITERION_LINK_TRUE,
            link_false: CRITERION_LINK_FALSE,
        }
    }
}

/// Browsing: everything on a subject, however much of it a page covers. The top
/// level is a page *about* the subject rather than the page to start from,
/// because what a caller collecting a subject wants is every page on it.
pub const ABOUT: Mode = Mode {
    name: Cow::Borrowed("about"),
    file_question: Cow::Borrowed("How much of `file` is on the subject of `query`?"),
    file_levels: &[
        "unrelated — `file` has nothing to do with `query`.",
        "mention — `file` mentions the subject of `query` in passing, without covering it.",
        "related — `file` is on a subject next to `query`, and covers part of it.",
        "on the subject — `file` is about the subject of `query`: the page to collect.",
    ],
    section_question: Cow::Borrowed(
        "Is `sections[{index}]` — the part of `file` under that heading, at the lines given — on the subject of `query`?",
    ),
    section_true: "The text under that heading covers the subject, so reading those lines is worth the reader's next step.",
    section_false: OFF_SUBJECT_SECTION,
    link_question: Cow::Borrowed(
        "Does following `links[{index}]` lead to content on the subject of `query`?",
    ),
    link_true: "The target is about the subject, or is a page of links that lead to pages about it.",
    link_false: OFF_SUBJECT,
};

/// The default, and the criterion the spike measured: would this help someone
/// doing what the query describes?
pub const USEFUL_FOR: Mode = Mode {
    name: Cow::Borrowed("useful-for"),
    file_question: Cow::Borrowed("How useful is `file` for someone doing what `query` describes?"),
    file_levels: &[
        "unrelated — nothing in `file` bears on `query`.",
        "tangential — `file` is on a nearby subject, but someone doing what `query` describes would not read it.",
        "supporting — `file` holds context or part of what `query` needs, but is not where that person should start.",
        "central — `file` is about what `query` describes, or is the page to start from.",
    ],
    section_question: Cow::Borrowed(
        "Is `sections[{index}]` — the part of `file` under that heading, at the lines given — useful for someone doing what `query` describes?",
    ),
    section_true: "The text under that heading is on the subject, or is where that person should look, so reading those lines is worth their next step.",
    section_false: OFF_SUBJECT_SECTION,
    link_question: Cow::Borrowed(
        "Is following `links[{index}]` likely to lead to content useful for someone doing what `query` describes?",
    ),
    link_true: "The target is on the subject, or is a page of links that lead to it, so following this link is worth a reader's next step.",
    link_false: OFF_SUBJECT,
};

/// Question lookup: the page that answers the query, and the pages on the way
/// to it. A page that answers part of the question ranks above one that is only
/// background, which is what separates this mode from `useful-for`.
pub const ANSWERS: Mode = Mode {
    name: Cow::Borrowed("answers"),
    file_question: Cow::Borrowed("Does `file` contain the answer to `query`?"),
    file_levels: &[
        "no answer — `file` does not bear on `query`.",
        "background — `file` is context for `query`, but does not answer any part of it.",
        "part of the answer — `file` answers part of `query`, or names where the answer is.",
        "the answer — `file` contains the answer to `query`.",
    ],
    section_question: Cow::Borrowed(
        "Does `sections[{index}]` — the part of `file` under that heading, at the lines given — contain the answer to `query`, or part of it?",
    ),
    section_true: "The text under that heading contains the answer or part of it, or names where the answer is.",
    section_false: "The text under that heading does not answer `query`, or holds nothing to read: navigation, a bare list of links, boilerplate, or an empty stub.",
    link_question: Cow::Borrowed(
        "Does following `links[{index}]` lead to content containing the answer to `query`?",
    ),
    link_true: "The target contains the answer or part of it, or is a page of links that lead to content that does.",
    link_false: "The target does not answer `query`, or following it reaches nothing to read: navigation, boilerplate, an empty stub, or an unrelated page.",
};

/// What a link's no is when the target is not about the subject: `about` and
/// `useful-for` ask the same thing of a link here, so they say the same thing
/// about one that leads nowhere.
const OFF_SUBJECT: &str = "The target is off the subject, or following it reaches nothing to read: navigation, boilerplate, an empty stub, or an unrelated page.";

/// What a section's no is when its text is not about the subject, or holds
/// nothing: `about` and `useful-for` ask the same thing of a section here, so
/// they say the same thing about one that leads nowhere.
const OFF_SUBJECT_SECTION: &str = "The text under that heading is off the subject, or holds nothing to read: navigation, a bare list of links, boilerplate, an empty stub, or a heading whose section is somewhere else.";

/// The ladder a criterion of the caller's own is scored on: the same four
/// degrees for every criterion, because the criterion itself is in the
/// instructions and a caller's sentence comes with no ladder of its own.
const CRITERION_LEVELS: &[&str] = &[
    "unrelated — `file` does not meet the criterion.",
    "tangential — `file` touches the criterion without meeting it.",
    "supporting — `file` meets the criterion in part, or holds the context for meeting it.",
    "central — `file` meets the criterion: it is where a reader should start.",
];

const CRITERION_LINK_TRUE: &str =
    "The target meets the criterion, or is a page of links that lead to content that does.";

const CRITERION_LINK_FALSE: &str = "The target does not meet the criterion, or following it reaches nothing to read: navigation, boilerplate, an empty stub, or an unrelated page.";

const CRITERION_SECTION_TRUE: &str = "The text under that heading meets the criterion, so reading those lines is worth the reader's next step.";

const CRITERION_SECTION_FALSE: &str = "The text under that heading does not meet the criterion, or holds nothing to read: navigation, a bare list of links, boilerplate, or an empty stub.";

// ------------------------------------------------------------- the request

/// The `state` of one request: the question, the file, and what is known about
/// each way into it — its sections — and out of it — its links.
#[derive(Debug, Serialize)]
struct State {
    query: String,
    file: FileState,
    /// The file's heading sections, in the parser's document order;
    /// `sections[i]` is what question `section_i` asks about. A post carries
    /// only its own share when the questions were split across posts.
    sections: Vec<SectionState>,
    links: Vec<LinkState>,
}

#[derive(Debug, Clone, Serialize)]
struct FileState {
    path: String,
    title: String,
    content: String,
}

/// One heading section, as the model sees it.
///
/// The section's text is not repeated here: it is already in
/// [`FileState::content`], and a parent's text contains its subsections', so
/// copying it per section would multiply the state by the nesting depth. The
/// heading and the lines are what point at the part of the file the question is
/// about — and, for a file long enough that its content was cut short, they are
/// all the model has, the way a link with no readable target is judged from its
/// anchor.
///
/// `level` is carried because duplicate headings are common ("See also"): the
/// depth tells the model which of them it is being asked about.
#[derive(Debug, Clone, Serialize)]
struct SectionState {
    /// `None` for the content before the first heading, as the parser spells
    /// it.
    heading: Option<String>,
    level: u8,
    /// `[first, last]` line, inclusive, as the parser gave them.
    lines: [usize; 2],
}

/// One outgoing link, as the model sees it.
#[derive(Debug, Clone, Serialize)]
struct LinkState {
    anchor: String,
    sentence: String,
    heading: Option<String>,
    target: String,
    /// `None` when previews are off, when the target escapes the root, or when
    /// the file named is not readable.
    target_preview: Option<PreviewState>,
}

/// What a target file looks like from here: what
/// [`parse::preview`] can say about it without reading the whole page.
#[derive(Debug, Clone, Serialize)]
struct PreviewState {
    title: String,
    /// `None` when the run leaves the frontmatter out
    /// ([`JevScorer::with_preview_frontmatter`]), and when the target has none:
    /// both mean the model is shown no frontmatter, so the state carries no
    /// key for it either way.
    #[serde(skip_serializing_if = "Option::is_none")]
    frontmatter: Option<Vec<FrontmatterField>>,
    first_paragraph: Option<String>,
}

/// One file's request: the body to post, or the bodies when the file's sections
/// and links do not fit the API's state budget in one.
///
/// Public because it is what [`Cacheable`] hands the cache to key an answer on,
/// and deliberately opaque — no fields, no accessors — so that nothing outside
/// this module can be built against its shape. What the API sees is this
/// module's business; what the cache needs is [`Cacheable::key`]'s bytes, and
/// the whole split is part of them.
#[derive(Debug, Serialize)]
pub struct Request {
    /// The posts to make, in order. One, unless the file had to be split: the
    /// answers to all of them are one file's judgment.
    posts: Vec<Post>,
}

/// One post to the API, and where its answers go.
#[derive(Debug, Serialize)]
struct Post {
    /// What is sent, in the API's own shape.
    body: Body,
    /// The position in the file's sections of each entry of
    /// [`State::sections`]: the API is told about a section's place in its own
    /// post, and this is what knows where that came from. Not sent — the API
    /// has no use for it, and a request is not a place to explain itself.
    #[serde(skip_serializing)]
    sections: Vec<usize>,
    /// The position in the file's links of each entry of [`State::links`], the
    /// same way.
    #[serde(skip_serializing)]
    links: Vec<usize>,
}

/// The body, in the API's own shape.
#[derive(Debug, Serialize)]
struct Body {
    state: State,
    model: &'static str,
    questions: BTreeMap<String, Question>,
}

/// One question waiting for a post to go in.
#[derive(Debug, Clone, Copy)]
enum Item {
    /// The file's `index`-th section.
    Section(usize),
    /// The file's `index`-th link.
    Link(usize),
}

/// One post's share of the file's questions: which of the file's sections and
/// links it asks about, in the file's own order.
///
/// The API is told about positions inside the post — `sections[0]` is the first
/// section that post asks about, whatever its place in the file — so this is
/// what turns an answer back into the file's own section or link.
#[derive(Debug, Default, Clone, PartialEq)]
struct Share {
    sections: Vec<usize>,
    links: Vec<usize>,
}

impl Share {
    /// Whether this share has any question beyond the file's own.
    fn is_empty(&self) -> bool {
        self.sections.is_empty() && self.links.is_empty()
    }
}

/// What one question adds to a post: its state entry, the comma after that
/// entry, the quotes and colon around the id the question is filed under, and
/// the question itself. Measured in characters, for [`JevScorer::shares`].
fn item_cost<T: Serialize>(entry: &T, question: &Question, id: &str) -> usize {
    json_len(entry) + 1 + json_len(question) + json_len(id) + 4
}

/// The characters one part of a post costs, measured as the JSON it is written
/// as. This is how [`JevScorer::pack`] estimates a budget that is stated in
/// tokens, without tokenising anything.
///
/// Nothing this file sends can fail to serialise — it is strings, numbers and
/// options of both — so a failure here would be a bug in these types rather
/// than a state of the world.
fn json_len<T: Serialize + ?Sized>(part: &T) -> usize {
    serde_json::to_string(part)
        .expect("a request part is strings and numbers")
        .len()
}

/// One typed question. `type` is the API's discriminator, so the fields that do
/// not belong to this shape are simply absent.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Question {
    Noul {
        instructions: String,
        criteria: NoulCriteria,
    },
    Score {
        instructions: String,
        criteria: &'static [&'static str],
    },
}

#[derive(Debug, Serialize)]
struct NoulCriteria {
    #[serde(rename = "true")]
    yes: &'static str,
    #[serde(rename = "false")]
    no: &'static str,
}

/// The answer to link `index` comes back under this id, and the question asks
/// about `links[index]` — the post's own `links`, not the file's.
fn link_question(index: usize) -> String {
    format!("link_{index}")
}

/// The answer to section `index` comes back under this id, and the question
/// asks about `sections[index]` — the post's own `sections`, not the file's.
fn section_question(index: usize) -> String {
    format!("section_{index}")
}

// ------------------------------------------------------------ the response

#[derive(Debug, Deserialize)]
struct Response {
    model: String,
    answers: BTreeMap<String, Answer>,
    usage: Usage,
}

#[derive(Debug, Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}

/// One typed answer, tagged on the same `type` the question carried. A shape
/// this code does not know is a decode error, which is the honest outcome: the
/// two shapes below are the two it asks for.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Answer {
    Noul {
        noul: f64,
    },
    Score {
        score: f64,
        /// How peaked the level distribution behind the score was.
        confidence: f64,
    },
}

impl Answer {
    fn noul(self, id: &str) -> Result<f64, ScorerError> {
        match self {
            Answer::Noul { noul } => Ok(noul),
            Answer::Score { .. } => Err(ScorerError::WrongAnswerType {
                id: id.to_string(),
                expected: "noul",
                found: "score",
            }),
        }
    }

    fn score(self, id: &str) -> Result<(f64, f64), ScorerError> {
        match self {
            Answer::Score { score, confidence } => Ok((score, confidence)),
            Answer::Noul { .. } => Err(ScorerError::WrongAnswerType {
                id: id.to_string(),
                expected: "score",
                found: "noul",
            }),
        }
    }
}

// -------------------------------------------------------------- the scorer

/// What one call cost, and which model answered it. Not part of the [`Scorer`]
/// contract: this is the accounting the spike notes and `s1m score-file` need.
///
/// `Serialize`/`Deserialize` are for the cache, which stores it with the answer
/// it came with ([`crate::cache`]): a run whose answers were all bought earlier
/// can still say what they cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JevDetail {
    /// The versioned model that answered, as the API reported it.
    pub model: String,
    /// How many questions the request carried, over all its posts: the file,
    /// its sections and its links.
    pub questions: usize,
    /// How many requests the judgment took. One, unless the file's sections and
    /// links did not fit the API's state budget in one: this module splits a
    /// file into posts, and a post is a request.
    pub requests: usize,
    /// The raw Score answer, 0 to the top level, before it became a relevance.
    pub relevance_level: f64,
    pub relevance_confidence: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub latency: Duration,
}

impl JevDetail {
    /// What the call cost at the list price: input tokens only, output is free.
    pub fn cost_usd(&self) -> f64 {
        self.input_tokens as f64 / 1_000_000.0 * PRICE_PER_MTOK
    }
}

/// One file's judgment, with the accounting that came back with it.
#[derive(Debug, Clone, PartialEq)]
pub struct JevOutcome {
    pub judgment: FileJudgment,
    pub detail: JevDetail,
}

/// Scores files with Jev over the TypeSafe HTTP API.
///
/// `root` is the directory the [`ParsedFile`]'s links were resolved against: it
/// is what turns a link target back into a readable path for its preview.
pub struct JevScorer {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
    root: PathBuf,
    mode: Mode,
    previews: bool,
    preview_frontmatter: bool,
}

impl JevScorer {
    /// A scorer with an explicit key, for callers that have one. It judges by
    /// [`USEFUL_FOR`] until [`JevScorer::with_mode`] says otherwise.
    pub fn new(api_key: impl Into<String>, root: impl Into<PathBuf>) -> Result<Self, ScorerError> {
        Ok(JevScorer {
            client: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .map_err(|source| ScorerError::Client { source })?,
            endpoint: ENDPOINT.to_string(),
            api_key: api_key.into(),
            root: root.into(),
            mode: USEFUL_FOR.clone(),
            previews: true,
            preview_frontmatter: true,
        })
    }

    /// A scorer with the key from `TYPESAFE_API_KEY`, the variable
    /// [`.env.example`](../../.env.example) names.
    pub fn from_env(root: impl Into<PathBuf>) -> Result<Self, ScorerError> {
        JevScorer::new(
            required_key(std::env::var("TYPESAFE_API_KEY").ok())?,
            root.into(),
        )
    }

    /// Points the scorer at another endpoint: the tests' fake server, or a
    /// proxy. `endpoint` is the full URL of the evaluation call.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Sends previews (or does not) with each link. Off is the control case for
    /// the question of whether a preview earns its tokens — [`JevScorer::new`]
    /// turns them on, and the query path never turns them off: the spike's
    /// verdict is that the preview is the main ranking signal, so it is part of
    /// the request rather than a caller's choice. See `docs/spike-notes.md`.
    pub fn with_previews(mut self, previews: bool) -> Self {
        self.previews = previews;
        self
    }

    /// Sends each target's frontmatter with its preview, or leaves it out.
    ///
    /// On by default, and the query path never turns it off. Off is the
    /// experiment [#10] deferred: the frontmatter is the part of a preview most
    /// likely to mislead — `related:` and `tags` make every page look connected
    /// to every other — and the spike varied the whole preview as one knob, so
    /// it could not say which part did the work. The evaluation harness runs
    /// both ways and reports what it found ([#11]); this is that knob, not a
    /// caller's choice.
    ///
    /// [#10]: https://github.com/mikekelly/s1m/issues/10
    /// [#11]: https://github.com/mikekelly/s1m/issues/11
    pub fn with_preview_frontmatter(mut self, frontmatter: bool) -> Self {
        self.preview_frontmatter = frontmatter;
        self
    }

    /// Judges by `mode`'s criterion instead of [`USEFUL_FOR`].
    ///
    /// The criterion is picked before any request is built, because it is the
    /// questions that carry it — and, the questions being part of what
    /// [`Cacheable::key`] hashes, a criterion never reads another's answers.
    pub fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// Judges one file, keeping the accounting [`Scorer::score`] drops.
    pub async fn judge(&self, query: &str, file: &ParsedFile) -> Result<JevOutcome, ScorerError> {
        let request = self.request(query, file)?;
        self.judge_request(&request, file).await
    }

    /// Sends one already-built request for `file`, keeping the accounting.
    ///
    /// The cache builds the request to key it, and this is the call that
    /// follows a miss: the request that was hashed is the request that is sent,
    /// every post of it.
    async fn judge_request(
        &self,
        request: &Request,
        file: &ParsedFile,
    ) -> Result<JevOutcome, ScorerError> {
        let started = Instant::now();
        let responses = join_all(request.posts.iter().map(|post| self.send(&post.body))).await;
        let latency = started.elapsed();
        // A post that failed fails the file: one judgment needs all its
        // answers, and the caller has no file to rank on half of them.
        let responses = responses.into_iter().collect::<Result<Vec<_>, _>>()?;
        self.outcome(file, request, responses, latency)
    }

    /// One request for one file: its content, its sections, and a question per
    /// section and per link.
    fn request(&self, query: &str, file: &ParsedFile) -> Result<Request, ScorerError> {
        let content = fs::read_to_string(&file.path).map_err(|source| ScorerError::Read {
            path: file.path.clone(),
            source,
        })?;

        let state = FileState {
            path: file.path.display().to_string(),
            title: file.title.clone(),
            content: clamp(&content, CONTENT_LIMIT),
        };
        let sections = file
            .sections
            .iter()
            .map(|section| SectionState {
                heading: section.heading.clone(),
                level: section.level,
                lines: section.lines,
            })
            .collect();
        let links = file
            .links
            .iter()
            .map(|link| LinkState {
                anchor: link.anchor.clone(),
                sentence: link.sentence.clone(),
                heading: link.heading.clone(),
                target: link.target.display().to_string(),
                target_preview: self.preview(link),
            })
            .collect();

        Ok(self.pack(query, state, sections, links))
    }

    /// The file's questions, split across as many posts as the API's state
    /// budget needs.
    ///
    /// One post is the normal case: every question of a request sees the same
    /// state and is evaluated in parallel, so splitting gains latency nothing.
    /// This is a size question. The docs allow 32k tokens for `state` plus the
    /// longest question, and what can grow without bound is the file's content
    /// — capped at [`CONTENT_LIMIT`], so about 10k tokens — and then its link
    /// table, at about 250 tokens a link with previews (`docs/spike-notes.md`).
    /// Only the questions are left to split, and the file goes in every post,
    /// so each post is the file plus the sections and links that fit beside it.
    ///
    /// The questions are dealt out in the file's own order, sections first, so
    /// the answers of the posts in order are the file's sections and links in
    /// order. A post's cost is measured as the JSON of its parts at
    /// [`CHARS_PER_TOKEN`], rounded up, with [`POST_MARGIN`] to spare: nothing
    /// here tokenises, so the estimate is deliberately a conservative one.
    fn pack(
        &self,
        query: &str,
        file: FileState,
        sections: Vec<SectionState>,
        links: Vec<LinkState>,
    ) -> Request {
        // The file's Score is about the whole file, so it is asked once, in the
        // first post, and every post pays for the file state it asks about.
        let fixed = json_len(&query) + json_len(&file) + json_len(&self.score_question());
        let shares = self.shares(&sections, &links, fixed);

        Request {
            posts: shares
                .iter()
                .enumerate()
                .map(|(index, share)| {
                    // The file's Score is about the whole file, so the first
                    // post asks it and later ones do not: three posts would
                    // otherwise buy three answers to one question.
                    self.post(query, &file, &sections, &links, share, index == 0)
                })
                .collect(),
        }
    }

    /// Deals the file's sections and links out into shares that each fit the
    /// state budget beside the file.
    ///
    /// Always at least one share, even for a file with no sections and no
    /// links: the file still has a Score to ask.
    fn shares(&self, sections: &[SectionState], links: &[LinkState], fixed: usize) -> Vec<Share> {
        // An item's cost is its state entry, the question, and the characters
        // that hold them in the body: the comma between entries, and the quotes
        // and colon around the id each question is filed under. The question is
        // built with the item's position in the file, which is at least its
        // position in the post that asks it, so the estimate never runs short
        // of the text sent.
        let costs = sections
            .iter()
            .enumerate()
            .map(|(index, section)| {
                (
                    Item::Section(index),
                    item_cost(
                        section,
                        &self.section_question(index),
                        &section_question(index),
                    ),
                )
            })
            .chain(links.iter().enumerate().map(|(index, link)| {
                (
                    Item::Link(index),
                    item_cost(link, &self.link_question(index), &link_question(index)),
                )
            }));

        let mut shares = vec![Share::default()];
        let mut used = 0;
        for (item, cost) in costs {
            let open = shares.last().expect("a share is always open");
            // A share with something in it gives way to an item that would not
            // fit; an empty one takes the item whatever it costs, so dealing
            // always makes progress.
            if !open.is_empty()
                && (fixed + used + cost + POST_MARGIN).div_ceil(CHARS_PER_TOKEN) > STATE_TOKENS
            {
                shares.push(Share::default());
                used = 0;
            }
            match item {
                Item::Section(index) => shares
                    .last_mut()
                    .expect("a share is always open")
                    .sections
                    .push(index),
                Item::Link(index) => {
                    shares
                        .last_mut()
                        .expect("a share is always open")
                        .links
                        .push(index);
                }
            }
            used += cost;
        }
        shares
    }

    /// One post: the file, its share of the sections and links, and where each
    /// answer goes. `score` is whether this post asks the file's own question,
    /// which is the first post's to ask.
    fn post(
        &self,
        query: &str,
        file: &FileState,
        sections: &[SectionState],
        links: &[LinkState],
        share: &Share,
        score: bool,
    ) -> Post {
        let mut questions = BTreeMap::new();
        if score {
            questions.insert(FILE_QUESTION.to_string(), self.score_question());
        }

        let mut state = State {
            query: query.to_string(),
            file: file.clone(),
            sections: Vec::with_capacity(share.sections.len()),
            links: Vec::with_capacity(share.links.len()),
        };
        for (position, &index) in share.sections.iter().enumerate() {
            state.sections.push(sections[index].clone());
            questions.insert(section_question(position), self.section_question(position));
        }
        for (position, &index) in share.links.iter().enumerate() {
            state.links.push(links[index].clone());
            questions.insert(link_question(position), self.link_question(position));
        }

        Post {
            body: Body {
                state,
                model: MODEL,
                questions,
            },
            sections: share.sections.clone(),
            links: share.links.clone(),
        }
    }

    /// The file's own Score question.
    fn score_question(&self) -> Question {
        Question::Score {
            instructions: self.mode.file_question.to_string(),
            criteria: self.mode.file_levels,
        }
    }

    /// The Noul question about one section, named by its position in the post
    /// that asks it.
    fn section_question(&self, index: usize) -> Question {
        Question::Noul {
            instructions: self
                .mode
                .section_question
                .replace("{index}", &index.to_string()),
            criteria: NoulCriteria {
                yes: self.mode.section_true,
                no: self.mode.section_false,
            },
        }
    }

    /// The Noul question about one link, named by its position in the post that
    /// asks it.
    fn link_question(&self, index: usize) -> Question {
        Question::Noul {
            instructions: self
                .mode
                .link_question
                .replace("{index}", &index.to_string()),
            criteria: NoulCriteria {
                yes: self.mode.link_true,
                no: self.mode.link_false,
            },
        }
    }

    /// Posts the body, retrying the two statuses the docs call retryable.
    async fn send(&self, body: &Body) -> Result<Response, ScorerError> {
        let mut attempt = 1;
        loop {
            let response = self
                .client
                .post(&self.endpoint)
                .bearer_auth(&self.api_key)
                .json(body)
                .send()
                .await
                .map_err(|source| ScorerError::Transport {
                    endpoint: self.endpoint.clone(),
                    source,
                })?;
            let status = response.status();
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok());
            let body = response
                .text()
                .await
                .map_err(|source| ScorerError::Transport {
                    endpoint: self.endpoint.clone(),
                    source,
                })?;

            if status.is_success() {
                return serde_json::from_str(&body).map_err(|source| ScorerError::Decode {
                    endpoint: self.endpoint.clone(),
                    source,
                });
            }
            if matches!(status.as_u16(), 429 | 529) && attempt < ATTEMPTS {
                let wait = retry_after
                    .map_or_else(|| backoff(attempt), Duration::from_secs)
                    .min(MAX_BACKOFF);
                tokio::time::sleep(wait).await;
                attempt += 1;
                continue;
            }
            return Err(ScorerError::Status {
                endpoint: self.endpoint.clone(),
                status: status.as_u16(),
                body,
            });
        }
    }

    /// Reads every post's answers back onto the file.
    ///
    /// `responses` are the answers to `request.posts`, in that order, which is
    /// the order the questions were dealt out in: the file's sections in
    /// document order, then its links in the order they appear. So the answers
    /// of the posts in order are the file's sections and links in order.
    fn outcome(
        &self,
        file: &ParsedFile,
        request: &Request,
        responses: Vec<Response>,
        latency: Duration,
    ) -> Result<JevOutcome, ScorerError> {
        // Every post is answered by the same model alias; the first is the one
        // to report, and a request always has at least one post.
        let model = responses
            .first()
            .map(|response| response.model.clone())
            .unwrap_or_default();

        let mut level = None;
        let mut questions = 0;
        let mut input_tokens = 0;
        let mut output_tokens = 0;
        let mut sections = Vec::with_capacity(file.sections.len());
        let mut links = Vec::with_capacity(file.links.len());

        for (post, response) in request.posts.iter().zip(responses) {
            let mut answers = response.answers;
            questions += post.body.questions.len();
            input_tokens += response.usage.input_tokens;
            output_tokens += response.usage.output_tokens;

            // Only the first post carries the file's Score; a later post's
            // answers are all sections and links.
            if let Some(answer) = answers.remove(FILE_QUESTION) {
                level = Some(answer.score(FILE_QUESTION)?);
            }

            for (position, &index) in post.sections.iter().enumerate() {
                let id = section_question(position);
                let score = answers
                    .remove(&id)
                    .ok_or_else(|| ScorerError::MissingAnswer { id: id.clone() })?
                    .noul(&id)?;
                let section = &file.sections[index];
                sections.push(SectionJudgment {
                    heading: section.heading.clone(),
                    // The parser's own range, never a derived one: what the
                    // caller reads is the section that was judged.
                    lines: section.lines,
                    score,
                });
            }

            for (position, &index) in post.links.iter().enumerate() {
                let id = link_question(position);
                let scent = answers
                    .remove(&id)
                    .ok_or_else(|| ScorerError::MissingAnswer { id: id.clone() })?
                    .noul(&id)?;
                links.push(LinkJudgment {
                    target: file.links[index].target.clone(),
                    scent,
                });
            }
        }

        let (level, confidence) = level.ok_or_else(|| ScorerError::MissingAnswer {
            id: FILE_QUESTION.to_string(),
        })?;

        Ok(JevOutcome {
            judgment: FileJudgment {
                relevance: (level / self.mode.top_level()).clamp(0.0, 1.0),
                sections,
                links,
            },
            detail: JevDetail {
                model,
                questions,
                requests: request.posts.len(),
                relevance_level: level,
                relevance_confidence: confidence,
                input_tokens,
                output_tokens,
                latency,
            },
        })
    }

    /// The target's title, frontmatter and first paragraph.
    ///
    /// Nothing when previews are off, when the link leaves the root — that
    /// content is outside what the caller asked s1m to look at — or when the
    /// target cannot be read, which is the normal state of a broken link. Those
    /// links are still judged, from their anchor, sentence and heading. The
    /// frontmatter is the one part that can be dropped on its own
    /// ([`JevScorer::with_preview_frontmatter`]).
    fn preview(&self, link: &Link) -> Option<PreviewState> {
        if !self.previews || !link.in_root {
            return None;
        }
        let preview = parse::preview(self.root.join(&link.target)).ok()?;
        Some(PreviewState {
            title: preview.title,
            frontmatter: self
                .preview_frontmatter
                .then_some(preview.frontmatter)
                .filter(|frontmatter| !frontmatter.is_empty()),
            first_paragraph: preview
                .first_paragraph
                .map(|paragraph| clamp(&paragraph, PREVIEW_LIMIT)),
        })
    }
}

#[async_trait]
impl Scorer for JevScorer {
    async fn score(&self, query: &str, file: &ParsedFile) -> Result<FileJudgment, ScorerError> {
        Ok(self.judge(query, file).await?.judgment)
    }
}

/// One Jev request is one question, and an answer to it can be kept.
#[async_trait]
impl Cacheable for JevScorer {
    type Request = Request;
    type Detail = JevDetail;

    fn request(&self, query: &str, file: &ParsedFile) -> Result<Request, ScorerError> {
        JevScorer::request(self, query, file)
    }

    /// The endpoint as well as the body: a proxy and the API can answer one body
    /// differently, and a test's fake server must not read the entries a real
    /// run wrote. Everything else an answer depends on — the model, the mode's
    /// wording and criteria, the questions, how they were split across posts,
    /// the file's content and path, each section's heading and lines, each
    /// link's preview — is in the body.
    fn key(&self, request: &Request) -> Result<Vec<u8>, ScorerError> {
        serde_json::to_vec(&(&self.endpoint, request))
            .map_err(|source| ScorerError::Encode { source })
    }

    async fn call(
        &self,
        request: &Request,
        file: &ParsedFile,
    ) -> Result<(FileJudgment, JevDetail), ScorerError> {
        let outcome = self.judge_request(request, file).await?;
        Ok((outcome.judgment, outcome.detail))
    }
}

/// The key to call with: a blank variable is as good as an unset one.
fn required_key(value: Option<String>) -> Result<String, ScorerError> {
    match value {
        Some(key) if !key.trim().is_empty() => Ok(key),
        _ => Err(ScorerError::MissingApiKey),
    }
}

/// `text` cut to at most `limit` characters, on a character boundary, and told
/// so when something was dropped: the request never splits a character, and the
/// model never reads a fragment believing it is the whole thing.
fn clamp(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}\n[truncated at {limit} characters]")
}

/// 250 ms, then 500 ms: short enough that a retry is invisible next to the call
/// it repeats, and the server's `retry-after` wins when it sends one.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(250 * 2u64.pow(attempt - 1))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use serde_json::{Map, Value, json};

    use super::*;
    use crate::cache::{Cacheable, CachedScorer};
    use crate::testkit::TempDir;

    // --------------------------------------------------------- the fixture

    /// The wiki from #4: eight links on the index page, covering a target
    /// inside the root, one that escapes it, one that does not exist, and two
    /// links to the same page.
    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wiki")
    }

    fn fixture(page: &str) -> ParsedFile {
        parse::parse(root().join(page), root()).unwrap_or_else(|err| panic!("{page}: {err}"))
    }

    // ------------------------------------------------------ the fake server

    /// A one-endpoint HTTP server on a loopback port: every request is recorded
    /// whole — head and body — and `reply` decides the response to each attempt
    /// in turn. Tests therefore exercise the real request bytes, the real serde
    /// types and the real retry loop with no network and no test-only HTTP
    /// dependency.
    struct FakeApi {
        url: String,
        address: SocketAddr,
        exchanges: Arc<Mutex<Vec<Exchange>>>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    /// One request as it arrived: the head as bytes off the wire, and the body
    /// parsed, so a test can assert the protocol as well as the payload.
    #[derive(Clone)]
    struct Exchange {
        head: String,
        body: Value,
    }

    impl FakeApi {
        fn new(reply: impl Fn(usize, &Value) -> (u16, String) + Send + 'static) -> FakeApi {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
            let address = listener.local_addr().expect("the bound address");
            let exchanges = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let recorded = Arc::clone(&exchanges);
            let flag = Arc::clone(&stop);
            let thread = thread::spawn(move || {
                for stream in listener.incoming() {
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut stream) = stream else { break };
                    let Some(exchange) = read_request(&mut stream) else {
                        continue;
                    };
                    let request = exchange.body.clone();
                    let attempt = {
                        let mut seen = recorded.lock().expect("the lock is not poisoned");
                        seen.push(exchange);
                        seen.len() - 1
                    };
                    let (status, body) = reply(attempt, &request);
                    let _ = stream.write_all(response_bytes(status, &body).as_bytes());
                }
            });
            FakeApi {
                url: format!("http://{address}/v1/systemone"),
                address,
                exchanges,
                stop,
                thread: Some(thread),
            }
        }

        /// Every request body the server saw, in order.
        fn requests(&self) -> Vec<Value> {
            self.exchanges()
                .into_iter()
                .map(|exchange| exchange.body)
                .collect()
        }

        /// Every request head the server saw, in order: the request line and
        /// the headers, verbatim.
        fn heads(&self) -> Vec<String> {
            self.exchanges()
                .into_iter()
                .map(|exchange| exchange.head)
                .collect()
        }

        fn exchanges(&self) -> Vec<Exchange> {
            self.exchanges
                .lock()
                .expect("the lock is not poisoned")
                .clone()
        }

        /// A scorer pointed at this server, with previews on.
        fn scorer(&self) -> JevScorer {
            JevScorer::new("test-key", root())
                .expect("a client")
                .with_endpoint(&self.url)
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

    /// One HTTP request: its head and the body of the length the head promised.
    fn read_request(stream: &mut TcpStream) -> Option<Exchange> {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(head_end) = buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|at| at + 4)
            {
                let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if name.eq_ignore_ascii_case("content-length") {
                            value.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                if buffer.len() >= head_end + length {
                    let body =
                        String::from_utf8_lossy(&buffer[head_end..head_end + length]).to_string();
                    return Some(Exchange {
                        head,
                        body: serde_json::from_str(&body).unwrap_or(Value::Null),
                    });
                }
            }
            let read = stream.read(&mut chunk).ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
    }

    fn response_bytes(status: u16, body: &str) -> String {
        let reason = match status {
            200 => "OK",
            401 => "Unauthorized",
            422 => "Unprocessable Entity",
            429 => "Too Many Requests",
            529 => "Overloaded",
            _ => "Error",
        };
        format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    // ------------------------------------------------------- the responses

    /// A response body shaped like the API's.
    fn reply(answers: Map<String, Value>) -> String {
        json!({
            "model": "jev-1.13.0",
            "answers": Value::Object(answers),
            "usage": {"input_tokens": 1234, "output_tokens": 5},
        })
        .to_string()
    }

    /// A reply that answers every question a file with `sections` sections and
    /// `links` links asks. Section `i` gets `0.3 + i/100` and link `i` gets
    /// `0.4 + i/100`, so an answer landing on the wrong section or target is
    /// visible. A file asked in several posts is answered by
    /// [`numbered_reply`] instead, which numbers answers by the state entry
    /// each question names rather than by its place in the post.
    fn full_reply(sections: usize, links: usize, level: f64) -> String {
        let mut answers = Map::new();
        answers.insert(
            FILE_QUESTION.to_string(),
            json!({"type": "score", "score": level, "confidence": 0.87, "legend": {}, "probabilities": {}}),
        );
        for index in 0..sections {
            answers.insert(
                section_question(index),
                json!({"type": "noul", "noul": 0.3 + index as f64 / 100.0}),
            );
        }
        for index in 0..links {
            answers.insert(
                link_question(index),
                json!({"type": "noul", "noul": 0.4 + index as f64 / 100.0}),
            );
        }
        reply(answers)
    }

    /// A reply that answers whatever the request asks, read off the request
    /// itself: the file's Score is `level`, a section is scored from its own
    /// first line, and a link from the number in its own target. So a request
    /// split across posts is answered post by post, and every answer still
    /// belongs to the section or link it names rather than to its place in the
    /// post that asked.
    fn numbered_reply(request: &Value, level: f64) -> String {
        let mut answers = Map::new();
        let state = &request["state"];
        for id in request["questions"]
            .as_object()
            .expect("a question map")
            .keys()
        {
            let answer = if id == FILE_QUESTION {
                json!({"type": "score", "score": level, "confidence": 0.87})
            } else if let Some(position) = id
                .strip_prefix("section_")
                .and_then(|position| position.parse::<usize>().ok())
            {
                let first = state["sections"][position]["lines"][0]
                    .as_u64()
                    .expect("a section's first line");
                json!({"type": "noul", "noul": 0.3 + first as f64 / 1000.0})
            } else {
                let position = id
                    .strip_prefix("link_")
                    .and_then(|position| position.parse::<usize>().ok())
                    .unwrap_or_else(|| panic!("an unknown question id: {id}"));
                let target = state["links"][position]["target"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a target path in link {position}"));
                json!({"type": "noul", "noul": 0.4 + page_number(target) as f64 / 1000.0})
            };
            answers.insert(id.clone(), answer);
        }
        reply(answers)
    }

    /// The number in a generated page's name: `page-042.md` is 42. The hub
    /// fixture below names its pages this way so that an answer can be traced
    /// back to the link it belongs to.
    fn page_number(target: &str) -> u64 {
        target
            .trim_start_matches("page-")
            .trim_end_matches(".md")
            .parse()
            .unwrap_or_else(|error| panic!("{target}: {error}"))
    }

    /// What the fixture's index page links to, in order.
    const INDEX_TARGETS: [&str; 8] = [
        "payments/README.md",
        "payments/cutoffs.md",
        "payments/settlement.md",
        "notes/ledger.md",
        "../outside.md",
        "payments/missing.md",
        "payments/cutoffs.md",
        "../outside.md",
    ];

    // ------------------------------------------------------------ the tests

    /// One request per mode over the real builder, read off the wire: each mode
    /// sends its own questions — the file's, its sections' and its links' — and
    /// no two modes send the same ones. The criterion is the mode's, all of it,
    /// so a request cannot carry one mode's instructions and another's
    /// criteria.
    #[tokio::test]
    async fn every_mode_sends_its_own_instructions_and_criteria() {
        let query = "how are payments settled";
        let mut sent = Vec::new();
        for mode in [ABOUT.clone(), USEFUL_FOR.clone(), ANSWERS.clone()] {
            let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.0)));
            api.scorer()
                .with_mode(mode.clone())
                .judge(query, &fixture("index.md"))
                .await
                .expect("a judgment");
            sent.push((mode, api.requests().remove(0)));
        }

        for (mode, request) in &sent {
            let questions = request["questions"].as_object().expect("a question map");
            let file = &questions[FILE_QUESTION];
            assert_eq!(file["instructions"], mode.file_question.as_ref());
            assert_eq!(file["criteria"], json!(mode.file_levels));
            for index in 0..1 {
                let section = &questions[&section_question(index)];
                assert_eq!(
                    section["instructions"],
                    mode.section_question.replace("{index}", &index.to_string()),
                    "section {index} under {}",
                    mode.name
                );
                assert_eq!(section["criteria"]["true"], mode.section_true);
                assert_eq!(section["criteria"]["false"], mode.section_false);
            }
            for index in 0..INDEX_TARGETS.len() {
                let link = &questions[&link_question(index)];
                assert_eq!(
                    link["instructions"],
                    mode.link_question.replace("{index}", &index.to_string()),
                    "link {index} under {}",
                    mode.name
                );
                assert_eq!(link["criteria"]["true"], mode.link_true);
                assert_eq!(link["criteria"]["false"], mode.link_false);
            }
        }

        // A mode is a criterion, not a different query: the query, the file and
        // its links go in exactly as they are whatever mode asks about them.
        for (mode, request) in &sent {
            assert_eq!(
                request["state"], sent[0].1["state"],
                "{} changed the state, not just the questions",
                mode.name
            );
        }

        for (index, (mode, request)) in sent.iter().enumerate() {
            for (other, other_request) in &sent[index + 1..] {
                for question in [FILE_QUESTION, "section_0", "link_0"] {
                    assert_ne!(
                        request["questions"][question]["instructions"],
                        other_request["questions"][question]["instructions"],
                        "{} and {} ask the same question",
                        mode.name,
                        other.name
                    );
                }
                assert_ne!(
                    request["questions"][FILE_QUESTION]["criteria"],
                    other_request["questions"][FILE_QUESTION]["criteria"],
                    "{} and {} score on the same ladder",
                    mode.name,
                    other.name
                );
                for question in ["section_0", "link_0"] {
                    assert_ne!(
                        request["questions"][question]["criteria"],
                        other_request["questions"][question]["criteria"],
                        "{} and {} call the same thing a yes",
                        mode.name,
                        other.name
                    );
                }
            }
        }
    }

    /// A criterion of the caller's own replaces a mode's: the sentence reaches
    /// all three questions, the ladder is the criterion-independent one, and
    /// the request is nothing like the default mode's.
    #[tokio::test]
    async fn a_criterion_from_a_file_replaces_the_modes_wording() {
        let query = "how are payments settled";
        let criterion = "The content states the cut-off that decides when a payout is sent.";
        let mode = Mode::custom("criteria/payouts.md", criterion);

        let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.0)));
        api.scorer()
            .with_mode(mode.clone())
            .judge(query, &fixture("index.md"))
            .await
            .expect("a judgment");

        let requests = api.requests();
        let questions = requests[0]["questions"]
            .as_object()
            .expect("a question map");
        let file = &questions[FILE_QUESTION];
        assert!(
            file["instructions"]
                .as_str()
                .expect("instructions")
                .contains(criterion),
            "the file question judges by the caller's criterion: {file}"
        );
        assert_eq!(file["criteria"], json!(mode.file_levels));
        assert_eq!(file["criteria"].as_array().expect("levels").len(), 4);
        for question in ["section_0", "link_0"] {
            assert!(
                questions[question]["instructions"]
                    .as_str()
                    .expect("instructions")
                    .contains(criterion),
                "and so does the {question} question"
            );
        }
        assert_eq!(
            questions["section_0"]["criteria"]["true"], mode.section_true,
            "a caller's criterion gets this module's wording around it"
        );

        assert_ne!(
            file["instructions"],
            json!(USEFUL_FOR.file_question.as_ref())
        );
        assert_ne!(
            file["criteria"],
            json!(USEFUL_FOR.file_levels),
            "a caller's criterion gets the criterion-independent ladder"
        );
    }

    #[tokio::test]
    async fn one_request_carries_the_file_its_sections_and_every_link() {
        let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.4)));
        let file = fixture("index.md");

        api.scorer()
            .judge("how are payments settled", &file)
            .await
            .expect("a judgment");

        let requests = api.requests();
        assert_eq!(
            requests.len(),
            1,
            "one request per file, however many sections and links"
        );

        // The wire, not just the payload: the method, the path and the key are
        // what the API authenticates on, and no other test sees them.
        let head = &api.heads()[0];
        assert!(
            head.starts_with("POST /v1/systemone HTTP/1.1\r\n"),
            "{head}"
        );
        assert!(
            head.to_lowercase()
                .contains("authorization: bearer test-key"),
            "{head}"
        );

        let request = &requests[0];
        assert_eq!(request["model"], MODEL);

        let state = &request["state"];
        assert_eq!(state["query"], "how are payments settled");
        assert_eq!(state["file"]["title"], "Home");
        assert!(
            state["file"]["path"]
                .as_str()
                .expect("a path")
                .ends_with("tests/fixtures/wiki/index.md")
        );
        assert!(
            state["file"]["content"]
                .as_str()
                .expect("content")
                .contains("The index of the wiki."),
            "the state carries the file itself, not only its title"
        );

        // The sections are the parser's, heading, depth, lines and all: what
        // the model is asked about is what the caller will be told to read.
        assert_eq!(
            state["sections"],
            json!([{"heading": "Home", "level": 1, "lines": [6, 20]}]),
            "the sections are the ones #4's parser reports"
        );
        assert_eq!(
            state["sections"].as_array().expect("a section array").len(),
            file.sections.len()
        );

        let links = state["links"].as_array().expect("a link array");
        assert_eq!(links.len(), INDEX_TARGETS.len());
        assert_eq!(links[0]["target"], "payments/README.md");
        assert_eq!(links[0]["anchor"], "payments");
        assert_eq!(links[0]["heading"], "Home");
        assert!(
            links[0]["sentence"]
                .as_str()
                .expect("a sentence")
                .contains("payments")
        );
        assert_eq!(links[0]["target_preview"]["title"], "Payments");
        assert_eq!(
            links[2]["sentence"], "See also settlement and the ledger.",
            "the sentence is the context the scent judgment is made in"
        );
        assert_eq!(
            links[2]["target_preview"]["title"],
            "Instant payout settlement"
        );
        assert_eq!(
            links[2]["target_preview"]["frontmatter"][0]["key"], "title",
            "the preview carries the target's frontmatter"
        );

        let questions = request["questions"].as_object().expect("a question map");
        assert_eq!(
            questions.len(),
            file.sections.len() + INDEX_TARGETS.len() + 1,
            "one Score for the file, one Noul per section and one per link"
        );
        let file_question = &questions[FILE_QUESTION];
        assert_eq!(file_question["type"], "score");
        assert_eq!(
            file_question["criteria"].as_array().expect("levels").len(),
            USEFUL_FOR.levels()
        );
        assert!(
            file_question["instructions"]
                .as_str()
                .expect("instructions")
                .contains("`query`"),
            "the file question points at the state it judges"
        );

        for index in 0..file.sections.len() {
            let question = &questions[&section_question(index)];
            assert_eq!(question["type"], "noul");
            assert!(
                question["instructions"]
                    .as_str()
                    .expect("instructions")
                    .contains(&format!("`sections[{index}]`")),
                "section {index} is named in its own question"
            );
            assert_eq!(question["criteria"]["true"], USEFUL_FOR.section_true);
            assert_eq!(question["criteria"]["false"], USEFUL_FOR.section_false);
        }

        for index in 0..INDEX_TARGETS.len() {
            let question = &questions[&link_question(index)];
            assert_eq!(question["type"], "noul");
            assert!(
                question["instructions"]
                    .as_str()
                    .expect("instructions")
                    .contains(&format!("`links[{index}]`")),
                "link {index} is named in its own question"
            );
            assert_eq!(question["criteria"]["true"], USEFUL_FOR.link_true);
            assert_eq!(question["criteria"]["false"], USEFUL_FOR.link_false);
        }
        assert!(questions.get("link_8").is_none());
    }

    #[tokio::test]
    async fn every_section_and_link_answer_lands_on_its_own_entry() {
        let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.0)));
        let file = fixture("index.md");
        let outcome = api
            .scorer()
            .judge("how are payments settled", &file)
            .await
            .expect("a judgment");

        // The sections come back in the parser's order, with the parser's
        // ranges and the score of the question that named them.
        assert_eq!(
            outcome.judgment.sections,
            [SectionJudgment {
                heading: Some("Home".to_string()),
                lines: [6, 20],
                score: 0.3,
            }]
        );
        for (index, section) in outcome.judgment.sections.iter().enumerate() {
            assert_eq!(
                section.lines, file.sections[index].lines,
                "section {index} is the range the parser gave it"
            );
            assert!(
                (section.score - (0.3 + index as f64 / 100.0)).abs() < 1e-9,
                "section {index} got {}",
                section.score
            );
        }

        let targets: Vec<String> = outcome
            .judgment
            .links
            .iter()
            .map(|link| link.target.display().to_string())
            .collect();
        assert_eq!(targets, INDEX_TARGETS);

        for (index, link) in outcome.judgment.links.iter().enumerate() {
            assert!(
                (link.scent - (0.4 + index as f64 / 100.0)).abs() < 1e-9,
                "link {index} got {}",
                link.scent
            );
        }

        assert_eq!(
            outcome.detail.questions,
            file.sections.len() + INDEX_TARGETS.len() + 1
        );
        assert_eq!(outcome.detail.requests, 1, "everything fits one request");
        assert_eq!(outcome.detail.model, "jev-1.13.0");
        assert_eq!(outcome.detail.input_tokens, 1234);
        assert_eq!(outcome.detail.output_tokens, 5);
        assert!((outcome.detail.cost_usd() - 0.000_051_828).abs() < 1e-12);
    }

    #[tokio::test]
    async fn the_top_level_is_full_relevance_and_the_bottom_is_none() {
        let api = FakeApi::new(|attempt, _| (200, full_reply(1, 8, [3.0, 1.5, 0.0][attempt % 3])));
        let scorer = api.scorer();
        let file = fixture("index.md");

        let mut relevance = Vec::new();
        for _ in 0..3 {
            let outcome = scorer
                .judge("how are payments settled", &file)
                .await
                .expect("a judgment");
            relevance.push(outcome.judgment.relevance);
        }
        assert_eq!(relevance, [1.0, 0.5, 0.0]);

        let outcome = scorer
            .judge("how are payments settled", &file)
            .await
            .expect("a judgment");
        assert_eq!(outcome.detail.relevance_level, 3.0, "answers cycle again");
        assert_eq!(outcome.detail.relevance_confidence, 0.87);
    }

    #[tokio::test]
    async fn a_link_without_a_readable_target_is_judged_from_its_own_text() {
        let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.0)));
        api.scorer()
            .judge("how are payments settled", &fixture("index.md"))
            .await
            .expect("a judgment");

        let requests = api.requests();
        let links = requests[0]["state"]["links"]
            .as_array()
            .expect("a link array");

        // Link 4 escapes the root. The fixture has that file, and it is still
        // not read: its content is outside what the caller asked about.
        assert!(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/outside.md")
                .is_file()
        );
        assert_eq!(links[4]["target"], "../outside.md");
        assert!(links[4]["target_preview"].is_null());

        // Link 5 points at no file at all, so there is nothing to preview.
        assert_eq!(links[5]["target"], "payments/missing.md");
        assert!(links[5]["target_preview"].is_null());

        // Both are still asked about: the link is judged, not dropped.
        let questions = requests[0]["questions"]
            .as_object()
            .expect("a question map");
        assert_eq!(questions["link_4"]["type"], "noul");
        assert_eq!(questions["link_5"]["type"], "noul");
    }

    #[tokio::test]
    async fn a_file_with_no_links_asks_about_its_sections_and_nothing_else() {
        let api = FakeApi::new(|_, _| (200, full_reply(1, 0, 1.0)));
        let file = fixture("notes/reading.md");
        let outcome = api
            .scorer()
            .judge("what is there to read", &file)
            .await
            .expect("a judgment");

        let requests = api.requests();
        assert!(
            requests[0]["state"]["links"]
                .as_array()
                .expect("a link array")
                .is_empty()
        );
        assert_eq!(
            file.sections,
            [parse::Section {
                heading: Some("Reading".to_string()),
                level: 1,
                lines: [1, 3],
            }]
        );
        assert_eq!(
            requests[0]["questions"]
                .as_object()
                .expect("a question map")
                .len(),
            file.sections.len() + 1,
            "the file's Score and one Noul per section"
        );
        assert!(outcome.judgment.links.is_empty());
        assert_eq!(outcome.judgment.sections.len(), file.sections.len());
        assert_eq!(outcome.detail.questions, file.sections.len() + 1);
    }

    /// The content before a file's first heading is a section of its own: the
    /// state carries it with no heading and the parser's lines, and the
    /// judgment comes back with it in place, so the one range that covers that
    /// text reaches the caller.
    #[tokio::test]
    async fn the_preamble_is_a_section_with_no_heading() {
        let api = FakeApi::new(|_, _| (200, full_reply(2, 1, 2.0)));
        let file = fixture("notes/scratch.md");
        assert_eq!(file.sections[0].heading, None);

        let outcome = api
            .scorer()
            .judge("what is there to read", &file)
            .await
            .expect("a judgment");

        assert_eq!(
            api.requests()[0]["state"]["sections"],
            json!([
                {"heading": null, "level": 0, "lines": [1, 2]},
                {"heading": "Scratch", "level": 1, "lines": [3, 5]},
            ])
        );
        assert!(
            api.requests()[0]["questions"]
                .get(section_question(0))
                .is_some(),
            "the preamble is asked about like any other section"
        );
        assert_eq!(outcome.judgment.sections[0].heading, None);
        assert_eq!(outcome.judgment.sections[0].lines, [1, 2]);
        assert_eq!(
            outcome.judgment.sections[1].heading.as_deref(),
            Some("Scratch")
        );
    }

    /// A file with nothing in it has no sections either, so its one question is
    /// the file's own — and it is still a request, not nothing.
    #[tokio::test]
    async fn a_file_with_no_sections_still_has_its_score_asked() {
        let api = FakeApi::new(|_, _| (200, full_reply(0, 0, 2.0)));
        let dir = TempDir::new("empty-page");
        let path = dir.path().join("empty.md");
        fs::write(&path, "").expect("an empty page");
        let file = parse::parse(&path, dir.path()).expect("a parse");
        assert!(file.sections.is_empty());

        let outcome = api
            .scorer()
            .judge("what is there to read", &file)
            .await
            .expect("a judgment");

        let requests = api.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["questions"]
                .as_object()
                .expect("a question map")
                .len(),
            1
        );
        assert!(outcome.judgment.sections.is_empty());
        assert_eq!(outcome.detail.questions, 1);
    }

    #[tokio::test]
    async fn previews_can_be_left_out_of_the_state() {
        let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.0)));
        api.scorer()
            .with_previews(false)
            .judge("how are payments settled", &fixture("index.md"))
            .await
            .expect("a judgment");

        let requests = api.requests();
        let links = requests[0]["state"]["links"]
            .as_array()
            .expect("a link array");
        assert!(links.iter().all(|link| link["target_preview"].is_null()));

        // What the caller wrote about the link is still there, and so are the
        // questions: only the target's own text is withheld.
        assert_eq!(links[0]["anchor"], "payments");
        assert!(links[0]["sentence"].is_string());
        assert!(requests[0]["questions"].get("link_7").is_some());
    }

    /// The experiment #10 deferred: the frontmatter is the one part of a
    /// preview that can be dropped on its own. The title and the first
    /// paragraph are still there, so the link is judged on the same text minus
    /// the part most likely to be about the wiki's plumbing rather than the
    /// subject.
    #[tokio::test]
    async fn previews_can_carry_no_frontmatter() {
        let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.0)));
        api.scorer()
            .with_preview_frontmatter(false)
            .judge("how are payments settled", &fixture("index.md"))
            .await
            .expect("a judgment");

        let requests = api.requests();
        let links = requests[0]["state"]["links"]
            .as_array()
            .expect("a link array");
        assert!(
            links
                .iter()
                .all(|link| link["target_preview"].get("frontmatter").is_none()),
            "no link's preview carries frontmatter"
        );
        assert_eq!(
            links[2]["target_preview"]["title"],
            "Instant payout settlement"
        );
        assert!(
            links[2]["target_preview"]["first_paragraph"].is_string(),
            "the paragraph is still read: only the frontmatter is dropped"
        );
    }

    #[tokio::test]
    async fn an_answer_the_response_omits_is_an_error() {
        let api = FakeApi::new(|_, _| {
            let mut answers = Map::new();
            answers.insert(
                FILE_QUESTION.to_string(),
                json!({"type": "score", "score": 2.0, "confidence": 0.9}),
            );
            answers.insert(section_question(0), json!({"type": "noul", "noul": 0.5}));
            for index in 0..8 {
                if index == 3 {
                    continue;
                }
                answers.insert(link_question(index), json!({"type": "noul", "noul": 0.5}));
            }
            (200, reply(answers))
        });

        let error = api
            .scorer()
            .judge("query", &fixture("index.md"))
            .await
            .expect_err("link 3 has no answer");
        assert!(
            matches!(error, ScorerError::MissingAnswer { ref id } if id == "link_3"),
            "{error}"
        );
    }

    /// A section the response does not answer is a failed judgment, the same as
    /// a missing link answer: half a file's sections would be a reading list
    /// with a hole in it that the caller could not see.
    #[tokio::test]
    async fn a_section_answer_the_response_omits_is_an_error() {
        let api = FakeApi::new(|_, _| {
            let mut answers = Map::new();
            answers.insert(
                FILE_QUESTION.to_string(),
                json!({"type": "score", "score": 2.0, "confidence": 0.9}),
            );
            for index in 0..8 {
                answers.insert(link_question(index), json!({"type": "noul", "noul": 0.5}));
            }
            (200, reply(answers))
        });

        let error = api
            .scorer()
            .judge("query", &fixture("index.md"))
            .await
            .expect_err("section 0 has no answer");
        assert!(
            matches!(error, ScorerError::MissingAnswer { ref id } if id == "section_0"),
            "{error}"
        );
    }

    /// A section's answer of the wrong shape is an error too, and it names the
    /// section rather than the file.
    #[tokio::test]
    async fn a_section_answered_with_a_score_is_an_error() {
        let api = FakeApi::new(|_, _| {
            let mut answers = Map::new();
            answers.insert(
                FILE_QUESTION.to_string(),
                json!({"type": "score", "score": 2.0, "confidence": 0.9}),
            );
            answers.insert(
                section_question(0),
                json!({"type": "score", "score": 2.0, "confidence": 0.9}),
            );
            for index in 0..8 {
                answers.insert(link_question(index), json!({"type": "noul", "noul": 0.5}));
            }
            (200, reply(answers))
        });

        let error = api
            .scorer()
            .judge("query", &fixture("index.md"))
            .await
            .expect_err("a score where a noul belongs");
        assert!(
            matches!(
                error,
                ScorerError::WrongAnswerType { ref id, expected, found }
                    if id == "section_0" && expected == "noul" && found == "score"
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn an_answer_of_the_wrong_shape_is_an_error() {
        let api = FakeApi::new(|_, _| {
            let mut answers = Map::new();
            // The file question answered as if it had been asked as a Noul.
            answers.insert(
                FILE_QUESTION.to_string(),
                json!({"type": "noul", "noul": 0.9}),
            );
            answers.insert(section_question(0), json!({"type": "noul", "noul": 0.5}));
            for index in 0..8 {
                answers.insert(link_question(index), json!({"type": "noul", "noul": 0.5}));
            }
            (200, reply(answers))
        });

        let error = api
            .scorer()
            .judge("query", &fixture("index.md"))
            .await
            .expect_err("a noul where a score belongs");
        assert!(
            matches!(
                error,
                ScorerError::WrongAnswerType { ref id, expected, found }
                    if id == FILE_QUESTION && expected == "score" && found == "noul"
            ),
            "{error}"
        );
    }

    // ----------------------------------------------------------- the posts

    /// A generated page under `dir`: an H1, `headings` H2 sections of its own,
    /// and `pages` links to pages it also writes, each named `page-NNN.md` and
    /// each named in its own anchor, so a scent can be traced back to the link
    /// it belongs to.
    fn generated(dir: &TempDir, headings: usize, pages: usize) -> ParsedFile {
        let mut source = String::from("# Hub\n\nThe generated hub page.\n\n");
        for index in 0..headings {
            source.push_str(&format!(
                "## Heading {index}\n\nThe text under heading {index}.\n\n"
            ));
        }
        for index in 0..pages {
            let page = format!("page-{index:03}.md");
            fs::write(
                dir.path().join(&page),
                format!("# Page {index}\n\nPage {index} covers step {index} of the runbook.\n"),
            )
            .expect("a generated page");
            source.push_str(&format!(
                "- [Page {index}]({page}) — step {index} of the runbook.\n"
            ));
        }
        let path = dir.path().join("hub.md");
        fs::write(&path, &source).expect("a generated hub");
        parse::parse(&path, dir.path()).expect("a parse")
    }

    /// What one post costs, as the characters the API receives: the estimate
    /// [`JevScorer::shares`] works from is of these bytes, so a post over the
    /// budget is a split that did not happen.
    fn post_length(request: &Value) -> usize {
        serde_json::to_string(request)
            .expect("a request is strings and numbers")
            .len()
    }

    /// The budget in characters, at the four-per-token rule the split uses.
    const BUDGET_CHARS: usize = STATE_TOKENS * CHARS_PER_TOKEN;

    /// A hub page whose links are too many for one post is asked in several:
    /// every post inside the state budget, the file in every one, the file's
    /// Score in the first, and every answer still on the link that named it.
    #[tokio::test]
    async fn a_hub_page_too_big_for_one_post_is_split_across_posts() {
        let dir = TempDir::new("hub-posts");
        let file = generated(&dir, 0, 300);
        let content = fs::read_to_string(dir.path().join("hub.md")).expect("the hub");
        let api = FakeApi::new(|_, request| (200, numbered_reply(request, 2.0)));

        let outcome = api
            .scorer()
            .judge("how do I cut a release", &file)
            .await
            .expect("a judgment");

        let requests = api.requests();
        assert!(
            requests.len() > 1,
            "{} links should not fit one post, got {:?} characters",
            file.links.len(),
            requests.iter().map(post_length).collect::<Vec<_>>()
        );
        for request in &requests {
            assert!(
                post_length(request) <= BUDGET_CHARS,
                "a post of {} characters is over the budget",
                post_length(request)
            );
            assert_eq!(
                request["state"]["file"]["content"], content,
                "every post carries the file itself"
            );
        }

        // The file's Score is about the whole file, so it is asked once — and
        // in the first post, which is also the one whose answers a caller
        // reads first if the split ever has to be undone.
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["questions"].get(FILE_QUESTION).is_some())
                .count(),
            1
        );
        assert!(requests[0]["questions"].get(FILE_QUESTION).is_some());

        // Between them the posts ask about the file exactly once over: no
        // question is dropped in the split, and none is asked twice.
        let asked: usize = requests
            .iter()
            .map(|request| {
                request["questions"]
                    .as_object()
                    .expect("a question map")
                    .len()
            })
            .sum();
        assert_eq!(asked, file.sections.len() + file.links.len() + 1);

        // And the answers of the posts in order are the file's sections and
        // links in order, each on the entry whose own state named it.
        assert_eq!(outcome.judgment.links.len(), file.links.len());
        for (index, (judged, link)) in outcome.judgment.links.iter().zip(&file.links).enumerate() {
            assert_eq!(judged.target, link.target, "link {index}");
            let target = link.target.to_str().expect("a target path");
            let expected = 0.4 + page_number(target) as f64 / 1000.0;
            assert!(
                (judged.scent - expected).abs() < 1e-9,
                "link {index} got {}",
                judged.scent
            );
        }
        assert_eq!(outcome.judgment.sections.len(), file.sections.len());
        for (index, (judged, section)) in outcome
            .judgment
            .sections
            .iter()
            .zip(&file.sections)
            .enumerate()
        {
            assert_eq!(judged.heading, section.heading, "section {index}");
            assert_eq!(judged.lines, section.lines, "section {index}");
        }
        assert_eq!(outcome.detail.questions, asked);
        assert_eq!(
            outcome.detail.requests,
            requests.len(),
            "the accounting says how many requests the judgment took"
        );
    }

    /// Sections split the same way, in the file's own order: a page with more
    /// sections than fit one post comes back with every one of them, in
    /// document order, the ranges the parser gave them.
    #[tokio::test]
    async fn a_page_with_more_sections_than_fit_one_post_splits_them() {
        let dir = TempDir::new("section-posts");
        let file = generated(&dir, 300, 0);
        assert!(file.links.is_empty());
        let api = FakeApi::new(|_, request| (200, numbered_reply(request, 2.0)));

        let outcome = api
            .scorer()
            .judge("how do I cut a release", &file)
            .await
            .expect("a judgment");

        let requests = api.requests();
        assert!(
            requests.len() > 1,
            "{} sections should not fit one post, got {:?} characters",
            file.sections.len(),
            requests.iter().map(post_length).collect::<Vec<_>>()
        );
        for request in &requests {
            assert!(
                post_length(request) <= BUDGET_CHARS,
                "a post is over the budget"
            );
        }

        assert_eq!(outcome.judgment.sections.len(), file.sections.len());
        for (index, (judged, section)) in outcome
            .judgment
            .sections
            .iter()
            .zip(&file.sections)
            .enumerate()
        {
            assert_eq!(judged.heading, section.heading, "section {index}");
            assert_eq!(judged.lines, section.lines, "section {index}");
            let expected = 0.3 + section.lines[0] as f64 / 1000.0;
            assert!(
                (judged.score - expected).abs() < 1e-9,
                "section {index} got {}",
                judged.score
            );
        }
        assert!(outcome.judgment.links.is_empty());
    }

    #[tokio::test]
    async fn a_rejected_request_reports_the_status_and_the_body() {
        let api = FakeApi::new(|_, _| (422, r#"{"error":"state is too long"}"#.to_string()));

        let error = api
            .scorer()
            .judge("query", &fixture("index.md"))
            .await
            .expect_err("the request is rejected");
        match error {
            ScorerError::Status {
                status, ref body, ..
            } => {
                assert_eq!(status, 422);
                assert!(body.contains("state is too long"), "{body}");
            }
            other => panic!("expected a status error, got {other}"),
        }
        assert_eq!(
            api.requests().len(),
            1,
            "a rejected request is this code's bug, not a reason to retry"
        );
    }

    #[tokio::test]
    async fn a_rate_limited_request_is_retried_then_answered() {
        let api = FakeApi::new(|attempt, _| {
            if attempt == 0 {
                (429, r#"{"error":"rate limited"}"#.to_string())
            } else {
                (200, full_reply(1, 8, 2.0))
            }
        });

        let outcome = api
            .scorer()
            .judge("query", &fixture("index.md"))
            .await
            .expect("the retry is answered");

        assert_eq!(api.requests().len(), 2);
        assert_eq!(outcome.judgment.links.len(), INDEX_TARGETS.len());
    }

    #[tokio::test]
    async fn a_file_that_cannot_be_read_is_an_error() {
        let scorer = JevScorer::new("test-key", ".")
            .expect("a client")
            .with_endpoint("http://127.0.0.1:1/v1/systemone");
        let file = ParsedFile {
            path: PathBuf::from("no/such/file.md"),
            title: "gone".to_string(),
            frontmatter: Vec::new(),
            sections: Vec::new(),
            links: Vec::new(),
        };

        let error = scorer
            .judge("query", &file)
            .await
            .expect_err("there is nothing to send");
        assert!(matches!(error, ScorerError::Read { .. }), "{error}");
    }

    // ------------------------------------------------------------- the cache

    /// Every part of the request is part of the key, which is what keeps a
    /// stored answer from being served to a different question.
    #[test]
    fn the_key_covers_everything_the_answer_depends_on() {
        let dir = TempDir::new("key-coverage");
        let path = dir.path().join("page.md");
        fs::write(&path, "# Home\n\n[one](one.md)\n").expect("a page");
        let page = parse::parse(&path, dir.path()).expect("a parse");
        let scorer = JevScorer::new("test-key", dir.path()).expect("a client");
        let key = |scorer: &JevScorer, query: &str, file: &ParsedFile| {
            scorer
                .key(&scorer.request(query, file).expect("a request"))
                .expect("the key bytes")
        };
        let query = "how are payments settled";

        let base = key(&scorer, query, &page);
        assert_eq!(base, key(&scorer, query, &page), "one request, one key");

        // A second scorer, as the next run of the process would build it: the
        // same configuration has to land on the same key, or every run pays
        // again.
        let next_run = JevScorer::new("test-key", dir.path()).expect("a client");
        assert_eq!(
            base,
            key(&next_run, query, &page),
            "a repeat run is the same request"
        );

        assert_ne!(
            base,
            key(&scorer, "how do refunds work", &page),
            "the query is in the key"
        );

        // The same page, rewritten: the key follows what is on disk, so a
        // stored answer for the old text is never served to the new text.
        fs::write(&path, "# Home\n\n[one](one.md) and more.\n").expect("an edit");
        let edited = key(&scorer, query, &page);
        assert_ne!(base, edited, "the content is in the key");

        // A mode asks different questions of the same file: the criterion is
        // what the model answers, so it is what the answer depends on.
        let mut by_about = JevScorer::new("test-key", dir.path()).expect("a client");
        by_about.mode = ABOUT.clone();
        assert_ne!(
            edited,
            key(&by_about, query, &page),
            "the mode's questions are in the key"
        );

        // And a criterion of the caller's own, which is no mode's wording.
        let mut by_criteria = JevScorer::new("test-key", dir.path()).expect("a client");
        by_criteria.mode = Mode::custom("criteria.md", "It states the settlement cut-off.");
        assert_ne!(
            edited,
            key(&by_criteria, query, &page),
            "a criteria file's criterion is in the key too"
        );

        // And where the request goes: a fake server is not the API, and its
        // answers must not be read back as the API's.
        let elsewhere = JevScorer::new("test-key", dir.path())
            .expect("a client")
            .with_endpoint("http://127.0.0.1:1/v1/systemone");
        assert_ne!(
            edited,
            key(&elsewhere, query, &page),
            "the endpoint is in the key"
        );
    }

    /// The acceptance criterion, over the real request path: a second identical
    /// run is answered from disk, and an edit is not.
    #[tokio::test]
    async fn a_second_identical_run_makes_no_request() {
        let api = FakeApi::new(|_, _| (200, full_reply(1, 1, 2.0)));
        let dir = TempDir::new("cached-run");
        let path = dir.path().join("page.md");
        fs::write(&path, "# Home\n\n[one](one.md)\n").expect("a page");
        let page = parse::parse(&path, dir.path()).expect("a parse");
        let cached = CachedScorer::new(api.scorer(), dir.path()).expect("a cache");
        let query = "how are payments settled";

        let first = cached.score(query, &page).await.expect("a judgment");
        let second = cached.score(query, &page).await.expect("a judgment");

        assert_eq!(
            first, second,
            "the second run returns what the first stored"
        );
        assert_eq!(api.requests().len(), 1, "two runs, one request");
        assert_eq!(cached.calls(), 1, "and one real call");
        assert_eq!(cached.hits(), 1);

        // The page changes, so the stored answer is for text that is no longer
        // there.
        fs::write(&path, "# Home\n\n[one](one.md) and more.\n").expect("an edit");
        cached.score(query, &page).await.expect("a judgment");
        assert_eq!(api.requests().len(), 2, "an edit is a new request");

        // A different query about the same page, too.
        cached
            .score("how do refunds work", &page)
            .await
            .expect("a judgment");
        assert_eq!(api.requests().len(), 3, "so is a new query");
        assert_eq!(cached.calls(), 3, "every one of them was a call");
    }

    #[test]
    fn a_blank_key_is_no_key() {
        assert!(matches!(
            required_key(None),
            Err(ScorerError::MissingApiKey)
        ));
        assert!(matches!(
            required_key(Some(String::new())),
            Err(ScorerError::MissingApiKey)
        ));
        assert!(matches!(
            required_key(Some("  ".to_string())),
            Err(ScorerError::MissingApiKey)
        ));
        assert_eq!(
            required_key(Some("apik_test".to_string())).expect("a key"),
            "apik_test"
        );
    }

    #[test]
    fn text_over_the_limit_is_cut_on_a_character_boundary_and_says_so() {
        assert_eq!(clamp("short", 10), "short");
        assert_eq!(
            clamp("ééééé", 3),
            "ééé\n[truncated at 3 characters]",
            "a limit in characters, never half of one"
        );
    }
}
