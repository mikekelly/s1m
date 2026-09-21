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
//! its own share of the questions, and the answers merge into one judgment. The
//! split measures what it sends — the file, each link's entry and preview, and
//! the questions — and keeps every post under both of the API's budgets; see
//! [`JevScorer::pack`] and [`CHARS_PER_TOKEN`].
//!
//! There is no Rust SDK, so this calls the HTTP API directly:
//! <https://docs.typesafe.ai/api.md>. The wording of every question lives in
//! [`Mode`], so adding a relevance mode (#10) means adding a table entry, not
//! changing the builder.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
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
/// plus the longest question and 64k for the whole request; at the two
/// characters per token the split measures with ([`CHARS_PER_TOKEN`]), this
/// spends about five eighths of the state budget and leaves the link table the
/// rest.
const CONTENT_LIMIT: usize = 40_000;

/// The tokens the API allows for `state` plus the longest question, from
/// <https://docs.typesafe.ai/models>. Past this the request is rejected with
/// `max_tokens_exceeded`, which is what [#37] found the state of a link-heavy
/// page to be: the file's own text is not what fills the budget, its link table
/// is, and a link table that is split is what keeps every post under it.
///
/// [#37]: https://github.com/mikekelly/s1m/issues/37
const STATE_TOKENS: usize = 32_000;

/// The tokens the API allows for one request in total, same source. A state
/// that fits can still be over this when a file has many questions, so the
/// split keeps both budgets: `state` plus the longest question against
/// [`STATE_TOKENS`], and everything against this.
const REQUEST_TOKENS: usize = 64_000;

/// The characters one token is worth in this module's estimate: two, where what
/// it sends really measures two and a half to three.
///
/// Nothing here tokenises, so the split decides on a character count and this
/// is the only conversion in it. Measured for [#37] against the API's own
/// `usage`, over generated hubs and the vendored wiki: the JSON of a state —
/// the file's text, its section entries, and a link table with a preview per
/// link — came back at 2.9 to 3.1 characters per input token, the spike's
/// hundred-link hub at 2.5, and prose on its own better than four. The estimate
/// takes the worst of those, because the link table is the part that grows.
///
/// [#37]: https://github.com/mikekelly/s1m/issues/37
const CHARS_PER_TOKEN: usize = 2;

/// The characters [`STATE_TOKENS`] is spent in, at [`CHARS_PER_TOKEN`].
const STATE_CHARS: usize = STATE_TOKENS * CHARS_PER_TOKEN;

/// The characters [`REQUEST_TOKENS`] is spent in, at [`CHARS_PER_TOKEN`].
const REQUEST_CHARS: usize = REQUEST_TOKENS * CHARS_PER_TOKEN;

/// A title or a first paragraph cut to this many characters: a preview is a
/// hint for the scent judgment, not the page.
const PREVIEW_LIMIT: usize = 600;

/// The frontmatter a preview carries, whole fields in the target's own order
/// until this many characters are spent and no further field after that.
///
/// The frontmatter is the larger half of what a preview buys (`eval/REPORT.md`,
/// the preview experiment), and the largest block on either vendored wiki is
/// 653 characters of text — which this counts as 562 — so no measured page is
/// cut here. What the cap bounds is a page whose frontmatter is an essay:
/// without it one target adds its whole frontmatter to the state of every page
/// that links to it, which is the other half of what [#37] found over the state
/// budget.
///
/// [#37]: https://github.com/mikekelly/s1m/issues/37
const FRONTMATTER_LIMIT: usize = 1_200;

/// The headings a preview carries at most, from the target's own H2s and H3s
/// in order ([`JevScorer::with_preview_headings`]).
///
/// This and [`LEADS`] are the richer link state
/// [#46](https://github.com/mikekelly/s1m/issues/46) measures: a link whose
/// target's first paragraph says nothing about what the target leads to can
/// still be recognized from the headings under it. Both go through the same
/// link-table cost the split measures, so a cap here is what keeps a preview a
/// hint rather than a page.
///
/// [#46]: https://github.com/mikekelly/s1m/issues/46
const HEADINGS: usize = 40;

/// One heading of a preview, cut to this many characters and told so in the
/// text, the way [`clamp`] tells every other part of a preview it was cut.
const HEADING_LIMIT: usize = 80;

/// The links a preview carries at most, by their anchor text, from the target's
/// own in-root links in order and deduped
/// ([`JevScorer::with_preview_leads`]).
const LEADS: usize = 30;

/// One lead anchor of a preview, cut to this many characters and told so in the
/// text, the way [`clamp`] tells every other part of a preview it was cut.
const LEAD_LIMIT: usize = 60;

/// Room left over in a post for what holds it together — the braces, the
/// commas between items, the `model` field, the escaping of a quote in the
/// file's own text. A post is only split when the estimate crosses a budget
/// with this margin, so the split errs towards an early one.
const POST_MARGIN: usize = 1_024;

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
/// unusable for another's. The link question is here twice — one hop, and the
/// two-hop phrasing [`JevScorer::with_two_hop_links`] switches to — because the
/// wording is the criterion's, and what a yes means changes with it.
///
/// Only the name and the questions are [`Cow`]s, because a criteria file
/// supplies those in the caller's own words: its path names the mode and its
/// criterion goes into all of them. The ladder and the yes/no wording are this
/// module's and are static, which is why the criteria file borrows them.
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
    /// The same question asked about two hops instead of one: what this link
    /// reaches, or what the pages it leads to reach. [`JevScorer::with_two_hop_links`]
    /// sends this in place of [`Mode::link_question`], because a link to a page
    /// whose own title and first paragraph say nothing about its parts can still
    /// be worth following ([#46]). `{index}` is replaced the same way.
    ///
    /// [#46]: https://github.com/mikekelly/s1m/issues/46
    pub link_question_two_hop: Cow<'static, str>,
    /// What a yes means for that link, when the question is the two-hop one.
    pub link_true_two_hop: &'static str,
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
        let mut link_question_two_hop = String::from(
            "Is following `links[{index}]` likely to lead, directly or through the pages it links to, to content that meets this criterion: ",
        );
        link_question_two_hop.push_str(criterion);
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
            link_question_two_hop: Cow::Owned(link_question_two_hop),
            link_true_two_hop: CRITERION_LINK_TRUE_TWO_HOP,
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
    link_question_two_hop: Cow::Borrowed(
        "Does following `links[{index}]` lead, directly or through the pages it links to, to content on the subject of `query`?",
    ),
    link_true_two_hop: "The target is about the subject, or the pages it links to are.",
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
    link_question_two_hop: Cow::Borrowed(
        "Is following `links[{index}]` likely to lead, directly or through the pages it links to, to content useful for someone doing what `query` describes?",
    ),
    link_true_two_hop: "The target is on the subject, or the pages it links to are, so following this link is worth a reader's next step.",
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
    link_question_two_hop: Cow::Borrowed(
        "Does following `links[{index}]` lead, directly or through the pages it links to, to content containing the answer to `query`?",
    ),
    link_true_two_hop: "The target contains the answer or part of it, or leads to a page that does.",
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

const CRITERION_LINK_TRUE_TWO_HOP: &str =
    "The target meets the criterion, or leads to content that does.";

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
    /// The target's H2/H3 headings, in order, at most [`HEADINGS`] of them and
    /// each cut at [`HEADING_LIMIT`] characters. `None` when the run leaves
    /// them out ([`JevScorer::with_preview_headings`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    headings: Option<Vec<String>>,
    /// The anchor text of the target's own in-root links, in order, deduped, at
    /// most [`LEADS`] of them and each cut at [`LEAD_LIMIT`] characters.
    /// `None` when the run leaves them out
    /// ([`JevScorer::with_preview_leads`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    leads_to: Option<Vec<String>>,
}

/// What every post of one file carries whatever its share of the questions:
/// the query and the file itself. [`JevScorer::pack`] builds one and hands the
/// same one to every post.
///
/// It is one value rather than two arguments because the two go in together — a
/// post that carried the file but not the query would be a post about nothing —
/// and because they are the same bytes in every post of a file, which is what
/// [`Fixed::state`] measures once.
struct Head {
    query: String,
    file: FileState,
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

/// What one item of a post costs, in characters: its entry in the state, and
/// the question asked about it with the quotes and colon that hold the id it is
/// filed under.
///
/// Measured as the JSON each part is written as, for [`JevScorer::shares`].
#[derive(Debug, Clone, Copy, Default)]
struct Cost {
    state: usize,
    question: usize,
}

impl Cost {
    fn of<T: Serialize>(entry: &T, question: &Question, id: &str) -> Cost {
        Cost {
            // The comma that separates one state entry from the next.
            state: json_len(entry) + 1,
            question: json_len(question) + json_len(id) + 4,
        }
    }
}

/// What every post of one file pays before its share of the sections and
/// links: the state the file brings with it, and the questions the API's state
/// budget is measured against.
#[derive(Debug, Clone, Copy)]
struct Fixed {
    /// The query and the file, in every post's state.
    state: usize,
    /// The longest question the file asks, which the docs' 32k budget adds to
    /// the state.
    longest: usize,
    /// The file's own Score question, which the first post asks and no other.
    score: usize,
}

impl Fixed {
    /// Whether a post holding `used` so far can take one more item of `cost`
    /// under both of the API's budgets — `state` plus the longest question, and
    /// the whole request — with [`POST_MARGIN`] left over in each for the
    /// braces, the `model` field and the escaping of the file's own text.
    fn fits(&self, used: Cost, cost: Cost) -> bool {
        let state = self.state + used.state + cost.state;
        let whole = state + used.question + cost.question;
        state + self.longest + POST_MARGIN <= STATE_CHARS && whole + POST_MARGIN <= REQUEST_CHARS
    }
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
    preview_headings: bool,
    preview_leads: bool,
    two_hop: bool,
}

/// The link state a request carries, and the phrasing of the link question it
/// asks.
///
/// What ships since [#46] is every one of these except the switches the
/// experiment [#10] settled: a preview with the target's title, frontmatter and
/// first paragraph, its own H2/H3 headings and the anchor text of its own
/// in-root links, and the link question asked about two hops rather than one.
///
/// The whole set is what the evaluation harness's ablation rows and the CLI's
/// hidden opt-outs are built from, so the two cannot drift.
///
/// [#10]: https://github.com/mikekelly/s1m/issues/10
/// [#46]: https://github.com/mikekelly/s1m/issues/46
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    /// The target's title and first paragraph.
    pub previews: bool,
    /// Its frontmatter as well.
    pub frontmatter: bool,
    /// Its H2/H3 headings ([`JevScorer::with_preview_headings`]).
    pub headings: bool,
    /// The anchor text of its own in-root links
    /// ([`JevScorer::with_preview_leads`]).
    pub leads: bool,
    /// The link question asked about two hops rather than one
    /// ([`JevScorer::with_two_hop_links`]).
    pub two_hop: bool,
}

impl Default for Context {
    /// What ships.
    fn default() -> Context {
        Context::DEFAULT
    }
}

impl Context {
    /// What ships, as a value a `const` can hold: [`Context::default`] is the
    /// same state, and the evaluation harness's tables are built from this one
    /// because they are consts.
    pub const DEFAULT: Context = Context {
        previews: true,
        frontmatter: true,
        headings: true,
        leads: true,
        two_hop: true,
    };

    /// A scorer carrying this state.
    pub fn apply(self, scorer: JevScorer) -> JevScorer {
        scorer
            .with_previews(self.previews)
            .with_preview_frontmatter(self.frontmatter)
            .with_preview_headings(self.headings)
            .with_preview_leads(self.leads)
            .with_two_hop_links(self.two_hop)
    }
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
            preview_headings: true,
            preview_leads: true,
            two_hop: true,
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

    /// Sends each target's own H2/H3 headings with its preview, or leaves them
    /// out.
    ///
    /// Off by default. The experiment [#46] runs: a link is judged from its
    /// anchor, its sentence and the target's title, frontmatter and first
    /// paragraph, and a target's first paragraph does not always say what sits
    /// under it. A hub links to a page called "Payments" whose opening line is
    /// about payments, while the query's answer is in the section called
    /// "Cutoffs"; the heading list says so where the paragraph does not, and it
    /// costs no extra call. Whether it earns the state it adds is the
    /// evaluation harness's question, so this is an experiment's knob rather
    /// than a caller's choice until it answers.
    ///
    /// [#46]: https://github.com/mikekelly/s1m/issues/46
    pub fn with_preview_headings(mut self, headings: bool) -> Self {
        self.preview_headings = headings;
        self
    }

    /// Sends the anchor text of each target's own in-root links with its
    /// preview, or leaves them out.
    ///
    /// Off by default, and the same experiment [#46]: one hop of lookahead
    /// past the target. The link being judged leads to a section page whose
    /// own links name the topics under it, which is evidence about where the
    /// walk ends up that the target's own prose may not carry. Read from the
    /// target's links, which the preview reads anyway — nothing is followed,
    /// and no page is read for this that no preview would read — and bounded by
    /// [`LEADS`] and [`LEAD_LIMIT`] so that one hub cannot fill the state of
    /// every page that links to it.
    ///
    /// [#46]: https://github.com/mikekelly/s1m/issues/46
    pub fn with_preview_leads(mut self, leads: bool) -> Self {
        self.preview_leads = leads;
        self
    }

    /// Asks the link question about two hops rather than one: what this link
    /// reaches, or what the pages it leads to reach.
    ///
    /// Off by default, and the same experiment [#46]. The question it replaces
    /// ("is following this link likely to lead to content useful for someone
    /// doing what `query` describes") can be answered from one hop of evidence,
    /// and the model answers it that way: a link to a page that is only a step
    /// on the way scores low, however well it leads. The two-hop phrasing
    /// ([`Mode::link_question_two_hop`]) says how far the judgment reaches, and
    /// the `headings` and `leads` knobs are what give it something to reach
    /// with.
    ///
    /// [#46]: https://github.com/mikekelly/s1m/issues/46
    pub fn with_two_hop_links(mut self, two_hop: bool) -> Self {
        self.two_hop = two_hop;
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
    /// This is a size question, and the size that binds is the state: the docs
    /// allow 32k tokens for `state` plus the longest question, and the state
    /// grows with a link table at roughly 250 tokens a link with previews
    /// (`docs/spike-notes.md`) — a page of 17k characters and 92 previewed
    /// links is one request the API refuses and two it answers ([#37]). The
    /// file's own text is capped at [`CONTENT_LIMIT`], so the sections and
    /// links are dealt out into shares that fit beside it, the file goes in
    /// every post, and its answers merge into one judgment.
    ///
    /// The questions are dealt out in the file's own order, sections first, so
    /// the answers of the posts in order are the file's sections and links in
    /// order. A post's cost is measured as the JSON of its parts at
    /// [`CHARS_PER_TOKEN`] against [`STATE_CHARS`] and [`REQUEST_CHARS`], with
    /// [`POST_MARGIN`] to spare: nothing here tokenises, so the estimate is
    /// deliberately a conservative one.
    ///
    /// [#37]: https://github.com/mikekelly/s1m/issues/37
    fn pack(
        &self,
        query: &str,
        file: FileState,
        sections: Vec<SectionState>,
        mut links: Vec<LinkState>,
    ) -> Request {
        let fixed = Fixed {
            // The file's own state: what every post carries whatever its share.
            state: json_len(&query) + json_len(&file),
            // The API's state budget is the state plus the longest question,
            // and every post asks at least one question about what it carries.
            longest: self.longest_question(),
            // The file's Score is about the whole file, so it is asked once, in
            // the first post, and only that post pays for it.
            score: json_len(&self.score_question()),
        };

        // A file whose own state and longest question leave no room for a
        // previewed link drops its previews before it drops links: a link
        // judged from its anchor is worth more than a link never judged at all,
        // and a preview is a hint (`JevScorer::preview`). What this bounds is a
        // file whose text fills the budget on its own — content of
        // [`CONTENT_LIMIT`] characters is sent as more than that once its
        // quotes and newlines are escaped.
        let cheapest = links
            .iter()
            .filter(|link| link.target_preview.is_some())
            .map(json_len)
            .min();
        if cheapest.is_some_and(|entry| fixed.state + fixed.longest + entry > STATE_CHARS) {
            for link in &mut links {
                link.target_preview = None;
            }
        }

        let shares = self.shares(&sections, &links, &fixed);
        let head = Head {
            query: query.to_string(),
            file,
        };

        Request {
            posts: shares
                .iter()
                .enumerate()
                .map(|(index, share)| {
                    // The file's Score is about the whole file, so the first
                    // post asks it and later ones do not: three posts would
                    // otherwise buy three answers to one question.
                    self.post(&head, &sections, &links, share, index == 0)
                })
                .collect(),
        }
    }

    /// The characters of the longest question this file asks: what the state
    /// budget is measured with, because the API's 32k is `state` plus the
    /// longest question and every post carries both.
    ///
    /// The three shapes differ by a sentence or two of the mode's wording and
    /// by the id they are filed under; the longest of them is what the state
    /// has to fit beside.
    fn longest_question(&self) -> usize {
        [
            self.score_question(),
            self.section_question(0),
            self.link_question(0),
        ]
        .iter()
        .map(json_len)
        .max()
        .expect("three questions")
    }

    /// Deals the file's sections and links out into shares that each fit the
    /// API's budgets beside the file.
    ///
    /// Always at least one share, even for a file with no sections and no
    /// links: the file still has a Score to ask.
    fn shares(&self, sections: &[SectionState], links: &[LinkState], fixed: &Fixed) -> Vec<Share> {
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
                    Cost::of(
                        section,
                        &self.section_question(index),
                        &section_question(index),
                    ),
                )
            })
            .chain(links.iter().enumerate().map(|(index, link)| {
                (
                    Item::Link(index),
                    Cost::of(link, &self.link_question(index), &link_question(index)),
                )
            }));

        let mut shares = vec![Share::default()];
        // What this share holds so far: its own state entries and its own
        // questions. The file's Score is the first share's to ask, and no later
        // one pays for it.
        let mut used = Cost {
            question: fixed.score,
            ..Cost::default()
        };
        for (item, cost) in costs {
            let open = shares.last().expect("a share is always open");
            // A share with something in it gives way to an item that would not
            // fit; an empty one takes the item whatever it costs, so dealing
            // always makes progress.
            if !open.is_empty() && !fixed.fits(used, cost) {
                shares.push(Share::default());
                used = Cost::default();
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
            used.state += cost.state;
            used.question += cost.question;
        }
        shares
    }

    /// One post: the file, its share of the sections and links, and where each
    /// answer goes. `score` is whether this post asks the file's own question,
    /// which is the first post's to ask.
    fn post(
        &self,
        head: &Head,
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
            query: head.query.clone(),
            file: head.file.clone(),
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
    /// asks it. [`JevScorer::with_two_hop_links`] asks the mode's two-hop
    /// phrasing of it instead, and takes that phrasing's yes with it.
    fn link_question(&self, index: usize) -> Question {
        let (instructions, yes) = match self.two_hop {
            true => (
                &self.mode.link_question_two_hop,
                self.mode.link_true_two_hop,
            ),
            false => (&self.mode.link_question, self.mode.link_true),
        };
        Question::Noul {
            instructions: instructions.replace("{index}", &index.to_string()),
            criteria: NoulCriteria {
                yes,
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

    /// The target's title, frontmatter and first paragraph, each bounded.
    ///
    /// Nothing when previews are off, when the link leaves the root — that
    /// content is outside what the caller asked s1m to look at — or when the
    /// target cannot be read, which is the normal state of a broken link. Those
    /// links are still judged, from their anchor, sentence and heading. The
    /// frontmatter is the one part that can be dropped on its own
    /// ([`JevScorer::with_preview_frontmatter`]).
    ///
    /// Every part is bounded, because a preview is a hint about a target and
    /// the target is a file this walk did not choose: a title is one line, the
    /// first paragraph is cut at [`PREVIEW_LIMIT`], and the frontmatter at
    /// [`FRONTMATTER_LIMIT`]. Without those bounds one page's frontmatter is
    /// added to the state of every page that links to it.
    fn preview(&self, link: &Link) -> Option<PreviewState> {
        if !self.previews || !link.in_root {
            return None;
        }
        let preview = parse::preview(self.root.join(&link.target), &self.root).ok()?;
        Some(PreviewState {
            title: clamp(&preview.title, PREVIEW_LIMIT),
            frontmatter: self
                .preview_frontmatter
                .then(|| frontmatter(preview.frontmatter))
                .filter(|frontmatter| !frontmatter.is_empty()),
            first_paragraph: preview
                .first_paragraph
                .map(|paragraph| clamp(&paragraph, PREVIEW_LIMIT)),
            headings: self.preview_headings.then(|| headings(preview.headings)),
            leads_to: self.preview_leads.then(|| leads_to(preview.leads)),
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

/// The headings a preview carries: the target's own, in order, at most
/// [`HEADINGS`] of them and each cut at [`HEADING_LIMIT`] characters.
fn headings(headings: Vec<String>) -> Vec<String> {
    headings
        .into_iter()
        .take(HEADINGS)
        .map(|heading| clamp(&heading, HEADING_LIMIT))
        .collect()
}

/// The lead anchors a preview carries: the target's in-root link text, in
/// order, with an anchor that repeats an earlier one left out, at most [`LEADS`]
/// of them and each cut at [`LEAD_LIMIT`] characters.
///
/// A page names the same target in its opening sentence, its overview table and
/// its "see also"; the model reading one anchor three times learns nothing the
/// first did not say, and the state pays for each copy.
fn leads_to(leads: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    leads
        .into_iter()
        .filter(|lead| seen.insert(lead.clone()))
        .take(LEADS)
        .map(|lead| clamp(&lead, LEAD_LIMIT))
        .collect()
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

/// The frontmatter a preview carries: whole fields in the target's own order
/// until [`FRONTMATTER_LIMIT`] characters are spent, and no field after that.
///
/// The fields are kept whole rather than cut, so a preview never shows half a
/// `related:` list as if it were the list. A target whose frontmatter is an
/// essay keeps its opening fields and loses the rest, which is what bounds one
/// page's frontmatter to one page's worth of every request that links to it.
fn frontmatter(fields: Vec<FrontmatterField>) -> Vec<FrontmatterField> {
    let mut kept = Vec::new();
    let mut used = 0;
    for field in fields {
        let cost = json_len(&field) + 1;
        if used + cost > FRONTMATTER_LIMIT {
            break;
        }
        used += cost;
        kept.push(field);
    }
    kept
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

        /// A scorer pointed at this server, with previews on and the fixture
        /// wiki as the root link previews are read from.
        fn scorer(&self) -> JevScorer {
            self.scorer_in(&root())
        }

        /// A scorer pointed at this server whose root is `root`: the directory
        /// a link's preview is read from, which is the temporary directory for
        /// a test that generated the pages it links to.
        fn scorer_in(&self, root: &Path) -> JevScorer {
            JevScorer::new("test-key", root)
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

    /// A reply that answers every question the request asks, whatever its ids
    /// are: a Score for the file's own, a Noul for every other.
    ///
    /// [`numbered_reply`] numbers its answers by the link a question names,
    /// which is what a test tracing a scent back to its link needs and what a
    /// page whose targets are not `page-NNN.md` cannot use.
    fn any_reply(request: &Value, level: f64) -> String {
        let answers = request["questions"]
            .as_object()
            .expect("a question map")
            .keys()
            .map(|id| {
                let answer = if id == FILE_QUESTION {
                    json!({"type": "score", "score": level, "confidence": 0.87})
                } else {
                    json!({"type": "noul", "noul": 0.6})
                };
                (id.clone(), answer)
            })
            .collect::<Map<String, Value>>();
        reply(answers)
    }

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
                    mode.link_question_two_hop
                        .replace("{index}", &index.to_string()),
                    "link {index} under {} is asked about what it reaches",
                    mode.name
                );
                assert_eq!(link["criteria"]["true"], mode.link_true_two_hop);
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
            assert_eq!(question["criteria"]["true"], USEFUL_FOR.link_true_two_hop);
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

    /// The `headings` half of the link state [#46] added, which ships: the
    /// target's own H2s and H3s, in order, in its preview. The H1 is not among
    /// them — the preview's title already carries it — and a link that leaves
    /// the root has no preview, so no headings either.
    ///
    /// Left out ([`JevScorer::with_preview_headings`], the hidden
    /// `--no-preview-headings`), the key is absent and everything else in the
    /// preview is what it was.
    ///
    /// [#46]: https://github.com/mikekelly/s1m/issues/46
    #[tokio::test]
    async fn the_targets_headings_ship_in_the_preview() {
        let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.0)));
        api.scorer()
            .judge("how are payments settled", &fixture("index.md"))
            .await
            .expect("a judgment");

        let links = api.requests().remove(0)["state"]["links"].clone();
        // The first link is payments/README.md: heading levels 2 and 3, in the
        // file's own order.
        assert_eq!(
            links[0]["target_preview"]["headings"],
            json!(["Instant payouts", "Windows", "Settlement"])
        );
        assert_eq!(INDEX_TARGETS[4], "../outside.md");
        assert!(
            links[4]["target_preview"].is_null(),
            "a link that leaves the root has no preview, so no headings either"
        );
        // The whole preview, which is what a link is judged from: the headings
        // and the lead anchors beside the title, the frontmatter and the
        // paragraph.
        let mut keys: Vec<&str> = links[2]["target_preview"]
            .as_object()
            .expect("a preview")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "first_paragraph",
                "frontmatter",
                "headings",
                "leads_to",
                "title"
            ]
        );

        // Left out, no link carries the key and the rest of the preview is
        // untouched.
        let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.0)));
        api.scorer()
            .with_preview_headings(false)
            .judge("how are payments settled", &fixture("index.md"))
            .await
            .expect("a judgment");

        let preview = &api.requests()[0]["state"]["links"][0]["target_preview"];
        assert!(preview.get("headings").is_none());
        assert!(preview["first_paragraph"].is_string());
        assert!(
            preview["leads_to"].is_array(),
            "one switch does not turn the other with it"
        );
    }

    /// The `leads_to` half of the same state: the anchor text of the target's
    /// own in-root links, in order and deduped, which is one hop of lookahead
    /// past the page the link being judged points at.
    ///
    /// [#46]: https://github.com/mikekelly/s1m/issues/46
    #[tokio::test]
    async fn the_targets_own_link_text_ships_in_the_preview() {
        let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.0)));
        api.scorer()
            .judge("how are payments settled", &fixture("index.md"))
            .await
            .expect("a judgment");

        let links = api.requests().remove(0)["state"]["links"].clone();
        // payments/README.md links to payouts, then the ledger, then the
        // cutoffs — twice, once from its prose and once as a wikilink — then to
        // a window. The repeat is dropped; the link inside its fenced code
        // block was never a link.
        assert_eq!(
            links[0]["target_preview"]["leads_to"],
            json!(["payouts", "ledger", "cutoffs", "ten minute"])
        );
        // payments/cutoffs.md leads on with one wikilink, and the fixture's
        // index page links to it second.
        assert_eq!(
            links[1]["target_preview"]["leads_to"],
            json!(["settlement"])
        );

        let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.0)));
        api.scorer()
            .with_preview_leads(false)
            .judge("how are payments settled", &fixture("index.md"))
            .await
            .expect("a judgment");
        let preview = &api.requests()[0]["state"]["links"][0]["target_preview"];
        assert!(preview.get("leads_to").is_none());
        assert!(
            preview["headings"].is_array(),
            "the headings are still there: only the leads are dropped"
        );
    }

    /// The fourth part of [#46], which ships: the link question asked about two
    /// hops, with the yes-criterion that matches it. One question's wording
    /// changes — the file's and the sections' are the mode's, and the
    /// no-criterion of a link is still what a link that leads nowhere gets.
    ///
    /// Asked about one hop again ([`JevScorer::with_two_hop_links`], the hidden
    /// `--one-hop-links`), the question and its criterion are the mode's own:
    /// the state is unchanged either way, because this is a question rather than
    /// a field.
    ///
    /// [#46]: https://github.com/mikekelly/s1m/issues/46
    #[tokio::test]
    async fn the_link_question_is_asked_about_two_hops() {
        let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.0)));
        api.scorer()
            .judge("how are payments settled", &fixture("index.md"))
            .await
            .expect("a judgment");

        let request = api.requests().remove(0);
        let questions = request["questions"].clone();
        for index in 0..INDEX_TARGETS.len() {
            assert_eq!(
                questions[&link_question(index)]["instructions"],
                USEFUL_FOR
                    .link_question_two_hop
                    .replace("{index}", &index.to_string()),
                "link {index} is asked about what it reaches"
            );
            assert_eq!(
                questions[&link_question(index)]["criteria"]["true"],
                USEFUL_FOR.link_true_two_hop
            );
            assert_eq!(
                questions[&link_question(index)]["criteria"]["false"],
                USEFUL_FOR.link_false
            );
        }
        assert_eq!(
            questions[FILE_QUESTION]["instructions"],
            USEFUL_FOR.file_question.as_ref(),
            "the file is still judged by the mode"
        );
        assert_eq!(
            questions["section_0"]["instructions"],
            USEFUL_FOR.section_question.replace("{index}", "0")
        );

        // The one-hop question is the same state asked less far.
        let api = FakeApi::new(|_, _| (200, full_reply(1, 8, 2.0)));
        api.scorer()
            .with_two_hop_links(false)
            .judge("how are payments settled", &fixture("index.md"))
            .await
            .expect("a judgment");

        let one_hop = api.requests().remove(0);
        assert_eq!(
            one_hop["questions"][&link_question(0)]["instructions"],
            USEFUL_FOR.link_question.replace("{index}", "0")
        );
        assert_eq!(
            one_hop["questions"][&link_question(0)]["criteria"]["true"],
            USEFUL_FOR.link_true
        );
        assert_eq!(
            one_hop["state"], request["state"],
            "and the state does not move with it"
        );
    }

    /// Each part of the richer state is bounded where [#46] says: 40 headings
    /// at 80 characters each, 30 lead anchors at 60, and an anchor the target
    /// repeats sent once. A preview is a hint, and a page that is all headings
    /// or links is exactly the page whose state has to stay a hint.
    ///
    /// [#46]: https://github.com/mikekelly/s1m/issues/46
    #[tokio::test]
    async fn the_headings_and_leads_a_preview_carries_are_bounded() {
        let api = FakeApi::new(|_, _| (200, full_reply(1, 1, 2.0)));
        let dir = TempDir::new("bounded-preview");
        let long = "a phrase that runs on ".repeat(6);
        let mut target =
            String::from("# Target\n\nA page with more under it than a preview carries.\n\n");
        for index in 0..HEADINGS + 5 {
            target.push_str(&format!("### Heading {index} {long}\n\nText.\n\n"));
        }
        for index in 0..LEADS + 10 {
            // The second link repeats the first link's anchor, word for word:
            // the same target named twice in a page's prose and its table.
            let anchor = match index {
                1 => format!("Lead 0 {long}"),
                _ => format!("Lead {index} {long}"),
            };
            target.push_str(&format!("- [{anchor}](lead-{index}.md)\n"));
        }
        let path = dir.path().join("target.md");
        fs::write(&path, &target).expect("a target page");
        let hub = dir.path().join("hub.md");
        fs::write(
            &hub,
            "# Hub\n\nA [target](target.md) with a lot under it.\n",
        )
        .expect("a hub page");

        let file = parse::parse(&hub, dir.path()).expect("a parse");
        api.scorer_in(dir.path())
            .with_preview_headings(true)
            .with_preview_leads(true)
            .judge("what is under the target", &file)
            .await
            .expect("a judgment");

        let preview = api.requests().remove(0)["state"]["links"][0]["target_preview"].clone();
        let headings = preview["headings"].as_array().expect("a heading list");
        assert_eq!(headings.len(), HEADINGS, "the list stops at {HEADINGS}");
        assert_eq!(
            headings[0],
            json!(clamp(&format!("Heading 0 {long}"), HEADING_LIMIT)),
            "each heading is cut at {HEADING_LIMIT} characters and says so"
        );
        assert!(
            headings[0]
                .as_str()
                .is_some_and(|heading| heading.ends_with("[truncated at 80 characters]")),
            "the cut is visible to the model: {}",
            headings[0]
        );

        let leads = preview["leads_to"].as_array().expect("a lead list");
        assert_eq!(leads.len(), LEADS, "the list stops at {LEADS}");
        assert_eq!(
            leads[1],
            json!(clamp(&format!("Lead 2 {long}"), LEAD_LIMIT)),
            "the lead whose anchor repeated the one before it is not sent twice"
        );
        assert!(
            leads[1]
                .as_str()
                .is_some_and(|lead| lead.ends_with("[truncated at 60 characters]")),
            "each anchor is cut at {LEAD_LIMIT} characters and says so: {}",
            leads[1]
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
    /// [`JevScorer::shares`] works from is of these bytes.
    fn post_length(request: &Value) -> usize {
        json_len(request)
    }

    /// What one post's state costs, the same way: what the docs' 32k budget is
    /// measured on, plus the longest question the post asks.
    fn state_length(request: &Value) -> usize {
        json_len(&request["state"])
            + request["questions"]
                .as_object()
                .expect("a question map")
                .values()
                .map(json_len)
                .max()
                .unwrap_or(0)
    }

    /// Both of the API's budgets, checked on one post as the API receives it.
    ///
    /// These are the characters the estimate is made of, so a post over either
    /// is the split letting the API refuse a request: `state` plus the longest
    /// question against [`STATE_CHARS`], and the whole request against
    /// [`REQUEST_CHARS`].
    fn assert_within_budget(request: &Value) {
        assert!(
            state_length(request) <= STATE_CHARS,
            "a post is over the state budget: {} characters of state and longest question, over {STATE_CHARS}",
            state_length(request)
        );
        assert!(
            post_length(request) <= REQUEST_CHARS,
            "a post is over the request budget: {} characters, over {REQUEST_CHARS}",
            post_length(request)
        );
    }

    /// The richer state is still state ([#46]): a page whose every link carries
    /// the target's headings and its own link text adds more per link than one
    /// post holds, so it is split like any other over-budget page — every post
    /// inside both of the API's budgets, the path the walk came by in each, and
    /// every link still judged.
    ///
    /// [#46]: https://github.com/mikekelly/s1m/issues/46
    #[tokio::test]
    async fn a_page_whose_previews_carry_headings_and_leads_is_split_across_posts() {
        let api = FakeApi::new(|_, request| (200, any_reply(request, 2.0)));
        let dir = TempDir::new("richer-previews");
        let mut hub = String::from("# Hub\n\nThe hub page for the runbook.\n\n");
        for index in 0..160 {
            let page = format!("target-{index:03}.md");
            let mut target = format!("# Target {index}\n\nWhat target {index} is about.\n\n");
            for heading in 0..10 {
                target.push_str(&format!(
                    "## Heading {heading} of target {index}, in the page's own words\n\nSomething under it.\n\n"
                ));
            }
            for lead in 0..10 {
                target.push_str(&format!(
                    "- [Outbound link {lead} to another page of the wiki](other-{lead}.md)\n"
                ));
            }
            fs::write(dir.path().join(&page), &target).expect("a generated target");
            hub.push_str(&format!(
                "- [Target {index}]({page}) — step {index} of the runbook.\n"
            ));
        }
        let path = dir.path().join("hub.md");
        fs::write(&path, &hub).expect("the hub");
        let file = parse::parse(&path, dir.path()).expect("a parse");
        let outcome = api
            .scorer_in(dir.path())
            .with_preview_headings(true)
            .with_preview_leads(true)
            .judge("how does step 12 of the runbook work", &file)
            .await
            .expect("a judgment");

        let requests = api.requests();
        assert!(
            whole_state(&requests) > STATE_CHARS,
            "the fixture's state has to be over one post's budget for this to test the split: {} characters",
            whole_state(&requests)
        );
        assert!(
            requests.len() > 1,
            "so it is split: {} posts",
            requests.len()
        );
        for request in &requests {
            assert_within_budget(request);
        }
        assert_eq!(
            outcome.detail.requests,
            requests.len(),
            "what the judgment reports is what it took"
        );
        assert_eq!(
            outcome.judgment.links.len(),
            file.links.len(),
            "every link is judged, whichever post asked about it"
        );
        assert_eq!(outcome.judgment.sections.len(), file.sections.len());
    }

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
            assert_within_budget(request);
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
            assert_within_budget(request);
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

    // -------------------------------------------------------- the state budget

    /// The page [#37] was found on: a page of around 17,000 characters with 22
    /// headings and 92 links, each to a sibling page with frontmatter of its
    /// own, so every link carries a preview into the state.
    ///
    /// [#37]: https://github.com/mikekelly/s1m/issues/37
    fn big_page(dir: &TempDir) -> ParsedFile {
        let headings = 22;
        let pages = 92;
        let mut source = String::from(
            "---\ntitle: Hub\ntags: release, runbook\n---\n\n# Hub\n\nThe hub page for the release process.\n\n",
        );
        let mut next = 0;
        for heading in 0..headings {
            source.push_str(&format!(
                "## Heading {heading}\n\nThe text under heading {heading} explains how the release process works, in a sentence of ordinary wiki length.\n\n"
            ));
            // Four links under every heading, and one more under the first four:
            // 92 in all, so `page_number` finds the link an answer belongs to.
            for _ in 0..4 + usize::from(heading < pages - headings * 4) {
                source.push_str(&format!(
                    "- [Page {next}](page-{next:03}.md) — step {next} of the release runbook, and a sentence about it that runs on a little.\n"
                ));
                next += 1;
            }
            source.push('\n');
        }
        // The file's own text, up to the size the issue reports: it is not what
        // fills the budget, but it is what the page is.
        while source.chars().count() < 16_984 {
            source.push_str("Filler prose that a wiki author would have written here.\n\n");
        }
        for index in 0..pages {
            fs::write(
                dir.path().join(format!("page-{index:03}.md")),
                format!(
                    "---\ntitle: Page {index}\ntags: release, runbook, packaging\nrelated: [page-001.md, page-002.md, page-003.md]\nsummary: How step {index} of the release runbook works, with notes about packaging and publishing.\nupdated: 2026-09-20\nowner: someone\n---\n\n# Page {index}\n\nPage {index} covers step {index} of the runbook in a paragraph of the ordinary length a wiki author writes.\n"
                ),
            )
            .expect("a generated page");
        }
        let path = dir.path().join("hub.md");
        fs::write(&path, &source).expect("the hub");
        parse::parse(&path, dir.path()).expect("a parse")
    }

    /// The state one post would carry if the file were not split: every section
    /// and every link of every post, plus the file the posts all carry.
    fn whole_state(requests: &[Value]) -> usize {
        let file =
            json_len(&requests[0]["state"]["query"]) + json_len(&requests[0]["state"]["file"]);
        let entries: usize = requests
            .iter()
            .map(|request| {
                let sections = request["state"]["sections"]
                    .as_array()
                    .expect("a section list");
                let links = request["state"]["links"].as_array().expect("a link list");
                sections
                    .iter()
                    .chain(links.iter())
                    .map(json_len)
                    .sum::<usize>()
                    + sections.len()
                    + links.len()
            })
            .sum();
        file + entries
    }

    /// A page of 17k characters with 92 links to pages that have frontmatter:
    /// one request, whose state was over the API's budget, and the API refused
    /// it with `max_tokens_exceeded` ([#37]).
    ///
    /// The state of that page does not fit one post, so it is judged in
    /// several, and every post — the state plus its longest question, and the
    /// whole request — is inside the budget the estimate keeps it in. Every
    /// answer still lands on the link that named it.
    ///
    /// [#37]: https://github.com/mikekelly/s1m/issues/37
    #[tokio::test]
    async fn a_page_of_17k_chars_with_92_previewed_links_is_split_across_posts() {
        let dir = TempDir::new("issue-37");
        let file = big_page(&dir);
        assert_eq!(file.links.len(), 92);
        assert_eq!(file.sections.len(), 23);
        let api = FakeApi::new(|_, request| (200, numbered_reply(request, 2.0)));

        let outcome = api
            .scorer_in(dir.path())
            .judge("how do I cut a release and publish the package", &file)
            .await
            .expect("a judgment");

        let requests = api.requests();
        assert!(
            whole_state(&requests) > STATE_CHARS,
            "the page's own state has to be over one post's budget for this to be the reported bug: {} characters",
            whole_state(&requests)
        );
        assert!(
            requests.len() > 1,
            "a state that does not fit is judged in several posts, got {}",
            requests.len()
        );
        for request in &requests {
            assert_within_budget(request);
        }

        // The answers still come back on the entries that asked for them: one
        // per link, on its own target, with the scent the fake numbered it by.
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
        assert_eq!(outcome.detail.requests, requests.len());
    }

    /// A preview is a hint about a target, and the target is a file this walk
    /// did not choose: every part of one is bounded, so no page's frontmatter
    /// or first paragraph is added whole to every request that links to it
    /// ([#37]).
    ///
    /// [#37]: https://github.com/mikekelly/s1m/issues/37
    #[tokio::test]
    async fn a_preview_is_bounded_by_the_state_it_adds() {
        let dir = TempDir::new("bounded-preview");
        let essay = "Long summary prose about packaging and publishing. ".repeat(500);
        let heading = "# ".to_string() + &"T".repeat(4_000);
        fs::write(
            dir.path().join("big.md"),
            format!(
                "---\ntitle: Big\ntags: release, runbook\nsummary: {essay}\n---\n\n{heading}\n\n{}\n",
                "Paragraph prose. ".repeat(400)
            ),
        )
        .expect("a page");
        fs::write(dir.path().join("hub.md"), "# Hub\n\n[Big](big.md)\n").expect("a hub");
        let file = parse::parse(dir.path().join("hub.md"), dir.path()).expect("a parse");

        let api = FakeApi::new(|_, request| (200, any_reply(request, 1.5)));
        api.scorer_in(dir.path())
            .judge("how do I cut a release", &file)
            .await
            .expect("a judgment");

        let requests = api.requests();
        let preview = &requests[0]["state"]["links"][0]["target_preview"];
        assert!(
            !preview.is_null(),
            "the target is readable, so there is a preview: {preview}"
        );
        let fields = preview["frontmatter"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            json_len(&fields) <= FRONTMATTER_LIMIT,
            "the frontmatter is over its own budget: {} characters",
            json_len(&fields)
        );
        assert!(
            fields.iter().any(|field| field["key"] == "title"),
            "the fields that fit are kept whole and in order: {fields:?}"
        );
        assert!(
            !fields.iter().any(|field| field["key"] == "summary"),
            "the field that would not fit is left out rather than cut in half: {fields:?}"
        );
        let paragraph = preview["first_paragraph"]
            .as_str()
            .expect("the page has a paragraph");
        assert!(
            paragraph.contains("[truncated at"),
            "a paragraph longer than the limit says so: {paragraph}"
        );
        let title = preview["title"].as_str().expect("a title");
        assert!(
            title.chars().count() <= PREVIEW_LIMIT,
            "the title is bounded too, and this one is {} characters",
            title.chars().count()
        );
        assert!(
            json_len(preview) < PREVIEW_LIMIT + FRONTMATTER_LIMIT + 200,
            "a preview costs a bounded number of characters"
        );
    }

    /// A file whose own text fills the state budget drops its links' previews
    /// rather than its links: a link judged from its anchor is worth more than
    /// a link never judged, and the preview is the hint that goes first
    /// ([#37]).
    ///
    /// [#37]: https://github.com/mikekelly/s1m/issues/37
    #[tokio::test]
    async fn a_file_that_fills_the_state_budget_drops_its_previews_before_its_links() {
        let api = FakeApi::new(|_, _| (200, reply(Map::new())));
        let scorer = api.scorer();
        let query = "a query";
        let link = LinkState {
            anchor: "Page".to_string(),
            sentence: "Page.".to_string(),
            heading: None,
            target: "page.md".to_string(),
            target_preview: Some(PreviewState {
                title: "Page".to_string(),
                frontmatter: Some(vec![FrontmatterField {
                    key: "tags".to_string(),
                    value: "release, runbook".to_string(),
                }]),
                first_paragraph: Some("A page about the release runbook.".to_string()),
                headings: None,
                leads_to: None,
            }),
        };
        let plain = LinkState {
            target_preview: None,
            ..link.clone()
        };
        let empty = FileState {
            path: "hub.md".to_string(),
            title: "Hub".to_string(),
            content: String::new(),
        };
        // The file's own state, all but the room one plain link needs: every
        // quote is escaped to two characters in the state, so the content is
        // sized from the budgets rather than guessed at.
        let overhead = json_len(&empty) + json_len(&query);
        let room = STATE_CHARS - overhead - scorer.longest_question() - json_len(&plain);
        let content = "\"".repeat(room.div_ceil(2));

        let request = scorer.pack(
            query,
            FileState {
                content: content.clone(),
                ..empty.clone()
            },
            Vec::new(),
            vec![link.clone()],
        );

        assert_eq!(request.posts.len(), 1, "there is one link to ask about");
        assert!(
            request.posts[0].body.state.links[0]
                .target_preview
                .is_none(),
            "the file's own state leaves no room for a previewed link, so the preview goes"
        );
        assert_eq!(
            request.posts[0].body.state.links[0].target, link.target,
            "the link itself is still judged"
        );
        let state = json_len(&request.posts[0].body.state);
        assert!(
            state <= STATE_CHARS,
            "and what is left fits: {state} characters of state"
        );

        // A file with room for a preview keeps them, which is the case the cap
        // and the split are for.
        let request = scorer.pack(
            query,
            FileState {
                content: "ordinary page text".to_string(),
                ..empty
            },
            Vec::new(),
            vec![link],
        );
        assert!(
            request.posts[0].body.state.links[0]
                .target_preview
                .is_some(),
            "a page that is not over the budget keeps its previews"
        );
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
