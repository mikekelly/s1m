//! What wiki a run was measured against.
//!
//! A pass resumes across run directories and shares what it has bought with
//! every other pass, so what a run was measured against has to be part of what
//! identifies it: two rows with the same revision are about the same cut of the
//! wiki, and a pass resumed against a wiki that has moved is a different
//! measurement rather than more of the same one.
//!
//! The revision is a hash of the pages the walk reads and not a git commit,
//! because a commit does not move when a working tree does: the same clone at
//! the same commit with one page edited is two wikis, and the numbers from the
//! two cannot be averaged.

use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

/// The revision of a wiki: `sha256:` and the hash of every page under it, in
/// the order the walk reads them — the same pages `graph-stats` measures.
pub fn revision(wiki: &Path) -> Result<String, String> {
    let pages = s1m::parse::pages(wiki);
    if pages.is_empty() {
        return Err(format!(
            "{}: no markdown or text pages under it, so it has no revision",
            wiki.display()
        ));
    }
    let mut digest = Sha256::new();
    for page in &pages {
        let path = wiki.join(page);
        let text =
            fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        // The name as well as the text: a page renamed is a different wiki, and
        // a hash of the contents alone would not say so. The zero byte is the
        // separator, so no name and text pair runs into the next one.
        digest.update(page.as_os_str().as_encoded_bytes());
        digest.update([0]);
        digest.update(text.as_bytes());
        digest.update([0]);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempDir;

    /// A revision is what the content is: the same pages hash the same, and a
    /// page edited is a different wiki.
    #[test]
    fn a_revision_follows_the_content() {
        let wiki = TempDir::new("revision");
        wiki.write("index.md", "# Index\n[one](one.md)\n");
        wiki.write("one.md", "# One\n");
        let before = revision(wiki.path()).expect("a revision");
        assert!(before.starts_with("sha256:"), "{before}");

        // Reading it again is the same wiki, and so is asking twice.
        assert_eq!(revision(wiki.path()).expect("again"), before);

        // A page edited, a page added and a page renamed are all another cut.
        wiki.write("one.md", "# One\n\nmore\n");
        let edited = revision(wiki.path()).expect("a revision");
        assert_ne!(edited, before);
        wiki.write("two.md", "# Two\n");
        assert_ne!(revision(wiki.path()).expect("a revision"), edited);
        fs::remove_file(wiki.path().join("two.md")).expect("the page back");
        fs::rename(wiki.path().join("one.md"), wiki.path().join("three.md")).expect("a rename");
        assert_ne!(revision(wiki.path()).expect("a revision"), edited);

        // A directory with no pages is not a wiki, and has no revision to
        // record: a row measured against nothing would key the same as any
        // other empty directory.
        let empty = TempDir::new("revision-empty");
        assert!(revision(empty.path()).is_err());
    }

    /// The pages the walk reads are the pages hashed, and nothing else: a file
    /// the walk never opens cannot change what a row was measured against.
    #[test]
    fn only_the_pages_the_walk_reads_are_hashed() {
        let wiki = TempDir::new("revision-pages");
        wiki.write("index.md", "# Index\n");
        let before = revision(wiki.path()).expect("a revision");

        // A dot directory and a file that is not a page are not the wiki.
        wiki.write(".private/notes.md", "# Not a page the walk reads\n");
        wiki.write("image.png", "not markdown at all");
        assert_eq!(revision(wiki.path()).expect("a revision"), before);
    }
}
