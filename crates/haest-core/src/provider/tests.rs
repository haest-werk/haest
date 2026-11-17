use super::*;
use crate::{
    messages::StopReason,
    testing::{ScriptedProvider, text_answer},
};

/// A provider whose stream ends without its answer.
struct Mute;

impl Provider for Mute {
    fn id(&self) -> &str {
        "mute"
    }
    fn stream(&self, _: ProviderRequest) -> ProviderStream {
        Box::pin(futures::stream::empty())
    }
}

#[test]
fn one_provider_serves_every_spec_by_its_model_part() {
    let provider = ProviderRef::new(ScriptedProvider::new([]));
    let resolved = provider.resolve("anything:x/y:free").unwrap();
    assert_eq!(resolved.model(), "x/y:free");
    assert!(ProviderRef::ptr_eq(resolved.provider(), &provider));
    assert!(matches!(
        provider.resolve("no-colon"),
        Err(ModelError::BadSpec { .. })
    ));
}

#[test]
fn one_model_serves_every_spec_as_itself() {
    let fixed = ResolvedModel::new(ScriptedProvider::new([]), "m-1");
    for spec in ["a:b", "no-colon", ""] {
        assert_eq!(fixed.resolve(spec).unwrap().model(), "m-1", "{spec}");
    }
}

#[tokio::test]
async fn a_completion_asks_its_model_and_is_the_streams_answer() {
    let scripted = ScriptedProvider::new([text_answer("Tides")]);
    let model = ResolvedModel::new(scripted.clone(), "titles-1");
    let mut deltas = Vec::new();
    let req = ProviderRequest::new("ignored", vec![Message::user("A title?")]);
    let answer = model.complete_streaming(req, |ev| deltas.push(ev)).await;
    assert_eq!(answer.text(), "Tides");
    assert_eq!(scripted.requests()[0].model, "titles-1");
    assert!(!deltas.iter().any(|ev| matches!(ev, ProviderEvent::Done(_))));
}

#[tokio::test]
async fn a_stream_that_ends_without_its_answer_completes_as_an_error() {
    let model = ResolvedModel::new(Mute, "m");
    let answer = model
        .complete(ProviderRequest::new("m", vec![Message::user("hi")]))
        .await;
    assert_eq!(answer.stop_reason, StopReason::Error);
    assert_eq!(answer.error_message.as_deref(), Some(NO_RESPONSE));
    assert_eq!(
        (answer.provider.as_str(), answer.model.as_str()),
        ("mute", "m")
    );
}

#[test]
fn a_spec_splits_on_the_first_colon_only() {
    assert_eq!(
        parse_model_spec("openrouter:x/y:free").unwrap(),
        ("openrouter", "x/y:free")
    );
    for bad in ["no-provider", ":model"] {
        assert!(matches!(
            parse_model_spec(bad),
            Err(ModelError::BadSpec { spec }) if spec == bad
        ));
    }
}
