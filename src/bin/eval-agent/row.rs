//! One measured run, as it is written to and read back from the JSONL.
//!
//! A row is deliberately in two halves. The keys — the query id, its category,
//! the condition and the repeat — and the `metrics` map are numbers and labels
//! a report may print. Everything else lives under `detail`: the query as it
//! was asked, the files an agent opened, the command that was run. Nothing
//! downstream of [`crate::aggregate`] reads `detail`, so what can be published
//! is decided here rather than in the renderer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The conditions a run can measure, as the `--conditions` flag spells them.
pub const CONDITIONS: [&str; 3] = ["explore", "s1m", "s1m-agent"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub query_id: String,
    pub category: String,
    /// One of [`CONDITIONS`], or a variant of one such as `s1m-cold`.
    pub condition: String,
    /// Which pass over the gold set this was, from 0.
    pub repeat: usize,
    /// Whether the run produced a measurement. A failed run is written too, so
    /// that a resumed run does not try it again forever.
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Everything a report may print, by name.
    pub metrics: BTreeMap<String, f64>,
    /// Everything it may not: this half never leaves `--out`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl Row {
    /// What identifies this run: a resumed pass skips a run it already has.
    pub fn key(&self) -> (String, String, usize) {
        (self.query_id.clone(), self.condition.clone(), self.repeat)
    }
}
