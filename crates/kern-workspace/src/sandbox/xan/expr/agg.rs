//! The aggregations a groupby (or agg, or stats) computes over a group.

use super::Value;

/// What a groupby can compute over a group.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AggFn {
    /// `count()`: the rows; `count(x)`: the rows where x is true (a cell: not empty).
    Count,
    CountEmpty,
    Sum,
    Mean,
    Median,
    Min,
    Max,
    First,
    Last,
    Values,
    DistinctValues,
    Cardinality,
    Mode,
    /// Population variance and standard deviation.
    Var,
    Stddev,
    LexFirst,
    LexLast,
    All,
    Any,
}

impl AggFn {
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "count" => AggFn::Count,
            "count_empty" => AggFn::CountEmpty,
            "sum" => AggFn::Sum,
            "mean" | "avg" | "average" => AggFn::Mean,
            "median" => AggFn::Median,
            "min" => AggFn::Min,
            "max" => AggFn::Max,
            "first" => AggFn::First,
            "last" => AggFn::Last,
            "values" => AggFn::Values,
            "distinct_values" => AggFn::DistinctValues,
            "cardinality" | "count_distinct" | "distinct_count" => AggFn::Cardinality,
            "mode" | "most_common" => AggFn::Mode,
            "var" | "variance" => AggFn::Var,
            "stddev" | "std" => AggFn::Stddev,
            "lex_first" => AggFn::LexFirst,
            "lex_last" => AggFn::LexLast,
            "all" => AggFn::All,
            "any" => AggFn::Any,
            _ => return None,
        })
    }

    /// What the aggregation makes of a group's values (`rows`: the group's
    /// row count, which `count()` reports). Empty cells and text count for
    /// nothing in the numeric ones; with no number at all they are empty, but
    /// a sum is 0.
    pub fn finish(self, vals: &[Value], rows: usize, counted_rows: bool) -> Value {
        let nums = || vals.iter().filter(|v| !v.is_empty()).filter_map(Value::num);
        let present = || vals.iter().filter(|v| !v.is_empty());
        let num_or_null = |n: Option<f64>| n.map_or(Value::Null, Value::Num);
        // The least (or the greatest) value present, as text.
        let lex = |least: bool| {
            let texts = present().map(Value::text);
            if least { texts.min() } else { texts.max() }.map_or(Value::Null, Value::Str)
        };
        match self {
            AggFn::Count if counted_rows => Value::Num(rows as f64),
            AggFn::Count => Value::Num(vals.iter().filter(|v| v.truthy()).count() as f64),
            AggFn::CountEmpty => Value::Num(vals.iter().filter(|v| v.is_empty()).count() as f64),
            AggFn::Sum => Value::Num(nums().sum()),
            AggFn::Mean => {
                let (s, n) = nums().fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
                num_or_null((n > 0).then(|| s / n as f64))
            }
            AggFn::Median => {
                let mut v: Vec<f64> = nums().collect();
                v.sort_by(f64::total_cmp);
                let n = v.len();
                num_or_null(match n {
                    0 => None,
                    _ if n % 2 == 1 => Some(v[n / 2]),
                    _ => Some((v[n / 2 - 1] + v[n / 2]) / 2.0),
                })
            }
            AggFn::Min | AggFn::Max => {
                let best = |a: f64, b: f64| {
                    if self == AggFn::Min {
                        a.min(b)
                    } else {
                        a.max(b)
                    }
                };
                match nums().reduce(best) {
                    Some(n) => Value::Num(n),
                    // No number at all: the text, in order.
                    None => lex(self == AggFn::Min),
                }
            }
            AggFn::First => present().next().cloned().unwrap_or(Value::Null),
            AggFn::Last => present().next_back().cloned().unwrap_or(Value::Null),
            AggFn::Values => Value::Str(present().map(Value::text).collect::<Vec<_>>().join("|")),
            AggFn::DistinctValues => {
                Value::Str(distinct(vals).into_iter().collect::<Vec<_>>().join("|"))
            }
            AggFn::Cardinality => Value::Num(distinct(vals).len() as f64),
            AggFn::Mode => {
                let mut counts: Vec<(String, usize)> = Vec::new();
                for t in present().map(Value::text) {
                    match counts.iter_mut().find(|(v, _)| *v == t) {
                        Some((_, c)) => *c += 1,
                        None => counts.push((t, 1)),
                    }
                }
                // The most frequent; on a tie, the first seen.
                let top = counts.iter().map(|(_, c)| *c).max();
                counts
                    .into_iter()
                    .find(|(_, c)| Some(*c) == top)
                    .map_or(Value::Null, |(v, _)| Value::Str(v))
            }
            AggFn::Var | AggFn::Stddev => {
                let v: Vec<f64> = nums().collect();
                if v.is_empty() {
                    return Value::Null;
                }
                let mean = v.iter().sum::<f64>() / v.len() as f64;
                let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64;
                Value::Num(if self == AggFn::Var { var } else { var.sqrt() })
            }
            AggFn::LexFirst => lex(true),
            AggFn::LexLast => lex(false),
            AggFn::All => Value::Bool(vals.iter().all(Value::truthy)),
            AggFn::Any => Value::Bool(vals.iter().any(Value::truthy)),
        }
    }
}

/// The distinct non-empty values, sorted.
fn distinct(vals: &[Value]) -> std::collections::BTreeSet<String> {
    vals.iter()
        .filter(|v| !v.is_empty())
        .map(Value::text)
        .collect()
}
