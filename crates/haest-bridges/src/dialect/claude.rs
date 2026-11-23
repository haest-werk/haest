//! The claude dialect: Claude Code's hook JSON
//! (<https://code.claude.com/docs/en/hooks>), mapped onto haest's events as
//! PLAN.md's D42 says, so an existing Claude Code hook script runs as it
//! is. Where Claude Code can do more than haest's event (ask the human,
//! stop everything) haest fails closed or ignores it; `model.before` and
//! `model.after` have no Claude Code event, so a `Claude` hook does not
//! bind to them. Which events Claude Code has is haest-mods'
//! [`ModEvent::claude_code`]: a mapping here of one it names none for does
//! not build.

use std::path::Path;

use haest_core::{AssistantMessage, ChildEnv, tool::JsonMap};
use haest_mods::{
    Before, Event, ModEvent, PromptSubmitPayload, SystemPromptPayload, ToolAfterPayload,
    ToolBeforePayload, TurnEndDecision, TurnEndPayload,
};
use serde_json::Value;

use super::{Dialect, HookFailure, Reply, Vars, blocked};

/// Claude Code's dialect: a hook reads the JSON Claude Code hands its
/// hooks and answers in Claude Code's words, so its hook scripts run under
/// haest as they are.
///
/// | haest event | Claude Code event | What its answer does |
/// |---|---|---|
/// | `tool.before` | `PreToolUse` | `permissionDecision: "deny"` (or the older `decision: "block"`) refuses the call with `permissionDecisionReason` (`reason`); `updatedInput` replaces the whole input; `allow` and `defer` pass; `ask` refuses too, with the hook's reason: a hook cannot ask the human |
/// | `tool.after` | `PostToolUse` | `updatedToolOutput` replaces the output (an object as its JSON text); `additionalContext` and a `decision: "block"`'s `reason` are added after the output the model reads |
/// | `prompt.submit` | `UserPromptSubmit` | `additionalContext`, or plain text, is added after the prompt; `decision: "block"` is a failure, since haest's event cannot refuse a prompt |
/// | `system.prompt` | `SessionStart` | `additionalContext`, or plain text, is added to the system prompt |
/// | `turn.end` | `Stop` | `decision: "block"` with its `reason`, or `additionalContext`, keeps the turn going with that text |
///
/// These five are the events a `ShellHook<Claude>` or an `HttpHook<Claude>`
/// hooks, as [`ModEvent::claude_code`] lists them; on `model.before` or
/// `model.after` one does not compile.
///
/// Its input has the fields Claude Code sends that haest has:
/// `hook_event_name`, `cwd` (the conversation's directory), `session_id`
/// and `transcript_path` when the host names the session (the payload's
/// [`SessionView`](haest_mods::SessionView), which the conversation's
/// `ModEnv::set_session` names as of each event: after the human's /clear,
/// the new one; [`Hooks`](haest_mods::Hooks) sets it, and a host that calls
/// `Hook::run` itself sets the payload's `session` first, from
/// `haest.session()`), and per event `tool_name`, `tool_input`, `tool_use_id`,
/// `tool_response` (the output's text, as Claude Code sends a failed
/// call's), `prompt`, and Stop's `stop_hook_active` (a `turn.end` answer
/// already kept this turn going) and `last_assistant_message` (the answer's
/// text). A command also gets `CLAUDE_PROJECT_DIR`, the conversation's
/// directory. `transcript_path` names the host's own transcript (haest's
/// session file), which is not Claude Code's format: a script that parses
/// one reads the other wrong.
///
/// Exit 2 is the event's blocking answer, its stderr the text: it refuses
/// a `tool.before` call and keeps a `turn.end` going; on the other events it
/// is a failure. Output that starts with `{` must be a JSON object; other
/// text is plain text, which only `prompt.submit` and `system.prompt` take
/// (the others ignore it, as Claude Code does). `continue: false`,
/// `stopReason`, `systemMessage` and `suppressOutput` have no haest
/// decision and are ignored. Matchers are haest's (an exact name or a
/// `prefix*`), never Claude Code's regular expressions.
///
/// Like [`Haest`](crate::Haest) it holds nothing: every fact a hook's input
/// has comes with the event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Claude;

/// The fields every event's input has: Claude Code's name for `E`, the
/// conversation's directory, the session `payload` names.
fn common<E: Event>(payload: &E, dir: &Path) -> JsonMap {
    let hook_event_name = const { claude_code(E::NAME) };
    let mut input = JsonMap::new();
    if let Some(session) = payload.session() {
        input.insert("session_id".into(), session.id.as_str().into());
        if let Some(path) = &session.transcript_path {
            input.insert("transcript_path".into(), path_text(path).into());
        }
    }
    input.insert("cwd".into(), path_text(dir).into());
    input.insert("hook_event_name".into(), hook_event_name.into());
    input
}

/// Claude Code's name for `event`, when the build evaluates it: a mapping
/// of an event haest-mods names none for fails to build here.
const fn claude_code(event: ModEvent) -> &'static str {
    match event.claude_code() {
        Some(name) => name,
        None => panic!("the claude dialect maps an event Claude Code does not have"),
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl Vars for Claude {
    fn env(&self, dir: &Path) -> ChildEnv {
        ChildEnv::new([("CLAUDE_PROJECT_DIR", path_text(dir))])
    }
}

/// What a Claude Code hook printed.
enum Said {
    Nothing,
    /// Plain text: what `UserPromptSubmit` and `SessionStart` take as context.
    Text(String),
    /// A JSON object: its top-level fields, and its `hookSpecificOutput`.
    Json(Answer),
}

/// A top-level `decision: "block"`, and its `reason` when it gave one.
struct Block {
    reason: Option<String>,
}

/// A JSON answer, read field by field for `event`.
struct Answer {
    event: ModEvent,
    top: JsonMap,
    specific: JsonMap,
}

impl Answer {
    /// The text under `key` of `fields`: absent or `null` is none, a string
    /// is the text, anything else is an answer haest does not take.
    fn text(&self, fields: &JsonMap, key: &str) -> Result<Option<String>, HookFailure> {
        match fields.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(got) => Err(self.unreadable(format!("`{key}` must be text, not {got}"))),
        }
    }

    /// A top-level field's text.
    fn top(&self, key: &str) -> Result<Option<String>, HookFailure> {
        self.text(&self.top, key)
    }

    /// A `hookSpecificOutput` field's text.
    fn specific(&self, key: &str) -> Result<Option<String>, HookFailure> {
        self.text(&self.specific, key)
    }

    /// The top-level `decision` when it is `block`, with its `reason`;
    /// `approve` and none pass.
    fn block(&self) -> Result<Option<Block>, HookFailure> {
        match self.top("decision")?.as_deref() {
            None | Some("approve") => Ok(None),
            Some("block") => Ok(Some(Block {
                reason: self.top("reason")?,
            })),
            Some(other) => Err(self.unreadable(format!(
                "`decision` is \"block\" or \"approve\", not {other:?}"
            ))),
        }
    }

    fn unreadable(&self, why: String) -> HookFailure {
        HookFailure::Unreadable {
            event: self.event,
            why,
        }
    }
}

/// What `text` says for `E`: nothing, plain text, or a JSON object, which
/// output starting with `{` must be.
fn said<E: Event>(text: &str) -> Result<Said, HookFailure> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Said::Nothing);
    }
    if !text.starts_with('{') {
        return Ok(Said::Text(text.to_string()));
    }
    let event = E::NAME;
    let mut top: JsonMap =
        serde_json::from_str(text).map_err(|source| HookFailure::Answer { event, source })?;
    let specific = match top.remove("hookSpecificOutput") {
        None | Some(Value::Null) => JsonMap::new(),
        Some(Value::Object(specific)) => specific,
        Some(got) => {
            return Err(HookFailure::Unreadable {
                event,
                why: format!("`hookSpecificOutput` must be an object, not {got}"),
            });
        }
    };
    Ok(Said::Json(Answer {
        event,
        top,
        specific,
    }))
}

/// The non-empty `parts`, a blank line between; none when all are empty.
fn joined(parts: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    let parts: Vec<String> = parts
        .into_iter()
        .flatten()
        .filter(|p| !p.trim().is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// A refusal's reason when the hook gave none.
const DENIED: &str = "a hook denied this call";
/// A refusal's reason when the hook asked for the human and gave no reason.
const ASKED: &str = "a hook asked for the human's approval, which a hook cannot get";

impl Dialect<ToolBeforePayload> for Claude {
    fn input(&self, p: &ToolBeforePayload, _: Value, dir: &Path) -> Result<Value, HookFailure> {
        let mut input = common(p, dir);
        input.insert("tool_name".into(), p.call.name.as_str().into());
        input.insert("tool_input".into(), Value::Object(p.call.input.clone()));
        if let Some(id) = &p.ctx.tool_call_id {
            input.insert("tool_use_id".into(), id.as_str().into());
        }
        Ok(Value::Object(input))
    }

    fn decide(&self, _: ToolBeforePayload, reply: Reply) -> Result<Before, HookFailure> {
        let text = match reply {
            Reply::Blocked(stderr) => return blocked::<ToolBeforePayload>(&stderr),
            Reply::Answer(text) => text,
        };
        let Said::Json(a) = said::<ToolBeforePayload>(&text)? else {
            return Ok(Before::Pass);
        };
        let reason = a.specific("permissionDecisionReason")?;
        match a.specific("permissionDecision")?.as_deref() {
            None | Some("allow" | "defer") => {}
            Some("deny") => return Ok(Before::Deny(reason.unwrap_or_else(|| DENIED.into()))),
            Some("ask") => return Ok(Before::Deny(reason.unwrap_or_else(|| ASKED.into()))),
            Some(other) => {
                return Err(a.unreadable(format!(
                    "`permissionDecision` is allow, deny, ask or defer, not {other:?}"
                )));
            }
        }
        if let Some(block) = a.block()? {
            return Ok(Before::Deny(block.reason.unwrap_or_else(|| DENIED.into())));
        }
        match a.specific.get("updatedInput") {
            None | Some(Value::Null) => Ok(Before::Pass),
            Some(Value::Object(input)) => Ok(Before::Input(input.clone())),
            Some(got) => Err(a.unreadable(format!("`updatedInput` must be an object, not {got}"))),
        }
    }
}

impl Dialect<ToolAfterPayload> for Claude {
    fn input(&self, p: &ToolAfterPayload, _: Value, dir: &Path) -> Result<Value, HookFailure> {
        let mut input = common(p, dir);
        input.insert("tool_name".into(), p.call.name.as_str().into());
        input.insert("tool_input".into(), Value::Object(p.call.input.clone()));
        input.insert("tool_response".into(), p.output.content.as_str().into());
        if let Some(id) = &p.ctx.tool_call_id {
            input.insert("tool_use_id".into(), id.as_str().into());
        }
        Ok(Value::Object(input))
    }

    fn decide(
        &self,
        p: ToolAfterPayload,
        reply: Reply,
    ) -> Result<Option<haest_core::ToolOutput>, HookFailure> {
        let text = match reply {
            Reply::Blocked(stderr) => return blocked::<ToolAfterPayload>(&stderr),
            Reply::Answer(text) => text,
        };
        let Said::Json(a) = said::<ToolAfterPayload>(&text)? else {
            return Ok(None);
        };
        let replaced = match a.specific.get("updatedToolOutput") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(other) => Some(other.to_string()),
        };
        let reason = a.block()?.and_then(|block| block.reason);
        let added = joined([reason, a.specific("additionalContext")?]);
        if replaced.is_none() && added.is_none() {
            return Ok(None);
        }
        let mut output = p.output;
        if let Some(text) = replaced {
            // A new output: what described the old one goes with it.
            output.content = text;
            output.result = None;
            output.media.clear();
            output.title = None;
            output.sized = false;
        }
        if let Some(added) = added {
            output.content = joined([Some(output.content), Some(added)]).unwrap_or_default();
        }
        Ok(Some(output))
    }
}

impl Dialect<PromptSubmitPayload> for Claude {
    fn input(&self, p: &PromptSubmitPayload, _: Value, dir: &Path) -> Result<Value, HookFailure> {
        let mut input = common(p, dir);
        input.insert("prompt".into(), p.text.as_str().into());
        Ok(Value::Object(input))
    }

    fn decide(&self, p: PromptSubmitPayload, reply: Reply) -> Result<Option<String>, HookFailure> {
        let text = match reply {
            Reply::Blocked(stderr) => return blocked::<PromptSubmitPayload>(&stderr),
            Reply::Answer(text) => text,
        };
        let context = match said::<PromptSubmitPayload>(&text)? {
            Said::Nothing => None,
            Said::Text(context) => Some(context),
            Said::Json(a) => {
                if let Some(block) = a.block()? {
                    let why = block.reason.map_or_else(String::new, |r| format!(": {r}"));
                    return Err(HookFailure::Refused {
                        event: a.event,
                        why: format!(
                            "the hook refused the prompt, which haest's prompt.submit cannot do{why}"
                        ),
                    });
                }
                a.specific("additionalContext")?
            }
        };
        Ok(context.and_then(|context| joined([Some(p.text), Some(context)])))
    }
}

impl Dialect<SystemPromptPayload> for Claude {
    fn input(&self, p: &SystemPromptPayload, _: Value, dir: &Path) -> Result<Value, HookFailure> {
        Ok(Value::Object(common(p, dir)))
    }

    fn decide(&self, _: SystemPromptPayload, reply: Reply) -> Result<Option<String>, HookFailure> {
        let text = match reply {
            Reply::Blocked(stderr) => return blocked::<SystemPromptPayload>(&stderr),
            Reply::Answer(text) => text,
        };
        match said::<SystemPromptPayload>(&text)? {
            Said::Nothing => Ok(None),
            Said::Text(context) => Ok(Some(context)),
            Said::Json(a) => a.specific("additionalContext"),
        }
    }
}

impl Dialect<TurnEndPayload> for Claude {
    fn input(&self, p: &TurnEndPayload, _: Value, dir: &Path) -> Result<Value, HookFailure> {
        let mut input = common(p, dir);
        input.insert("stop_hook_active".into(), p.continued.into());
        let answer = p
            .answer
            .clone()
            .map(serde_json::from_value::<AssistantMessage>);
        if let Some(Ok(answer)) = answer {
            input.insert("last_assistant_message".into(), answer.text().into());
        }
        Ok(Value::Object(input))
    }

    fn decide(&self, _: TurnEndPayload, reply: Reply) -> Result<TurnEndDecision, HookFailure> {
        let text = match reply {
            Reply::Blocked(stderr) => return blocked::<TurnEndPayload>(&stderr),
            Reply::Answer(text) => text,
        };
        let Said::Json(a) = said::<TurnEndPayload>(&text)? else {
            return Ok(TurnEndDecision::default());
        };
        let reason = match a.block()? {
            None => None,
            Some(Block {
                reason: Some(reason),
            }) => Some(reason),
            Some(Block { reason: None }) => {
                return Err(a.unreadable(
                    "`decision: \"block\"` needs a `reason`: the text the turn goes on with".into(),
                ));
            }
        };
        Ok(match joined([reason, a.specific("additionalContext")?]) {
            Some(text) => TurnEndDecision::default().with_continue(text),
            None => TurnEndDecision::default(),
        })
    }
}
