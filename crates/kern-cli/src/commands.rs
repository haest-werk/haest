//! Slash commands typed at the prompt -- the TS kern host's set, plus /exit.
//! Then the mods' commands (/plan is one), then skills: a line naming a skill
//! (`/review src/a.rs`) expands to that skill's body and goes to the model;
//! anything else starting with `/` is sent as typed.

use std::sync::Arc;

use kern::{
    Effort,
    ledger::LedgerCap,
    mods::CommandOutcome,
    session::SessionRecord,
    skills::{discover_skills, load_skill_body},
};
use tokio_util::sync::CancellationToken;

use crate::{
    changes,
    host::now_ms,
    session::new_session_id,
    setup::{Runtime, init_store, open_store},
};

pub enum Outcome {
    /// Send this to the model as the human's turn.
    Send(String),
    /// Handled here; nothing goes to the model.
    Done,
    Exit,
}

const HELP: &str = "Commands: /model [provider:model], /effort [level|default], /budget [usd | tokens <n>], /compact, /clear, /skills, /changes and /apply (sandbox edits to disk), /<skill> [args], /exit";

/// Resolve one input line. Plain text passes through unchanged.
pub async fn resolve(rt: &mut Runtime, line: &str) -> Outcome {
    let Some(rest) = line.trim().strip_prefix('/') else {
        return Outcome::Send(line.to_string());
    };
    let (name, args) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(n, a)| (n, a.trim()));
    match name {
        "model" => model(rt, args),
        "effort" => effort(rt, args),
        "budget" => budget(rt, args),
        "compact" => {
            let h = rt.host.clone();
            h.compact_now(&mut rt.agent, &CancellationToken::new(), false)
                .await;
        }
        "clear" => clear(rt),
        "skills" => skills(rt).await,
        "changes" => changes::show(rt).await,
        "apply" => changes::apply(rt).await,
        "help" => help(rt),
        "exit" | "quit" => return Outcome::Exit,
        _ => match rt.mods.command(name, args).await {
            CommandOutcome::Unknown => return skill_or_text(rt, name, args, line).await,
            CommandOutcome::Prompt(text) => return Outcome::Send(text),
            CommandOutcome::Handled => {}
        },
    }
    Outcome::Done
}

fn help(rt: &Runtime) {
    let mods: Vec<String> = rt.mods.commands().iter().map(|c| format!("/{c}")).collect();
    if mods.is_empty() {
        return rt.host.info(HELP);
    }
    rt.host
        .info(&format!("{HELP}\nFrom mods: {}", mods.join(", ")));
}

async fn skill_or_text(rt: &Runtime, name: &str, args: &str, line: &str) -> Outcome {
    let skill = rt
        .skills
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.name == name)
        .cloned();
    let Some(skill) = skill else {
        return Outcome::Send(line.to_string());
    };
    match load_skill_body(rt.fs.as_ref(), &skill, Some(args)).await {
        Ok(body) => Outcome::Send(body),
        Err(e) => {
            rt.host
                .error(&format!("Skill {name} could not be read: {e}"));
            Outcome::Done
        }
    }
}

fn model(rt: &mut Runtime, spec: &str) {
    if spec.is_empty() {
        return rt.host.info(&format!("Model: {}", rt.agent.model_spec));
    }
    if let Err(e) = rt.agent.deps().models.resolve(spec) {
        return rt.host.error(&format!("Model not changed: {e}"));
    }
    rt.agent.model_spec = spec.to_string();
    rt.host.set_model(spec);
    rt.host.append(SessionRecord::Config {
        t: now_ms(),
        model: Some(spec.to_string()),
        effort: None,
    });
    rt.host.info(&format!("Model set to {spec}"));
}

fn effort(rt: &mut Runtime, arg: &str) {
    let label = |e: Option<Effort>| e.map_or("model default", Effort::as_str).to_string();
    if arg.is_empty() {
        return rt.host.info(&format!("Effort: {}", label(rt.agent.effort)));
    }
    let effort = match arg {
        "default" => None,
        _ => match arg.parse::<Effort>() {
            Ok(e) => Some(e),
            Err(_) => {
                return rt.host.error(&format!(
                    "Unknown effort \"{arg}\". Use off, low, medium, high, xhigh, max or default."
                ));
            }
        },
    };
    rt.agent.effort = effort;
    rt.host.set_effort(effort);
    rt.host.append(SessionRecord::Config {
        t: now_ms(),
        model: None,
        effort: Some(effort),
    });
    rt.host.info(&format!("Effort set to {}", label(effort)));
}

/// `/budget` shows the ledger; `/budget 40` sets the USD cap; `/budget tokens 4e7` the unpriced-token cap.
fn budget(rt: &mut Runtime, args: &str) {
    let Some(ledger) = rt.agent.deps().ledger.clone() else {
        return rt.host.info("This conversation has no ledger.");
    };
    if args.is_empty() {
        return rt.host.info(&ledger.lock().unwrap().summary());
    }
    let (tokens, raw) = match args.split_once(char::is_whitespace) {
        Some((k, v)) if k.eq_ignore_ascii_case("tokens") => (true, v.trim()),
        _ => (false, args),
    };
    let value = raw
        .trim_start_matches('$')
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0);
    let Some(value) = value else {
        return rt.host.error(&format!(
            "\"{args}\" is not a budget. Usage: /budget (show), /budget <usd>, /budget tokens <n>. 0 removes the cap."
        ));
    };
    let cap = if tokens {
        LedgerCap::UnpricedTokens(value as u64)
    } else {
        LedgerCap::Usd(value)
    };
    let mut l = ledger.lock().unwrap();
    l.set_cap(cap);
    let state = l.exhausted().unwrap_or_else(|| "kern can continue.".into());
    let text = format!("{} {state}", l.summary());
    drop(l);
    rt.host.info(&text);
}

/// Fresh session: new id, new file, empty history and task list; the ledger carries over.
fn clear(rt: &mut Runtime) {
    let id = new_session_id();
    match open_store(&rt.fs, &rt.host.cfg.data_dir, &id) {
        Ok(store) => rt.host.swap_store(Arc::new(store)),
        Err(e) => return rt.host.error(&format!("Not cleared: {e}")),
    }
    rt.session_id = id.clone();
    rt.agent.replace_history(Vec::new());
    init_store(&rt.host, &rt.agent, &id);
    rt.board.restore(Vec::new());
    rt.host.info(&format!("Started a fresh session ({id})."));
}

async fn skills(rt: &mut Runtime) {
    let found = discover_skills(rt.fs.as_ref(), &rt.skill_opts).await;
    let lines: Vec<String> = found
        .iter()
        .map(|s| {
            let only = if s.model_invocable {
                ""
            } else {
                " (user only)"
            };
            format!("- /{}{only}: {}", s.name, s.description)
        })
        .collect();
    *rt.skills.lock().unwrap() = found;
    if lines.is_empty() {
        rt.host.info("No skills found.");
    } else {
        rt.host.info(&format!("Skills:\n{}", lines.join("\n")));
    }
}
