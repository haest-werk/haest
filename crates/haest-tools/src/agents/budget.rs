//! SUBAGENT BUDGETS, opt-in (`subagent_budgets`; `docs/plans/budgets.md` S):
//! the Agent tool's `budget {usd, minutes}`. Each value only tightens the
//! caps; the time must fit what is left of the caller's turn; the USD is held
//! on the conversation's ledger while the subagent runs, so subagents running
//! together fit the budget together. Every fit decision is here.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use haest_core::{
    ledger::{Hold, Ledger},
    tool::{JsonMap, ToolError},
};
use serde_json::{Value, json};

use super::{AgentError, run::wall_clock};

/// One subagent's budget: its spend and its wall clock. 0 (or a zero
/// duration) is no cap. Minutes exist only in the Agent tool's parameter.
///
/// # Examples
///
/// A host's own directory gets the call's budget already within its caps
/// ([`AgentToolOptions::budget_caps`](super::AgentToolOptions::budget_caps)).
///
/// ```
/// use std::{sync::{Arc, Mutex}, time::Duration};
///
/// use async_trait::async_trait;
/// use haest_core::{tool::ToolContext, workspace::ShutWorkspace};
/// use haest_tools::{
///     AgentDirectory, AgentError, AgentRef, AgentToolOptions, SpawnInput, SpawnOutcome,
///     SubagentBudget, agent_tools_with,
/// };
/// use serde_json::json;
/// use tokio_util::sync::CancellationToken;
///
/// #[derive(Default)]
/// struct Remote(Mutex<Option<SubagentBudget>>);
///
/// #[async_trait]
/// impl AgentDirectory for Remote {
///     async fn spawn(&self, _: &AgentRef, input: SpawnInput, _: CancellationToken) -> Result<SpawnOutcome, AgentError> {
///         *self.0.lock().unwrap() = input.budget; // the host runs it under this
///         Ok(SpawnOutcome { text: "done".into(), cost_usd: 0.0 })
///     }
///     fn deliver(&self, _: &str, to: &str, _: &str) -> Result<String, AgentError> {
///         Err(AgentError::Refused(format!("{to} is not running")))
///     }
///     fn stop(&self, _: &str) -> bool {
///         false
///     }
/// }
///
/// # #[tokio::main]
/// # async fn main() {
/// let remote = Arc::new(Remote::default());
/// let caps = SubagentBudget { usd: 0.5, time: Duration::from_secs(300) };
/// let opts = AgentToolOptions { budget_caps: Some(caps), ..Default::default() };
/// let agent = agent_tools_with(remote.clone(), AgentRef::main(), true, opts).remove(0);
/// assert!(agent.parameters()["properties"].get("budget").is_some());
///
/// // The model asks for more money than the cap and less time: it gets the cap and its time.
/// let call = json!({"description": "d", "prompt": "p", "budget": {"usd": 9, "minutes": 2}});
/// let ctx = ToolContext::new("/", Arc::new(ShutWorkspace));
/// agent.execute(call.as_object().unwrap().clone(), &ctx).await.unwrap();
/// let got = remote.0.lock().unwrap().unwrap();
/// assert_eq!(got, SubagentBudget { usd: 0.5, time: Duration::from_secs(120) });
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubagentBudget {
    /// Its spend in USD.
    pub usd: f64,
    /// Its wall clock.
    pub time: Duration,
}

/// The `budget` parameter, as the model reads it.
pub(super) fn schema(caps: &SubagentBudget) -> Value {
    let cap = |on: bool, text: String| if on { text } else { "no cap".into() };
    let usd = cap(caps.usd > 0.0, format!("${}", caps.usd));
    let time = cap(!caps.time.is_zero(), wall_clock(caps.time));
    json!({
        "type": "object",
        "description": format!("Tighter caps for this subagent (optional): at most {usd} and {time}; an absent value is the cap. Its spend is held from the conversation's budget while it runs, and its time must fit what is left of your turn; a spawn that does not fit is refused with what is left."),
        "properties": {
            "usd": {"type": "number", "description": "Its spend, in USD"},
            "minutes": {"type": "number", "description": "Its wall clock, in minutes"}
        }
    })
}

/// `asked` within `cap`: a cap of 0 (no cap) leaves `asked` as it is; no
/// `asked` is the cap.
fn tighten(asked: Option<f64>, cap: f64) -> f64 {
    match asked {
        Some(a) if cap > 0.0 => a.min(cap),
        Some(a) => a,
        None => cap,
    }
}

/// `budget` within `caps`, value by value; a 0 in `budget` is absent: the cap.
pub(super) fn clamp(budget: SubagentBudget, caps: &SubagentBudget) -> SubagentBudget {
    let usd = (budget.usd > 0.0).then_some(budget.usd);
    let secs = (!budget.time.is_zero()).then_some(budget.time.as_secs_f64());
    let time = tighten(secs, caps.time.as_secs_f64());
    SubagentBudget {
        usd: tighten(usd, caps.usd),
        time: Duration::try_from_secs_f64(time).unwrap_or(budget.time),
    }
}

/// A positive number under `budget.<key>`, or None when absent.
fn positive(budget: &JsonMap, key: &str) -> Result<Option<f64>, ToolError> {
    match budget.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() && n > 0.0 => Ok(Some(n)),
            _ => Err(ToolError::Input(format!(
                "budget.{key} must be a number above 0"
            ))),
        },
    }
}

/// The call's `budget`, clamped to `caps`: the caps themselves when it
/// names none.
pub(super) fn requested(
    input: &JsonMap,
    caps: &SubagentBudget,
) -> Result<SubagentBudget, ToolError> {
    let budget = match input.get("budget") {
        None | Some(Value::Null) => JsonMap::new(),
        Some(Value::Object(b)) => b.clone(),
        Some(_) => {
            return Err(ToolError::Input(
                "budget must be an object: {\"usd\": ..., \"minutes\": ...}".into(),
            ));
        }
    };
    let minutes = positive(&budget, "minutes")?;
    let asked = SubagentBudget {
        usd: positive(&budget, "usd")?.unwrap_or(0.0),
        time: minutes
            .and_then(|m| Duration::try_from_secs_f64(m * 60.0).ok())
            .unwrap_or(Duration::ZERO),
    };
    Ok(clamp(asked, caps))
}

/// Open question Q6, decided here and only here: a subagent whose time does
/// not fit what is left of its caller's turn is REFUSED with what is left
/// (agent-core's rule), never shrunk to fit. One with no time cap fits only a
/// turn with none.
pub(super) fn fit_time(
    budget: &SubagentBudget,
    left: Option<Duration>,
    setting: &'static str,
) -> Result<(), AgentError> {
    match left {
        Some(left) if budget.time.is_zero() || budget.time > left => Err(AgentError::OverTime {
            asked: budget.time,
            left,
            setting,
        }),
        _ => Ok(()),
    }
}

/// A subagent's USD, held on the conversation's ledger until dropped.
pub(super) struct HeldBudget {
    ledger: Arc<Mutex<Ledger>>,
    hold: Option<Hold>,
}

impl Drop for HeldBudget {
    fn drop(&mut self) {
        if let Some(hold) = self.hold.take() {
            self.ledger.lock().unwrap().release(hold);
        }
    }
}

/// Hold `budget.usd` on `ledger` for as long as the subagent runs; refused
/// with what is left when it does not fit. No ledger, nothing to hold. One
/// with no spend cap fits only a ledger with no USD cap.
pub(super) fn hold(
    ledger: Option<&Arc<Mutex<Ledger>>>,
    budget: &SubagentBudget,
) -> Result<Option<HeldBudget>, AgentError> {
    let Some(ledger) = ledger else {
        return Ok(None);
    };
    let mut l = ledger.lock().unwrap();
    if budget.usd <= 0.0
        && let Some(available) = l.available()
    {
        return Err(AgentError::OverBudget {
            asked: 0.0,
            available,
        });
    }
    let hold = l.reserve(budget.usd).map_err(|e| AgentError::OverBudget {
        asked: e.asked,
        available: e.available,
    })?;
    drop(l);
    Ok(Some(HeldBudget {
        ledger: ledger.clone(),
        hold: Some(hold),
    }))
}

/// [`AgentError::OverBudget`]'s text.
pub(super) fn over_budget(asked: f64, available: f64) -> String {
    let left =
        format!("${available:.2} is left of the conversation's budget (max_conversation_usd)");
    if asked <= 0.0 {
        format!(
            "a subagent with no spend cap does not fit: {left}; give it a budget of at most that"
        )
    } else {
        format!("a subagent's ${asked} does not fit: {left}; ask for less")
    }
}

/// [`AgentError::OverTime`]'s text.
pub(super) fn over_time(asked: Duration, left: Duration, setting: &str) -> String {
    let left = format!("{} is left of this turn ({setting})", wall_clock(left));
    if asked.is_zero() {
        format!(
            "a subagent with no time cap does not fit: {left}; give it a budget of at most that"
        )
    } else {
        format!(
            "a subagent's {} does not fit: {left}; ask for less",
            wall_clock(asked)
        )
    }
}
