//! `xan`: CSV verbs in the sandbox shell, after medialab's xan (the TS
//! just-bash shell kern's hosts used before had it, and models know it):
//! headers, count, head, select, filter, sort, groupby, agg, stats, frequency.
//! The expressions are the part of moonblade those need (`expr.rs`); the
//! selections are xan's (`select.rs`). Every sandbox has it (a host's own
//! `xan` replaces it, [`super::SandboxWorkspace::with_command`]).
//!
//! It reads a file through kern's [`Fs`](kern_core::fs::Fs) or the pipe, and
//! checks the shell call's deadline as it goes: a CPU-bound command never
//! yields to the shell's timer.

mod expr;
mod select;
#[cfg(test)]
mod tests;

use std::{collections::HashMap, io, time::Instant};

use async_trait::async_trait;
use kern_core::{
    fs::resolve_path,
    shell_command::{ShellCall, ShellCommand},
    workspace::ExecResult,
};
use tokio_util::sync::CancellationToken;

use expr::{AggFn, Value, compile, compile_aggs, parse_num};
use select::select;

/// The sandbox's `xan`.
pub(crate) struct Xan;

const NOTE: &str = "xan does CSV: `xan headers f.csv`, count, head, select, `filter 'amount > 100 and status eq \"paid\"'`, \
`sort -s amount -N -R`, `groupby country 'sum(amount) as total, count()'`, agg, stats, frequency (`xan --help`).";

#[async_trait]
impl ShellCommand for Xan {
    fn name(&self) -> &str {
        "xan"
    }

    fn note(&self) -> Option<String> {
        Some(NOTE.into())
    }

    async fn run(&self, call: ShellCall) -> ExecResult {
        match run(&call).await {
            Ok(stdout) => ExecResult {
                stdout,
                ..Default::default()
            },
            Err(Fail { msg, code }) => ExecResult {
                stderr: format!("xan: {msg}\n"),
                exit_code: code,
                ..Default::default()
            },
        }
    }
}

struct Fail {
    msg: String,
    code: i32,
}

type R<T> = Result<T, Fail>;

/// The input or an expression is wrong.
fn fail(msg: impl Into<String>) -> Fail {
    Fail {
        msg: msg.into(),
        code: 1,
    }
}

/// The command line is wrong.
fn usage(verb: Option<&str>, msg: impl Into<String>) -> Fail {
    let how = verb.map_or("xan --help".to_string(), |v| format!("xan {v} --help"));
    Fail {
        msg: format!("{} ({how} shows how)", msg.into()),
        code: 2,
    }
}

macro_rules! expr_help {
    () => {
        "Expressions: columns by name, col(\"Name with spaces\") or col(2); numbers; \"strings\"; + - * / // % **; ++ joins text;\n\
         == != < <= > >= compare as numbers when both sides are, eq ne lt le gt ge as text; x =~ \"regex\"; and or not;\n\
         len lower upper trim contains(x, \"s\") startswith endswith replace(x, \"a\", \"b\") abs round(x, 2) floor ceil int\n\
         is_empty is_numeric coalesce(a, b) if(cond, a, b) concat. A cell that is not a number makes arithmetic empty.\n"
    };
}

macro_rules! agg_help {
    () => {
        "Aggregations: count() (rows), count(x) (rows where x is true or not empty), count_empty(x), sum, mean, median,\n\
         min, max, first, last, values, distinct_values, cardinality, mode, var, stddev, lex_first, lex_last, all, any.\n\
         Each item: an expression over them, named by `as`: 'sum(amount) as total, sum(amount) / count() as avg'.\n\
         Empty cells and text count for nothing in the numeric ones.\n"
    };
}

const HELP: &str = concat!(
    "xan: CSV in the shell. xan <verb> [options] [file.csv]\n",
    "Every verb: -d ';' sets the delimiter (\\t for tabs; .tsv files are tabs), -n says there is no header row\n",
    "(columns are then 0, 1, ...). No file: the pipe.\n\n",
    "  headers    the columns, numbered (-j: names only)\n",
    "  count      how many rows\n",
    "  head       the first rows (-l N, default 10)\n",
    "  select     some columns: xan select name,amount (a:c ranges, 0-based indices, !notes leaves one out)\n",
    "  filter     rows that match: xan filter 'amount > 100 and status eq \"paid\"' (-v the others, -l N)\n",
    "  sort       xan sort -s amount -N -R (-s the columns, -N as numbers, -R largest first)\n",
    "  groupby    xan groupby country 'sum(amount) as total, count() as n'\n",
    "  agg        the same over every row: xan agg 'sum(amount), mean(amount)'\n",
    "  stats      per column: count, empties, type, sum, mean, median, stddev, min, max, lengths (-s columns)\n",
    "  frequency  the most common values per column (-s columns, -l N (10), -A all)\n\n",
    expr_help!(),
    agg_help!(),
);

/// A verb's command line.
struct Verb {
    name: &'static str,
    /// What it takes before the input file.
    args: &'static [&'static str],
    /// (short, long, takes a value).
    flags: &'static [(char, &'static str, bool)],
    help: &'static str,
}

const COMMON: &[(char, &str, bool)] = &[('d', "delimiter", true), ('n', "no-headers", false)];

const VERBS: &[Verb] = &[
    Verb {
        name: "headers",
        args: &[],
        flags: &[('j', "just-names", false)],
        help: "xan headers [-j] [file]: the columns, numbered from 0 (-j: just the names).\n",
    },
    Verb {
        name: "count",
        args: &[],
        flags: &[],
        help: "xan count [file]: how many rows, not counting the header.\n",
    },
    Verb {
        name: "head",
        args: &[],
        flags: &[('l', "limit", true)],
        help: "xan head [-l N] [file]: the header and the first N rows (10).\n",
    },
    Verb {
        name: "select",
        args: &["a selection"],
        flags: &[],
        help: "xan select <selection> [file]: those columns, in that order.\n\
               name,amount | \"Name, with comma\" | a:c (a range) | 0,2 (0-based indices) | !notes (all but notes) | *\n",
    },
    Verb {
        name: "filter",
        args: &["an expression"],
        flags: &[('v', "invert", false), ('l', "limit", true)],
        help: concat!(
            "xan filter [-v] [-l N] '<expression>' [file]: the rows where it is true (-v: where it is not; -l: the first N).\n",
            "  xan filter 'amount >= 100 and lower(status) eq \"paid\"' f.csv\n",
            expr_help!()
        ),
    },
    Verb {
        name: "sort",
        args: &[],
        flags: &[
            ('s', "select", true),
            ('N', "numeric", false),
            ('R', "reverse", false),
        ],
        help: "xan sort [-s columns] [-N] [-R] [file]: rows in order of those columns (all of them by default).\n\
               -N compares numbers (text after them), -R reverses. Rows that tie keep their order.\n",
    },
    Verb {
        name: "groupby",
        args: &["the columns to group by", "the aggregations"],
        flags: &[],
        help: concat!(
            "xan groupby <columns> '<aggregations>' [file]: one row per distinct value of the columns, in order of\n",
            "first appearance, with what the aggregations make of the group's rows (pipe into xan sort to rank them).\n",
            "  xan groupby country,year 'sum(amount) as total, count() as orders, max(amount)' f.csv\n",
            agg_help!(),
            expr_help!()
        ),
    },
    Verb {
        name: "agg",
        args: &["the aggregations"],
        flags: &[],
        help: concat!(
            "xan agg '<aggregations>' [file]: one row, the aggregations over every row.\n",
            "  xan agg 'sum(amount) as total, mean(amount), count()' f.csv\n",
            agg_help!()
        ),
    },
    Verb {
        name: "stats",
        args: &[],
        flags: &[('s', "select", true)],
        help: "xan stats [-s columns] [file]: per column, one row: field, count (not empty), count_empty, type\n\
               (int, float, string, empty; int|string when mixed), sum, mean, median, stddev, min, max (over the\n\
               numbers), lex_first, lex_last, min_length, max_length.\n",
    },
    Verb {
        name: "frequency",
        args: &[],
        flags: &[
            ('s', "select", true),
            ('l', "limit", true),
            ('A', "all", false),
        ],
        help: "xan frequency [-s columns] [-l N] [-A] [file]: field, value, count: each column's most common values,\n\
               most first (-l: how many per column, 10; -A or -l 0: all). An empty cell is <empty>.\n",
    },
];

fn verb(name: &str) -> Option<&'static Verb> {
    let name = match name {
        "freq" => "frequency",
        "group" | "group-by" | "group_by" => "groupby",
        other => other,
    };
    VERBS.iter().find(|v| v.name == name)
}

struct Parsed {
    /// By long name; a switch's value is empty.
    flags: HashMap<&'static str, String>,
    pos: Vec<String>,
}

impl Parsed {
    fn has(&self, flag: &str) -> bool {
        self.flags.contains_key(flag)
    }

    fn get(&self, flag: &str) -> Option<&str> {
        self.flags.get(flag).map(String::as_str)
    }

    fn count(&self, flag: &str, verb: &Verb) -> R<Option<usize>> {
        self.get(flag)
            .map(|v| {
                v.trim().parse().map_err(|_| {
                    usage(
                        Some(verb.name),
                        format!("--{flag} takes a number, not `{v}`"),
                    )
                })
            })
            .transpose()
    }
}

fn parse_args(verb: &Verb, args: &[String]) -> R<Parsed> {
    let specs: Vec<(char, &'static str, bool)> = COMMON.iter().chain(verb.flags).copied().collect();
    let unknown = |a: &str| {
        usage(
            Some(verb.name),
            format!("xan {} has no option {a}", verb.name),
        )
    };
    let mut p = Parsed {
        flags: HashMap::new(),
        pos: Vec::new(),
    };
    let mut it = args.iter();
    let mut only_positional = false;
    while let Some(a) = it.next() {
        if only_positional || a == "-" || !a.starts_with('-') || parse_num(a).is_some() {
            p.pos.push(a.clone());
            continue;
        }
        if a == "--" {
            only_positional = true;
            continue;
        }
        let mut value_for = |long: &'static str, inline: Option<&str>| -> R<String> {
            match inline.filter(|v| !v.is_empty()) {
                Some(v) => Ok(v.to_string()),
                None => it
                    .next()
                    .cloned()
                    .ok_or_else(|| usage(Some(verb.name), format!("--{long} needs a value"))),
            }
        };
        if let Some(long) = a.strip_prefix("--") {
            let (name, inline) = long
                .split_once('=')
                .map_or((long, None), |(n, v)| (n, Some(v)));
            let &(_, long, takes) = specs
                .iter()
                .find(|s| s.1 == name)
                .ok_or_else(|| unknown(a))?;
            let v = if takes {
                value_for(long, inline)?
            } else {
                String::new()
            };
            p.flags.insert(long, v);
            continue;
        }
        // Short ones: switches run together (-NR); one that takes a value takes
        // the rest of the word (-l5, -d';') or the next argument.
        let shorts = &a[1..];
        for (k, c) in shorts.char_indices() {
            let &(_, long, takes) = specs
                .iter()
                .find(|s| s.0 == c)
                .ok_or_else(|| unknown(&format!("-{c}")))?;
            if takes {
                let v = value_for(long, Some(&shorts[k + c.len_utf8()..]))?;
                p.flags.insert(long, v);
                break;
            }
            p.flags.insert(long, String::new());
        }
    }
    Ok(p)
}

/// The shell call's deadline and interrupt, checked every 1024 rows.
struct Clock {
    ends_at: Instant,
    cancel: CancellationToken,
    n: u32,
}

impl Clock {
    fn tick(&mut self) -> R<()> {
        self.n = self.n.wrapping_add(1);
        if self.n % 1024 == 0 && (self.cancel.is_cancelled() || Instant::now() >= self.ends_at) {
            return Err(fail("stopped: the shell call ran out of time"));
        }
        Ok(())
    }
}

struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    /// False under -n: then no verb writes a header row of its own.
    has_headers: bool,
}

fn cell(row: &[String], i: usize) -> &str {
    row.get(i).map_or("", String::as_str)
}

async fn input(call: &ShellCall, file: Option<&str>) -> R<String> {
    match file {
        Some(f) if f != "-" => {
            let bytes = call
                .fs
                .read(&resolve_path(&call.cwd, f))
                .await
                .map_err(|e| {
                    fail(match e.kind() {
                        io::ErrorKind::NotFound => format!("{f}: no such file"),
                        _ => format!("{f}: {e}"),
                    })
                })?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => call
            .stdin_text()
            .map(|s| s.into_owned())
            .ok_or_else(|| usage(None, "no input: give a CSV file, or pipe one in")),
    }
}

fn delimiter(p: &Parsed, file: Option<&str>, text: &str) -> R<u8> {
    if let Some(d) = p.get("delimiter") {
        return match d {
            "\\t" | "\t" | "tab" => Ok(b'\t'),
            d if d.len() == 1 => Ok(d.as_bytes()[0]),
            _ => Err(usage(None, "-d takes one character (\\t for a tab)")),
        };
    }
    if file.is_some_and(|f| f.ends_with(".tsv") || f.ends_with(".tab")) {
        return Ok(b'\t');
    }
    // A header line with tabs (or semicolons) and no comma is not comma separated.
    let first = text.lines().next().unwrap_or("");
    Ok(match () {
        _ if first.contains(',') => b',',
        _ if first.contains('\t') => b'\t',
        _ if first.contains(';') => b';',
        _ => b',',
    })
}

fn read_table(text: &str, delim: u8, has_headers: bool, clock: &mut Clock) -> R<Table> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delim)
        .from_reader(text.as_bytes());
    let mut rows = Vec::new();
    for record in reader.records() {
        clock.tick()?;
        let record = record.map_err(|e| fail(format!("the CSV does not parse: {e}")))?;
        rows.push(record.iter().map(str::to_string).collect::<Vec<_>>());
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let headers = if has_headers && !rows.is_empty() {
        let mut h = rows.remove(0);
        h.resize(width, String::new());
        h
    } else {
        (0..width).map(|i| i.to_string()).collect()
    };
    Ok(Table {
        headers,
        rows,
        has_headers,
    })
}

fn write_csv(header: Option<&[String]>, rows: impl IntoIterator<Item = Vec<String>>) -> R<String> {
    let mut w = csv::WriterBuilder::new()
        .flexible(true)
        .from_writer(Vec::new());
    fn broke(e: impl std::fmt::Display) -> Fail {
        fail(format!("writing the CSV: {e}"))
    }
    if let Some(h) = header {
        w.write_record(h).map_err(broke)?;
    }
    for r in rows {
        w.write_record(&r).map_err(broke)?;
    }
    let bytes = w.into_inner().map_err(broke)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn run(call: &ShellCall) -> R<String> {
    let Some(first) = call.args.first() else {
        return Ok(HELP.into());
    };
    if matches!(first.as_str(), "-h" | "--help" | "help") {
        return Ok(HELP.into());
    }
    let names = VERBS.iter().map(|v| v.name).collect::<Vec<_>>().join(", ");
    let verb =
        verb(first).ok_or_else(|| usage(None, format!("no verb `{first}`; xan has {names}")))?;
    let rest = &call.args[1..];
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(verb.help.into());
    }
    let p = parse_args(verb, rest)?;
    let need = verb.args.len();
    if p.pos.len() < need {
        return Err(usage(
            Some(verb.name),
            format!("xan {} needs {}", verb.name, verb.args.join(" and ")),
        ));
    }
    if p.pos.len() > need + 1 {
        return Err(usage(
            Some(verb.name),
            format!(
                "one input at a time; extra: {} (quote an expression with spaces)",
                p.pos[need + 1..].join(" ")
            ),
        ));
    }
    let file = p.pos.get(need).map(String::as_str);
    let text = input(call, file).await?;
    let delim = delimiter(&p, file, &text)?;
    let mut clock = Clock {
        ends_at: call.ends_at,
        cancel: call.cancel.clone(),
        n: 0,
    };
    let t = read_table(&text, delim, !p.has("no-headers"), &mut clock)?;
    let header = t.has_headers.then_some(t.headers.as_slice());
    let columns = |flag: &str| -> R<Vec<usize>> {
        match p.get(flag) {
            Some(spec) => select(spec, &t.headers).map_err(fail),
            None => Ok((0..t.headers.len()).collect()),
        }
    };
    match verb.name {
        "headers" if p.has("just-names") => {
            Ok(t.headers.iter().map(|h| format!("{h}\n")).collect())
        }
        "headers" => {
            let w = t.headers.len().saturating_sub(1).to_string().len();
            Ok(t.headers
                .iter()
                .enumerate()
                .map(|(i, h)| format!("{i:<w$}   {h}\n"))
                .collect())
        }
        "count" => Ok(format!("{}\n", t.rows.len())),
        "head" => {
            let n = p.count("limit", verb)?.unwrap_or(10);
            write_csv(header, t.rows.into_iter().take(n))
        }
        "select" => {
            let cols = select(&p.pos[0], &t.headers).map_err(fail)?;
            let pick = |r: &[String]| {
                cols.iter()
                    .map(|&i| cell(r, i).to_string())
                    .collect::<Vec<_>>()
            };
            let h = header.map(pick);
            write_csv(h.as_deref(), t.rows.iter().map(|r| pick(r)))
        }
        "filter" => {
            let e = compile(&p.pos[0], &t.headers).map_err(fail)?;
            let invert = p.has("invert");
            let limit = p
                .count("limit", verb)?
                .filter(|n| *n > 0)
                .unwrap_or(usize::MAX);
            let mut kept = Vec::new();
            for r in &t.rows {
                clock.tick()?;
                if e.eval(r, &[]).truthy() != invert {
                    kept.push(r.clone());
                    if kept.len() >= limit {
                        break;
                    }
                }
            }
            write_csv(header, kept)
        }
        "sort" => {
            let cols = columns("select")?;
            let numeric = p.has("numeric");
            let mut rows = t.rows;
            let order = |a: &Vec<String>, b: &Vec<String>| {
                cols.iter()
                    .map(|&i| compare(cell(a, i), cell(b, i), numeric))
                    .find(|o| o.is_ne())
                    .unwrap_or(std::cmp::Ordering::Equal)
            };
            if p.has("reverse") {
                rows.sort_by(|a, b| order(b, a));
            } else {
                rows.sort_by(order);
            }
            write_csv(header, rows)
        }
        "groupby" => {
            let keys = select(&p.pos[0], &t.headers).map_err(fail)?;
            aggregate(&t, &keys, &p.pos[1], &mut clock)
        }
        "agg" => aggregate(&t, &[], &p.pos[0], &mut clock),
        "stats" => stats(&t, &columns("select")?),
        "frequency" => {
            let limit = if p.has("all") {
                usize::MAX
            } else {
                p.count("limit", verb)?.unwrap_or(10)
            };
            frequency(
                &t,
                &columns("select")?,
                if limit == 0 { usize::MAX } else { limit },
            )
        }
        _ => unreachable!("every verb in VERBS has an arm"),
    }
}

/// As numbers when `numeric` (numbers first, then text in order), else as text.
fn compare(a: &str, b: &str, numeric: bool) -> std::cmp::Ordering {
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

fn aggregate(t: &Table, keys: &[usize], spec: &str, clock: &mut Clock) -> R<String> {
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

fn stats(t: &Table, cols: &[usize]) -> R<String> {
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

fn frequency(t: &Table, cols: &[usize], limit: usize) -> R<String> {
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
