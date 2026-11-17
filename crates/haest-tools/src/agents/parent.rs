//! What the default hub needs from the conversation it serves (the subagents'
//! parent): its deps, tools and system prompt to build a subagent from, and
//! where the subagents' activity goes.

use async_trait::async_trait;
use haest_core::{SharedDeps, ToolRef, ToolSourceRef, Tools, provider::Effort};
use tokio_util::sync::CancellationToken;

use super::{AgentError, AgentToolOptions, Member, MemberStart, SubagentEvent, subagent_note};
use crate::inbox::Inbox;

haest_core::plug_ref! {
    /// A [`SubagentParent`], shared: what [`SubagentHub::new`](super::SubagentHub::new)
    /// takes. Built from the parent itself, or from an `Arc` of one the host
    /// keeps too.
    pub struct SubagentParentRef(dyn SubagentParent);
}

/// The parent conversation's side of the default hub
/// ([`SubagentHub`](super::SubagentHub)): what a subagent is built from, and
/// where its activity goes.
/// [`Inbox::submit`] takes a message for main (SendMessage, the person), and
/// [`Inbox::submit_answer`] a background answer for main (main started it, or
/// its starter has finished).
///
/// [`SubagentHub`](super::SubagentHub)'s docs implement one.
#[async_trait]
pub trait SubagentParent: Inbox {
    /// What every agent of the conversation runs on: a subagent shares its
    /// models, cwd, workspace, spill dir, ledger, web search and hooks. Its
    /// tools, prompt and turn caps are its own, and it neither persists its
    /// messages nor compacts.
    fn deps(&self) -> SharedDeps;
    /// The tools every agent starts from (no agent tools): the hub drops the
    /// conversation-only ones (the task list) and adds the agent's own agent tools.
    fn base_tools(&self) -> Tools;
    /// The system prompt for an agent on `model_spec` with `tools`; the hub
    /// appends the subagent note. Built again before every model call.
    async fn system_prompt(&self, model_spec: &str, tools: &[ToolRef]) -> String;
    /// What a subagent runs on when the call names no model: the conversation's own.
    fn default_model(&self) -> (String, Option<Effort>);
    /// What the Agent tool of every agent here offers the model: the host's
    /// kinds of subagent (`subagent_type`) and the `model` parameter's text.
    /// Default: no kinds, haest's own text.
    fn agent_tool_options(&self) -> AgentToolOptions {
        AgentToolOptions::default()
    }
    /// The tools a subagent of `subagent_type` starts from: one of the kinds in
    /// [`agent_tool_options`](Self::agent_tool_options) (the call's, else the
    /// default kind), None when the host has none. The hub drops the
    /// conversation-only ones and adds the agent's own agent tools, as for
    /// [`base_tools`](Self::base_tools). Default: `base_tools`, whatever the kind.
    fn subagent_tools(&self, subagent_type: Option<&str>) -> Tools {
        let _ = subagent_type;
        self.base_tools()
    }
    /// Where an agent's tool list goes through (deferral); default: as it is.
    fn tool_source(&self, tools: Tools) -> ToolSourceRef {
        tools.into()
    }
    /// The running set changed: the ids of every subagent running now, for
    /// the host's roster. An id (`haest-<name>-<hub>`) is unique across hubs;
    /// a name only within one.
    fn on_activity(&self, _ids: &[String]) {}
    /// One subagent's activity, keyed by its id.
    fn on_subagent(&self, _id: &str, _event: SubagentEvent) {}
    /// A member is about to start, before its loop exists: run it in this
    /// process as the hub would (the default, [`Member::in_process`]), run it
    /// on a run of the host's own ([`Member::new`] over a
    /// [`MemberRun`](super::MemberRun): its model, caps, deps, and its end),
    /// or refuse.
    ///
    /// An `Err` is the spawn's refusal, as the model reads it
    /// ([`AgentError::Refused`] for a reason of the host's own): the member
    /// never ran, its name is free again and no end follows. Its name already
    /// counts toward `max_concurrent` while this runs. `cancel` is the caller's
    /// turn: when it fires, stop whatever the start began and refuse. The hub
    /// never drops a start half done.
    ///
    /// # Examples
    ///
    /// A parent that refuses deploys and hears every end.
    ///
    /// ```
    /// use std::sync::{Arc, Mutex};
    ///
    /// use async_trait::async_trait;
    /// use haest_core::{
    ///     SharedDeps, ToolRef, Tools,
    ///     caps::SubagentCaps,
    ///     provider::Effort,
    ///     testing::{ScriptedProvider, text_answer},
    ///     workspace::ShutWorkspace,
    /// };
    /// use haest_tools::{
    ///     AgentError, AgentRef, Ending, Inbox, Member, MemberEnd, MemberRun, MemberStart,
    ///     SpawnInput, SubagentHub, SubagentParent,
    /// };
    /// use tokio_util::sync::CancellationToken;
    ///
    /// struct Parent {
    ///     provider: ScriptedProvider,
    ///     ended: Arc<Mutex<Vec<(String, Ending)>>>,
    /// }
    ///
    /// struct Logged {
    ///     name: String,
    ///     ended: Arc<Mutex<Vec<(String, Ending)>>>,
    /// }
    ///
    /// #[async_trait]
    /// impl MemberRun for Logged {
    ///     async fn end(self: Box<Self>, end: MemberEnd) {
    ///         self.ended.lock().unwrap().push((self.name.clone(), end.ending));
    ///     }
    /// }
    ///
    /// impl Inbox for Parent {
    ///     fn submit(&self, _text: String) {}
    /// }
    ///
    /// #[async_trait]
    /// impl SubagentParent for Parent {
    ///     fn deps(&self) -> SharedDeps {
    ///         SharedDeps::new(self.provider.resolver(), "/", ShutWorkspace)
    ///     }
    ///     fn base_tools(&self) -> Tools {
    ///         Tools::new()
    ///     }
    ///     async fn system_prompt(&self, _model: &str, _tools: &[ToolRef]) -> String {
    ///         "You are a helper.".into()
    ///     }
    ///     fn default_model(&self) -> (String, Option<Effort>) {
    ///         ("scripted:m".into(), None)
    ///     }
    ///     async fn start_member(
    ///         &self,
    ///         start: &MemberStart,
    ///         _cancel: &CancellationToken,
    ///     ) -> Result<Member, AgentError> {
    ///         if start.input.description.contains("deploy") {
    ///             return Err(AgentError::Refused("helpers do not deploy".into()));
    ///         }
    ///         Ok(Member::new(Logged { name: start.name.clone(), ended: self.ended.clone() }))
    ///     }
    /// }
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), haest_core::error::BoxError> {
    /// let ended = Arc::new(Mutex::new(Vec::new()));
    /// let parent = Parent { provider: ScriptedProvider::new([text_answer("3 files")]), ended: ended.clone() };
    /// let hub = SubagentHub::new(parent, SubagentCaps::default());
    /// let ask = |description: &str| SpawnInput {
    ///     description: description.into(),
    ///     prompt: "Go.".into(),
    ///     ..Default::default()
    /// };
    /// let turn = CancellationToken::new();
    ///
    /// let refused = hub.spawn(&AgentRef::main(), ask("deploy it"), &turn).await;
    /// assert!(matches!(refused, Err(AgentError::Refused(_))));
    ///
    /// let out = hub.spawn(&AgentRef::main(), ask("count files"), &turn).await?;
    /// assert_eq!(out.text, "3 files");
    /// assert_eq!(*ended.lock().unwrap(), [("count-files-2".to_string(), Ending::Answered)]);
    /// # Ok(())
    /// # }
    /// ```
    async fn start_member(
        &self,
        start: &MemberStart,
        cancel: &CancellationToken,
    ) -> Result<Member, AgentError> {
        let _ = (start, cancel);
        Ok(Member::in_process())
    }
    /// The note the hub appends to a member's system prompt: who it is, who
    /// reads its answer, its caps, how to talk to other agents. `member` is the
    /// member as it runs: its run's model spec and caps where the run names
    /// them ([`MemberRun::model_spec`](super::MemberRun::model_spec),
    /// [`MemberRun::caps`](super::MemberRun::caps)), its kind in
    /// `input.subagent_type`. Default: haest's ([`subagent_note`]).
    ///
    /// # Examples
    ///
    /// A parent whose members hear about the person they work for.
    ///
    /// ```
    /// use async_trait::async_trait;
    /// use haest_core::{
    ///     SharedDeps, ToolRef, Tools,
    ///     caps::SubagentCaps,
    ///     provider::Effort,
    ///     testing::{ScriptedProvider, text_answer},
    ///     workspace::ShutWorkspace,
    /// };
    /// use haest_tools::{AgentRef, Inbox, MemberStart, SpawnInput, SubagentHub, SubagentParent};
    /// use tokio_util::sync::CancellationToken;
    ///
    /// struct Parent(ScriptedProvider);
    ///
    /// impl Inbox for Parent {
    ///     fn submit(&self, _text: String) {}
    /// }
    ///
    /// #[async_trait]
    /// impl SubagentParent for Parent {
    ///     fn deps(&self) -> SharedDeps {
    ///         SharedDeps::new(self.0.resolver(), "/", ShutWorkspace)
    ///     }
    ///     fn base_tools(&self) -> Tools {
    ///         Tools::new()
    ///     }
    ///     async fn system_prompt(&self, _model: &str, _tools: &[ToolRef]) -> String {
    ///         "You help a person.".into()
    ///     }
    ///     fn default_model(&self) -> (String, Option<Effort>) {
    ///         ("scripted:m".into(), None)
    ///     }
    ///     fn subagent_note(&self, member: &MemberStart) -> String {
    ///         let usd = member.caps.usd;
    ///         format!("You are {}, for {}. The person may write to you. Spend at most ${usd}.", member.name, member.parent)
    ///     }
    /// }
    ///
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), haest_core::error::BoxError> {
    /// let provider = ScriptedProvider::new([text_answer("done")]);
    /// let caps = SubagentCaps { usd: 0.5, ..SubagentCaps::default() };
    /// let hub = SubagentHub::new(Parent(provider.clone()), caps);
    /// let input = SpawnInput { description: "tidy".into(), prompt: "Tidy up.".into(), ..Default::default() };
    /// hub.spawn(&AgentRef::main(), input, &CancellationToken::new()).await?;
    /// let system = &provider.requests()[0].system;
    /// assert_eq!(system, "You help a person.\n\nYou are tidy-1, for main. The person may write to you. Spend at most $0.5.");
    /// # Ok(())
    /// # }
    /// ```
    fn subagent_note(&self, member: &MemberStart) -> String {
        subagent_note(
            &member.name,
            &member.parent,
            &member.input.description,
            &member.caps,
        )
    }
}
