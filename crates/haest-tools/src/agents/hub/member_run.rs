//! One subagent's run inside the hub: build its agent, drive its turn, close
//! its run, hand a background answer back.

use std::{panic::AssertUnwindSafe, sync::Arc};

use futures::FutureExt;
use haest_core::{
    __private::panic_message, Agent, AgentDeps, AgentOptions, SharedDeps, provider::Effort,
    tool::Tool,
};
use tokio_util::sync::CancellationToken;

use super::SubagentHub;
use crate::agents::{
    MemberEnd, MemberRun, MemberStart, SpawnOutcome, SubagentEvent,
    members::Status,
    run::{ChildCaps, ChildHooks, ChildPrompt, Run, drive, subagent_note},
};

impl SubagentHub {
    fn report_activity(&self) {
        let ids: Vec<String> = {
            let members = self.members.lock().unwrap();
            members.running().map(|m| m.id.clone()).collect()
        };
        self.parent.on_activity(&ids);
    }

    /// The agent for `run`: its run's deps (else the conversation's) with its
    /// own tools (its kind's,
    /// [`SubagentParent::subagent_tools`](crate::SubagentParent::subagent_tools)),
    /// prompt and caps, on its run's model (else the start's). No task tools:
    /// the task list belongs to the conversation, not to a helper.
    pub(super) fn build(
        &self,
        run: &Run,
        start: &MemberStart,
        effort: Option<Effort>,
        member: &dyn MemberRun,
    ) -> Agent {
        let mut tools: Vec<Arc<dyn Tool>> = self
            .parent
            .subagent_tools(start.input.subagent_type.as_deref())
            .into_iter()
            .filter(|t| crate::builtin_word(t.name()) != Some("tasks"))
            .collect();
        tools.extend(self.tools_for(&run.me));
        let source = self.parent.tool_source(tools);
        let model_spec = member
            .model_spec()
            .unwrap_or_else(|| start.model_spec.clone());
        let caps = member.caps().unwrap_or(start.caps);
        // Its own deps keep their hooks; the conversation's are lent, persisting nothing.
        let base = member.deps().unwrap_or_else(|| {
            let base = self.parent.deps();
            SharedDeps {
                hooks: Arc::new(ChildHooks {
                    hooks: base.hooks.clone(),
                    parent: self.parent.clone(),
                    id: run.id.clone(),
                }),
                ..base
            }
        });
        let deps = AgentDeps {
            tools: source.clone(),
            system_prompt: Arc::new(ChildPrompt {
                parent: self.parent.clone(),
                model_spec: model_spec.clone(),
                tools: source,
                note: subagent_note(&run.me.name, &run.parent, &run.description, &caps),
            }),
            shared: SharedDeps {
                turn_caps: Arc::new(ChildCaps {
                    caps: base.turn_caps.clone(),
                    subagent: caps,
                }),
                ..base
            },
        };
        let mut opts = AgentOptions::new(model_spec);
        opts.effort = effort;
        Agent::new(deps, opts)
    }

    /// Run `agent`'s one turn on `prompt`, reporting it to the parent, then
    /// close its run (`member`); the answer, with any message that arrived
    /// after it finished appended unread.
    pub(super) async fn run_member(
        &self,
        run: Run,
        mut agent: Agent,
        member: Box<dyn MemberRun>,
        prompt: String,
        caller: Option<CancellationToken>,
    ) -> SpawnOutcome {
        self.report_activity();
        let started = SubagentEvent::Started {
            name: run.me.name.clone(),
            parent: run.parent.clone(),
            description: run.description.clone(),
        };
        self.parent.on_subagent(&run.id, started);
        let parent = self.parent.as_ref();
        let out = drive(
            &mut agent,
            &prompt,
            &run.stop,
            caller.as_ref(),
            parent,
            &run.id,
        )
        .await;
        self.parent
            .on_subagent(&run.id, SubagentEvent::Stopped { ok: out.ok() });
        // Leave first: from here a message for it is refused, not lost.
        self.members.lock().unwrap().leave(&run.me.name);
        let late = agent.control().take_queued();
        self.report_activity();
        let unread = late.map_or_else(String::new, |late| {
            format!(
                "\n\n[messages that reached \"{}\" after it finished, unread:]\n{late}",
                run.me.name
            )
        });
        let text = format!("{}{unread}", out.text);
        let cost_usd = out.spend.usd();
        let end = MemberEnd {
            ending: out.ending,
            answer: text.clone(),
            spend: out.spend,
        };
        // A host's end that panics must not lose the answer (a background one is handed back next).
        if let Err(p) = AssertUnwindSafe(member.end(end)).catch_unwind().await {
            let why = panic_message(p.as_ref()).unwrap_or("a panic");
            let log = format!("subagent {}: its run's end panicked: {why}", run.id);
            self.parent.deps().hooks.debug(&log);
        }
        SpawnOutcome { text, cost_usd }
    }

    /// A background agent's answer goes to whoever started it -- or to main when
    /// that one has finished meanwhile. Never fails: an answer must not be lost to a race.
    pub(super) fn hand_back(&self, parent: &str, text: String) {
        let mut members = self.members.lock().unwrap();
        match members.get_mut(parent) {
            Some(p) if p.status == Status::Running => p.steer(text),
            _ => {
                drop(members);
                self.parent.submit(text);
            }
        }
    }
}
