//! Several handlers on one event in one mod file: they run in order by the
//! rules a `Hooks` binding's do (haest-mods' fold), and the file is ONE
//! mod, so a handler that fails fails the file's event whole.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use haest_core::{
    NoHooks, Tools,
    error::BoxError,
    testing::{ScriptedProvider, text_answer},
    workspace::WorkspaceMode,
};
use haest_mods::{Hooks, ModelBeforeDecision, ModelBeforePayload, Mods};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::testkit::*;

/// `source` loaded as the mod file `file`, alone in its [`Mods`].
async fn with_mod(ui: &Arc<TestUi>, file: &str, source: &str) -> Mods {
    let haest = env(ui.clone(), WorkspaceMode::Sandbox);
    let m = js(file, source, &haest).await.expect("the mod loads");
    Mods::new(haest, [m])
}

/// What a turn on "go" left: the user messages, the model calls made, and
/// the turn's error.
async fn turn(mods: &Mods) -> (Vec<String>, usize, Option<String>) {
    let p = ScriptedProvider::new(["a", "b"].map(text_answer));
    let mut agent = looped(&p, NoHooks, mods, Tools::new());
    let end = agent.run("go", &mut Vec::new()).await;
    let users = agent.history().iter().filter_map(user_text).collect();
    (users, p.requests().len(), end.error)
}

/// The next one sees the input the one before rewrote, every steer goes in
/// in order, and none is asked after the first deny or stop.
#[tokio::test]
async fn a_mod_files_handlers_chain_and_none_is_asked_after_the_first_deny_or_stop() {
    let source = r#"export default function (haest: any) {
      haest.on('tool.before', (call: any) => ({ input: { v: call.input.v + '!' } }))
      haest.on('tool.before', (call: any) => ({ deny: 'saw ' + call.input.v }))
      haest.on('tool.before', () => { haest.notice('asked after a deny') })
      haest.on('model.after', (v: any) => v.historyLength === 2 ? { steer: 'one' } : undefined)
      haest.on('model.after', (v: any) => v.historyLength === 2 ? { steer: 'two' } : undefined)
      haest.on('model.after', (v: any) => v.historyLength > 2 ? { stop: 'enough' } : undefined)
      haest.on('model.after', (v: any) => { if (v.historyLength > 2) haest.notice('asked after a stop') })
    }"#;
    let ui = TestUi::new();
    let mods = with_mod(&ui, "order.ts", source).await;

    let echo = Probe::new("Echo");
    let tools = wrapped(mods.clone(), Tools::from_iter([echo.clone()]));
    let refused = tools[0].execute(input(json!({ "v": "hi" })), &ctx()).await;
    assert!(
        refused.is_err_and(|e| e.to_string().contains("saw hi!")),
        "the second handler saw the first one's rewrite"
    );
    assert!(echo.ran().is_empty());

    // The steers queued between two calls go in as one message, in order,
    // and the stop after the second answer ends the turn.
    let (users, calls, error) = turn(&mods).await;
    assert_eq!((users, calls), (vec!["go".into(), "one\n\ntwo".into()], 2));
    assert!(error.is_some_and(|e| e.contains("enough")));
    assert!(ui.infos().is_empty(), "{:?}", ui.infos());
}

/// A handler that throws fails the file's event: what the handlers before
/// it decided is dropped and the ones after it are not asked, where a
/// `Hooks` binding that fails with `OnError::Continue` is reported and
/// skipped, the others' decisions kept.
#[tokio::test]
async fn a_later_handler_that_throws_drops_its_files_event_where_a_failed_binding_is_skipped() {
    let source = r#"export default function (haest: any) {
      let first = true
      haest.on('model.before', () => first ? (first = false, { steer: 'from the first' }) : undefined)
      haest.on('model.before', () => { throw new Error('boom') })
      haest.on('model.before', () => { haest.notice('asked after a throw') })
      haest.on('prompt.submit', (text: string) => text + ' rewritten')
      haest.on('prompt.submit', () => { throw new Error('bad') })
      haest.on('prompt.submit', (text: string) => { haest.notice('asked after a throw'); return text })
    }"#;
    let ui = TestUi::new();
    let mods = with_mod(&ui, "throws.ts", source).await;
    assert_eq!(mods.rewrite_prompt("hi").await, "hi");
    assert_eq!(turn(&mods).await, (vec!["go".into()], 1, None));
    assert!(ui.infos().is_empty(), "{:?}", ui.infos());
    let errors = ui.errors();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors.iter().all(|e| e.contains("throws.ts")));

    let asked = Arc::new(AtomicBool::new(false));
    let seen = asked.clone();
    let hooks = Hooks::new()
        .on_model_before(once::<ModelBeforePayload>(
            json!({ "steer": "from the first" }),
        ))
        .on_model_before(|_: ModelBeforePayload, _: CancellationToken| async {
            Err::<ModelBeforeDecision, BoxError>("boom".into())
        })
        .on_model_before(move |_: ModelBeforePayload, _: CancellationToken| {
            seen.store(true, Ordering::SeqCst);
            async { Ok::<_, BoxError>(ModelBeforeDecision::default()) }
        });
    let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Sandbox), [hooks]);
    assert_eq!(
        turn(&mods).await,
        (vec!["go".into(), "from the first".into()], 2, None)
    );
    assert!(
        asked.load(Ordering::SeqCst),
        "the binding after it was asked"
    );
}
