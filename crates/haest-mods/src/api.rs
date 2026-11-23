//! What a mod is ([`Mod`]), and what its events decide ([`Before`],
//! [`Step`], [`TurnEnd`]).

use async_trait::async_trait;
use haest_core::{
    Agent, AssistantMessage, Tools, TurnOutcome,
    error::BoxError,
    tool::{JsonMap, ToolContext, ToolOutput},
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::env::ModEnv;

/// One tool call as a mod sees it, in `tool.before` and `tool.after`.
///
/// A mod file's handler gets the same value as its `call` argument, in
/// camelCase: `{ name, input, readOnly }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModToolCall {
    /// The tool's name, as the model called it (`Bash`, `Write`, `mcp__github__search`).
    pub name: String,
    /// The input the tool will run with: the model's, or an earlier mod's replacement.
    pub input: JsonMap,
    /// The tool declares it changes nothing ([`Tool::read_only`](haest_core::Tool::read_only)).
    pub read_only: bool,
}

/// What a [`Mod::tool_before`] hook decided about one tool call: the
/// decision of `tool.before`, for a mod and a [`Hook`](crate::Hook) alike.
///
/// As JSON it is what a mod file's handler returns: `{ deny: reason }`,
/// `{ input: {..} }`, or anything else (`null`) to pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Before {
    /// Let the call through as it is.
    #[default]
    Pass,
    /// Refuse it: the model reads `Refused: <reason>` as the call's error
    /// result, and the tool does not run. The first deny wins; later mods are
    /// not asked.
    Deny(String),
    /// Replace its input, for the hooks after this one and for the tool.
    Input(JsonMap),
}

/// What a [`Mod::model_before`] or [`Mod::model_after`] hook decided about
/// the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Go on.
    Pass,
    /// End the turn here, as an error whose text names this mod and says
    /// `reason` ([`TurnOutcome::error`]). The first stop wins; later mods
    /// are not asked.
    Stop(String),
}

/// What a [`Mod::turn_end`] hook decided about a turn that would end on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEnd {
    /// Let it end.
    Pass,
    /// Keep it going: the text goes to the model as a user message, and the
    /// turn makes another model call under its caps. The first mod that keeps
    /// it going wins; later mods are not asked.
    Continue(String),
}

/// A NATIVE mod: plain Rust with the same events a mod file has. Every method
/// has a default that does nothing, so a mod implements only what it hooks.
/// Built-ins ([`ApprovalMod`](crate::ApprovalMod), [`PlanMod`](crate::PlanMod))
/// are mods like this; so is a host's own. The [crate docs](crate#a-native-mod)
/// have one.
///
/// A mod is put in a conversation with
/// [`ModOptions::with_mod`](crate::ModOptions::with_mod) or
/// [`Mods::new`](crate::Mods::new) (by
/// value, or as an `Arc` the host keeps to look at it), and held in a
/// [`ModRef`]; every event runs across the mods in the order they were added. An `Err` from an
/// event (any error, or a line of text: `Err("why".into())`) is reported to
/// the human through [`ModUi::error`](crate::ModUi::error) with its causes,
/// and never takes the conversation down. What else a failure means depends
/// on the event; [`Mods`](crate::Mods) has the rules (a failed `tool.before`
/// refuses the call, a failed `tool.after` leaves the output as it was, a
/// failed loop event changes nothing and the turn goes on).
///
/// The loop events (`model.before`, `model.after`, `turn.end`) run where the
/// host put [`ModHooks`](crate::ModHooks) around its own hooks, after the
/// host's, for the conversation's own agent (not its subagents). They get
/// the running [`Agent`] itself, to read and change, and the turn's cancel
/// token, to pass to any model call made there.
///
/// A mod runs with the HOST's full trust, sandbox or not.
#[async_trait]
pub trait Mod: Send + Sync {
    /// Who this mod is in a report: a built-in's name or a file's path.
    fn name(&self) -> &str;

    /// A gate (the approval mod) judges a call as it will run: its
    /// `tool.before` goes after every other mod's, on the input they rewrote,
    /// so no later rewrite slips past it. Gates run in load order.
    fn is_gate(&self) -> bool {
        false
    }

    /// `tool.before`: before every tool call. [`Before::Deny`] refuses it (the
    /// model reads the reason), [`Before::Input`] replaces its input.
    ///
    /// `ctx.cancel()` fires when the human interrupts the call; an ask made here
    /// should pass it on. An `Err` refuses the call: a guard that broke must
    /// not wave anything through.
    async fn tool_before(
        &self,
        _call: &ModToolCall,
        _ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        Ok(Before::Pass)
    }

    /// `tool.after`: after the call ran; Some replaces the output the model
    /// reads, but not what the call spent (its `cost_usd` and
    /// `child_cost_usd` stay the tool's). Every mod sees the output as the
    /// mods before it left it. An `Err` leaves the output as it was: the call
    /// happened, and the model must not retry it.
    async fn tool_after(
        &self,
        _call: &ModToolCall,
        _output: &ToolOutput,
        _ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Option<ToolOutput>, BoxError> {
        Ok(None)
    }

    /// `prompt.submit`: a human message before the model sees it; Some rewrites
    /// it. Runs when the host calls [`Mods::rewrite_prompt`](crate::Mods::rewrite_prompt).
    async fn prompt_submit(
        &self,
        _text: &str,
        _haest: &ModEnv,
    ) -> Result<Option<String>, BoxError> {
        Ok(None)
    }

    /// `system.prompt`: text appended to the system prompt, asked before every
    /// model call (through [`ModSystemPrompt`](crate::ModSystemPrompt)). None
    /// or empty adds nothing.
    async fn system_prompt(&self, _haest: &ModEnv) -> Result<Option<String>, BoxError> {
        Ok(None)
    }

    /// `model.before`: before every model call, once the host's
    /// [`before_model_call`](haest_core::AgentHooks::before_model_call) has
    /// run (its compaction included). [`Step::Stop`] ends the turn before the
    /// call.
    ///
    /// `agent` is the running agent: read its history, steer it
    /// ([`Agent::control`]), switch its model, or compact it
    /// ([`compact`](haest_core::compaction::compact), with `cancel`: what the
    /// summary costs counts toward the turn). An `Err` changes nothing: the
    /// turn goes on.
    async fn model_before(
        &self,
        _agent: &mut Agent,
        _cancel: &CancellationToken,
        _haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        Ok(Step::Pass)
    }

    /// `model.after`: once each answer is in history, before any of its tool
    /// calls runs (after the host's
    /// [`after_model_call`](haest_core::AgentHooks::after_model_call)).
    /// [`Step::Stop`] ends the turn, and the answer's calls are answered as
    /// not run.
    ///
    /// `answer` is the last message in `agent`'s history. Steer here
    /// ([`Agent::control`]) to put input after the answer's tool results, or
    /// to keep going a turn whose answer had none. Leave the history as it is
    /// (the results land after the answer): compact in
    /// [`model_before`](Self::model_before) or [`turn_end`](Self::turn_end).
    /// An `Err` changes nothing: the calls run.
    async fn model_after(
        &self,
        _agent: &mut Agent,
        _answer: &AssistantMessage,
        _cancel: &CancellationToken,
        _haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        Ok(Step::Pass)
    }

    /// `turn.end`: a turn is about to end on its own with `outcome` (an
    /// answer without tool calls, no steering queued), and the host's
    /// [`before_turn_end`](haest_core::AgentHooks::before_turn_end) let it.
    /// [`TurnEnd::Continue`] keeps it going: the stop hook that holds a turn
    /// to a check (the tests pass, the file is written). A mod that always
    /// continues ends the turn at a cap.
    ///
    /// `agent` is the running agent, its answer last in history; compact it
    /// here so the next turn starts small. An `Err` changes nothing: the
    /// turn ends.
    async fn turn_end(
        &self,
        _agent: &mut Agent,
        _outcome: &TurnOutcome,
        _cancel: &CancellationToken,
        _haest: &ModEnv,
    ) -> Result<TurnEnd, BoxError> {
        Ok(TurnEnd::Pass)
    }

    /// Tools this mod adds (asked again before every model call).
    /// [`ModTools`](crate::ModTools) runs them inside every mod's hooks, the
    /// same as the host's own.
    fn tools(&self) -> Tools {
        Tools::new()
    }

    /// The names of the tools this mod hides, whoever added them (the host,
    /// an MCP server, a mod): [`ModTools`](crate::ModTools) neither
    /// advertises nor runs them. Asked again before every model call, so a
    /// name the mod stops returning is back from the next call, as it was.
    fn disabled_tools(&self) -> Vec<String> {
        Vec::new()
    }

    /// The slash commands this mod answers, without the `/`.
    fn commands(&self) -> Vec<String> {
        Vec::new()
    }

    /// Run one of [`Mod::commands`]: Some text goes to the model; None = the mod
    /// handled the line itself. `name` comes without its `/`; `args` is the
    /// rest of the line.
    ///
    /// A host that gives up on a command drops the call: what it leaves at
    /// an await must be safe to leave there.
    async fn command(
        &self,
        _name: &str,
        _args: &str,
        _haest: &ModEnv,
    ) -> Result<Option<String>, BoxError> {
        Ok(None)
    }
}

haest_core::plug_ref! {
    /// A [`Mod`], shared: what [`Mods`](crate::Mods) holds. Its `Debug` shows
    /// the mod's name.
    pub struct ModRef(dyn Mod);
    debug(m, f) { f.debug_tuple("ModRef").field(&m.name()).finish() }
}
