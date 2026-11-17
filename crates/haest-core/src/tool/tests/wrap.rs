//! A wrapped tool is the wrapped one in every part the wrapper leaves alone.

use super::*;
use crate::workspace::ShutWorkspace;

/// A tool whose every part differs from `Tool`'s defaults.
struct Pay;

#[async_trait]
impl Tool for Pay {
    fn name(&self) -> &str {
        "Pay"
    }
    fn description(&self) -> String {
        "Send a payment.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"to": {"type": "string"}}})
    }
    fn kind(&self) -> &str {
        "payment.send"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn on_resume(&self) -> OnResume {
        OnResume::Lookup
    }
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        let mut out = input.clone();
        out.insert("canonical".into(), json!(true));
        out
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("paid"))
    }
}

/// A wrapper that says nothing of its own.
struct Bare(ToolRef);

impl ToolWrapper for Bare {
    fn inner(&self) -> &ToolRef {
        &self.0
    }
}

/// Every part of a tool, as the loop and a host read it.
#[derive(Debug, Clone, PartialEq)]
struct Parts {
    name: String,
    description: String,
    parameters: Value,
    kind: String,
    read_only: bool,
    on_resume: OnResume,
    canonical: JsonMap,
    output: String,
}

async fn parts(tool: &ToolRef) -> Parts {
    let input = map(json!({"to": "a"}));
    let ctx = ToolContext::new("/", ShutWorkspace);
    let out = tool.execute(input.clone(), &ctx).await.unwrap();
    Parts {
        name: tool.name().into(),
        description: tool.description(),
        parameters: tool.parameters(),
        kind: tool.kind().into(),
        read_only: tool.read_only(),
        on_resume: tool.on_resume(),
        canonical: tool.canonical_input(&input),
        output: out.content,
    }
}

#[tokio::test]
async fn a_wrapper_that_changes_nothing_is_the_wrapped_tool_in_every_part() {
    let pay = ToolRef::new(Pay);
    let wrapped = ToolRef::new(Bare(pay.clone()));
    assert_eq!(parts(&wrapped).await, parts(&pay).await);
}

#[tokio::test]
async fn an_override_changes_what_the_model_reads_and_nothing_else() {
    let schema = json!({"type": "object", "properties": {"payee": {"type": "string"}}});
    let changed = ToolRef::new(
        ToolOverride::new(Pay)
            .with_name("Transfer")
            .with_description("Move money.")
            .with_parameters(schema.clone()),
    );
    let want = Parts {
        name: "Transfer".into(),
        description: "Move money.".into(),
        parameters: schema,
        ..parts(&ToolRef::new(Pay)).await
    };
    assert_eq!(parts(&changed).await, want);
}

#[tokio::test]
async fn an_override_of_one_part_keeps_the_others() {
    let described = ToolRef::new(ToolOverride::new(Pay).with_description("Move money."));
    let want = Parts {
        description: "Move money.".into(),
        ..parts(&ToolRef::new(Pay)).await
    };
    assert_eq!(parts(&described).await, want);
}
