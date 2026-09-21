//! The rows, reduced to numbers keyed by query id, category, condition and
//! tier.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::row::{Row, asked_for};

/// One metric over a set of runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stat {
    pub mean: f64,
    /// The sample standard deviation, `null` for a single run: one measurement
    /// has no spread, and reporting zero would claim it does.
    pub sd: Option<f64>,
    pub min: f64,
    pub max: f64,
    pub n: usize,
}

/// Every metric over one set of runs, plus how many runs there were.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub runs: usize,
    /// Runs that produced no measurement, counted but not averaged.
    pub failed: usize,
    pub metrics: BTreeMap<String, Stat>,
}

/// One query's runs under one condition at one tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryGroup {
    pub category: String,
    #[serde(flatten)]
    pub group: Group,
}

/// One condition's runs at one tier: one cell of the results table.
///
/// A directory can hold one condition measured at two models, and a mean over
/// both would be a mean over two experiments. A condition that runs no agent
/// has no tier — it is the same run whatever a pass names — and stays one cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    /// The condition, as `--conditions` spells it. Empty on a group read from
    /// an aggregates file written before the pair was, whose key names it.
    #[serde(default)]
    pub condition: String,
    /// The model these runs were asked for, `None` where the pass named none or
    /// the condition runs no agent. What actually answered is
    /// [`Aggregates::models`], which is another question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub overall: Group,
    pub by_query: BTreeMap<String, QueryGroup>,
    pub by_category: BTreeMap<String, Group>,
}

/// Everything the report is rendered from, besides the graph statistics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Aggregates {
    /// Distinct queries any condition ran.
    pub queries: usize,
    /// The models the measured work ran on, by how many runs each: the tier the
    /// numbers were measured at, which is a label like a query id and not a
    /// detail.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, usize>,
    /// The wiki revisions the rows were measured against, one entry per
    /// distinct revision. More than one is a directory that mixed two cuts of a
    /// wiki, and a report says so rather than naming one of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wiki: Vec<String>,
    /// How many runs were bought for this directory, as the ledger recorded
    /// them. Only `run` knows, and only it fills this in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bought: Option<usize>,
    /// Distinct conditions, and the tiers each was asked for: one entry per
    /// `(condition, model)`, so a condition measured at two models is two
    /// cells. Keyed by [`filed_under`].
    pub conditions: BTreeMap<String, Condition>,
    /// How the runs were made, filled in by the runner: flags and constants,
    /// never a path or a query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<crate::report::Method>,
}

/// Reduces the rows to numbers. Rows that failed are counted and left out of
/// every average: a run that errored has no recall, and averaging it as zero
/// would make a broken harness look like a bad method.
///
/// A group is a condition and the model it was asked for. One directory can
/// hold one condition measured at two models — a screening pass at a cheaper
/// tier beside the one it screens — and a mean over both would be a mean over
/// two experiments: what a report has to show beside each other is the two
/// tiers, so they are two cells.
pub fn aggregate(rows: &[Row]) -> Aggregates {
    let mut queries = std::collections::BTreeSet::new();
    let mut models: BTreeMap<String, usize> = BTreeMap::new();
    let mut wiki = std::collections::BTreeSet::new();
    let mut conditions: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        queries.insert(row.query_id.clone());
        for model in &row.models {
            *models.entry(model.clone()).or_default() += 1;
        }
        if !row.wiki.is_empty() {
            wiki.insert(row.wiki.clone());
        }
        conditions
            .entry(filed_under(&row.condition, row.model_asked_for.as_deref()))
            .or_default()
            .push(row);
    }

    let conditions = conditions
        .into_iter()
        .map(|(name, rows)| {
            // Every row of a group was filed by its own condition and tier, so
            // the first one's are the group's.
            let first = rows[0];
            let condition = first.condition.clone();
            let model = asked_for(&condition, first.model_asked_for.as_deref());
            let mut by_query: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
            let mut by_category: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
            for row in &rows {
                by_query.entry(row.query_id.clone()).or_default().push(row);
                by_category
                    .entry(row.category.clone())
                    .or_default()
                    .push(row);
            }
            let condition = Condition {
                condition,
                model,
                overall: group(&rows),
                by_query: by_query
                    .into_iter()
                    .map(|(id, rows)| {
                        (
                            id,
                            QueryGroup {
                                category: rows[0].category.clone(),
                                group: group(&rows),
                            },
                        )
                    })
                    .collect(),
                by_category: by_category
                    .into_iter()
                    .map(|(category, rows)| (category, group(&rows)))
                    .collect(),
            };
            (name, condition)
        })
        .collect();

    Aggregates {
        queries: queries.len(),
        models,
        wiki: wiki.into_iter().collect(),
        bought: None,
        conditions,
        method: None,
    }
}

/// The key a set of runs is filed under: the condition, and the model it was
/// asked for where there was one. A condition that runs no agent takes no tier,
/// so it is one entry — `s1m` — whatever a pass named, and an agent condition is
/// `explore` where none was named and `explore@haiku` where one was.
fn filed_under(condition: &str, model: Option<&str>) -> String {
    match asked_for(condition, model) {
        Some(model) => format!("{condition}@{model}"),
        None => condition.to_string(),
    }
}

/// Every metric named by any run in the set, averaged over the runs that
/// carried it.
fn group(rows: &[&Row]) -> Group {
    let mut samples: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for row in rows.iter().filter(|row| row.ok) {
        for (metric, value) in &row.metrics {
            samples.entry(metric).or_default().push(*value);
        }
    }
    Group {
        runs: rows.len(),
        failed: rows.iter().filter(|row| !row.ok).count(),
        metrics: samples
            .into_iter()
            .map(|(metric, values)| (metric.to_string(), stat(&values)))
            .collect(),
    }
}

/// One metric's spread. The standard deviation is the sample one, over `n - 1`,
/// because these runs are a sample of what the agent would do and not the whole
/// of it.
fn stat(values: &[f64]) -> Stat {
    let n = values.len();
    let mean = values.iter().sum::<f64>() / n as f64;
    let sd = (n > 1).then(|| {
        let variance = values
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / (n - 1) as f64;
        variance.sqrt()
    });
    Stat {
        mean,
        sd,
        min: values.iter().copied().fold(f64::INFINITY, f64::min),
        max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The revision these rows were measured against.
    const WIKI: &str = "sha256:0f1e2d3c";

    fn row(id: &str, category: &str, condition: &str, repeat: usize, recall: f64) -> Row {
        Row {
            query_id: id.to_string(),
            category: category.to_string(),
            condition: condition.to_string(),
            repeat,
            wiki: WIKI.to_string(),
            ok: true,
            metrics: BTreeMap::from([("recall".to_string(), recall)]),
            model_asked_for: None,
            models: Vec::new(),
            detail: Some(serde_json::json!({"query": "the query text"})),
        }
    }

    #[test]
    fn averages_every_metric_per_condition_query_and_category() {
        let rows = vec![
            row("one", "how-to", "explore", 0, 1.0),
            row("one", "how-to", "explore", 1, 2.0),
            row("one", "how-to", "explore", 2, 6.0),
            row("two", "reference", "explore", 0, 0.0),
            row("one", "how-to", "s1m", 0, 0.5),
        ];

        let aggregates = aggregate(&rows);
        assert_eq!(aggregates.queries, 2);

        let explore = &aggregates.conditions["explore"];
        // By hand over 1, 2, 6 and 0: mean 2.25.
        assert_eq!(explore.overall.runs, 4);
        assert!((explore.overall.metrics["recall"].mean - 2.25).abs() < 1e-12);

        // By hand over 1, 2, 6: mean 3, sample sd sqrt(7).
        let one = &explore.by_query["one"].group.metrics["recall"];
        assert!((one.mean - 3.0).abs() < 1e-12);
        assert!((one.sd.expect("three runs have a spread") - 7.0_f64.sqrt()).abs() < 1e-12);
        assert_eq!((one.min, one.max, one.n), (1.0, 6.0, 3));
        assert_eq!(explore.by_query["one"].category, "how-to");

        // One run has no spread to report.
        assert_eq!(explore.by_query["two"].group.metrics["recall"].sd, None);
        assert_eq!(explore.by_category["reference"].metrics["recall"].mean, 0.0);
        assert_eq!(aggregates.conditions["s1m"].overall.runs, 1);
    }

    /// The models and the wiki revisions reach the aggregates: they are what a
    /// report names the tier and the cut by, and they belong to the rows rather
    /// than to the invocation that wrote them.
    #[test]
    fn the_models_and_the_revisions_reach_the_aggregates() {
        let rows = vec![
            Row {
                models: vec!["claude-sonnet-5".to_string()],
                ..row("one", "how-to", "explore", 0, 1.0)
            },
            Row {
                models: vec![
                    "claude-sonnet-5".to_string(),
                    "claude-haiku-4-5".to_string(),
                ],
                ..row("one", "how-to", "explore", 1, 0.0)
            },
            Row {
                wiki: "sha256:other".to_string(),
                ..row("one", "how-to", "s1m", 0, 0.5)
            },
        ];
        let aggregates = aggregate(&rows);
        assert_eq!(aggregates.models["claude-sonnet-5"], 2);
        assert_eq!(aggregates.models["claude-haiku-4-5"], 1);
        assert_eq!(
            aggregates.wiki,
            vec![WIKI.to_string(), "sha256:other".to_string()],
            "one entry per cut the rows were measured against"
        );
        assert_eq!(
            aggregates.bought, None,
            "only `run` knows what a pass bought"
        );

        // A row from before revisions were recorded adds nothing to the cut.
        let aggregates = aggregate(&[Row {
            wiki: String::new(),
            ..row("one", "how-to", "explore", 0, 1.0)
        }]);
        assert!(aggregates.wiki.is_empty(), "{:?}", aggregates.wiki);
    }

    /// One directory can hold one condition measured at two tiers, and the
    /// report has a cell per tier: averaging them into one would hide the thing
    /// the second pass was bought for.
    #[test]
    fn one_condition_at_two_tiers_is_two_groups() {
        let rows = vec![
            Row {
                model_asked_for: Some("sonnet".to_string()),
                models: vec!["claude-sonnet-5".to_string()],
                ..row("one", "how-to", "explore", 0, 1.0)
            },
            Row {
                model_asked_for: Some("sonnet".to_string()),
                models: vec!["claude-sonnet-5".to_string()],
                ..row("one", "how-to", "explore", 1, 1.0)
            },
            Row {
                model_asked_for: Some("haiku".to_string()),
                models: vec!["claude-haiku-4-5".to_string()],
                ..row("one", "how-to", "explore", 0, 0.0)
            },
            // A condition that runs no agent takes no tier, and keeps one
            // group whatever a row says: `s1m` here carries a model only a
            // version that did not normalise it could have written.
            Row {
                model_asked_for: Some("haiku".to_string()),
                ..row("one", "how-to", "s1m", 0, 0.5)
            },
        ];

        let aggregates = aggregate(&rows);
        assert_eq!(
            aggregates.conditions.len(),
            3,
            "{:?}",
            aggregates.conditions.keys().collect::<Vec<_>>()
        );
        let group = |condition: &str, model: Option<&str>| {
            aggregates
                .conditions
                .values()
                .find(|group| group.condition == condition && group.model.as_deref() == model)
                .unwrap_or_else(|| panic!("no `{condition}` group asked for {model:?}"))
        };

        // The sonnet rows are one cell, over their own runs, and the haiku row
        // is another: neither is averaged into the other.
        let sonnet = group("explore", Some("sonnet"));
        assert_eq!((sonnet.overall.runs, sonnet.overall.failed), (2, 0));
        assert_eq!(sonnet.overall.metrics["recall"].mean, 1.0);
        assert_eq!(sonnet.by_query["one"].group.metrics["recall"].n, 2);
        assert_eq!(sonnet.by_category["how-to"].runs, 2);

        let haiku = group("explore", Some("haiku"));
        assert_eq!(haiku.overall.runs, 1);
        assert_eq!(haiku.overall.metrics["recall"].mean, 0.0);

        assert_eq!(group("s1m", None).overall.runs, 1);

        // The tiers survive the file the report is rendered from.
        let text = serde_json::to_string(&aggregates).expect("json");
        assert_eq!(
            serde_json::from_str::<Aggregates>(&text).expect("json"),
            aggregates
        );

        // A directory whose rows all name one tier is one group, as it was.
        let one_tier = vec![
            Row {
                model_asked_for: Some("sonnet".to_string()),
                ..row("one", "how-to", "explore", 0, 1.0)
            },
            Row {
                model_asked_for: Some("sonnet".to_string()),
                ..row("one", "how-to", "explore", 1, 0.0)
            },
        ];
        let aggregates = aggregate(&one_tier);
        assert_eq!(aggregates.conditions.len(), 1);
        assert_eq!(
            aggregates
                .conditions
                .values()
                .next()
                .expect("the one group")
                .overall
                .runs,
            2
        );
    }

    #[test]
    fn a_failed_run_is_counted_but_never_averaged() {
        let mut failed = row("one", "how-to", "explore", 1, 0.0);
        failed.ok = false;
        failed.metrics.clear();
        failed.detail = Some(serde_json::json!({"error": "the agent timed out"}));
        let rows = vec![row("one", "how-to", "explore", 0, 1.0), failed];

        let explore = &aggregate(&rows).conditions["explore"];
        assert_eq!((explore.overall.runs, explore.overall.failed), (2, 1));
        assert_eq!(explore.overall.metrics["recall"].mean, 1.0);
        assert_eq!(explore.overall.metrics["recall"].n, 1);
    }
}
