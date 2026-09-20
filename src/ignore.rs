//! `.s1mignore`: the paths a run never reads.
//!
//! A wiki that holds anything private needs a way to say so, because the query
//! path sends what it visits to a third party. A root may hold a `.s1mignore`
//! beside the pages it covers; its lines are gitignore patterns, matched the way
//! git matches them — the matcher is the `ignore` crate's, the one ripgrep uses,
//! and [`Ignore::matched`] walks the levels of a path from the root down, so the
//! last pattern to match a level decides at that level and a `!` line cannot
//! re-include something inside a directory an earlier line excluded. The file is
//! read from the root the walk is bounded by, which is the directory every path
//! s1m sees is spelled against. A `.s1mignore` in a subdirectory is not read:
//! the root's is the one that counts, so what a wiki excludes is stated in one
//! place.
//!
//! What a matched path is not, on one run:
//!
//! - **not read**: an entry file the caller names that matches is exit 2 rather
//!   than a silent read ([`crate::cli::Error::Ignored`]), and a matched link
//!   target is taken out of the file before it is scored.
//! - **not previewed**: the preview a request carries is read from the target
//!   ([`crate::parse::preview`]), and a target this module drops never gets
//!   there.
//! - **not sent**: the link is gone before the file is judged, so the request
//!   carries no question about it and no target of its own — not the path and
//!   not a byte of the page's text.
//! - **not followed**: with the link gone there is nothing to queue, whatever
//!   scent the model would have given it.
//!
//! What is sent, and stays sent, is the prose of the pages the walk does read:
//! a page that links to a matched one still says so in its own words, and the
//! words are the page's. s1m removes the question about a matched target and
//! the target's own bytes; it does not edit another page's text to hide a link
//! its author wrote.
//!
//! One thing more is left: a name. `parse`'s page listing reads directory
//! entries to resolve a wikilink that names a file rather than a path, so the
//! *name* of a matched file can be read off a directory. No content is, and a
//! wikilink that resolves to one is dropped with the rest.
//!
//! A `.s1mignore` s1m cannot read, or that holds a pattern that does not parse,
//! is an error and the run stops. Dropping a rule quietly would send exactly the
//! files the rule was written for, which is the one failure this feature exists
//! to prevent.

use std::fs;
use std::path::{Path, PathBuf};

use ::ignore::gitignore::{Gitignore, GitignoreBuilder};

/// The name of the file, in the root the walk is bounded by.
pub const FILE: &str = ".s1mignore";

/// The patterns one root's `.s1mignore` holds.
///
/// [`Ignore::at`] reads them once per run, and the same value is what the CLI
/// asks about its entry files and the walk about what it may parse and link to,
/// so a run cannot see two versions of the rules.
#[derive(Debug)]
pub struct Ignore {
    /// The root the patterns were read against: a path given relative to it is
    /// joined on before matching, because the matcher works in paths that carry
    /// the root.
    root: PathBuf,
    /// `None` when the root holds no `.s1mignore`, which matches nothing.
    matcher: Option<Gitignore>,
}

impl Ignore {
    /// The patterns in `root`'s `.s1mignore`, or none when the root has no such
    /// file.
    ///
    /// A file that cannot be read, that holds a pattern that does not parse, or
    /// that is there but is not a readable file at all — a directory, a symlink
    /// whose target has gone — is [`IgnoreError`] and not an empty set of
    /// patterns: see the module docs.
    pub fn at(root: impl AsRef<Path>) -> Result<Ignore, IgnoreError> {
        let root = root.as_ref();
        let source = root.join(FILE);
        match fs::symlink_metadata(&source) {
            // No such name at all is the one case that means no rules. The name
            // being taken by something that is not a readable file is the
            // caller's mistake: a `.s1mignore` whose rules s1m cannot read must
            // not quietly widen what a run may send.
            Err(_) => return Ok(Ignore::none()),
            Ok(_) if !source.is_file() => {
                return Err(IgnoreError {
                    message: format!("{}: not a readable file", source.display()),
                });
            }
            Ok(_) => {}
        }
        let mut builder = GitignoreBuilder::new(root);
        // The builder's own message names the file, and a pattern's line, so it
        // is carried rather than restated.
        if let Some(error) = builder.add(&source) {
            return Err(IgnoreError {
                message: error.to_string(),
            });
        }
        let matcher = builder.build().map_err(|error| IgnoreError {
            message: error.to_string(),
        })?;
        Ok(Ignore {
            root: root.to_path_buf(),
            matcher: Some(matcher),
        })
    }

    /// No patterns: what a root without a `.s1mignore` holds, and what a caller
    /// that wants none — a test of the walk, or a run it knows is public —
    /// passes.
    pub fn none() -> Ignore {
        Ignore {
            root: PathBuf::new(),
            matcher: None,
        }
    }

    /// Whether `path`, spelled relative to the root the way the walk spells one,
    /// matches a pattern or lies under a directory that does.
    ///
    /// Matching is git's, component by component from the root down: the path is
    /// looked at a level at a time and the first level a pattern matches decides,
    /// which is why a pattern like `private/` covers `private/notes.md` without
    /// naming it, and why a `!` line re-includes what a broader pattern took at
    /// the level the line is written for and cannot reach inside a directory an
    /// earlier line excluded — git does not look in one either.
    ///
    /// A path the root does not hold is not one this matcher can answer for, and
    /// it says no rather than guessing: that is a path spelled against another
    /// base, or one outside the tree, and the mistake is the caller's to report —
    /// `parse` rejects a path and a root given on different bases a line after
    /// an ignore check.
    pub fn matched(&self, path: &Path) -> bool {
        let Some(matcher) = &self.matcher else {
            return false;
        };
        let joined = self.root.join(path);
        if !joined.starts_with(&self.root) {
            return false;
        }
        let mut candidate = self.root.clone();
        let mut components = path.components().peekable();
        while let Some(component) = components.next() {
            candidate.push(component);
            let is_dir = components.peek().is_some();
            if matcher.matched(&candidate, is_dir).is_ignore() {
                return true;
            }
        }
        false
    }
}

/// Why a `.s1mignore` could not be used.
///
/// One message, and it names the file — and, for a pattern, the line it is on —
/// because it is the matcher's own.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct IgnoreError {
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempDir;

    /// A root holding `lines` as its `.s1mignore`.
    fn ignoring(lines: &str) -> (TempDir, Ignore) {
        let dir = TempDir::new("ignore");
        std::fs::write(dir.path().join(FILE), lines).expect("the fixture should be writable");
        let ignore = Ignore::at(dir.path()).expect("the fixture should parse");
        (dir, ignore)
    }

    #[test]
    fn a_root_without_the_file_matches_nothing() {
        let dir = TempDir::new("ignore-none");
        let ignore = Ignore::at(dir.path()).expect("a root without one is not an error");

        assert!(!ignore.matched(Path::new("private/notes.md")));
        assert!(!ignore.matched(Path::new("notes.md")));
    }

    /// The pattern the feature exists for: a directory, named with the trailing
    /// slash gitignore gives it, covers everything under it — at any depth, the
    /// way git reads a pattern with no slash in it.
    #[test]
    fn a_matched_directory_covers_what_is_under_it() {
        let (_dir, ignore) = ignoring("private/\n");

        assert!(ignore.matched(Path::new("private/notes.md")));
        assert!(ignore.matched(Path::new("private/notes/deep.md")));
        assert!(ignore.matched(Path::new("notes/private/ledger.md")));
        assert!(!ignore.matched(Path::new("notes.md")));
        assert!(!ignore.matched(Path::new("notes/ledger.md")));
    }

    /// Gitignore's other half: `!` keeps a path a broader pattern would have
    /// taken, and the last line that matches decides.
    #[test]
    fn a_negated_pattern_keeps_a_path() {
        let (_dir, ignore) = ignoring("*.md\n!keep.md\n");

        assert!(ignore.matched(Path::new("draft.md")));
        assert!(!ignore.matched(Path::new("keep.md")));
        assert!(ignore.matched(Path::new("notes/draft.md")));
    }

    /// A leading slash anchors a pattern to the root, which is what tells
    /// `drafts.md` apart from the same file name further down the tree.
    #[test]
    fn a_leading_slash_anchors_a_pattern_to_the_root() {
        let (_dir, ignore) = ignoring("/drafts.md\n");

        assert!(ignore.matched(Path::new("drafts.md")));
        assert!(!ignore.matched(Path::new("notes/drafts.md")));
    }

    /// Comments and blank lines are gitignore's, not patterns: a wiki's
    /// `.s1mignore` can say why it excludes something.
    #[test]
    fn comments_and_blank_lines_are_not_patterns() {
        let (_dir, ignore) = ignoring("# nothing here leaves the machine\n\nprivate/\n");

        assert!(ignore.matched(Path::new("private/notes.md")));
        assert!(!ignore.matched(Path::new("nothing here leaves the machine")));
    }

    /// Git's semantics, not the flat matcher's: the last pattern that matches a
    /// level decides at that level, and a `!` line cannot reach inside a
    /// directory an earlier line excluded — git does not look in one either. So
    /// `private/readme.md` stays ignored however loudly a later line asks for it,
    /// and a whitelist with nothing excluding its parent does work.
    #[test]
    fn negation_cannot_reach_inside_an_excluded_directory() {
        let (_dir, ignore) = ignoring("private/\n!private/readme.md\n");
        assert!(
            ignore.matched(Path::new("private/readme.md")),
            "an excluded directory is not descended into, so nothing in it is re-included"
        );

        let (_dir, ignore) = ignoring("!private/readme.md\n");
        assert!(!ignore.matched(Path::new("private/readme.md")));
    }

    /// The root's name being taken by something that is not a readable file is
    /// not the same as the root having no `.s1mignore`: every rule would be
    /// dropped, and the pages they were written for would be read and sent.
    #[test]
    fn a_s1mignore_that_is_not_a_file_is_an_error() {
        let dir = TempDir::new("ignore-directory");
        std::fs::create_dir(dir.path().join(FILE)).expect("a writable fixture");

        let error = Ignore::at(dir.path()).expect_err("a directory is not a set of rules");

        assert!(error.to_string().contains(FILE), "{error}");
        assert!(error.to_string().contains("not a readable file"), "{error}");
    }

    /// A symlink whose target has gone is the same mistake as a directory: the
    /// rules were meant to apply and cannot be read, so the run stops.
    #[cfg(unix)]
    #[test]
    fn a_s1mignore_that_is_a_broken_symlink_is_an_error() {
        let dir = TempDir::new("ignore-symlink");
        std::os::unix::fs::symlink(dir.path().join("gone"), dir.path().join(FILE))
            .expect("a writable fixture");

        let error = Ignore::at(dir.path()).expect_err("a broken symlink is not a set of rules");
        assert!(error.to_string().contains(FILE), "{error}");
    }

    /// A path the root does not hold is not matched, and — the matcher asserting
    /// rather than guessing — does not bring the process down either: a path
    /// spelled against another base or living outside the tree is the caller's
    /// mistake, and the caller reports it. A path spelled relative to the root
    /// still answers correctly whatever base the root itself is on.
    #[test]
    fn a_path_the_root_does_not_hold_matches_nothing() {
        let (_dir, ignore) = ignoring("private/\n");

        assert!(!ignore.matched(Path::new("/private/notes.md")));
        assert!(ignore.matched(Path::new("private/notes.md")));
    }

    /// A glob that does not parse is the caller's mistake, and the message says
    /// which line — a rule that was dropped quietly would send the file it was
    /// written for.
    #[test]
    fn a_pattern_that_does_not_parse_is_an_error_naming_its_line() {
        let dir = TempDir::new("ignore-broken");
        std::fs::write(dir.path().join(FILE), "private/\na{b\n").expect("a writable fixture");

        let error = Ignore::at(dir.path()).expect_err("a broken pattern is not an empty set");

        assert!(error.to_string().contains("line 2"), "{error}");
        assert!(error.to_string().contains(FILE), "{error}");
    }
}
