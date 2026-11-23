//! A host's own prompt-cache policy, against a scripted model: no network,
//! no keys. `cargo run -p haest --example cache_policy`; `cargo test` runs it
//! too.
//!
//! A coding agent whose system prompt is long and never changes, and whose
//! human often steps away for longer than five minutes. Its policy caches
//! the system prompt for an hour, so a return after a coffee still reads it
//! from the cache, and the conversation (the newest message, the rolling
//! breakpoint) for five minutes, the cheaper write, since each turn writes
//! it again anyway.
//!
//! haest checks every plan before it is sent: here the hour comes before the
//! five minutes, as the provider requires, so the plan goes out as asked.

#![expect(
    clippy::print_stdout,
    reason = "an example shows on the terminal what it does"
)]

use haest::{
    Agent, AgentDeps, AgentOptions, BoxError, CachePlan, CachePolicy, CacheTtl, LoopEvent,
    RequestShape, SharedDeps, ShutWorkspace, Tools,
    core::testing::{ScriptedProvider, text_answer},
};

/// The system prompt for an hour, the conversation for five minutes.
struct LongSystem;

impl CachePolicy for LongSystem {
    fn plan(&self, request: &RequestShape<'_>) -> CachePlan {
        println!(
            "planning a request of {} messages to {}",
            request.messages.len(),
            request.model_spec
        );
        CachePlan::none()
            .with_system(CacheTtl::OneHour)
            .with_history(CacheTtl::FiveMinutes)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), BoxError> {
    run().await
}

#[tokio::test]
async fn the_system_prompt_is_cached_for_an_hour_and_the_tail_for_five_minutes()
-> Result<(), BoxError> {
    run().await
}

async fn run() -> Result<(), BoxError> {
    let provider = ScriptedProvider::new([
        text_answer("The parser lives in src/parse.rs."),
        text_answer("It reads one token ahead."),
    ]);
    let shared = SharedDeps::new(provider.resolver(), "/work", ShutWorkspace);
    let system = "You are a careful coding agent. ".repeat(200);
    let deps = AgentDeps::new(shared, Tools::new(), system);
    let opts = AgentOptions::new("scripted:coder").with_cache_policy(LongSystem);
    let mut agent = Agent::new(deps, opts);

    let mut sink = |ev: LoopEvent| {
        if let LoopEvent::TextDelta { text } = ev {
            println!("model: {text}");
        }
    };
    for question in ["Where is the parser?", "How does it look ahead?"] {
        let end = agent.run(question, &mut sink).await;
        assert!(end.error.is_none(), "{end:?}");
    }

    let planned = CachePlan::none()
        .with_system(CacheTtl::OneHour)
        .with_history(CacheTtl::FiveMinutes);
    for request in provider.requests() {
        println!(
            "sent: system {:?}, newest message {:?}",
            request.cache.system, request.cache.history
        );
        assert_eq!(request.cache, planned);
    }
    assert_eq!(agent.last_cache_plan(), Some(&planned));
    Ok(())
}
