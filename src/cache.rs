//! Caching Jev's answers on disk, keyed by the request that produced them.
//!
//! Jev is stable but not bit-for-bit deterministic: the same request sent three
//! times returned the same top links and moved the numbers under them
//! (`docs/spike-notes.md`). Traversal that re-scored a page would therefore rank
//! one wiki differently on every run, and pay for it every time. Storing the
//! answers is what makes a repeat run identical, and free.
//!
//! [`CachedScorer`] wraps any [`Cacheable`] scorer behind the same [`Scorer`]
//! trait the rest of s1m already takes, so traversal does not know it is there.
//! The key is a digest of the request the scorer would send — the query, the
//! file's content, the mode's questions and the model — so the entry that
//! answers a question cannot drift from the question asked.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::parse::ParsedFile;
use crate::scorer::{FileJudgment, Scorer, ScorerError};

/// The variable that puts the cache somewhere else, so a caller — or a test —
/// does not have to write to `~/.cache`.
pub const DIR_VAR: &str = "S1M_CACHE_DIR";

/// The subdirectory entries go in, so a later kind of entry (section scores,
/// say) can share the cache directory without sharing file names.
const ENTRIES: &str = "judgments";

/// The shape of a stored entry. An entry written by another format is a miss
/// rather than a wrong answer, which is what makes it safe to change
/// [`FileJudgment`] later. 3 is the shape that also stores what the call cost
/// ([#11](https://github.com/mikekelly/s1m/issues/11)); 2 is the shape with
/// section scores in it ([#9](https://github.com/mikekelly/s1m/issues/9)); 1 had
/// the file and its links only, and an entry of that shape cannot answer for a
/// file's sections.
const FORMAT: u32 = 3;

// ------------------------------------------------------------------ caching

/// A scorer whose answers can be cached.
///
/// A cache can only key an answer if it can see the question, so the scorer
/// hands it the request it would send. Building the request is separate from
/// sending it, rather than one `score`-shaped method, so that the digest and
/// the call describe the same request: a miss hashes exactly what it sends.
#[async_trait::async_trait]
pub trait Cacheable: Scorer {
    /// One request, built and ready to send. Opaque to the cache, which only
    /// reads [`Cacheable::key`]'s bytes.
    type Request: Send;

    /// What a real call cost. [`Scorer::score`] drops this; the cache keeps it
    /// for callers that report on what they spent, as `s1m score-file` does —
    /// and, stored beside the answer, for a caller that reports on a run whose
    /// answers were bought earlier, as `src/bin/eval.rs` does.
    type Detail: Send + Sync + Serialize + DeserializeOwned;

    /// Builds the request for one file: the query, the file and its links.
    fn request(&self, query: &str, file: &ParsedFile) -> Result<Self::Request, ScorerError>;

    /// The bytes the answer depends on, and nothing else: the query, the
    /// content, the questions and the model. Equal bytes from two calls must
    /// mean equal judgments, which is the whole premise of caching them.
    fn key(&self, request: &Self::Request) -> Result<Vec<u8>, ScorerError>;

    /// Sends one built request for `file`: the real call, and what it cost.
    async fn call(
        &self,
        request: &Self::Request,
        file: &ParsedFile,
    ) -> Result<(FileJudgment, Self::Detail), ScorerError>;
}

/// One file's answer, and whether the API was called for it.
///
/// Both sides carry the accounting, so that a caller reporting on a run — what
/// a query cost, what it took — reads the same numbers whether the answers came
/// off the disk or were bought just now. What separates the two is whether a
/// call was made, which is what [`CachedScorer::calls`] counts and what
/// `s1m score-file` reports as a hit or a miss.
#[derive(Debug, Clone, PartialEq)]
pub enum Scored<D> {
    /// The answer was on disk: no call was made, and `detail` is what the call
    /// that stored it cost at the time.
    Reused { judgment: FileJudgment, detail: D },
    /// The answer came from a call made now, which cost `detail`.
    Called { judgment: FileJudgment, detail: D },
}

impl<D> Scored<D> {
    /// The judgment, whichever side it came from.
    pub fn judgment(&self) -> &FileJudgment {
        match self {
            Scored::Reused { judgment, .. } | Scored::Called { judgment, .. } => judgment,
        }
    }

    /// The accounting that came with the judgment: what this answer cost, when
    /// it was bought.
    pub fn detail(&self) -> &D {
        match self {
            Scored::Reused { detail, .. } | Scored::Called { detail, .. } => detail,
        }
    }

    /// Whether the API was called for this answer.
    pub fn called(&self) -> bool {
        matches!(self, Scored::Called { .. })
    }

    /// The judgment, leaving the accounting behind.
    pub fn into_judgment(self) -> FileJudgment {
        match self {
            Scored::Reused { judgment, .. } | Scored::Called { judgment, .. } => judgment,
        }
    }
}

/// One stored answer, in the shape it is written in.
///
/// `detail` is what the call that produced `judgment` cost. It is stored, not
/// derived, because it cannot be derived: it is what the API reported, and a
/// caller that reports on a run of stored answers would otherwise have nothing
/// to report.
#[derive(Debug, Serialize, Deserialize)]
struct Entry<D> {
    format: u32,
    judgment: FileJudgment,
    detail: D,
}

/// Wraps a scorer, keeping its answers in files under a cache directory.
///
/// Best-effort on both sides. An answer that cannot be stored is an answer
/// recomputed next time, and an entry that cannot be read — truncated by a full
/// disk, edited by hand, written by another version — is a miss: neither is
/// worth failing a run over, and neither can turn into a wrong answer.
pub struct CachedScorer<S: Cacheable> {
    inner: S,
    /// The directory the entries live in, under the cache directory the caller
    /// named.
    dir: PathBuf,
    /// Calls that reached the scorer. Separate from the files visited: this is
    /// the number a run's cost depends on.
    calls: AtomicU64,
    /// Answers served from disk.
    hits: AtomicU64,
}

impl<S: Cacheable> CachedScorer<S> {
    /// Caches under [`cache_dir`]: `S1M_CACHE_DIR` if it is set and not blank,
    /// else `$XDG_CACHE_HOME/s1m`, else `~/.cache/s1m`.
    pub fn from_env(inner: S) -> Result<Self, ScorerError> {
        Self::new(
            inner,
            cache_dir(
                std::env::var_os(DIR_VAR),
                std::env::var_os("XDG_CACHE_HOME"),
                std::env::var_os("HOME"),
            )?,
        )
    }

    /// Caches under `dir`, which is created if it is not there.
    ///
    /// A cache directory that cannot be created is an error rather than a
    /// silent nothing: a mistyped `S1M_CACHE_DIR` should fail at startup, not
    /// cost an API call on every request of every run.
    pub fn new(inner: S, dir: impl Into<PathBuf>) -> Result<Self, ScorerError> {
        let dir = dir.into().join(ENTRIES);
        fs::create_dir_all(&dir).map_err(|source| ScorerError::Cache {
            path: dir.clone(),
            source,
        })?;
        Ok(CachedScorer {
            inner,
            dir,
            calls: AtomicU64::new(0),
            hits: AtomicU64::new(0),
        })
    }

    /// The directory the entries go in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// How many calls reached the scorer: one per miss, so the real API calls a
    /// run made.
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }

    /// How many answers came off the disk instead.
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Judges one file, reusing the answer to a request that has been made
    /// before. [`Scorer::score`] with where the answer came from, and what it
    /// cost — now, or when it was bought.
    pub async fn judge(
        &self,
        query: &str,
        file: &ParsedFile,
    ) -> Result<Scored<S::Detail>, ScorerError> {
        let request = self.inner.request(query, file)?;
        let key = self.key(&request)?;

        if let Some((judgment, detail)) = self.load(&key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Scored::Reused { judgment, detail });
        }

        let (judgment, detail) = self.inner.call(&request, file).await?;
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.store(&key, &judgment, &detail);
        Ok(Scored::Called { judgment, detail })
    }

    /// The name of the entry that answers `request`.
    fn key(&self, request: &S::Request) -> Result<String, ScorerError> {
        Ok(format!("{:x}", Sha256::digest(self.inner.key(request)?)))
    }

    /// The stored answer for `key`, or `None` for anything this version cannot
    /// read: no entry, an unreadable one, a truncated one, one from another
    /// format.
    fn load(&self, key: &str) -> Option<(FileJudgment, S::Detail)> {
        let bytes = fs::read(self.entry(key)).ok()?;
        let entry: Entry<S::Detail> = serde_json::from_slice(&bytes).ok()?;
        (entry.format == FORMAT).then_some((entry.judgment, entry.detail))
    }

    /// Stores one answer and what it cost, and says nothing when it cannot: two
    /// tasks racing the same miss both write, one wins, and a half-written entry
    /// is recomputed rather than read.
    fn store(&self, key: &str, judgment: &FileJudgment, detail: &S::Detail) {
        let entry = Entry {
            format: FORMAT,
            judgment: judgment.clone(),
            detail,
        };
        if let Ok(bytes) = serde_json::to_vec(&entry) {
            let _ = fs::write(self.entry(key), bytes);
        }
    }

    /// Where one answer is stored.
    fn entry(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }
}

#[async_trait::async_trait]
impl<S: Cacheable> Scorer for CachedScorer<S> {
    async fn score(&self, query: &str, file: &ParsedFile) -> Result<FileJudgment, ScorerError> {
        Ok(self.judge(query, file).await?.into_judgment())
    }
}

/// Where entries go: `S1M_CACHE_DIR` if it is set, else the XDG cache
/// directory's `s1m`, else `~/.cache/s1m`.
///
/// The three values come in as arguments rather than being read here, so the
/// order between them is testable in one process without writing to the
/// environment. A variable that is set but blank counts as unset: `S1M_CACHE_DIR=`
/// in a shell profile must not mean the working directory.
fn cache_dir(
    cache: Option<OsString>,
    xdg: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, ScorerError> {
    let set =
        |value: Option<OsString>| value.filter(|var| !var.to_string_lossy().trim().is_empty());

    if let Some(dir) = set(cache) {
        return Ok(PathBuf::from(dir));
    }
    if let Some(base) = set(xdg) {
        return Ok(PathBuf::from(base).join("s1m"));
    }
    if let Some(home) = set(home) {
        return Ok(PathBuf::from(home).join(".cache").join("s1m"));
    }
    Err(ScorerError::NoCacheDir)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::*;
    use crate::parse;
    use crate::scorer::{FileJudgment, LinkJudgment, SectionJudgment};
    use crate::testkit::TempDir;

    // ------------------------------------------------------------- the fake

    /// A scorer that counts the calls made to it and answers with a number the
    /// caller can predict, so a stale entry shows up as a wrong answer rather
    /// than a plausible one. `config` stands in for everything a real scorer's
    /// answers depend on that is not the query or the file — the mode, the
    /// model — and the fake keys on it exactly as the real one does.
    struct Fake {
        config: &'static str,
        calls: Arc<AtomicU64>,
    }

    impl Fake {
        /// The scorer, and the handle that counts what it was called with.
        fn new(config: &'static str) -> (Fake, Arc<AtomicU64>) {
            let calls = Arc::new(AtomicU64::new(0));
            (
                Fake {
                    config,
                    calls: Arc::clone(&calls),
                },
                calls,
            )
        }
    }

    /// The answer to a request: its own length, so two requests that differ by
    /// a character differ by an answer.
    fn answer(request: &str) -> FileJudgment {
        FileJudgment {
            relevance: request.chars().count() as f64,
            sections: vec![SectionJudgment {
                heading: Some("Heading".to_string()),
                lines: [1, 3],
                score: 0.5,
            }],
            links: vec![LinkJudgment {
                target: PathBuf::from("linked.md"),
                scent: 0.5,
            }],
        }
    }

    #[async_trait]
    impl Scorer for Fake {
        async fn score(&self, query: &str, file: &ParsedFile) -> Result<FileJudgment, ScorerError> {
            let request = self.request(query, file)?;
            Ok(self.call(&request, file).await?.0)
        }
    }

    #[async_trait]
    impl Cacheable for Fake {
        type Request = String;
        /// What a call cost, in the accounting the tests can check is carried
        /// through the cache: a real scorer reports what the API charged, and
        /// the fake reports the length of the request it answered, which is
        /// predictable from the same bytes the key is made of.
        type Detail = u64;

        fn request(&self, query: &str, file: &ParsedFile) -> Result<String, ScorerError> {
            let content = fs::read_to_string(&file.path).map_err(|source| ScorerError::Read {
                path: file.path.clone(),
                source,
            })?;
            Ok(format!("{}\n{query}\n{content}", self.config))
        }

        fn key(&self, request: &String) -> Result<Vec<u8>, ScorerError> {
            Ok(request.as_bytes().to_vec())
        }

        async fn call(
            &self,
            request: &String,
            _file: &ParsedFile,
        ) -> Result<(FileJudgment, u64), ScorerError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok((answer(request), request.len() as u64))
        }
    }

    /// Writes `content` to the same page in `dir` — so a rewrite changes the
    /// content and nothing else — and parses it against `dir`.
    fn page(dir: &TempDir, content: &str) -> ParsedFile {
        let path = dir.path().join("page.md");
        fs::write(&path, content).expect("a page");
        parse::parse(&path, dir.path()).expect("a parse")
    }

    // ------------------------------------------------------------ the tests

    /// The acceptance criterion: a second identical run makes no call.
    ///
    /// And the accounting comes with it: what the answer cost is stored beside
    /// it, so a run that made no call still knows what was paid. That is what
    /// lets a committed cache reproduce a report's cost column.
    #[tokio::test]
    async fn a_second_identical_run_does_not_call_the_scorer_again() {
        let dir = TempDir::new("cache-repeat");
        let file = page(&dir, "# Home\n");
        let (fake, calls) = Fake::new("useful-for");
        let cached = CachedScorer::new(fake, dir.path()).expect("a cache");

        let first = cached.judge("query", &file).await.expect("an answer");
        let second = cached.judge("query", &file).await.expect("an answer");

        assert_eq!(
            first.judgment(),
            second.judgment(),
            "the stored answer is the answer"
        );
        assert!(
            matches!(second, Scored::Reused { .. }),
            "the second run made no call"
        );
        assert!(matches!(first, Scored::Called { .. }), "the first run did");
        assert!(!second.called(), "and says so");
        assert_eq!(
            first.detail(),
            second.detail(),
            "a stored answer keeps what the call cost"
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1, "two runs, one call");
        assert_eq!(cached.calls(), 1, "and the counter reports it");
        assert_eq!(cached.hits(), 1);
        assert_eq!(cached.dir(), dir.path().join("judgments"));
    }

    /// The acceptance criterion: the query, the content and the mode are each
    /// part of the question, so changing one is a miss.
    #[tokio::test]
    async fn a_changed_request_is_a_miss() {
        let dir = TempDir::new("cache-changed");
        let file = page(&dir, "# Home\n");
        let (fake, calls) = Fake::new("useful-for");
        let cached = CachedScorer::new(fake, dir.path()).expect("a cache");

        cached.judge("query", &file).await.expect("an answer");
        cached
            .judge("another query", &file)
            .await
            .expect("an answer");
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "the query is part of the request"
        );

        let edited = page(&dir, "# Home\n\nMore.\n");
        cached
            .judge("another query", &edited)
            .await
            .expect("an answer");
        assert_eq!(
            calls.load(Ordering::Relaxed),
            3,
            "the content is part of the request, at the same path"
        );

        // A scorer asked a different question — another mode, another model —
        // about the same page and query is a different request too, in front of
        // the same cache directory.
        let (other, other_calls) = Fake::new("about");
        let other = CachedScorer::new(other, dir.path()).expect("a cache");
        let scored = other
            .judge("another query", &edited)
            .await
            .expect("an answer");
        assert!(
            matches!(scored, Scored::Called { .. }),
            "a different mode is a different request"
        );
        assert_eq!(other_calls.load(Ordering::Relaxed), 1);
    }

    /// The acceptance criterion: an entry that cannot be read is a miss, not a
    /// failure, and not a wrong answer.
    #[tokio::test]
    async fn an_entry_this_version_cannot_read_is_recomputed() {
        let dir = TempDir::new("cache-corrupt");
        let file = page(&dir, "# Home\n");
        let (fake, calls) = Fake::new("useful-for");
        let cached = CachedScorer::new(fake, dir.path()).expect("a cache");

        let request = cached.inner.request("query", &file).expect("a request");
        let entry = cached.entry(&cached.key(&request).expect("a key"));
        let stale = serde_json::to_string(&Entry {
            format: FORMAT + 1,
            judgment: answer("stale"),
            detail: 0u64,
        })
        .expect("an entry");

        // A half-written entry, an empty file, and one from another format.
        for unreadable in ["{ half a", "", stale.as_str()] {
            fs::write(&entry, unreadable).expect("a written entry");

            let scored = cached.judge("query", &file).await.expect("a judgment");
            match scored {
                Scored::Called { judgment, .. } => {
                    assert_eq!(
                        judgment,
                        answer("useful-for\nquery\n# Home\n"),
                        "the answer is the call's, not the entry's"
                    );
                }
                other => panic!("{unreadable:?} should have been a miss, not {other:?}"),
            }
        }
        assert_eq!(calls.load(Ordering::Relaxed), 3);

        // The fresh answer replaced the unreadable ones, so the next run is
        // served from disk again.
        assert!(matches!(
            cached.judge("query", &file).await.expect("an answer"),
            Scored::Reused { .. }
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    /// One cache serves a whole run: #6 joins a frontier round of these
    /// concurrently, so the cached scorer has to be shareable as one
    /// `Arc<dyn Scorer>`.
    #[test]
    fn the_cached_scorer_is_shareable() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CachedScorer<Fake>>();
    }

    /// A cache directory that cannot be created is a startup error, not a
    /// silent eternity of paid calls.
    #[test]
    fn a_cache_directory_that_cannot_be_created_is_an_error() {
        let dir = TempDir::new("cache-unusable");
        let not_a_directory = dir.path().join("a-file");
        fs::write(&not_a_directory, "not a directory").expect("a file");

        let (fake, _) = Fake::new("useful-for");
        let error = match CachedScorer::new(fake, not_a_directory.clone()) {
            Ok(_) => panic!("a file is not a cache directory"),
            Err(error) => error,
        };
        assert!(
            matches!(error, ScorerError::Cache { ref path, .. } if path.starts_with(&not_a_directory)),
            "{error}"
        );
    }

    /// The variables, in the order the issue names them, and a blank variable
    /// counting as no variable at all.
    #[test]
    fn the_cache_directory_follows_the_variables_in_order() {
        let path = |value: &str| Some(OsString::from(value));
        let override_dir = path("/tmp/override");
        let xdg = path("/xdg");
        let home = path("/home/u");

        assert_eq!(
            cache_dir(override_dir.clone(), xdg.clone(), home.clone()).expect("a directory"),
            PathBuf::from("/tmp/override"),
            "S1M_CACHE_DIR is the override"
        );
        assert_eq!(
            cache_dir(None, xdg.clone(), home.clone()).expect("a directory"),
            PathBuf::from("/xdg/s1m"),
            "then the XDG cache home"
        );
        assert_eq!(
            cache_dir(None, None, home.clone()).expect("a directory"),
            PathBuf::from("/home/u/.cache/s1m"),
            "then the XDG fallback under HOME"
        );
        assert!(matches!(
            cache_dir(None, None, None),
            Err(ScorerError::NoCacheDir)
        ));

        assert_eq!(
            cache_dir(path(""), path(" "), home).expect("a directory"),
            PathBuf::from("/home/u/.cache/s1m"),
            "a variable set to nothing is no override"
        );
        assert!(cache_dir(path("\t"), None, None).is_err());
    }
}
