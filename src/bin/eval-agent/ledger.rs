//! What has been bought, across every run directory.
//!
//! Resume keyed on a run directory buys the same run twice: the same query
//! under the same condition against the same wiki is one measurement, and a
//! second directory is not a second wiki. This is the one record of what has
//! been bought, keyed on what a run is — the wiki revision and the
//! `(query, condition, repeat)` triple — and shared by every directory on the
//! machine.
//!
//! A run is recorded twice: once when it is bought, before it is paid for, and
//! once when it has been measured. The last line for a run wins on the way back
//! in, so a pass killed between the two has still said what it bought, the run
//! is owed again by whoever comes next, and a purchase with no row is visible
//! as money spent with nothing to show for it.
//!
//! A run bought under one model is not a run bought under another. The same
//! query at a cheaper tier is another measurement, which is the point of
//! pinning the tier: the ledger matches on the model asked for as well as on
//! the key, so a screening pass is not skipped because someone else measured
//! the query at an expensive one.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::row::Key;

/// What identifies an entry: the run, and the model it was bought under.
pub type Id = (Key, Option<String>);

/// One run, as the ledger records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The wiki revision it was measured against and the triple.
    #[serde(flatten)]
    pub key: Key,
    /// The model named on the command line, which every agent in the run
    /// inherits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The directory the run was bought for, canonical: what a report says was
    /// bought is what was bought for the rows it holds.
    pub out: PathBuf,
    /// `None` while the run is in flight: bought, and its row not written yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// What it cost, once it is known. A run that failed was not priced at all,
    /// and a zero from it would read as a run that came free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl Entry {
    /// A run that is about to be bought.
    pub fn buying(key: Key, model: Option<&str>, out: &Path) -> Entry {
        Entry {
            key,
            model: model.map(str::to_string),
            out: out.to_path_buf(),
            ok: None,
            cost_usd: None,
        }
    }

    /// The same run, once it has been measured: a failed run is a row too, and
    /// carrying no metrics it has no price.
    pub fn measured(&self, ok: bool, cost_usd: Option<f64>) -> Entry {
        Entry {
            ok: Some(ok),
            cost_usd,
            ..self.clone()
        }
    }

    pub fn id(&self) -> Id {
        (self.key.clone(), self.model.clone())
    }
}

/// The file the ledger is kept in.
#[derive(Debug, Clone)]
pub struct Ledger {
    path: PathBuf,
}

/// The file name the ledger is kept under.
pub const LEDGER: &str = "ledger.jsonl";

/// Where the ledger lives when `--ledger` names none: one file for the machine,
/// so a second `--out` cannot buy what the first one already has.
pub fn default_path(out: &Path) -> PathBuf {
    let env = |name: &str| std::env::var(name).ok();
    path_from(&env).unwrap_or_else(|| out.join(LEDGER))
}

/// The ledger the environment names: `$S1M_LEDGER` outright, else the file
/// under `$XDG_DATA_HOME` or `~/.local/share`. `None` is a machine that keeps no
/// home directory, which has nowhere to share one ledger between directories
/// and gets one under `--out` instead — which is where the rows are anyway.
///
/// That the default is outside every run directory, and outside this
/// repository, is deliberate: its lines carry query ids and what they cost.
fn path_from(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(path) = env("S1M_LEDGER") {
        return Some(PathBuf::from(path));
    }
    let base = env("XDG_DATA_HOME").or_else(|| {
        env("HOME").map(|home| Path::new(&home).join(".local/share").display().to_string())
    })?;
    Some(Path::new(&base).join("eval-agent").join(LEDGER))
}

impl Ledger {
    /// The ledger at a path. Nothing is read until [`Ledger::read`].
    pub fn at(path: PathBuf) -> Ledger {
        Ledger { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every run the ledger holds, the last line for a run winning.
    ///
    /// A ledger that is not there is an empty one: the first pass on a machine
    /// has nothing to resume from.
    pub fn read(&self) -> Result<BTreeMap<Id, Entry>, String> {
        let Ok(text) = fs::read_to_string(&self.path) else {
            return Ok(BTreeMap::new());
        };
        let mut entries: BTreeMap<Id, Entry> = BTreeMap::new();
        for (at, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let entry: Entry = serde_json::from_str(line)
                .map_err(|error| format!("{}:{}: {error}", self.path.display(), at + 1))?;
            entries.insert(entry.id(), entry);
        }
        Ok(entries)
    }

    /// Adds a line: one entry per write of a run's life, and the last one wins.
    pub fn record(&self, entry: &Entry) -> Result<(), String> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let line = serde_json::to_string(entry).map_err(|error| error.to_string())?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| format!("{}: {error}", self.path.display()))?;
        writeln!(file, "{line}").map_err(|error| format!("{}: {error}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TempDir;

    fn key(query: &str, condition: &str, repeat: usize) -> Key {
        Key {
            wiki: "sha256:abc123".to_string(),
            query: query.to_string(),
            condition: condition.to_string(),
            repeat,
        }
    }

    /// A run is recorded when it is bought and again when it is measured: the
    /// ledger reads back what the last line for each run said.
    #[test]
    fn a_run_is_a_purchase_and_then_a_measurement() {
        let dir = TempDir::new("ledger");
        let ledger = Ledger::at(dir.path().join("ledger.jsonl"));
        let buying = Entry::buying(key("one", "explore", 0), Some("sonnet"), dir.path());

        assert!(ledger.read().expect("an empty ledger").is_empty());
        ledger.record(&buying).expect("a purchase");
        let entries = ledger.read().expect("one entry");
        let entry = &entries[&buying.id()];
        // Bought, and not yet measured: no outcome and no price.
        assert_eq!((entry.ok, entry.cost_usd), (None, None));

        ledger
            .record(&buying.measured(true, Some(0.27)))
            .expect("the measurement");
        let entries = ledger.read().expect("one entry");
        assert_eq!(entries.len(), 1, "one run is one entry: {entries:?}");
        let entry = &entries[&buying.id()];
        assert_eq!((entry.ok, entry.cost_usd), (Some(true), Some(0.27)));
        assert_eq!(entry.model.as_deref(), Some("sonnet"));
        assert_eq!(entry.key, key("one", "explore", 0));
    }

    /// The model a run was bought under is part of it: the same query at
    /// another tier is another measurement, and both are held.
    #[test]
    fn the_same_run_under_another_model_is_another_entry() {
        let dir = TempDir::new("ledger-models");
        let ledger = Ledger::at(dir.path().join("ledger.jsonl"));
        for model in [Some("sonnet"), Some("haiku"), None] {
            ledger
                .record(&Entry::buying(key("one", "explore", 0), model, dir.path()))
                .expect("a purchase");
        }
        let entries = ledger.read().expect("three entries");
        assert_eq!(entries.len(), 3, "{entries:?}");
        assert!(
            entries.contains_key(&(key("one", "explore", 0), None)),
            "a run that named no model is its own entry: {entries:?}"
        );
    }

    /// A ledger line that is not an entry stops the pass: it is the record of
    /// what was bought, and a record that cannot be read is not one.
    #[test]
    fn a_line_that_is_not_an_entry_is_an_error() {
        let dir = TempDir::new("ledger-broken");
        let ledger = Ledger::at(dir.path().join("ledger.jsonl"));
        ledger
            .record(&Entry::buying(key("one", "explore", 0), None, dir.path()))
            .expect("a purchase");
        fs::write(ledger.path(), "not json\n").expect("a broken ledger");
        let error = ledger.read().expect_err("a broken line");
        assert!(error.contains("ledger.jsonl:1"), "{error}");
    }

    /// One file for the machine, named outright by the environment and never
    /// inside a run directory unless there is nowhere else to put it.
    #[test]
    fn the_default_ledger_is_one_file_for_the_machine() {
        let env = |pairs: &[(&str, &str)]| {
            let pairs: BTreeMap<String, String> = pairs
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect();
            move |name: &str| pairs.get(name).cloned()
        };

        // Named outright.
        assert_eq!(
            path_from(&env(&[("S1M_LEDGER", "/tmp/eval.jsonl")])),
            Some(PathBuf::from("/tmp/eval.jsonl"))
        );
        // Elsewhere in the data directory, and XDG wins over HOME where both
        // are set, which is what XDG_DATA_HOME means.
        assert_eq!(
            path_from(&env(&[("XDG_DATA_HOME", "/data")])),
            Some(PathBuf::from("/data/eval-agent/ledger.jsonl"))
        );
        assert_eq!(
            path_from(&env(&[("HOME", "/home/someone")])),
            Some(PathBuf::from(
                "/home/someone/.local/share/eval-agent/ledger.jsonl"
            ))
        );
        assert_eq!(
            path_from(&env(&[
                ("HOME", "/home/someone"),
                ("XDG_DATA_HOME", "/data")
            ])),
            Some(PathBuf::from("/data/eval-agent/ledger.jsonl"))
        );
        // A machine that keeps no home directory has nothing to share, and the
        // ledger goes where the rows are.
        assert_eq!(path_from(&env(&[])), None);
        assert_eq!(
            Path::new("/somewhere/run")
                .join(LEDGER)
                .parent()
                .expect("a run directory"),
            Path::new("/somewhere/run")
        );
    }
}
