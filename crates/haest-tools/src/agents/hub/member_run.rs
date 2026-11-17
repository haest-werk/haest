//! One subagent's run inside the hub: build its agent, drive its turn, close
//! its run.

use std::{panic::AssertUnwindSafe, sync::PoisonError};

use futures::FutureExt;
use haest_core::{
    __private::panic_message, Agent, AgentDeps, AgentOptions, Tools, provider::Effort,
};
use tokio_util::sync::CancellationToken;

use super::SubagentHub;
use crate::agents::{
    Member, MemberCaps, MemberEnd, MemberStart, SpawnOutcome, SubagentBudget, SubagentEvent,
    run::{ChildCaps, ChildHooks, ChildPrompt, ChildServices, Run, drive},
};

/// A member's place in the hub's table, given back however its spawn or its
/// run stops: at its end, or when its future is dropped (a host's timeout, a
/// runtime shutting down) or a panic unwinds through it (the host's
/// `start_member`, or one of its callbacks). Without it the member stays
/// Running for good, and `max_concurrent` refuses every later spawn.
pub(super) struct Seat {
    hub: SubagentHub,
    /// None once given back.
    name: Option<String>,
    /// Its run started: it leaves (its name stays taken). Before that it is
    /// forgotten (its name is free again).
    started: bool,
}

impl Seat {
    /// The seat `name` just reserved; forgotten unless its run starts.
    pub(super) fn reserved(hub: &SubagentHub, name: &str) -> Self {
        Self {
            hub: hub.clone(),
            name: Some(name.to_string()),
            started: false,
        }
    }

    /// Its run starts: from here on it leaves, never forgotten.
    pub(super) fn started(mut self) -> Self {
        self.started = true;
        self
    }

    /// Its turn ended: it leaves now.
    fn leave(mut self) {
        if let Some(name) = self.name.take() {
            self.hub.lock_members().leave(&name);
        }
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        let Some(name) = self.name.take() else {
            return;
        };
        // This runs while a panic unwinds too: a poisoned table is still the
        // table, and a second panic here would abort the process.
        let members = &self.hub.0.members;
        let mut members = members.lock().unwrap_or_else(PoisonError::into_inner);
        if self.started {
            members.leave(&name);
        } else {
            members.forget(&name);
        }
    }
}

impl SubagentHub {
    /// What a subagent's budget is clamped to: the per-agent caps.
    pub(super) fn budget_caps(&self) -> SubagentBudget {
        SubagentBudget {
            usd: self.0.caps.usd,
            time: self.0.caps.time,
        }
    }

    /// A member's caps: the per-agent caps, with a budget's spend and time.
    pub(super) fn member_caps(&self, budget: Option<&SubagentBudget>) -> MemberCaps {
        let caps = MemberCaps::from(self.0.caps);
        match budget {
            Some(b) => MemberCaps {
                usd: b.usd,
                time: b.time,
                ..caps
            },
            None => caps,
        }
    }

    fn report_activity(&self) {
        let ids: Vec<String> = {
            let members = self.lock_members();
            members.running().map(|m| m.id.clone()).collect()
        };
        self.0.parent.on_activity(&ids);
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
        member: &Member,
    ) -> Agent {
        let mut tools: Tools = self
            .0
            .parent
            .subagent_tools(start.input.subagent_type.as_deref())
            .into_iter()
            .filter(|t| crate::BuiltinTool::of(t.name()) != Some(crate::BuiltinTool::Tasks))
            .collect();
        tools.extend(self.tools_for(&run.me));
        let source = self.0.parent.tool_source(tools);
        let member = member.run();
        let model_spec = member
            .model_spec()
            .unwrap_or_else(|| start.model_spec.clone());
        let caps = member.caps().unwrap_or(start.caps);
        // The note tells the member what it runs on: its run's model spec and caps.
        let runs = MemberStart {
            model_spec: model_spec.clone(),
            caps,
            ..start.clone()
        };
        let note = self.0.parent.subagent_note(&runs);
        // Its own deps keep their hooks; the conversation's are lent, persisting nothing.
        let base = member.deps().unwrap_or_else(|| {
            let base = self.0.parent.deps();
            let hooks = ChildHooks {
                hooks: base.hooks().clone(),
            };
            let services = ChildServices {
                services: base.services().clone(),
                parent: self.0.parent.clone(),
                id: run.id.clone(),
            };
            base.with_hooks(hooks).with_services(services)
        });
        let child_caps = ChildCaps {
            caps: base.turn_caps().clone(),
            subagent: caps,
        };
        let prompt = ChildPrompt {
            parent: self.0.parent.clone(),
            model_spec: model_spec.clone(),
            tools: source.clone(),
            note,
        };
        let deps = AgentDeps::new(base.with_turn_caps(child_caps), source, prompt);
        let mut opts = AgentOptions::new(model_spec);
        if let Some(effort) = effort {
            opts = opts.with_effort(effort);
        }
        Agent::new(deps, opts)
    }

    /// Run `agent`'s one turn on `prompt`, reporting it to the parent, then
    /// give back its `seat` and close its run (`member`); the answer, with any
    /// message that arrived after it finished appended unread.
    pub(super) async fn run_member(
        &self,
        seat: Seat,
        run: Run,
        mut agent: Agent,
        member: Member,
        prompt: String,
        caller: Option<CancellationToken>,
    ) -> SpawnOutcome {
        self.report_activity();
        let started = SubagentEvent::Started {
            name: run.me.name.clone(),
            parent: run.parent.clone(),
            description: run.description.clone(),
        };
        self.0.parent.on_subagent(&run.id, started);
        let parent = &*self.0.parent;
        let out = drive(
            &mut agent,
            &prompt,
            &run.stop,
            caller.as_ref(),
            parent,
            &run.id,
        )
        .await;
        self.0
            .parent
            .on_subagent(&run.id, SubagentEvent::Stopped { ok: out.ok() });
        // Leave first: from here a message for it is refused, not lost.
        seat.leave();
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
        if let Err(p) = AssertUnwindSafe(member.into_run().end(end))
            .catch_unwind()
            .await
        {
            let why = panic_message(p.as_ref()).unwrap_or("a panic");
            let log = format!("subagent {}: its run's end panicked: {why}", run.id);
            self.0.parent.deps().services().debug(&log);
        }
        SpawnOutcome { text, cost_usd }
    }
}
