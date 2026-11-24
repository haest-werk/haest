//! `xan`: CSV verbs in the sandbox shell, after medialab's xan (models know
//! it): headers, count, head, select, filter, sort, groupby, agg, stats, frequency.
//! The command line is `cli.rs`, reading and writing CSV `table.rs`, the verbs
//! that compute `verbs.rs`; the expressions are the part of moonblade those
//! need (`expr/`); the selections are xan's (`select.rs`). Every sandbox has it
//! (a host's own `xan` replaces it, [`super::SandboxWorkspace::with_command`]).
//!
//! It reads a file through haest's [`Fs`](haest_core::fs::Fs) or the pipe, and
//! checks the shell call's deadline as it goes: a CPU-bound command never
//! yields to the shell's timer.

mod cli;
mod expr;
mod select;
mod table;
#[cfg(test)]
mod tests;
mod verbs;

use std::time::Instant;

use async_trait::async_trait;
use haest_core::{
    shell_command::{ShellCall, ShellCommand},
    workspace::ExecResult,
};
use tokio_util::sync::CancellationToken;

use cli::{HELP, VERBS, parse_args, verb};
use expr::compile;
use select::select;
use table::{cell, delimiter, input, read_table, write_csv};
use verbs::{aggregate, compare, frequency, stats};

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

/// The shell call's deadline and interrupt, checked every 1024 units of work
/// (a row parsed, filtered or written; a column's rows; a group's values).
/// Every loop over the input counts here, so none runs on past the deadline.
struct Clock {
    ends_at: Instant,
    cancel: CancellationToken,
    n: u64,
}

impl Clock {
    fn tick(&mut self) -> R<()> {
        self.tick_by(1)
    }

    /// `units` of work done at once; checked when the count passes a 1024.
    fn tick_by(&mut self, units: usize) -> R<()> {
        let before = self.n / 1024;
        self.n += units as u64;
        if self.n / 1024 != before && (self.cancel.is_cancelled() || Instant::now() >= self.ends_at)
        {
            return Err(fail("stopped: the shell call ran out of time"));
        }
        Ok(())
    }
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
            write_csv(header, t.rows.into_iter().take(n), &mut clock)
        }
        "select" => {
            let cols = select(&p.pos[0], &t.headers).map_err(fail)?;
            let pick = |r: &[String]| {
                cols.iter()
                    .map(|&i| cell(r, i).to_string())
                    .collect::<Vec<_>>()
            };
            let h = header.map(pick);
            write_csv(h.as_deref(), t.rows.iter().map(|r| pick(r)), &mut clock)
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
            write_csv(header, kept, &mut clock)
        }
        "sort" => {
            let cols = columns("select")?;
            let numeric = p.has("numeric");
            let mut rows = t.rows;
            // The sort itself cannot stop halfway; whether to start it can.
            clock.tick_by(rows.len())?;
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
            write_csv(header, rows, &mut clock)
        }
        "groupby" => {
            let keys = select(&p.pos[0], &t.headers).map_err(fail)?;
            aggregate(&t, &keys, &p.pos[1], &mut clock)
        }
        "agg" => aggregate(&t, &[], &p.pos[0], &mut clock),
        "stats" => stats(&t, &columns("select")?, &mut clock),
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
                &mut clock,
            )
        }
        _ => unreachable!("every verb in VERBS has an arm"),
    }
}
