//! Slash commands typed at the prompt: haest's own.
//! Then the mods' commands (/plan is one), then skills: a line naming a skill
//! (`/review src/a.rs`) expands to that skill's body and goes to the model;
//! anything else starting with `/` is sent as typed.

use haest::{Effort, core::caps::ConversationCap, core::error::report, mods::CommandOutcome};
use tokio_util::sync::CancellationToken;

use crate::{changes, term::Runtime};

pub(crate) enum Outcome {
    /// Send this to the model as the human's turn.
    Send(String),
    /// Handled here; nothing goes to the model.
    Done,
    Exit,
}

const HELP: &str = "Commands: /model [provider:model], /effort [level|default], /budget [usd | tokens <n>], /compact, /clear, /skills, /changes and /apply (sandbox edits to disk), /<skill> [args], /exit";

/// Resolve one input line. Plain text passes through unchanged. `cancel`
/// stops a command that heeds it (`/compact`'s summary call).
pub(crate) async fn resolve(rt: &mut Runtime, line: &str, cancel: &CancellationToken) -> Outcome {
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
            let _ = rt.compact(cancel).await;
        }
        "clear" => clear(rt).await,
        "skills" => skills(rt).await,
        "changes" => changes::show(rt).await,
        "apply" => changes::apply(rt).await,
        "help" => help(rt),
        "exit" | "quit" => return Outcome::Exit,
        _ => match rt.command(name, args).await {
            CommandOutcome::Unknown => return Outcome::Send(line.to_string()),
            CommandOutcome::Prompt(text) => return Outcome::Send(text),
            // Handled, or an outcome newer than this host: the mod or the
            // skill had its say.
            _ => {}
        },
    }
    Outcome::Done
}

fn help(rt: &Runtime) {
    let mods: Vec<String> = rt
        .mods()
        .commands()
        .iter()
        .map(|c| format!("/{c}"))
        .collect();
    if mods.is_empty() {
        return rt.hooks().info(HELP);
    }
    rt.hooks()
        .info(&format!("{HELP}\nFrom mods: {}", mods.join(", ")));
}

fn model(rt: &mut Runtime, spec: &str) {
    if spec.is_empty() {
        return rt
            .hooks()
            .info(&format!("Model: {}", rt.agent().model_spec()));
    }
    match rt.set_model_spec(spec) {
        Ok(()) => rt.hooks().info(&format!("Model set to {spec}")),
        Err(e) => rt
            .hooks()
            .error(&format!("Model not changed: {}", report(&e))),
    }
}

fn effort(rt: &mut Runtime, arg: &str) {
    let label = |e: Option<Effort>| e.map_or("model default", Effort::as_str).to_string();
    if arg.is_empty() {
        return rt
            .hooks()
            .info(&format!("Effort: {}", label(rt.agent().effort())));
    }
    let effort = match arg {
        "default" => None,
        _ => match arg.parse::<Effort>() {
            Ok(e) => Some(e),
            Err(_) => {
                return rt.hooks().error(&format!(
                    "Unknown effort \"{arg}\". Use off, low, medium, high, xhigh, max or default."
                ));
            }
        },
    };
    rt.set_effort(effort);
    rt.hooks().info(&format!("Effort set to {}", label(effort)));
}

/// `/budget` shows the ledger; `/budget 40` sets the USD cap; `/budget tokens 4e7` the unpriced-token cap.
fn budget(rt: &mut Runtime, args: &str) {
    let Some(ledger) = rt.agent().deps().shared().ledger().cloned() else {
        return rt.hooks().info("This conversation has no ledger.");
    };
    if args.is_empty() {
        return rt.hooks().info(&ledger.summary());
    }
    let (tokens, raw) = match args.split_once(char::is_whitespace) {
        Some((k, v)) if k.eq_ignore_ascii_case("tokens") => (true, v.trim()),
        _ => (false, args),
    };
    let value = raw
        .trim_start_matches('$')
        .parse::<f64>()
        .ok()
        // Tokens are whole: a fraction is refused, never cut to a smaller
        // cap (0.5 cut to 0 would remove it).
        .filter(|v| v.is_finite() && *v >= 0.0 && (!tokens || v.fract() == 0.0));
    let Some(value) = value else {
        return rt.hooks().error(&format!(
            "\"{args}\" is not a budget. Usage: /budget (show), /budget <usd>, /budget tokens <n>. 0 removes the cap."
        ));
    };
    let cap = if tokens {
        ConversationCap::UnpricedTokens(value as u64)
    } else {
        ConversationCap::Usd(value)
    };
    ledger.set_cap(cap);
    let state = ledger
        .exhausted()
        .unwrap_or_else(|| "haest can continue.".into());
    let text = format!("{} {state}", ledger.summary());
    rt.hooks().info(&text);
}

async fn clear(rt: &mut Runtime) {
    match rt.clear().await {
        Ok(()) => rt
            .hooks()
            .info(&format!("Started a fresh session ({}).", rt.session_id())),
        Err(e) => rt.hooks().error(&format!("Not cleared: {}", report(&e))),
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
        rt.hooks().info("No skills found.");
    } else {
        rt.hooks().info(&format!("Skills:\n{}", lines.join("\n")));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use haest::{
        core::fs::MemFs,
        runtime::{BuildOptions, RuntimeInputs, World, build},
        session::{Body, Hub, SessionPrompter},
        settings::Settings,
        wire::{Level, Outbound},
    };

    use super::*;
    use crate::term::TermUi;

    /// A conversation over files of its own, its lines said into `hub`.
    async fn runtime(hub: &Hub) -> Runtime {
        let inputs = RuntimeInputs::new(Settings::bare("/w", "/h"), World::files(MemFs::new()))
            .with_keys(|k: &str| (k == "OPENROUTER_API_KEY").then(|| "k".to_string()));
        let ui = Arc::new(TermUi::with_hub(false, hub.clone()));
        let asks = SessionPrompter::new(hub.clone());
        build(inputs, ui, asks, BuildOptions::default())
            .await
            .unwrap()
    }

    /// The errors said into `hub` after frame `after`.
    async fn errors_since(hub: &Hub, after: u64) -> Vec<String> {
        let mut sub = hub.subscribe(after);
        let mut said = Vec::new();
        while !sub.caught_up() {
            let Some(frame) = sub.recv().await else { break };
            if let Body::Wire(Outbound::Notice {
                level: Level::Error,
                text,
            }) = &frame.body
            {
                said.push(text.clone());
            }
        }
        said
    }

    /// A token cap counts whole tokens: a fraction is refused, never cut to a
    /// smaller cap. `0.5` cut to 0 removed the cap the human asked to tighten.
    #[tokio::test]
    async fn budget_tokens_refuses_a_fraction_and_keeps_the_cap() {
        let hub = Hub::new("t");
        let mut rt = runtime(&hub).await;
        let ledger = rt.agent().deps().shared().ledger().cloned().unwrap();
        let cancel = CancellationToken::new();
        let before = ledger.caps();
        for fraction in ["0.5", "1.5"] {
            let after = hub.last_seq();
            resolve(&mut rt, &format!("/budget tokens {fraction}"), &cancel).await;
            assert_eq!(ledger.caps(), before, "/budget tokens {fraction}");
            assert!(
                !errors_since(&hub, after).await.is_empty(),
                "/budget tokens {fraction} said no error"
            );
        }

        // A whole number, in the doc's own spelling, is still a cap.
        resolve(&mut rt, "/budget tokens 4e7", &cancel).await;
        assert_eq!(ledger.caps().max_unpriced_tokens, 40_000_000);
    }
}
