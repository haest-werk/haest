//! xan's command line: the help, the verbs and their flags, and parsing one.

use std::collections::HashMap;

use super::{R, expr::parse_num, usage};

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

pub(super) const HELP: &str = concat!(
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
pub(super) struct Verb {
    pub(super) name: &'static str,
    /// What it takes before the input file.
    pub(super) args: &'static [&'static str],
    /// (short, long, takes a value).
    flags: &'static [(char, &'static str, bool)],
    pub(super) help: &'static str,
}

const COMMON: &[(char, &str, bool)] = &[('d', "delimiter", true), ('n', "no-headers", false)];

pub(super) const VERBS: &[Verb] = &[
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

pub(super) fn verb(name: &str) -> Option<&'static Verb> {
    let name = match name {
        "freq" => "frequency",
        "group" | "group-by" | "group_by" => "groupby",
        other => other,
    };
    VERBS.iter().find(|v| v.name == name)
}

pub(super) struct Parsed {
    /// By long name; a switch's value is empty.
    flags: HashMap<&'static str, String>,
    pub(super) pos: Vec<String>,
}

impl Parsed {
    pub(super) fn has(&self, flag: &str) -> bool {
        self.flags.contains_key(flag)
    }

    pub(super) fn get(&self, flag: &str) -> Option<&str> {
        self.flags.get(flag).map(String::as_str)
    }

    pub(super) fn count(&self, flag: &str, verb: &Verb) -> R<Option<usize>> {
        self.get(flag)
            .map(|v| {
                v.trim().parse().map_err(|_not_a_number| {
                    usage(
                        Some(verb.name),
                        format!("--{flag} takes a number, not `{v}`"),
                    )
                })
            })
            .transpose()
    }
}

pub(super) fn parse_args(verb: &Verb, args: &[String]) -> R<Parsed> {
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
