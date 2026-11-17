//! What a summary call's reply must be before it replaces the head of history:
//! a turn that ended on its own, with text, no tool call, and no cancel.

use super::*;
use crate::provider::{Provider, ProviderRequest, ProviderStream, ResolvedModel};

/// Cancels the run's token while the reply streams (Esc), then answers with what it had.
struct EscMidStream(ScriptedProvider);

impl Provider for EscMidStream {
    fn id(&self) -> &str {
        "esc"
    }

    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        req.cancel.cancel();
        self.0.stream(req)
    }
}

fn ended(text: &str, stop_reason: StopReason) -> AssistantMessage {
    let mut m = text_answer(text);
    m.stop_reason = stop_reason;
    m
}

/// A stream that died after `text`.
fn died(text: &str) -> AssistantMessage {
    let mut m = ended(text, StopReason::Error);
    m.error_message = Some("stream reset".into());
    m
}

/// The warm reply is `warm`, the cold one `cold`: what compaction made of them.
async fn compact_on(
    warm: AssistantMessage,
    cold: AssistantMessage,
) -> (Result<Option<CompactionResult>, CompactionError>, Agent) {
    let p = ScriptedProvider::new([warm, cold]);
    let mut agent = agent_with(&p);
    let res = compact(&mut agent, &CancellationToken::new(), &CFG).await;
    (res, agent)
}

#[tokio::test]
async fn a_cancelled_summary_is_never_committed() {
    for stop_reason in [StopReason::Aborted, StopReason::EndTurn] {
        let p = ScriptedProvider::new([ended("The user asked", stop_reason), text_answer("COLD")]);
        let mut agent = agent_with(&p);
        let esc = Arc::new(EscMidStream(p.clone()));
        agent.map_deps(|d| {
            d.with_models(Arc::new(move |_: &str| {
                Ok(ResolvedModel::new(esc.clone(), "m"))
            }))
        });
        let res = compact(&mut agent, &CancellationToken::new(), &CFG).await;
        assert!(
            matches!(res, Err(CompactionError::Failed { .. })),
            "{stop_reason:?}: {res:?}"
        );
        assert_eq!(agent.history(), history(), "{stop_reason:?}");
        assert_eq!(p.requests().len(), 1, "no second summary after the cancel");
    }
}

#[tokio::test]
async fn a_summary_stream_that_died_mid_way_is_not_a_summary() {
    let (res, _) = compact_on(died("The user wa"), text_answer("COLD")).await;
    let res = res.unwrap().unwrap();
    assert_eq!(
        (res.summary.as_str(), res.path),
        ("COLD", SummaryPath::Cold)
    );

    let (res, agent) = compact_on(died("The user wa"), died("Summ")).await;
    assert!(
        matches!(res, Err(CompactionError::Failed { .. })),
        "{res:?}"
    );
    assert_eq!(agent.history(), history());
}

#[tokio::test]
async fn a_summary_cut_off_at_its_token_limit_is_not_a_summary() {
    let cut = || ended("The user asked for", StopReason::MaxTokens);
    let (res, agent) = compact_on(cut(), cut()).await;
    assert!(
        matches!(res, Err(CompactionError::Failed { .. })),
        "{res:?}"
    );
    assert_eq!(agent.history(), history());
}

#[tokio::test]
async fn a_warm_reply_that_calls_a_tool_is_not_a_summary() {
    let mut warm = tool_answer(&[("x", "Echo", json!({}))]);
    warm.content.insert(
        0,
        AssistantPart::Text {
            text: "Let me check X first.".into(),
        },
    );
    let (res, _) = compact_on(warm.clone(), text_answer("COLD")).await;
    let res = res.unwrap().unwrap();
    assert_eq!(
        (res.summary.as_str(), res.path),
        ("COLD", SummaryPath::Cold)
    );

    let (res, agent) = compact_on(warm.clone(), warm).await;
    assert!(
        matches!(res, Err(CompactionError::Failed { .. })),
        "{res:?}"
    );
    assert_eq!(agent.history(), history());
}
