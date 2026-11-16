//! The verbs that compute over a whole table: sort's order, groupby / agg,
//! stats and frequency.

use std::collections::HashMap;

use super::{
    Clock, R,
    expr::{AggFn, Value, compile_aggs, parse_num},
    fail,
    table::{Table, cell, write_csv},
};

/// As numbers when `numeric` (numbers first, then text in order), else as text.
pub(super) fn compare(a: &str, b: &str, numeric: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    if !numeric {
        return a.cmp(b);
    }
    match (parse_num(a), parse_num(b)) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => Less,
        (None, Some(_)) => Greater,
        (None, None) => a.cmp(b),
    }
}

pub(super) fn aggregate(t: &Table, keys: &[usize], spec: &str, clock: &mut Clock) -> R<String> {
    let (slots, items) = compile_aggs(spec, &t.headers).map_err(fail)?;
    struct Group {
        key: Vec<String>,
        rows: usize,
        vals: Vec<Vec<Value>>,
    }
    let group = |key| Group {
        key,
        rows: 0,
        vals: vec![Vec::new(); slots.len()],
    };
    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<Vec<String>, usize> = HashMap::new();
    if keys.is_empty() {
        // agg: one group, even over no rows (count 0, sum 0).
        groups.push(group(Vec::new()));
        index.insert(Vec::new(), 0);
    }
    for r in &t.rows {
        clock.tick()?;
        let key: Vec<String> = keys.iter().map(|&i| cell(r, i).to_string()).collect();
        let g = match index.get(&key) {
            Some(&g) => g,
            None => {
                index.insert(key.clone(), groups.len());
                groups.push(group(key));
                groups.len() - 1
            }
        };
        let g = &mut groups[g];
        g.rows += 1;
        for (s, slot) in slots.iter().enumerate() {
            if let Some(arg) = &slot.arg {
                g.vals[s].push(arg.eval(r, &[]));
            }
        }
    }
    let header: Vec<String> = keys
        .iter()
        .map(|&i| t.headers[i].clone())
        .chain(items.iter().map(|(n, _)| n.clone()))
        .collect();
    let rows = groups.into_iter().map(|g| {
        let done: Vec<Value> = slots
            .iter()
            .zip(&g.vals)
            .map(|(s, v)| s.func.finish(v, g.rows, s.arg.is_none()))
            .collect();
        g.key
            .into_iter()
            .chain(items.iter().map(|(_, e)| e.eval(&[], &done).text()))
            .collect::<Vec<_>>()
    });
    write_csv(Some(&header), rows)
}

const STATS: &[&str] = &[
    "field",
    "count",
    "count_empty",
    "type",
    "sum",
    "mean",
    "median",
    "stddev",
    "min",
    "max",
    "lex_first",
    "lex_last",
    "min_length",
    "max_length",
];

pub(super) fn stats(t: &Table, cols: &[usize]) -> R<String> {
    let header: Vec<String> = STATS.iter().map(|s| s.to_string()).collect();
    let rows = cols.iter().map(|&c| {
        let vals: Vec<Value> = t
            .rows
            .iter()
            .map(|r| Value::Str(cell(r, c).to_string()))
            .collect();
        let present: Vec<&str> = t
            .rows
            .iter()
            .map(|r| cell(r, c))
            .filter(|s| !s.trim().is_empty())
            .collect();
        let (mut ints, mut floats, mut texts) = (0, 0, 0);
        for s in &present {
            match parse_num(s) {
                Some(_)
                    if s.trim()
                        .bytes()
                        .all(|b| b.is_ascii_digit() || b == b'-' || b == b'+') =>
                {
                    ints += 1
                }
                Some(_) => floats += 1,
                None => texts += 1,
            }
        }
        let number_type = if floats > 0 { "float" } else { "int" };
        let kind = match (ints + floats > 0, texts > 0) {
            (false, false) => "empty".to_string(),
            (true, false) => number_type.to_string(),
            (false, true) => "string".to_string(),
            (true, true) => format!("{number_type}|string"),
        };
        let numeric = |f: AggFn| {
            if ints + floats > 0 {
                f.finish(&vals, vals.len(), false).text()
            } else {
                String::new()
            }
        };
        let lengths = present.iter().map(|s| s.chars().count());
        vec![
            t.headers[c].clone(),
            present.len().to_string(),
            (vals.len() - present.len()).to_string(),
            kind,
            numeric(AggFn::Sum),
            numeric(AggFn::Mean),
            numeric(AggFn::Median),
            numeric(AggFn::Stddev),
            numeric(AggFn::Min),
            numeric(AggFn::Max),
            AggFn::LexFirst.finish(&vals, vals.len(), false).text(),
            AggFn::LexLast.finish(&vals, vals.len(), false).text(),
            lengths
                .clone()
                .min()
                .map_or(String::new(), |n| n.to_string()),
            lengths.max().map_or(String::new(), |n| n.to_string()),
        ]
    });
    write_csv(Some(&header), rows)
}

pub(super) fn frequency(t: &Table, cols: &[usize], limit: usize) -> R<String> {
    let header: Vec<String> = ["field", "value", "count"].map(String::from).to_vec();
    let mut out = Vec::new();
    for &c in cols {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for r in &t.rows {
            *counts.entry(cell(r, c)).or_default() += 1;
        }
        let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        for (v, count) in ranked.into_iter().take(limit) {
            let value = if v.is_empty() { "<empty>" } else { v };
            out.push(vec![
                t.headers[c].clone(),
                value.to_string(),
                count.to_string(),
            ]);
        }
    }
    write_csv(Some(&header), out)
}
