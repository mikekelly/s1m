//! What traversal asks a model about a file, and what it gets back.
//!
//! Traversal is written against [`Scorer`], so tests drive it with a fake and
//! the real thing — one Jev request per file — lands behind the same trait in
//! [#5](https://github.com/mikekelly/s1m/issues/5). Nothing here talks to the
//! network: the trait is the seam.
//!
//! A judgment answers two questions, and traversal never lets one answer for
//! the other: how useful the file is ([`FileJudgment::relevance`]), and how
//! likely each outgoing link is to lead somewhere useful
//! ([`LinkJudgment::scent`]). An index page is usually irrelevant itself and
//! still has its links followed.

use std::path::PathBuf;

use crate::parse::ParsedFile;

/// One outgoing link, judged: how likely following it is to reach content the
/// query wants, from 0 to 1.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkJudgment {
    /// The target, resolved against the root the way [`ParsedFile::links`]
    /// resolve it.
    pub target: PathBuf,
    /// The link's scent: a probability, so 0.5 is "uncertain" rather than
    /// "moderately relevant".
    pub scent: f64,
}

/// One file, judged.
#[derive(Debug, Clone, PartialEq)]
pub struct FileJudgment {
    /// How useful the file itself is for the query, from 0 to 1.
    pub relevance: f64,
    /// One entry per outgoing link the scorer judged. A link left out is
    /// treated as unjudged, so it is reported with no scent and not followed.
    pub links: Vec<LinkJudgment>,
}

/// Scores one file against one query.
///
/// Synchronous on purpose: traversal scores a round of files from one thread
/// each, so a blocking client still scores them at the same time, and no
/// runtime is needed to use it. That is why implementations must be `Sync`.
pub trait Scorer: Sync {
    /// One request per file: the file's own relevance, and a scent per outgoing
    /// link. `query` is the same string for every file of a traversal.
    fn score(&self, query: &str, file: &ParsedFile) -> Result<FileJudgment, ScorerError>;
}

/// Why a scorer could not judge a file: no key, a failed request, a response
/// that cannot be read.
///
/// One file failing never ends a traversal — the file is reported as failed and
/// the walk carries on — so this is only ever reported per file.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ScorerError {
    message: String,
}

impl ScorerError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
