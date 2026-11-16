//! One subagent's run inside the hub: build its agent, drive its turn, hand a
//! background answer back.

use std::sync::Arc;

use haest_core::{Agent, AgentDeps, AgentOptions, tool::Tool};
use tokio_util::sync::CancellationToken;

use super::SubagentHub;
use crate::agents::{
    SpawnInput, SpawnOutcome, SubagentEvent,
    members::Status,
    run::{ChildFacts, ChildHooks, ChildPrompt, Run, drive, subagent_note},
};

impl SubagentHub {
    fn report_activity(&self) {
        let ids: Vec<String> = {
            let members = self.members.lock().unwrap();
            members.running().map(|m| m.id.clone()).collect()
        };
        self.parent.on_activity(&ids);
    }

    /// The agent for `run`: the conversation's deps with its own tools (its
    /// kind's, [`SubagentParent::subagent_tools`](crate::SubagentParent::subagent_tools)),
    /// prompt and hooks. No task tools: the task list belongs to the
    /// conversation, not to a helper.
    pub(super) fn build(&self, run: &Run, input: &SpawnInput) -> Agent {
        let mut tools: Vec<Arc<dyn Tool>> = self
            .parent
            .subagent_tools(input.subagent_type.as_deref())
            .into_iter()
            .filter(|t| crate::builtin_word(t.name()) != Some("tasks"))
            .collect();
        tools.extend(self.tools_for(&run.me));
        let source = self.parent.tool_source(tools);
        let (main_model, effort) = self.parent.default_model();
        let model = input.model_spec.clone().unwrap_or(main_model);
        let base = self.parent.deps();
        let deps = AgentDeps {
            tools: source.clone(),
            system_prompt: Arc::new(ChildPrompt {
                parent: self.parent.clone(),
                model_spec: model.clone(),
                tools: source,
                note: subagent_note(&run.me.name, &run.parent, &run.description, &self.caps),
            }),
            hooks: Arc::new(ChildHooks {
                hooks: base.hooks.clone(),
                parent: self.parent.clone(),
                id: run.id.clone(),
            }),
            facts: Arc::new(ChildFacts {
                facts: base.facts.clone(),
                caps: self.caps,
            }),
            ..base
        };
        let mut opts = AgentOptions::new(model);
        opts.effort = effort;
        Agent::new(deps, opts)
    }

    /// Run `agent`'s one turn on `prompt`, reporting it to the parent; the
    /// answer, with any message that arrived after it finished appended unread.
    pub(super) async fn run_member(
        &self,
        run: Run,
        mut agent: Agent,
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
            .on_subagent(&run.id, SubagentEvent::Stopped { ok: out.ok });
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
        SpawnOutcome {
            text: format!("{}{unread}", out.text),
            cost_usd: out.cost_usd,
        }
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
