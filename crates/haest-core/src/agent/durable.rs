//! [`Agent::run_durable`]: one turn kept in a run store, so a turn stopped
//! part way goes on where it stopped ([`crate::run`]).

use std::time::Instant;

use crate::{
    agent::{Agent, control::Running, turn::Turn},
    events::{EventSink, Forward, TurnOutcome},
    messages::{Message, ToolCall},
    run::{
        Journal, ReplayedRun, RunEnd, RunError, RunKey, RunOutcome, Served, finished, input_hash,
    },
    tool::OnResume,
    tool_batch::{Redo, tool_batches},
    tool_exec::ToolIndex,
};

impl Agent {
    /// One human turn as a durable run under `key`, kept in
    /// [`SharedDeps::run_store`](crate::SharedDeps::run_store) ([`crate::run`]).
    ///
    /// | The store has for `key` | This does |
    /// |---|---|
    /// | nothing | runs the turn as [`Agent::run`] does, writing its records ([`Served::Fresh`]) |
    /// | a run with this input that never ended | resumes it ([`Served::Resumed`]) |
    /// | a run with this input that ended | returns its result, and emits nothing: no model call, no tool, no spend ([`Served::Stored`]) |
    ///
    /// A fresh or resumed turn emits its events as [`Agent::run`] does, from
    /// where it goes on: what ran before the stop is not emitted again, and
    /// its messages do not reach [`AgentHooks::on_message`](crate::AgentHooks::on_message)
    /// again. Its [`TurnEnd`](crate::LoopEvent::TurnEnd) usage, like the
    /// outcome's, includes what the run spent before the stop.
    ///
    /// The run starts on the agent's history as it is (a call a dropped turn
    /// left without a result answered first, as [`Agent::run`] answers it),
    /// and keeps only its own messages; to resume, hand the agent the history
    /// it had when the run started. A history that holds the run's messages after that is cut back
    /// to it (the run's records are the authority on them).
    ///
    /// A turn that ends on its own (an answer, a cap) is finished: a second
    /// start returns it. One that ends with an error or an abort is not: a
    /// second start goes on from where it stopped. The run's records have
    /// landed in the store when this returns.
    ///
    /// # Errors
    ///
    /// Nothing is written and no event emitted on an error:
    /// [`RunError::NoStore`] with no run store, [`RunError::InputMismatch`]
    /// when `key` was started with another input, [`RunError::HistoryMismatch`]
    /// when the agent's history does not begin with the one the run started
    /// on, [`RunError::TurnRunning`] from inside a running turn, and
    /// [`RunError::Store`] when the store cannot load the run.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "testing")]
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), haest_core::run::RunError> {
    /// use haest_core::{
    ///     Agent, AgentDeps, AgentOptions, LoopEvent, SharedDeps, ShutWorkspace, Tools,
    ///     run::{MemoryRunStore, RunError, RunKey, Served},
    ///     testing::{ScriptedProvider, text_answer},
    /// };
    ///
    /// let provider = ScriptedProvider::new([text_answer("Report sent.")]);
    /// let shared = SharedDeps::new(provider.resolver(), "/", ShutWorkspace);
    /// let deps = AgentDeps::new(shared, Tools::new(), "").with_run_store(MemoryRunStore::new());
    /// let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
    /// let key = RunKey::new("report-2026-11-16");
    /// let mut events = Vec::<LoopEvent>::new();
    ///
    /// let first = agent.run_durable(&key, "send the report", &mut events).await?;
    /// assert_eq!(first.served, Served::Fresh);
    ///
    /// // The same job again: the stored result, and the model is not asked.
    /// let again = agent.run_durable(&key, "send the report", &mut events).await?;
    /// assert_eq!((again.served, again.text.as_str()), (Served::Stored, "Report sent."));
    /// assert_eq!(provider.requests().len(), 1);
    ///
    /// // The same key for another job is refused.
    /// let other = agent.run_durable(&key, "delete the report", &mut events).await;
    /// assert!(matches!(other, Err(RunError::InputMismatch { .. })));
    /// # Ok(())
    /// # }
    /// # #[cfg(not(feature = "testing"))]
    /// # fn main() {}
    /// ```
    pub async fn run_durable(
        &mut self,
        key: &RunKey,
        input: &str,
        sink: &mut (impl EventSink + ?Sized),
    ) -> Result<RunOutcome, RunError> {
        self.run_durable_on(key, input, &mut Forward(sink)).await
    }

    /// [`run_durable`](Self::run_durable), compiled once.
    async fn run_durable_on(
        &mut self,
        key: &RunKey,
        input: &str,
        sink: &mut dyn EventSink,
    ) -> Result<RunOutcome, RunError> {
        let store = self
            .deps
            .shared()
            .run_store()
            .cloned()
            .ok_or(RunError::NoStore)?;
        let started = Instant::now();
        let cancel = self
            .control
            .begin()
            .map_err(|_running| RunError::TurnRunning)?;
        let _running = Running(self.control.clone());
        let given = input_hash(input);
        let stored = (store.load(key).await).map_err(|source| RunError::Store {
            key: key.clone(),
            source,
        })?;
        if let Some(run) = &stored {
            if run.input_hash != given {
                return Err(RunError::InputMismatch {
                    key: key.clone(),
                    stored: run.input_hash.clone(),
                    given,
                });
            }
            if let Some(end) = &run.end {
                return Ok(RunOutcome::served(end.clone(), Served::Stored));
            }
            self.check_base(run)?;
        }
        // Spent between turns (a compaction the human asked for) belongs to no turn.
        self.take_side_spend();
        let journal = Journal::new(key.clone(), store.sink(key));
        let mut guard = self.guard();
        let restored = stored.as_ref().and_then(|r| r.turn.clone());
        if let Some(state) = &restored {
            guard.restore(state);
        }
        let usage = restored.map(|s| s.usage).unwrap_or_default();
        let mut turn = Turn::new(cancel, guard, sink).journaled(journal.clone(), usage);
        let (base, served, end) = match stored {
            None => {
                // A resumed run answers its own pending calls (`resume`);
                // a fresh one starts on a history every provider takes.
                self.answer_dropped_calls();
                let base = self.history.len();
                journal.started(&given, base);
                self.add_user(input, &mut turn).await;
                (base, Served::Fresh, self.steps(&mut turn).await)
            }
            Some(run) => (
                run.base,
                Served::Resumed,
                self.resume(run, input, &mut turn).await,
            ),
        };
        let end = turn.finish(end, started);
        let end = RunEnd {
            stop_reason: end.stop_reason,
            error: end.error,
            text: self.run_text(base),
            usage: turn.usage().clone(),
            limit: end.limit,
        };
        if finished(end.stop_reason) {
            journal.ended(end.clone());
        }
        journal.flush().await;
        Ok(RunOutcome::served(end, served))
    }

    /// The agent's history must begin with the one `run` started on.
    fn check_base(&self, run: &ReplayedRun) -> Result<(), RunError> {
        let found = self.history.len();
        let fits = found == run.base
            || (found > run.base && run.messages.first() == Some(&self.history[run.base]));
        if fits {
            return Ok(());
        }
        Err(RunError::HistoryMismatch {
            key: run.key.clone(),
            base: run.base,
            found,
        })
    }

    /// Go on with `run` from its last record: answer the calls that have no
    /// result, then the step loop as usual.
    async fn resume(&mut self, run: ReplayedRun, input: &str, t: &mut Turn<'_>) -> TurnOutcome {
        self.history.truncate(run.base);
        let Some(last) = run.messages.last().cloned() else {
            // The start landed and the input did not: the turn never began.
            self.add_user(input, t).await;
            return self.steps(t).await;
        };
        self.history.extend(run.messages);
        // A resumed turn says nothing to the model about the stop. A note
        // telling it would go in here, after the redone calls' results.
        if !run.pending.is_empty() {
            let (redo, rest) = self.redo_plan(&run.pending);
            self.run_tools(&redo, &rest, t).await;
            self.after_tools(t).await;
        } else if let Message::Assistant(a) = &last
            && a.tool_calls().next().is_none()
            && finished(a.stop_reason)
        {
            // The last answer landed and the run's end did not: the turn was over.
            return TurnOutcome::new(a.stop_reason);
        }
        self.steps(t).await
    }

    /// What to do with each call that has no result. Results land in call
    /// order and the loop flushes before every batch with side effects, so the
    /// first batch of `pending` may have run (its calls follow their tool's
    /// [`OnResume`]); a later batch with side effects cannot have started, and
    /// a later read-only one is safe to run again: they run as usual.
    fn redo_plan(&self, pending: &[ToolCall]) -> (Vec<Redo>, Vec<ToolCall>) {
        let tools = ToolIndex::new(self.deps.tools().callable());
        let first = tool_batches(pending, &tools).first().map_or(0, Vec::len);
        let redo = pending[..first]
            .iter()
            .map(|call| Redo {
                call: call.clone(),
                // An unknown tool runs nothing: running it again answers that.
                how: tools
                    .get(&call.name)
                    .map_or(OnResume::Rerun, |t| t.on_resume()),
            })
            .collect();
        (redo, pending[first..].to_vec())
    }

    /// The text of the run's last answer. The run's messages start at `base`
    /// unless a compaction since cut the history shorter.
    fn run_text(&self, base: usize) -> String {
        let run = self.history.get(base..).unwrap_or(&self.history);
        run.iter()
            .rev()
            .find_map(|m| match m {
                Message::Assistant(a) => Some(a.text()),
                _ => None,
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests;
