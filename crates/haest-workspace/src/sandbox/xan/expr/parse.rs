//! Compiling an expression (a filter's) or an aggregation list (a groupby's)
//! against the headers: precedence climbing over the tokens.

use regex::Regex;

use super::{
    AggFn, Bin, Expr, Func, Value,
    lex::{Tok, describe, infix, tokenize},
};
use crate::sandbox::xan::select::{column_list, resolve_name};

/// One aggregation a groupby computes per group.
pub struct AggSlot {
    pub func: AggFn,
    /// None: `count()`.
    pub arg: Option<Expr>,
}

// Parsing, [`Expr::eval`] and dropping an expression all recurse on the
// caller's stack (a tokio worker's, 2 MiB, in a host), so an expression past
// these is refused, never a stack overflow that aborts the process. Two caps,
// as the two recursions cost: a level of the parser takes 5-8 KB of stack in a
// debug build, a level of the tree about 1 KB.

/// How deep the parser may recurse: each `(`, `!`, `-`, `not`, call and
/// operand of `**` nests one level.
pub const MAX_NESTING: usize = 128;

/// How deep the tree may be: those, and one level per operator of a chain
/// (`a or b or c` is two deep, with no recursion in the parser). A list of 500
/// `or`s fits.
pub const MAX_DEPTH: usize = 512;

fn too_deep(cap: usize) -> String {
    format!(
        "the expression nests more than {cap} deep; split it up, or match a long list of values with x =~ \"^(a|b|c)$\""
    )
}

struct Parser<'a> {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    headers: &'a [String],
    /// Some: a groupby's list, where the aggregations live.
    aggs: Option<Vec<AggSlot>>,
    in_agg: bool,
    /// How many `expr` calls are open: the parser's own recursion.
    nesting: usize,
}

/// An expression and how deep its tree is (a leaf is 1).
type Parsed = Result<(Expr, usize), String>;

/// The depth of a node over children `below` deep, if it is not too deep.
fn deeper(below: usize) -> Result<usize, String> {
    if below >= MAX_DEPTH {
        return Err(too_deep(MAX_DEPTH));
    }
    Ok(below + 1)
}

/// A filter's expression, columns bound to `headers`.
pub fn compile(src: &str, headers: &[String]) -> Result<Expr, String> {
    let mut p = Parser::new(src, headers, false)?;
    let (e, _) = p.expr(0)?;
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
        let (e, _) = p.expr(0)?;
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

const UNARY: u8 = 6;

impl<'a> Parser<'a> {
    fn new(src: &'a str, headers: &'a [String], aggs: bool) -> Result<Self, String> {
        Ok(Self {
            toks: tokenize(src)?,
            pos: 0,
            headers,
            aggs: aggs.then(Vec::new),
            in_agg: false,
            nesting: 0,
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

    fn expr(&mut self, min: u8) -> Parsed {
        // Checked on the way in: `((((` never returns until the innermost does.
        if self.nesting >= MAX_NESTING {
            return Err(too_deep(MAX_NESTING));
        }
        self.nesting += 1;
        let e = self.climb(min);
        self.nesting -= 1;
        e
    }

    fn climb(&mut self, min: u8) -> Parsed {
        let (mut left, mut depth) = self.prefix()?;
        loop {
            // `x =~ "re"` / `x !~ "re"`: the pattern is compiled once.
            if let Tok::Op(op @ ("=~" | "!~")) = self.peek() {
                if 3 < min {
                    break;
                }
                let negate = *op == "!~";
                self.pos += 1;
                let re = self.pattern()?;
                depth = deeper(depth)?;
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
            let (right, below) = self.expr(if op == Bin::Pow { bp } else { bp + 1 })?;
            // A chain (`a or b or c`) builds a deeper tree with no recursion here.
            depth = deeper(depth.max(below))?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok((left, depth))
    }

    /// `wrap` over the operand of a prefix operator.
    fn unary(&mut self, wrap: fn(Box<Expr>) -> Expr) -> Parsed {
        let (e, below) = self.expr(UNARY)?;
        let depth = deeper(below)?;
        Ok((wrap(Box::new(e)), depth))
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

    fn prefix(&mut self) -> Parsed {
        let leaf = |e| Ok((e, 1));
        match self.next() {
            Tok::Num(n) => leaf(Expr::Lit(Value::Num(n))),
            Tok::Str(s) => leaf(Expr::Lit(Value::Str(s))),
            Tok::Op("-") => self.unary(Expr::Neg),
            Tok::Op("!") => self.unary(Expr::Not),
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
                "true" => leaf(Expr::Lit(Value::Bool(true))),
                "false" => leaf(Expr::Lit(Value::Bool(false))),
                "null" => leaf(Expr::Lit(Value::Null)),
                "not" => self.unary(Expr::Not),
                _ => leaf(self.column(&w)?),
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

    /// The arguments up to `)`, and the depth of the deepest (0: none).
    fn args(&mut self) -> Result<(Vec<Expr>, usize), String> {
        let mut args = Vec::new();
        let mut depth = 0;
        if self.peek() == &Tok::RParen {
            self.pos += 1;
            return Ok((args, depth));
        }
        loop {
            let (arg, below) = self.expr(0)?;
            args.push(arg);
            depth = depth.max(below);
            match self.next() {
                Tok::Comma => continue,
                Tok::RParen => return Ok((args, depth)),
                t => {
                    return Err(format!(
                        "expected `,` or `)` in a call, got {}",
                        describe(&t)
                    ));
                }
            }
        }
    }

    fn call(&mut self, name: &str) -> Parsed {
        if name == "col" {
            let (arg, _) = self.args()?;
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
            return i.map(|i| (Expr::Col(i), 1)).ok_or_else(|| {
                format!(
                    "no such column in col(); the columns are {}",
                    column_list(self.headers)
                )
            });
        }
        if name == "match" {
            let (subject, below) = self.expr(0)?;
            if self.next() != Tok::Comma {
                return Err("match(x, \"pattern\") takes two arguments".into());
            }
            let re = self.pattern()?;
            if self.next() != Tok::RParen {
                return Err("match(x, \"pattern\") takes two arguments".into());
            }
            let depth = deeper(below)?;
            return Ok((Expr::Match(Box::new(subject), re, false), depth));
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
            // Its argument is a tree of its own, in the slot; here it is a leaf.
            let (mut args, _) = args?;
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
            return Ok((Expr::Agg(slots.len() - 1), 1));
        }
        let Some(func) = Func::named(name) else {
            let hint = if AggFn::named(name).is_some() {
                " (an aggregation: use it in groupby or agg)"
            } else {
                ""
            };
            return Err(format!("unknown function {name}(){hint}"));
        };
        let (args, below) = self.args()?;
        let (least, most) = func.arity();
        if args.len() < least || args.len() > most {
            return Err(format!("{name}() takes {}", arity_text(least, most)));
        }
        let depth = deeper(below)?;
        Ok((Expr::Call(func, args), depth))
    }
}

fn arity_text(least: usize, most: usize) -> String {
    match (least, most) {
        (l, m) if l == m => format!("{l} argument{}", if l == 1 { "" } else { "s" }),
        (l, usize::MAX) => format!("{l} or more arguments"),
        (l, m) => format!("{l} to {m} arguments"),
    }
}
