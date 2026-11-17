//! The Agent tool's host options: the host's kinds of subagent, its `model`
//! text, and budget caps (whose time must fit what is left of the turn).

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use haest_core::{
    ToolRef,
    tool::{ToolContext, ToolError},
    workspace::ShutWorkspace,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::support::{Rig, call, explore_or_general, now, say};
use crate::{
    agents::{
        AgentDirectory, AgentError, AgentRef, AgentToolOptions, SpawnInput, SpawnOutcome,
        SubagentBudget, SubagentCaps, SubagentTypes, agent_tools, agent_tools_with,
    },
    testkit::{input, shut_ctx},
};

/// Keeps every spawn it is asked for.
#[derive(Default)]
struct Recording(Mutex<Vec<SpawnInput>>);

#[async_trait]
impl AgentDirectory for Recording {
    async fn spawn(
        &self,
        _from: &AgentRef,
        input: SpawnInput,
        _cancel: CancellationToken,
    ) -> Result<SpawnOutcome, AgentError> {
        self.0.lock().unwrap().push(input);
        Ok(SpawnOutcome {
            text: "done".into(),
            cost_usd: 0.0,
        })
    }
    fn deliver(&self, _from: &str, _to: &str, _text: &str) -> Result<String, AgentError> {
        Ok(String::new())
    }
    fn stop(&self, _name: &str) -> bool {
        false
    }
}

fn agent(dir: &Arc<Recording>, opts: AgentToolOptions) -> ToolRef {
    agent_tools_with(dir.clone(), AgentRef::main(), true, opts)
        .into_iter()
        .find(|t| t.name() == "Agent")
        .unwrap()
}

fn kinds() -> AgentToolOptions {
    AgentToolOptions {
        types: Some(SubagentTypes {
            names: vec!["explore".into(), "general".into()],
            description: "explore only reads".into(),
            default: "explore".into(),
        }),
        model_description: Some("one of: a:x, b:y".into()),
        budget_caps: None,
    }
}

#[tokio::test]
async fn the_hosts_kinds_reach_the_model_and_the_spawn_with_its_default_and_a_refusal() {
    let dir = Arc::new(Recording::default());
    let tool = agent(&dir, kinds());
    let p = tool.parameters();
    assert_eq!(
        p["properties"]["subagent_type"]["enum"],
        json!(["explore", "general"])
    );
    assert_eq!(p["properties"]["model"]["description"], "one of: a:x, b:y");

    let c = shut_ctx();
    let brief = |kind: Option<&str>| {
        let mut v = json!({"description": "look", "prompt": "find x"});
        if let Some(k) = kind {
            v["subagent_type"] = json!(k);
        }
        input(v)
    };
    tool.execute(brief(None), &c).await.unwrap();
    tool.execute(brief(Some("general")), &c).await.unwrap();
    let wrong = tool.execute(brief(Some("admin")), &c).await;
    assert!(matches!(wrong, Err(ToolError::Input(_))));
    let seen: Vec<_> = dir
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.subagent_type.clone())
        .collect();
    assert_eq!(seen, [Some("explore".into()), Some("general".into())]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hubs_agent_tool_offers_the_parents_kinds_and_each_kind_gets_its_own_tools() {
    let main = now(|_, n| match n {
        0 => vec![call(
            "a",
            "Agent",
            json!({"description": "look", "prompt": "x"}),
        )],
        1 => vec![call(
            "b",
            "Agent",
            json!({"description": "fix", "prompt": "y", "subagent_type": "general"}),
        )],
        _ => say("ok"),
    });
    let mut r = Rig::with_kinds(
        main,
        now(|_, _| say("done")),
        SubagentCaps::default(),
        Some(explore_or_general()),
    );
    let agent = r.hub.tools_for(&AgentRef::main())[0].clone();
    assert_eq!(
        agent.parameters()["properties"]["subagent_type"]["enum"],
        json!(["explore", "general"])
    );

    r.turn("go").await;
    let seen = r.provider_sub_tools();
    assert_eq!(seen.len(), 2, "{seen:?}");
    // The default kind, explore, has no tools of the parent's; general has them.
    assert!(!seen[0].contains(&"Glob".to_string()), "{seen:?}");
    assert!(seen[1].contains(&"Glob".to_string()), "{seen:?}");
    // Neither gets the conversation's task list.
    assert!(seen.iter().flatten().all(|t| t != "TaskCreate"));
}

#[tokio::test]
async fn with_no_kinds_there_is_no_subagent_type_at_all() {
    let dir = Arc::new(Recording::default());
    let tool = agent_tools(dir.clone(), AgentRef::main(), true)
        .into_iter()
        .find(|t| t.name() == "Agent")
        .unwrap();
    assert!(
        tool.parameters()["properties"]
            .get("subagent_type")
            .is_none()
    );
    let v = input(json!({"description": "look", "prompt": "x", "subagent_type": "explore"}));
    tool.execute(v, &shut_ctx()).await.unwrap();
    assert_eq!(dir.0.lock().unwrap()[0].subagent_type, None);
}

fn agent_tool(dir: &Arc<Recording>, caps: SubagentBudget) -> ToolRef {
    let opts = AgentToolOptions {
        budget_caps: Some(caps),
        ..Default::default()
    };
    agent_tools_with(dir.clone(), AgentRef::main(), true, opts)[0].clone()
}

#[tokio::test]
async fn a_childs_time_must_fit_what_is_left_of_its_callers_turn() {
    let dir = Arc::new(Recording::default());
    let quarter_hour = SubagentBudget {
        usd: 1.0,
        time: Duration::from_secs(15 * 60),
    };
    let tool = agent_tool(&dir, quarter_hour);
    let mut ctx = ToolContext::new("/", Arc::new(ShutWorkspace));
    ctx.turn_time_left = Some(Duration::from_secs(120));
    let ask = |budget: Value| input(json!({"description": "d", "prompt": "p", "budget": budget}));
    let too_long = AgentError::OverTime {
        asked: Duration::from_secs(300),
        left: Duration::from_secs(120),
        setting: "max_turn_minutes",
    };
    let refused = tool.execute(ask(json!({"minutes": 5})), &ctx).await;
    assert!(matches!(refused, Err(ToolError::Input(t)) if t == too_long.to_string()));
    // No minutes is the cap: 15 minutes do not fit 2 either.
    assert!(tool.execute(ask(json!({})), &ctx).await.is_err());
    tool.execute(ask(json!({"minutes": 1})), &ctx)
        .await
        .unwrap();
    // A turn with no time cap fits any child.
    ctx.turn_time_left = None;
    tool.execute(ask(json!({})), &ctx).await.unwrap();
    let times: Vec<_> = (dir.0.lock().unwrap().iter())
        .map(|s| s.budget.unwrap().time)
        .collect();
    assert_eq!(times, [Duration::from_secs(60), quarter_hour.time]);
    // With no time cap of its own, a child fits only a turn with none.
    let uncapped = agent_tool(
        &dir,
        SubagentBudget {
            time: Duration::ZERO,
            ..quarter_hour
        },
    );
    ctx.turn_time_left = Some(Duration::from_secs(120));
    assert!(uncapped.execute(ask(json!({})), &ctx).await.is_err());
}
