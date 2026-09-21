//! The judgment interface the rest of s1m is built on: one parsed file in, a
//! relevance score for the file, a score for each of its heading sections and a
//! scent for each of its outgoing links out.
//!
//! The trait is deliberately narrow, and deliberately async. Ranking a file is
//! a single model call, and traversal scores a whole frontier round at once, so
//! the caller needs futures it can join rather than threads it has to block.
//! `#[async_trait]` keeps the trait object-safe, so one scorer can be shared as
//! `Arc<dyn Scorer>` across a round.
//!
//! [`crate::jev::JevScorer`] is the real implementation; tests inject a fake.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::parse::ParsedFile;

/// What one link is worth: how likely following it is to reach something
/// useful for the query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkJudgment {
    /// The link's target, resolved against the root by
    /// [`crate::parse::parse`], and the same path as
    /// [`crate::parse::Link::target`].
    pub target: PathBuf,
    /// The model's answer, 0 to 1: near 1 means following the link is likely to
    /// reach useful content, near 0.5 means the model is unsure rather than
    /// that the link is middling, which is why callers threshold above 0.5.
    pub scent: f64,
}

/// What one heading section is worth: whether its lines are worth reading for
/// the query.
///
/// The range is [`crate::parse::Section::lines`] as the parser gave it — this
/// type carries a judgment, never a re-derived range — so a section here and
/// the section it judges always name the same lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SectionJudgment {
    /// Heading text, `None` for content before the first heading, as the parser
    /// spelled it.
    pub heading: Option<String>,
    /// `[first, last]` line, inclusive, 1-based, counted in the file as
    /// written.
    pub lines: [usize; 2],
    /// The model's answer, 0 to 1: near 1 means the section is worth reading,
    /// near 0.5 means the model is unsure rather than that the section is
    /// middling, which is why callers threshold above 0.5.
    pub score: f64,
}

/// What one file is worth: the file, its sections, and each of its links.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileJudgment {
    /// How useful the file is for the query, 0 to 1, where 1 means the file is
    /// central to it. Ordered, so it can rank a reading list directly.
    pub relevance: f64,
    /// One entry per [`ParsedFile::sections`], in the same order — the parser's
    /// document order, so a judgment pairs with the section it judges by index.
    /// A parent section's range contains its subsections'.
    pub sections: Vec<SectionJudgment>,
    /// One entry per [`ParsedFile::links`], in the same order.
    pub links: Vec<LinkJudgment>,
}

/// What stops a file from being judged.
#[derive(Debug, thiserror::Error)]
pub enum ScorerError {
    #[error("TYPESAFE_API_KEY is not set")]
    MissingApiKey,
    /// The HTTP client could not be built, which for a build against rustls
    /// means a broken TLS configuration rather than anything the caller did.
    #[error("could not build the HTTP client: {source}")]
    Client {
        #[source]
        source: reqwest::Error,
    },
    /// The file being scored was listed by a parse but could not be read back:
    /// it was deleted, or is not valid UTF-8.
    #[error("could not read {path} to send its content: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the request to {endpoint} failed: {source}")]
    Transport {
        endpoint: String,
        #[source]
        source: reqwest::Error,
    },
    /// The API answered, and the answer was not a success.
    #[error("{endpoint} returned {status}: {body}")]
    Status {
        endpoint: String,
        status: u16,
        body: String,
    },
    #[error("could not read the response from {endpoint}: {source}")]
    Decode {
        endpoint: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("the response has no answer for question {id}")]
    MissingAnswer { id: String },
    #[error("the answer for question {id} is {found}, expected {expected}")]
    WrongAnswerType {
        id: String,
        expected: &'static str,
        found: &'static str,
    },
    /// Nowhere to put cached answers: none of `S1M_CACHE_DIR`, `XDG_CACHE_HOME`
    /// and `HOME` is set. `--no-cache` runs without a cache at all.
    #[error(
        "no cache directory: set S1M_CACHE_DIR, XDG_CACHE_HOME or HOME, or run with --no-cache"
    )]
    NoCacheDir,
    /// The cache directory could not be created: a typo in `S1M_CACHE_DIR`, a
    /// path that is a file, a filesystem that will not take it.
    #[error("could not use the cache directory {path}: {source}")]
    Cache {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The request could not be turned into the bytes a cache keys on. Nothing
    /// this crate sends can fail to serialise — the request is strings and
    /// numbers — but [`crate::cache::Cacheable`] does not promise that of every
    /// implementation.
    #[error("could not serialise the request: {source}")]
    Encode {
        #[source]
        source: serde_json::Error,
    },
}

/// Grades one parsed file.
#[async_trait::async_trait]
pub trait Scorer: Send + Sync {
    /// Judges `file` as a source of material about `query`.
    ///
    /// Implementations must return one [`SectionJudgment`] per
    /// [`ParsedFile::sections`] entry and one [`LinkJudgment`] per
    /// [`ParsedFile::links`] entry, each in that order, so the caller can pair
    /// them by index. `file.links` that point outside the root are still
    /// judged: the caller decides whether to follow them.
    ///
    async fn score(&self, query: &str, file: &ParsedFile) -> Result<FileJudgment, ScorerError>;
}
