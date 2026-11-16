//! The parser's leaves that name something: a column, and a call (`col()`,
//! `match()`, an aggregation inside a groupby list, a function).

use super::{AggSlot, Parsed, Parser, deeper};
use crate::sandbox::xan::{
    expr::{AggFn, Expr, Func, Value, lex::Tok, lex::describe},
    select::{column_list, resolve_name},
};

impl Parser<'_> {
    pub(super) fn column(&self, name: &str) -> Result<Expr, String> {
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

    pub(super) fn call(&mut self, name: &str) -> Parsed {
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
