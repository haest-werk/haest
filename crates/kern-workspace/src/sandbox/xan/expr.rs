//! xan's expressions: the part of moonblade (xan's own language) a filter and a
//! groupby need. Columns by name (or `col("Name with spaces")`, `col(2)`),
//! numbers, strings, arithmetic, comparisons, `and`/`or`/`not`, a handful of
//! functions, and in a groupby the aggregations (`sum(amount) as total`).
//!
//! Columns bind to indices when the expression is compiled against the
//! headers, so a misspelt column fails before the first row, with the list of
//! the real ones.

use regex::Regex;

use super::select::{column_list, resolve_name};

/// A cell, a literal, or what an expression makes of them.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
}

impl Value {
    /// The number this value reads as: a cell is a number when it parses as one.
    pub fn num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::Str(s) => parse_num(s),
            Value::Null => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
        }
    }

    /// Null, or a cell with nothing but spaces.
    pub fn is_empty(&self) -> bool {
        match self {
            Value::Null => true,
            Value::Str(s) => s.trim().is_empty(),
            _ => false,
        }
    }

    /// As a CSV cell.
    pub fn text(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Num(n) => fmt_num(*n),
            Value::Str(s) => s.clone(),
        }
    }
}

/// A cell as a number. Rust's parser also takes "inf" and "NaN": a cell that
/// says "nan" is text here, so a number needs a digit.
pub fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if !t.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<f64>().ok()
}

/// A number as a cell: whole numbers without `.0`, and float noise past 15
/// significant digits rounded off (0.1 + 0.2 is 0.3, a sum of money is the sum).
pub fn fmt_num(n: f64) -> String {
    if n.is_nan() {
        return String::new();
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    let rounded: f64 = format!("{n:.14e}").parse().unwrap_or(n);
    format!("{rounded}")
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bin {
    Or,
    And,
    /// `==` and friends: as numbers when both sides are, else as text.
    Cmp(Ordering),
    /// `eq` and friends: always as text.
    StrCmp(Ordering),
    Add,
    Sub,
    Mul,
    Div,
    IntDiv,
    Mod,
    Pow,
    Concat,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ordering {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Ordering {
    fn holds(self, o: std::cmp::Ordering) -> bool {
        use std::cmp::Ordering::*;
        match self {
            Ordering::Eq => o == Equal,
            Ordering::Ne => o != Equal,
            Ordering::Lt => o == Less,
            Ordering::Le => o != Greater,
            Ordering::Gt => o == Greater,
            Ordering::Ge => o != Less,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Func {
    Len,
    Lower,
    Upper,
    Trim,
    Ltrim,
    Rtrim,
    Contains,
    StartsWith,
    EndsWith,
    Replace,
    Abs,
    Round,
    Floor,
    Ceil,
    Int,
    Float,
    IsEmpty,
    IsNumeric,
    Not,
    Coalesce,
    If,
    Concat,
}

impl Func {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "len" | "length" => Func::Len,
            "lower" => Func::Lower,
            "upper" => Func::Upper,
            "trim" => Func::Trim,
            "ltrim" => Func::Ltrim,
            "rtrim" => Func::Rtrim,
            "contains" => Func::Contains,
            "startswith" | "starts_with" => Func::StartsWith,
            "endswith" | "ends_with" => Func::EndsWith,
            "replace" => Func::Replace,
            "abs" => Func::Abs,
            "round" => Func::Round,
            "floor" => Func::Floor,
            "ceil" => Func::Ceil,
            "int" | "trunc" => Func::Int,
            "float" | "number" => Func::Float,
            "is_empty" | "empty" => Func::IsEmpty,
            "is_numeric" => Func::IsNumeric,
            "not" => Func::Not,
            "coalesce" => Func::Coalesce,
            "if" => Func::If,
            "concat" => Func::Concat,
            _ => return None,
        })
    }

    /// (least, most) arguments.
    fn arity(self) -> (usize, usize) {
        match self {
            Func::Contains | Func::StartsWith | Func::EndsWith => (2, 2),
            Func::Replace => (3, 3),
            Func::Round => (1, 2),
            Func::If => (2, 3),
            Func::Coalesce | Func::Concat => (1, usize::MAX),
            _ => (1, 1),
        }
    }
}

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

#[derive(Clone, Debug)]
pub enum Expr {
    Lit(Value),
    Col(usize),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(Bin, Box<Expr>, Box<Expr>),
    Call(Func, Vec<Expr>),
    /// `x =~ "re"`, `match(x, "re")`; negated for `!~`.
    Match(Box<Expr>, Regex, bool),
    /// An aggregation's result, by slot (groupby only).
    Agg(usize),
}

impl Expr {
    /// The value for `row`; `aggs`: the group's finished aggregations.
    pub fn eval(&self, row: &[String], aggs: &[Value]) -> Value {
        match self {
            Expr::Lit(v) => v.clone(),
            Expr::Col(i) => Value::Str(row.get(*i).cloned().unwrap_or_default()),
            Expr::Agg(slot) => aggs.get(*slot).cloned().unwrap_or(Value::Null),
            Expr::Neg(e) => e
                .eval(row, aggs)
                .num()
                .map_or(Value::Null, |n| Value::Num(-n)),
            Expr::Not(e) => Value::Bool(!e.eval(row, aggs).truthy()),
            Expr::Match(e, re, negate) => {
                Value::Bool(re.is_match(&e.eval(row, aggs).text()) != *negate)
            }
            Expr::Bin(Bin::Or, a, b) => {
                Value::Bool(a.eval(row, aggs).truthy() || b.eval(row, aggs).truthy())
            }
            Expr::Bin(Bin::And, a, b) => {
                Value::Bool(a.eval(row, aggs).truthy() && b.eval(row, aggs).truthy())
            }
            Expr::Bin(op, a, b) => binary(*op, a.eval(row, aggs), b.eval(row, aggs)),
            Expr::Call(f, args) => call(*f, args.iter().map(|a| a.eval(row, aggs)).collect()),
        }
    }
}

fn binary(op: Bin, a: Value, b: Value) -> Value {
    if let Bin::Cmp(o) = op {
        // Two numbers as numbers, two texts as text; a number and a text are
        // simply different (an "n/a" cell is not > 100, as text it would be).
        let ord = match (a.num(), b.num()) {
            (Some(x), Some(y)) => x.partial_cmp(&y),
            (None, None) => Some(a.text().cmp(&b.text())),
            _ => None,
        };
        return Value::Bool(ord.map_or(o == Ordering::Ne, |ord| o.holds(ord)));
    }
    if let Bin::StrCmp(o) = op {
        return Value::Bool(o.holds(a.text().cmp(&b.text())));
    }
    if op == Bin::Concat {
        return Value::Str(a.text() + &b.text());
    }
    // Arithmetic: a side that is not a number makes the result empty, so one
    // "n/a" cell drops out of a filter instead of failing the whole file.
    let (Some(x), Some(y)) = (a.num(), b.num()) else {
        return Value::Null;
    };
    let n = match op {
        Bin::Add => x + y,
        Bin::Sub => x - y,
        Bin::Mul => x * y,
        Bin::Div | Bin::IntDiv | Bin::Mod if y == 0.0 => return Value::Null,
        Bin::Div => x / y,
        Bin::IntDiv => (x / y).floor(),
        Bin::Mod => x.rem_euclid(y),
        Bin::Pow => x.powf(y),
        _ => unreachable!("handled above"),
    };
    Value::Num(n)
}

fn call(f: Func, args: Vec<Value>) -> Value {
    let text = |i: usize| args.get(i).map(Value::text).unwrap_or_default();
    let num = |i: usize| args.get(i).and_then(Value::num);
    let math = |g: fn(f64) -> f64| num(0).map_or(Value::Null, |n| Value::Num(g(n)));
    match f {
        Func::Len => Value::Num(text(0).chars().count() as f64),
        Func::Lower => Value::Str(text(0).to_lowercase()),
        Func::Upper => Value::Str(text(0).to_uppercase()),
        Func::Trim => Value::Str(text(0).trim().into()),
        Func::Ltrim => Value::Str(text(0).trim_start().into()),
        Func::Rtrim => Value::Str(text(0).trim_end().into()),
        Func::Contains => Value::Bool(text(0).contains(&text(1))),
        Func::StartsWith => Value::Bool(text(0).starts_with(&text(1))),
        Func::EndsWith => Value::Bool(text(0).ends_with(&text(1))),
        Func::Replace => Value::Str(text(0).replace(&text(1), &text(2))),
        Func::Abs => math(f64::abs),
        Func::Floor => math(f64::floor),
        Func::Ceil => math(f64::ceil),
        Func::Int => math(f64::trunc),
        Func::Float => math(|n| n),
        Func::Round => {
            let digits = num(1).unwrap_or(0.0).clamp(0.0, 15.0) as i32;
            let scale = 10f64.powi(digits);
            num(0).map_or(Value::Null, |n| Value::Num((n * scale).round() / scale))
        }
        Func::IsEmpty => Value::Bool(args[0].is_empty()),
        Func::IsNumeric => Value::Bool(!args[0].is_empty() && args[0].num().is_some()),
        Func::Not => Value::Bool(!args[0].truthy()),
        Func::Coalesce => args
            .into_iter()
            .find(|v| !v.is_empty())
            .unwrap_or(Value::Null),
        Func::If => {
            let mut it = args.into_iter();
            let cond = it.next().is_some_and(|c| c.truthy());
            let (yes, no) = (it.next(), it.next());
            (if cond { yes } else { no }).unwrap_or(Value::Null)
        }
        Func::Concat => Value::Str(args.iter().map(Value::text).collect()),
    }
}

// ---- tokens --------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    End,
}

/// The operators, longest first so `**` is not read as `*` `*`.
const OPS: &[&str] = &[
    "||", "&&", "==", "!=", "<=", ">=", "=~", "!~", "++", "**", "//", "<", ">", "=", "+", "-", "*",
    "/", "%", "!",
];

/// Each token and the byte where it starts.
fn tokenize(src: &str) -> Result<Vec<(Tok, usize)>, String> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = src[i..].chars().next().expect("in bounds");
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let start = i;
        let tok = if c == '(' {
            i += 1;
            Tok::LParen
        } else if c == ')' {
            i += 1;
            Tok::RParen
        } else if c == ',' {
            i += 1;
            Tok::Comma
        } else if c == '"' || c == '\'' {
            let (s, end) = read_string(src, i)?;
            i = end;
            Tok::Str(s)
        } else if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let end = number_end(b, i);
            let n = src[i..end]
                .parse()
                .map_err(|_| format!("not a number: {}", &src[i..end]))?;
            i = end;
            Tok::Num(n)
        } else if c.is_alphabetic() || c == '_' {
            let end = src[i..]
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .map_or(src.len(), |n| i + n);
            let word = &src[i..end];
            i = end;
            Tok::Ident(word.into())
        } else if let Some(op) = OPS.iter().find(|op| src[i..].starts_with(**op)) {
            i += op.len();
            Tok::Op(op)
        } else {
            return Err(format!("unexpected `{c}` at position {i}"));
        };
        out.push((tok, start));
    }
    out.push((Tok::End, src.len()));
    Ok(out)
}

fn number_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            i = j;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    i
}

fn read_string(src: &str, start: usize) -> Result<(String, usize), String> {
    let quote = src.as_bytes()[start] as char;
    let mut out = String::new();
    let mut chars = src[start + 1..].char_indices();
    while let Some((n, c)) = chars.next() {
        match c {
            _ if c == quote => return Ok((out, start + 1 + n + 1)),
            '\\' => match chars.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 't')) => out.push('\t'),
                Some((_, 'r')) => out.push('\r'),
                Some((_, other)) => out.push(other),
                None => break,
            },
            _ => out.push(c),
        }
    }
    Err(format!(
        "a string that starts at position {start} never ends"
    ))
}

// ---- parser --------------------------------------------------------------

/// One aggregation a groupby computes per group.
pub struct AggSlot {
    pub func: AggFn,
    /// None: `count()`.
    pub arg: Option<Expr>,
}

struct Parser<'a> {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    headers: &'a [String],
    /// Some: a groupby's list, where the aggregations live.
    aggs: Option<Vec<AggSlot>>,
    in_agg: bool,
}

/// A filter's expression, columns bound to `headers`.
pub fn compile(src: &str, headers: &[String]) -> Result<Expr, String> {
    let mut p = Parser::new(src, headers, false)?;
    let e = p.expr(0)?;
    p.expect_end()?;
    Ok(e)
}

/// A compiled groupby list: the aggregations to run per group, and each output
/// column (its name, an expression over their results).
pub type Aggs = (Vec<AggSlot>, Vec<(String, Expr)>);

/// A groupby's list: `sum(amount) as total, count(), mean(price)`. Each item
/// is an expression over aggregations, named by `as` or by its own text.
pub fn compile_aggs(src: &str, headers: &[String]) -> Result<Aggs, String> {
    let mut p = Parser::new(src, headers, true)?;
    let mut items = Vec::new();
    loop {
        let start = p.at();
        let before = p.aggs.as_ref().map_or(0, Vec::len);
        let e = p.expr(0)?;
        let end = p.at();
        if p.aggs.as_ref().map_or(0, Vec::len) == before {
            return Err(format!(
                "`{}` aggregates nothing: each item needs an aggregation such as sum(x), mean(x) or count()",
                src[start..end].trim()
            ));
        }
        let name = if p.peek() == &Tok::Ident("as".into()) {
            p.pos += 1;
            match p.next() {
                Tok::Ident(n) | Tok::Str(n) => n,
                t => return Err(format!("`as` needs a name, got {}", describe(&t))),
            }
        } else {
            src[start..end].trim().to_string()
        };
        items.push((name, e));
        match p.next() {
            Tok::Comma => continue,
            Tok::End => break,
            t => {
                return Err(format!(
                    "expected `,` or the end after `{}`, got {}",
                    src[start..end].trim(),
                    describe(&t)
                ));
            }
        }
    }
    Ok((p.aggs.take().unwrap_or_default(), items))
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Num(n) => format!("the number {}", fmt_num(*n)),
        Tok::Str(s) => format!("the string \"{s}\""),
        Tok::Ident(s) => format!("`{s}`"),
        Tok::Op(o) => format!("`{o}`"),
        Tok::LParen => "`(`".into(),
        Tok::RParen => "`)`".into(),
        Tok::Comma => "`,`".into(),
        Tok::End => "the end".into(),
    }
}

/// Binding power of an infix operator, and what it builds.
fn infix(t: &Tok) -> Option<(u8, Bin)> {
    use Ordering::*;
    Some(match t {
        Tok::Op("||") => (1, Bin::Or),
        Tok::Ident(w) if w == "or" => (1, Bin::Or),
        Tok::Op("&&") => (2, Bin::And),
        Tok::Ident(w) if w == "and" => (2, Bin::And),
        Tok::Op("==" | "=") => (3, Bin::Cmp(Eq)),
        Tok::Op("!=") => (3, Bin::Cmp(Ne)),
        Tok::Op("<") => (3, Bin::Cmp(Lt)),
        Tok::Op("<=") => (3, Bin::Cmp(Le)),
        Tok::Op(">") => (3, Bin::Cmp(Gt)),
        Tok::Op(">=") => (3, Bin::Cmp(Ge)),
        Tok::Ident(w) => match w.as_str() {
            "eq" => (3, Bin::StrCmp(Eq)),
            "ne" => (3, Bin::StrCmp(Ne)),
            "lt" => (3, Bin::StrCmp(Lt)),
            "le" => (3, Bin::StrCmp(Le)),
            "gt" => (3, Bin::StrCmp(Gt)),
            "ge" => (3, Bin::StrCmp(Ge)),
            _ => return None,
        },
        Tok::Op("+") => (4, Bin::Add),
        Tok::Op("-") => (4, Bin::Sub),
        Tok::Op("++") => (4, Bin::Concat),
        Tok::Op("*") => (5, Bin::Mul),
        Tok::Op("/") => (5, Bin::Div),
        Tok::Op("//") => (5, Bin::IntDiv),
        Tok::Op("%") => (5, Bin::Mod),
        Tok::Op("**") => (7, Bin::Pow),
        _ => return None,
    })
}

const UNARY: u8 = 6;

impl<'a> Parser<'a> {
    fn new(src: &'a str, headers: &'a [String], aggs: bool) -> Result<Self, String> {
        Ok(Self {
            toks: tokenize(src)?,
            pos: 0,
            headers,
            aggs: aggs.then(Vec::new),
            in_agg: false,
        })
    }

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].0
    }

    fn at(&self) -> usize {
        self.toks[self.pos].1
    }

    fn next(&mut self) -> Tok {
        let t = self.toks[self.pos].0.clone();
        if t != Tok::End {
            self.pos += 1;
        }
        t
    }

    fn expect_end(&mut self) -> Result<(), String> {
        match self.next() {
            Tok::End => Ok(()),
            t => Err(format!(
                "unexpected {} at position {}",
                describe(&t),
                self.toks[self.pos.saturating_sub(1)].1
            )),
        }
    }

    fn expr(&mut self, min: u8) -> Result<Expr, String> {
        let mut left = self.prefix()?;
        loop {
            // `x =~ "re"` / `x !~ "re"`: the pattern is compiled once.
            if let Tok::Op(op @ ("=~" | "!~")) = self.peek() {
                if 3 < min {
                    break;
                }
                let negate = *op == "!~";
                self.pos += 1;
                let re = self.pattern()?;
                left = Expr::Match(Box::new(left), re, negate);
                continue;
            }
            let Some((bp, op)) = infix(self.peek()) else {
                break;
            };
            if bp < min {
                break;
            }
            self.pos += 1;
            // `**` binds to the right; everything else to the left.
            let right = self.expr(if op == Bin::Pow { bp } else { bp + 1 })?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn pattern(&mut self) -> Result<Regex, String> {
        match self.next() {
            Tok::Str(p) => Regex::new(&p).map_err(|e| format!("bad pattern \"{p}\": {e}")),
            t => Err(format!(
                "a pattern is a string literal (\"^a.*\"), got {}",
                describe(&t)
            )),
        }
    }

    fn prefix(&mut self) -> Result<Expr, String> {
        match self.next() {
            Tok::Num(n) => Ok(Expr::Lit(Value::Num(n))),
            Tok::Str(s) => Ok(Expr::Lit(Value::Str(s))),
            Tok::Op("-") => Ok(Expr::Neg(Box::new(self.expr(UNARY)?))),
            Tok::Op("!") => Ok(Expr::Not(Box::new(self.expr(UNARY)?))),
            Tok::LParen => {
                let e = self.expr(0)?;
                match self.next() {
                    Tok::RParen => Ok(e),
                    t => Err(format!("expected `)`, got {}", describe(&t))),
                }
            }
            Tok::Ident(w) if self.peek() == &Tok::LParen => {
                self.pos += 1;
                self.call(&w)
            }
            Tok::Ident(w) => match w.as_str() {
                "true" => Ok(Expr::Lit(Value::Bool(true))),
                "false" => Ok(Expr::Lit(Value::Bool(false))),
                "null" => Ok(Expr::Lit(Value::Null)),
                "not" => Ok(Expr::Not(Box::new(self.expr(UNARY)?))),
                _ => self.column(&w),
            },
            t => Err(format!("expected a value, got {}", describe(&t))),
        }
    }

    fn column(&self, name: &str) -> Result<Expr, String> {
        if self.aggs.is_some() && !self.in_agg {
            return Err(format!(
                "`{name}` is outside an aggregation: group by it, or aggregate it (first({name}))"
            ));
        }
        resolve_name(self.headers, name)
            .map(Expr::Col)
            .ok_or_else(|| {
                format!(
                    "no column `{name}`; the columns are {}",
                    column_list(self.headers)
                )
            })
    }

    /// The arguments up to `)`.
    fn args(&mut self) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        if self.peek() == &Tok::RParen {
            self.pos += 1;
            return Ok(args);
        }
        loop {
            args.push(self.expr(0)?);
            match self.next() {
                Tok::Comma => continue,
                Tok::RParen => return Ok(args),
                t => {
                    return Err(format!(
                        "expected `,` or `)` in a call, got {}",
                        describe(&t)
                    ));
                }
            }
        }
    }

    fn call(&mut self, name: &str) -> Result<Expr, String> {
        if name == "col" {
            let arg = self.args()?;
            let i = match arg.as_slice() {
                [Expr::Lit(Value::Str(n))] => resolve_name(self.headers, n),
                [Expr::Lit(Value::Num(n))] if n.fract() == 0.0 && *n >= 0.0 => {
                    Some(*n as usize).filter(|i| *i < self.headers.len())
                }
                _ => return Err("col() takes one column name (a string) or index".into()),
            };
            if self.aggs.is_some() && !self.in_agg {
                return Err("a column is outside an aggregation: group by it, or aggregate it (first(col(...)))".into());
            }
            return i.map(Expr::Col).ok_or_else(|| {
                format!(
                    "no such column in col(); the columns are {}",
                    column_list(self.headers)
                )
            });
        }
        if name == "match" {
            let subject = self.expr(0)?;
            if self.next() != Tok::Comma {
                return Err("match(x, \"pattern\") takes two arguments".into());
            }
            let re = self.pattern()?;
            if self.next() != Tok::RParen {
                return Err("match(x, \"pattern\") takes two arguments".into());
            }
            return Ok(Expr::Match(Box::new(subject), re, false));
        }
        if self.aggs.is_some()
            && let Some(func) = AggFn::named(name)
        {
            if self.in_agg {
                return Err(format!("{name}() inside another aggregation"));
            }
            self.in_agg = true;
            let args = self.args();
            self.in_agg = false;
            let mut args = args?;
            let arg = match (func, args.len()) {
                (AggFn::Count, 0) => None,
                (_, 1) => args.pop(),
                _ => {
                    return Err(format!(
                        "{name}() takes one argument{}",
                        if func == AggFn::Count { " or none" } else { "" }
                    ));
                }
            };
            let slots = self.aggs.as_mut().expect("in a groupby");
            slots.push(AggSlot { func, arg });
            return Ok(Expr::Agg(slots.len() - 1));
        }
        let Some(func) = Func::named(name) else {
            let hint = if AggFn::named(name).is_some() {
                " (an aggregation: use it in groupby or agg)"
            } else {
                ""
            };
            return Err(format!("unknown function {name}(){hint}"));
        };
        let args = self.args()?;
        let (least, most) = func.arity();
        if args.len() < least || args.len() > most {
            return Err(format!("{name}() takes {}", arity_text(least, most)));
        }
        Ok(Expr::Call(func, args))
    }
}

fn arity_text(least: usize, most: usize) -> String {
    match (least, most) {
        (l, m) if l == m => format!("{l} argument{}", if l == 1 { "" } else { "s" }),
        (l, usize::MAX) => format!("{l} or more arguments"),
        (l, m) => format!("{l} to {m} arguments"),
    }
}
