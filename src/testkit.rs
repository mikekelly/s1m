//! Helpers for the unit tests in more than one module.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A directory under the system temp directory, removed when it is dropped.
///
/// The name is the process and a counter, so tests running in parallel — in one
/// binary or in several — never share one, and the directory is emptied on the
/// way in as well as on the way out: a `cargo test` that died holding one of
/// these names must not hand its entries to the next run.
pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub(crate) fn new(label: &str) -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "s1m-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        TempDir { path }
    }

    /// The directory itself.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
