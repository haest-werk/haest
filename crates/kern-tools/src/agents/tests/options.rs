//! The Agent tool's host options: the host's kinds of subagent and its `model` text.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kern_core::tool::{Tool, ToolError};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{
    agents::{
        AgentDirectory, AgentRef, AgentToolOptions, SpawnInput, SpawnOutcome, SubagentTypes,
        agent_tools, agent_tools_with,
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
    ) -> Result<SpawnOutcome, String> {
        self.0.lock().unwrap().push(input);
        Ok(SpawnOutcome {
            text: "done".into(),
            cost_usd: 0.0,
        })
    }
    fn deliver(&self, _from: &str, _to: &str, _text: &str) -> Result<String, String> {
        Ok(String::new())
    }
    fn stop(&self, _name: &str) -> bool {
        false
    }
}

fn agent(dir: &Arc<Recording>, opts: AgentToolOptions) -> Arc<dyn Tool> {
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
        models: Some("one of: a:x, b:y".into()),
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
