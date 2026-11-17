//! Slash commands typed at the prompt -- the TS haest host's set, plus /exit.
//! Then the mods' commands (/plan is one), then skills: a line naming a skill
//! (`/review src/a.rs`) expands to that skill's body and goes to the model;
//! anything else starting with `/` is sent as typed.

use haest::{
    Effort, core::caps::ConversationCap, core::error::report, core::skills::load_skill_body,
    mods::CommandOutcome,
};
use tokio_util::sync::CancellationToken;

use crate::{changes, term::Runtime};

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
            // A failure was said to the UI; /compact has nothing more to do.
            let _ = rt.compact(&CancellationToken::new()).await;
        }
        "clear" => clear(rt).await,
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
        return rt.hooks.info(HELP);
    }
    rt.hooks
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
            rt.hooks
                .error(&format!("Skill {name} could not be read: {e}"));
            Outcome::Done
        }
    }
}

fn model(rt: &mut Runtime, spec: &str) {
    if spec.is_empty() {
        return rt
            .hooks
            .info(&format!("Model: {}", rt.agent().model_spec()));
    }
    match rt.set_model_spec(spec) {
        Ok(()) => rt.hooks.info(&format!("Model set to {spec}")),
        Err(e) => rt
            .hooks
            .error(&format!("Model not changed: {}", report(&e))),
    }
}

fn effort(rt: &mut Runtime, arg: &str) {
    let label = |e: Option<Effort>| e.map_or("model default", Effort::as_str).to_string();
    if arg.is_empty() {
        return rt
            .hooks
            .info(&format!("Effort: {}", label(rt.agent().effort())));
    }
    let effort = match arg {
        "default" => None,
        _ => match arg.parse::<Effort>() {
            Ok(e) => Some(e),
            Err(_) => {
                return rt.hooks.error(&format!(
                    "Unknown effort \"{arg}\". Use off, low, medium, high, xhigh, max or default."
                ));
            }
        },
    };
    rt.set_effort(effort);
    rt.hooks.info(&format!("Effort set to {}", label(effort)));
}

/// `/budget` shows the ledger; `/budget 40` sets the USD cap; `/budget tokens 4e7` the unpriced-token cap.
fn budget(rt: &mut Runtime, args: &str) {
    let Some(ledger) = rt.agent().deps().shared().ledger().cloned() else {
        return rt.hooks.info("This conversation has no ledger.");
    };
    if args.is_empty() {
        return rt.hooks.info(&ledger.lock().unwrap().summary());
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
        return rt.hooks.error(&format!(
            "\"{args}\" is not a budget. Usage: /budget (show), /budget <usd>, /budget tokens <n>. 0 removes the cap."
        ));
    };
    let cap = if tokens {
        ConversationCap::UnpricedTokens(value as u64)
    } else {
        ConversationCap::Usd(value)
    };
    let mut l = ledger.lock().unwrap();
    l.set_cap(cap);
    let state = l
        .exhausted()
        .unwrap_or_else(|| "haest can continue.".into());
    let text = format!("{} {state}", l.summary());
    drop(l);
    rt.hooks.info(&text);
}

async fn clear(rt: &mut Runtime) {
    match rt.clear().await {
        Ok(()) => rt
            .hooks
            .info(&format!("Started a fresh session ({}).", rt.session_id())),
        Err(e) => rt.hooks.error(&format!("Not cleared: {}", report(&e))),
    }
}

async fn skills(rt: &mut Runtime) {
    let found = rt.rescan_skills().await;
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
    if lines.is_empty() {
        rt.hooks.info("No skills found.");
    } else {
        rt.hooks.info(&format!("Skills:\n{}", lines.join("\n")));
    }
}
