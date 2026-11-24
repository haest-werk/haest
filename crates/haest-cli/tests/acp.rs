//! `haest acp` driven by the Agent Client Protocol's own client, as an editor
//! drives it: the binary a child on pipes, against a model on a local socket.

mod support;

use std::{path::PathBuf, process::Stdio, time::Duration};

use agent_client_protocol::{
    ByteStreams, Client, Error, on_receive_notification, on_receive_request,
    schema::ProtocolVersion,
    v1::{
        ConnectionTo,
        schema::{
            CancelNotification, CloseSessionRequest, ContentBlock, InitializeRequest,
            LoadSessionRequest, NewSessionRequest, PermissionOptionKind, PromptRequest,
            RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
            SelectedPermissionOutcome, SessionId, SessionNotification, StopReason, TextContent,
        },
    },
};
use serde_json::Value;
use support::{FakeModel, Home, LOCAL_KEY, Reply, bash};
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// What the client was told: an update (as JSON, the way it travelled), or
/// a permission request (allowed once).
#[derive(Debug)]
enum Seen {
    Update(Value),
    Asked(Value),
}

/// What reached the client, read by the test as it goes.
struct Told(mpsc::UnboundedReceiver<Seen>);

impl Told {
    /// Everything up to and including the first `stop` matches.
    async fn until(&mut self, stop: impl Fn(&Seen) -> bool) -> Vec<Seen> {
        let mut seen = Vec::new();
        loop {
            let next = tokio::time::timeout(Duration::from_secs(20), self.0.recv()).await;
            let Ok(Some(s)) = next else {
                panic!("nothing more told; seen: {seen:#?}");
            };
            let done = stop(&s);
            seen.push(s);
            if done {
                return seen;
            }
        }
    }

    /// What is in hand now.
    fn drain(&mut self) -> Vec<Seen> {
        std::iter::from_fn(|| self.0.try_recv().ok()).collect()
    }
}

fn kind(s: &Seen) -> &str {
    match s {
        Seen::Update(u) => u["sessionUpdate"].as_str().unwrap_or_default(),
        Seen::Asked(_) => "permission",
    }
}

fn updates<'a>(seen: &'a [Seen], of: &str) -> Vec<&'a Value> {
    let of_kind = seen.iter().filter(|s| kind(s) == of);
    of_kind
        .filter_map(|s| match s {
            Seen::Update(u) => Some(u),
            Seen::Asked(_) => None,
        })
        .collect()
}

/// The text the agent's message chunks said, in order.
fn said(seen: &[Seen]) -> String {
    let chunks = updates(seen, "agent_message_chunk");
    chunks
        .iter()
        .filter_map(|u| u["content"]["text"].as_str())
        .collect()
}

fn text(t: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text(TextContent::new(t))]
}

/// `haest acp` on the [`FakeModel`] in `home`'s project, the real disk, Bash
/// on; `drive` runs as the client, its permission requests allowed once.
async fn with_haest<F>(home: &Home, drive: F)
where
    F: AsyncFnOnce(ConnectionTo<agent_client_protocol::Agent>, Told, PathBuf) -> Result<(), Error>,
{
    let args = [
        "acp",
        "-m",
        "local:m",
        "--workspace",
        "real",
        "--tools",
        "bash",
    ];
    let mut cmd = tokio::process::Command::from(home.command(&args));
    let mut child = cmd
        .env(LOCAL_KEY.0, LOCAL_KEY.1)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let to_haest = child.stdin.take().unwrap().compat_write();
    let from_haest = child.stdout.take().unwrap().compat();
    let (tx, rx) = mpsc::unbounded_channel();
    let asked = tx.clone();
    let cwd = std::fs::canonicalize(home.0.join("w")).unwrap();

    let run = Client
        .v1()
        .on_receive_notification(
            async move |n: SessionNotification, _cx| {
                let _ = tx.send(Seen::Update(serde_json::to_value(&n.update).unwrap()));
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: RequestPermissionRequest, responder, _cx| {
                let _ = asked.send(Seen::Asked(serde_json::to_value(&req).unwrap()));
                let once = req
                    .options
                    .iter()
                    .find(|o| o.kind == PermissionOptionKind::AllowOnce)
                    .map(|o| o.option_id.clone())
                    .expect("an allow-once option");
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(once)),
                ))
            },
            on_receive_request!(),
        )
        .connect_with(ByteStreams::new(to_haest, from_haest), async move |cx| {
            drive(cx, Told(rx), cwd).await
        });
    match tokio::time::timeout(Duration::from_secs(60), run).await {
        Ok(done) => done.unwrap(),
        Err(_) => panic!("the client did not finish in 60 s"),
    }
    // The editor closes the pipes; haest ends its sessions and exits.
    let status = tokio::time::timeout(Duration::from_secs(20), child.wait()).await;
    assert!(status.unwrap().unwrap().success());
}

/// initialize, session/new, a prompt whose tool call asks first: the client
/// sees the call, allows it, sees it complete and the answer stream, and the
/// turn ends. Then the session closes, and a load tells its history again.
#[tokio::test(flavor = "multi_thread")]
async fn an_editor_opens_a_session_allows_a_call_and_loads_it_again() {
    let home = Home::new("acp-allow");
    std::fs::create_dir_all(home.0.join("w/build")).unwrap();
    let model = FakeModel::new([bash("rm -rf build"), Reply::Text("cleaned up".into())]);
    home.serve(&model);
    let build = home.0.join("w/build");

    with_haest(&home, async |cx, mut told, cwd| {
        let init = cx
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await?;
        let init = serde_json::to_value(&init).unwrap();
        assert_eq!(init["agentInfo"]["name"], "haest", "{init}");
        assert_eq!(init["agentCapabilities"]["loadSession"], true, "{init}");

        let new = cx
            .send_request(NewSessionRequest::new(&cwd))
            .block_task()
            .await?;
        let modes = new.modes.as_ref().expect("haest's modes");
        assert_eq!(&*modes.current_mode_id.0, "default");
        let sid: SessionId = new.session_id.clone();
        let commands = told.until(|s| kind(s) == "available_commands_update").await;
        let names = &updates(&commands, "available_commands_update")[0]["availableCommands"];
        assert!(names.to_string().contains("\"compact\""), "{names}");

        let done = cx
            .send_request(PromptRequest::new(sid.clone(), text("clean up")))
            .block_task()
            .await?;
        assert_eq!(done.stop_reason, StopReason::EndTurn);
        let seen = told.drain();
        // Announced as the model starts it, before its input is in; its
        // input, as it runs, says what it does.
        let call = &updates(&seen, "tool_call")[0];
        assert_eq!(call["toolCallId"], "call-1", "{seen:#?}");
        assert_eq!(call["kind"], "execute");
        // Pending is the protocol's default, left off the wire.
        assert_eq!(call["status"].as_str().unwrap_or("pending"), "pending");
        let ended = updates(&seen, "tool_call_update");
        let running = ended.iter().find(|u| u["status"] == "in_progress");
        let running = running.unwrap_or_else(|| panic!("never in progress: {seen:#?}"));
        assert_eq!(running["title"], "rm -rf build");
        let ask = seen.iter().find_map(|s| match s {
            Seen::Asked(a) => Some(a),
            Seen::Update(_) => None,
        });
        let ask = ask.unwrap_or_else(|| panic!("no permission asked: {seen:#?}"));
        assert_eq!(
            ask["toolCall"]["toolCallId"], "call-1",
            "the ask is on the call"
        );
        let completed = ended.iter().find(|u| u["status"] == "completed");
        assert!(completed.is_some(), "{seen:#?}");
        assert_eq!(said(&seen), "cleaned up");
        assert!(!build.exists(), "the allowed call ran");

        // Closed, then loaded: its history told again before the answer.
        cx.send_request(CloseSessionRequest::new(sid.clone()))
            .block_task()
            .await?;
        cx.send_request(LoadSessionRequest::new(sid, &cwd))
            .block_task()
            .await?;
        let history = told.until(|s| kind(s) == "available_commands_update").await;
        let first = &updates(&history, "user_message_chunk")[0];
        assert_eq!(first["content"]["text"], "clean up", "{history:#?}");
        assert_eq!(updates(&history, "tool_call")[0]["toolCallId"], "call-1");
        assert_eq!(said(&history), "cleaned up");
        Ok(())
    })
    .await;
}

/// A cancel mid-turn: the running call is stopped, and the prompt answers
/// cancelled.
#[tokio::test(flavor = "multi_thread")]
async fn an_editor_cancels_a_turn_and_its_prompt_answers_cancelled() {
    let home = Home::new("acp-cancel");
    let model = FakeModel::new([bash("sleep 30"), Reply::Text("never".into())]);
    home.serve(&model);

    with_haest(&home, async |cx, mut told, cwd| {
        cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await?;
        let sid = cx
            .send_request(NewSessionRequest::new(&cwd))
            .block_task()
            .await?
            .session_id;
        let prompt = cx
            .send_request(PromptRequest::new(sid.clone(), text("wait a while")))
            .block_task();
        let cancel = async {
            let running = |s: &Seen| kind(s) == "tool_call_update";
            told.until(running).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            cx.send_notification(CancelNotification::new(sid.clone()))
        };
        let (done, cancelled) = tokio::join!(prompt, cancel);
        cancelled?;
        assert_eq!(done?.stop_reason, StopReason::Cancelled);
        assert_eq!(model.requests().len(), 1, "no model call after the cancel");
        Ok(())
    })
    .await;
}
