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
