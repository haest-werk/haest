//! xan's expressions: the part of moonblade (xan's own language) a filter and a
//! groupby need. Columns by name (or `col("Name with spaces")`, `col(2)`),
//! numbers, strings, arithmetic, comparisons, `and`/`or`/`not`, a handful of
//! functions, and in a groupby the aggregations (`sum(amount) as total`).
//!
//! Columns bind to indices when the expression is compiled against the
//! headers, so a misspelt column fails before the first row, with the list of
//! the real ones.
//!
//! A value (`value.rs`), the aggregations (`agg.rs`), the tokens (`lex.rs`)
//! and the parser (`parse.rs`); here, what an expression is and computes.

mod agg;
mod lex;
mod parse;
mod value;

use regex::Regex;

pub use agg::AggFn;
pub use parse::{compile, compile_aggs};
pub use value::{Value, parse_num};

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
