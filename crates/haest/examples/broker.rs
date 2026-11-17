//! A conversation linked to a broker, the way a fleet view elsewhere watches
//! (or drives) it: haest dials out over a WebSocket, says `hello` with its
//! token, and waits for the broker's ack (`events` with the last seq it
//! holds) before it sends anything. Here the broker lives in this process.
//!
//! The broker checks the token, acks from the start, sends a turn (this host
//! gave the link command access; watch is the default), reads its frames up
//! to the reply, and cuts the link. While it is down the host runs a turn of
//! its own. haest dials again; the broker acks with the last seq it holds
//! and gets what came after, nothing twice. When the conversation ends, the
//! link does.
//!
//! `cargo run -p haest --example broker --features transport-broker`
//!
//! The model is scripted, so nothing here calls a provider.

use std::{sync::Arc, time::Duration};

use futures::{SinkExt, StreamExt};
use haest::{
    Agent, AgentDeps, AgentOptions, ShutWorkspace, Tools,
    core::testing::{ScriptedProvider, text_answer},
    session::{Backend, Hub, HubOptions, Input, start},
    transport::{Backoff, Broker, Grant, Transport},
    wire::Origin,
};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

/// The broker's end of one link.
struct Side(WebSocketStream<TcpStream>);

impl Side {
    async fn send(&mut self, request: Value) {
        self.0
            .send(Message::Text(request.to_string().into()))
            .await
            .unwrap();
    }

    /// The next frame; None once the link closed.
    async fn next(&mut self) -> Option<Value> {
        while let Some(msg) = self.0.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    return Some(serde_json::from_str(text.as_str()).unwrap());
                }
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
        None
    }

    /// Frames until `stop` matches one; the last stream seq seen.
    async fn until(&mut self, stop: impl Fn(&Value) -> bool, mut last: u64) -> u64 {
        loop {
            let frame = self.next().await.expect("the link is open");
            println!("{frame}");
            last = frame["seq"].as_u64().unwrap_or(last);
            if stop(&frame) {
                return last;
            }
        }
    }
}

/// The next link that dials the broker.
async fn accept(listener: &TcpListener) -> Side {
    let (tcp, _) = listener.accept().await.unwrap();
    Side(tokio_tungstenite::accept_async(tcp).await.unwrap())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let model = ScriptedProvider::new([
        text_answer("The build is green."),
        text_answer("The deploy is done."),
    ]);
    let deps = AgentDeps::new(model.resolver(), Tools::new(), "", "/", ShutWorkspace);
    let agent = Agent::new(deps, AgentOptions::new("scripted:m"));
    let session = start(agent, Hub::new("example-link", HubOptions::default()));
    let backend: Arc<dyn Backend> = Arc::new(session);

    // The broker: a WebSocket listener.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/agents", listener.local_addr().unwrap());

    // The host: its session linked to the broker, with a token from its keys.
    let quick = Backoff {
        base: Duration::from_millis(50),
        cap: Duration::from_secs(1),
    };
    let attached = Broker::new(&url)
        .token(Some("example-token".into()))
        .backoff(quick)
        .attach(backend.clone(), Grant::command(Origin::Agent))
        .unwrap();

    // The first link: hello, the ack from the start, a turn.
    let mut side = accept(&listener).await;
    let hello = side.next().await.unwrap();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["token"], "example-token");
    side.send(json!({"v": 1, "id": "ack", "type": "events", "after": 0, "follow": true}))
        .await;
    side.send(json!({"v": 1, "id": "t1", "type": "turn", "text": "is the build green?"}))
        .await;
    let held = side
        .until(|f| f["type"] == "reply" && f["id"] == "t1", 0)
        .await;

    // The broker cuts the link. While it is down the host runs a turn of
    // its own; haest dials again, and the ack says what the broker holds,
    // so what came after is sent and nothing twice.
    drop(side);
    backend.turn(Input::human("and the deploy?")).await.unwrap();
    let mut side = accept(&listener).await;
    assert_eq!(side.next().await.unwrap()["type"], "hello");
    side.send(json!({"v": 1, "id": "ack", "type": "events", "after": held, "follow": true}))
        .await;
    let mut missed = Vec::new();
    loop {
        let frame = side.next().await.expect("the link is open");
        println!("{frame}");
        if !matches!(frame["type"].as_str(), Some("reply" | "hello" | "gap")) {
            missed.push(frame["seq"].as_u64().unwrap());
        }
        if frame["type"] == "turn_end" {
            break;
        }
    }
    assert!(
        missed.iter().all(|seq| *seq > held),
        "sent twice: {missed:?}"
    );

    // The conversation ends, and with it the link.
    backend.close().await;
    while side.next().await.is_some() {}
    eprintln!("ended: {:?}", attached.ended().await);
}

/// `cargo test` runs the example, so a change that breaks it fails the suite.
#[test]
fn the_example_runs() {
    main();
}
