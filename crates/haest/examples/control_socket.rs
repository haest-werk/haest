//! A conversation on a control socket, the way `haest ctl` meets one: the
//! host serves its session on a Unix socket in a dir it names; another
//! program lists what runs there, follows one's stream, and (the socket
//! being a command one) sends it input. When the conversation ends, the
//! socket and its discovery file go.
//!
//! `cargo run -p haest --example control_socket --features transport-unix`
//!
//! The model is scripted, so nothing here calls a provider.

use haest::BoxError;

#[cfg(unix)]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), BoxError> {
    use std::sync::Arc;

    use haest::{
        Agent, AgentDeps, AgentOptions, LoopEvent, ShutWorkspace, Tools,
        core::testing::{ScriptedProvider, text_answer},
        session::{Backend, Body, Hub, HubOptions, start},
        transport::{Grant, Transport, UnixSocket, connect_session, running_sessions},
        wire::{Origin, Request},
    };

    let model = ScriptedProvider::new([text_answer("Noted.")]);
    let deps = AgentDeps::new(model.resolver(), Tools::new(), "", "/", ShutWorkspace);
    let agent = Agent::new(deps, AgentOptions::new("scripted:m"));
    let session = start(agent, Hub::new("example-1", HubOptions::default()));

    // The host: its session on a socket, input from it another program's.
    let dir = std::env::temp_dir().join(format!("haest-ex-{}", std::process::id()));
    let backend: Arc<dyn Backend> = Arc::new(session);
    let attached = UnixSocket::new(&dir).attach(backend.clone(), Grant::command(Origin::Agent))?;

    // Another program: what runs here?
    for found in running_sessions(&dir).await {
        println!(
            "{}  {}  pid {}",
            found.discovery.sid,
            found.model_spec(),
            found.discovery.pid
        );
    }

    // It follows the stream, and sends a turn on a second connection.
    let mut watcher = connect_session(&dir, "example").await?;
    let follow = Request::Events {
        after: 0,
        follow: true,
    };
    watcher.call(follow, |_| {}).await?;
    let mut driver = connect_session(&dir, "example-1").await?;
    let turn = Request::Turn {
        text: "the build is green".into(),
    };
    let end = driver
        .call(turn, |_| {})
        .await?
        .ok_or("a turn's reply carries how it ended")?;
    println!("the turn ended: {}", end["stopReason"]);

    // The watcher saw it.
    while let Some(frame) = watcher.recv().await {
        if let Body::Loop(LoopEvent::Assistant { message }) = frame?.body {
            println!("the watcher saw: {}", message.text());
            break;
        }
    }

    // The conversation ends: the transport ends, its files go.
    backend.close().await;
    attached.ended().await;
    assert!(running_sessions(&dir).await.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[cfg(not(unix))]
fn main() -> Result<(), BoxError> {
    Ok(())
}

/// `cargo test` runs the example, so a change that breaks it fails the suite.
#[test]
fn the_example_runs() -> Result<(), BoxError> {
    main()
}
