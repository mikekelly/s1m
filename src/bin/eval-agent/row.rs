//! One measured run, as it is written to and read back from the JSONL.
//!
//! A row is deliberately in two halves. The keys — the query id, its category,
//! the condition, the repeat and the wiki revision — the model that answered,
//! and the `metrics` map are numbers and labels a report may print. Everything
//! else lives under `detail`: the query as it was asked, the files an agent
//! opened, the command that was run. Nothing downstream of [`crate::aggregate`]
//! reads `detail`, so what can be published is decided here rather than in the
//! renderer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The conditions a run can measure, as the `--conditions` flag spells them.
/// `s1m-t<N>` — s1m at threshold N — is accepted too, and is not a fixed name.
pub const CONDITIONS: [&str; 3] = ["explore", "s1m", "s1m-agent"];

/// Whether a condition runs an agent: the two whose runs are measured on what a
/// model read and answered, and so the two whose rows carry a model. The others
/// are s1m's own walk, which takes a threshold rather than a model.
pub fn agent_condition(condition: &str) -> bool {
    matches!(condition, "explore" | "s1m-agent")
}

/// What identifies a run, for a resumed pass and for the ledger: the revision
/// of the wiki it was measured against, and the query, condition and repeat.
///
/// The revision is in the key rather than beside it because a wiki that has
/// changed makes a different measurement: a pass resumed across a page edit
/// would otherwise average two cuts together in one cell.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Key {
    /// The wiki revision, empty on a row written before one was recorded.
    #[serde(default)]
    pub wiki: String,
    pub query: String,
    pub condition: String,
    pub repeat: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub query_id: String,
    pub category: String,
    /// One of [`CONDITIONS`], or a variant of one such as `s1m-cold`.
    pub condition: String,
    /// Which pass over the gold set this was, from 0.
    pub repeat: usize,
    /// The wiki revision this run was measured against, empty on a row written
    /// before revisions were recorded.
    #[serde(default)]
    pub wiki: String,
    /// Whether the run produced a measurement. A failed run is written too, so
    /// that a resumed run does not try it again forever; it carries no metrics,
    /// and what went wrong is under `detail`, because an error message quotes
    /// requests, paths and stderr.
    pub ok: bool,
    /// Everything a report may print, by name.
    pub metrics: BTreeMap<String, f64>,
    /// The model named on the command line for this run, if one was: every
    /// agent in the run inherits it, so the tier that answered is not the
    /// default it would otherwise have been. `None` is no flag passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_asked_for: Option<String>,
    /// The models Claude Code resolved for the work measured here: the Explore
    /// subagent's, else the agent that answered when nothing was spawned. Empty
    /// on a condition that runs no agent. A label like a query id and not a
    /// detail: it is what lets a report name the tier it measured.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Everything it may not: this half never leaves `--out`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl Row {
    /// What identifies this run: a resumed pass skips a run it already has.
    pub fn key(&self) -> Key {
        Key {
            wiki: self.wiki.clone(),
            query: self.query_id.clone(),
            condition: self.condition.clone(),
            repeat: self.repeat,
        }
    }
}
