//! A run linked to a broker (`broker_url`): the broker meets it with the
//! token from the run's keys, watches its turns, and is refused what watch
//! does not allow; a broker that refuses the token is said, once.

mod support;

use std::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::Duration,
};

use serde_json::{Value, json};
use support::{FakeModel, Home, Parent, Reply, ty};
use tokio_tungstenite::tungstenite::{
    Message, WebSocket, accept,
    protocol::{CloseFrame, frame::coding::CloseCode},
};

/// The token the run's keys hold; never on any screen.
const TOKEN: &str = "broker-test-token";

/// A broker on a local port: each link that dials it is handed to the test.
struct TestBroker {
    url: String,
    links: mpsc::Receiver<Side>,
}

impl TestBroker {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/agents", listener.local_addr().unwrap());
        let (tx, links) = mpsc::channel();
        std::thread::spawn(move || {
            for tcp in listener.incoming() {
                let Ok(tcp) = tcp else { return };
                tcp.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
                if let Ok(ws) = accept(tcp)
                    && tx.send(Side(ws)).is_err()
                {
                    return;
                }
            }
        });
        Self { url, links }
    }

    fn link(&self) -> Side {
        self.links
            .recv_timeout(Duration::from_secs(20))
            .expect("a link within 20 s")
    }

    /// A link that dials within `wait`, if one does.
    fn another(&self, wait: Duration) -> Option<Side> {
        self.links.recv_timeout(wait).ok()
    }
}

/// The broker's end of one link.
struct Side(WebSocket<TcpStream>);

impl Side {
    /// The next frame; None once the link closed.
    fn next(&mut self) -> Option<Value> {
        loop {
            match self.0.read() {
                Ok(Message::Text(text)) => {
                    return Some(serde_json::from_str(text.as_str()).unwrap());
                }
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    /// Frames up to and including the first `stop` matches.
    fn until(&mut self, mut stop: impl FnMut(&Value) -> bool) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let frame = self
                .next()
                .unwrap_or_else(|| panic!("the link closed; seen: {seen:#?}"));
            let done = stop(&frame);
            seen.push(frame);
            if done {
                return seen;
            }
        }
    }

    fn send(&mut self, request: Value) {
        self.0
            .send(Message::Text(request.to_string().into()))
            .unwrap();
    }
}

/// Whether `f` is a notice whose text starts with `start`.
fn is_notice(f: &Value, start: &str) -> bool {
    ty(f) == "notice" && f["text"].as_str().unwrap_or_default().starts_with(start)
}

#[test]
fn the_broker_meets_the_run_with_its_token_and_watches_its_turns() {
    let home = Home::new("broker-watch");
    let model = FakeModel::new([Reply::Text("watched".into())]);
    home.serve(&model);
    let broker = TestBroker::start();
    let mut host = Parent::start_with(
        &home,
        &["--broker-url", &broker.url],
        &[("HAEST_BROKER_TOKEN", TOKEN)],
    );

    // The hello carries the token and the access the host gave: watch.
    let mut side = broker.link();
    let hello = side.next().unwrap();
    assert_eq!(ty(&hello), "hello");
    assert_eq!(hello["token"], TOKEN);
    assert_eq!(hello["access"], "watch");

    // The broker acks from the start and follows: the ack is answered, and
    // the host's line that it linked reaches the broker. That line is said
    // once the link is set up, while the dial already runs, so it comes in
    // the catch-up or after it: no order between the two is promised.
    side.send(json!({"v": 1, "id": "ack", "type": "events", "after": 0, "follow": true}));
    let (mut acked, mut said) = (false, false);
    let held = side.until(|f| {
        acked |= ty(f) == "reply" && f["id"] == "ack";
        said |= is_notice(f, "Broker link: ws://");
        acked && said
    });
    let ack = held
        .iter()
        .find(|f| ty(f) == "reply" && f["id"] == "ack")
        .unwrap();
    assert_eq!(ack["ok"], true, "{ack}");

    // A turn the broker sends is refused.
    side.send(json!({"v": 1, "id": "b1", "type": "turn", "text": "from the broker"}));
    let refused = side.until(|f| ty(f) == "reply" && f["id"] == "b1");
    let refusal = refused.last().unwrap();
    assert_eq!(refusal["ok"], false);
    assert!(
        refusal["error"]
            .as_str()
            .unwrap()
            .contains("may only watch"),
        "{refusal}"
    );

    // The parent's turn reaches the broker.
    host.turn("t1", "hello");
    let host_frames = host.reply("t1");
    let seen = side.until(|f| ty(f) == "turn_end");
    let said = seen.iter().rfind(|f| ty(f) == "assistant").unwrap();
    assert_eq!(said["message"]["content"][0]["text"], "watched");
    assert_eq!(model.requests().len(), 1, "the broker's turn never ran");

    // The run ends: so does the link.
    let (code, stderr) = host.finish();
    assert_eq!(code, Some(0), "{stderr}");
    while side.next().is_some() {}
    // Only the hello carries the token: no notice, frame or line shows it.
    let shown = format!("{stderr}{host_frames:?}{held:?}{refused:?}{seen:?}");
    assert!(!shown.contains(TOKEN), "the token is shown: {shown}");
}

#[test]
fn a_broker_that_refuses_the_token_is_said_and_not_dialed_again() {
    let home = Home::new("broker-refused");
    let model = FakeModel::new([Reply::Text("still here".into())]);
    home.serve(&model);
    let broker = TestBroker::start();
    let mut host = Parent::start_with(
        &home,
        &["--broker-url", &broker.url],
        &[("HAEST_BROKER_TOKEN", "wrong")],
    );

    let mut side = broker.link();
    assert_eq!(ty(&side.next().unwrap()), "hello");
    let refuse = CloseFrame {
        code: CloseCode::from(4401),
        reason: "unknown token".into(),
    };
    side.0.close(Some(refuse)).unwrap();
    while side.next().is_some() {}

    // The host says so, and the run goes on.
    let said = host.until(|f| {
        ty(f) == "notice"
            && f["text"]
                .as_str()
                .unwrap_or_default()
                .contains("refused the link")
    });
    let text = said.last().unwrap()["text"].as_str().unwrap().to_string();
    assert!(
        text.contains("4401") && text.contains("unknown token"),
        "{text}"
    );
    assert_eq!(said.last().unwrap()["level"], "warning");
    host.turn("t1", "hello");
    let frames = host.reply("t1");
    assert_eq!(frames.last().unwrap()["ok"], true);

    // Not dialed again (the first retry would come within half a second).
    assert!(
        broker.another(Duration::from_millis(1500)).is_none(),
        "dialed again"
    );
    let (code, stderr) = host.finish();
    assert_eq!(code, Some(0), "{stderr}");
}
