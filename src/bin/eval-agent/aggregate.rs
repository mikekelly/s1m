//! The rows, reduced to numbers keyed by query id, category and condition.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::row::Row;

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

/// One query's runs under one condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryGroup {
    pub category: String,
    #[serde(flatten)]
    pub group: Group,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
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
    pub conditions: BTreeMap<String, Condition>,
    /// How the runs were made, filled in by the runner: flags and constants,
    /// never a path or a query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<crate::report::Method>,
}

/// Reduces the rows to numbers. Rows that failed are counted and left out of
/// every average: a run that errored has no recall, and averaging it as zero
/// would make a broken harness look like a bad method.
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
            .entry(row.condition.clone())
            .or_default()
            .push(row);
    }

    let conditions = conditions
        .into_iter()
        .map(|(name, rows)| {
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
