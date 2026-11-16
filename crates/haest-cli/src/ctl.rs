//! `haest ctl`: watch and drive a haest that runs elsewhere on this machine,
//! over its control socket (the `control_socket` setting). The socket says
//! what it allows: a `watch` one shows the conversation, a `command` one
//! takes input and an interrupt too.

use std::{path::Path, process::ExitCode, time::Duration};

use haest::{
    core::{error::report, session::iso_utc},
    session::{Body, Frame},
    transport::{TransportError, UnixPeer, connect_session, running_sessions},
    wire::{Outbound, Request, Verb},
};
use serde_json::{Value, json};

use crate::{args::Ctl, render::Renderer};

/// How long a question that is not a turn may take.
const ANSWER_WAIT: Duration = Duration::from_secs(10);

pub async fn run(cmd: &Ctl, dir: &Path) -> ExitCode {
    let done = match cmd {
        Ctl::List { json } => {
            list(dir, *json).await;
            Ok(())
        }
        Ctl::Inspect { id, json } => inspect(dir, id, *json).await,
        Ctl::Events {
            id,
            after,
            follow,
            json,
        } => events(dir, id, *after, *follow, *json).await,
        Ctl::Send { id, text } => send(dir, id, &text.join(" ")).await,
        Ctl::Interrupt { id } => interrupt(dir, id).await,
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("haest: {why}");
            ExitCode::FAILURE
        }
    }
}

/// One line per running haest with a control socket, oldest first.
async fn list(dir: &Path, json: bool) {
    let found = running_sessions(dir).await;
    if json {
        let rows: Vec<Value> = found
            .iter()
            .map(|r| {
                json!({
                    "sid": r.discovery.sid,
                    "session": r.session(),
                    "model": r.model_spec(),
                    "pid": r.discovery.pid,
                    "access": r.discovery.access,
                    "startedMs": r.discovery.started_ms,
                })
            })
            .collect();
        println!("{}", Value::Array(rows));
        return;
    }
    if found.is_empty() {
        eprintln!("No haest with a control socket runs here.");
    }
    for r in &found {
        let when = iso_utc(r.discovery.started_ms);
        let access = serde_json::to_value(r.discovery.access).unwrap_or_default();
        println!(
            "{} {}Z  {}  {:7}  {}  pid {}",
            &when[..10],
            &when[11..16],
            r.discovery.sid,
            access.as_str().unwrap_or_default(),
            r.model_spec(),
            r.discovery.pid
        );
    }
}

/// The conversation now.
async fn inspect(dir: &Path, id: &str, json: bool) -> Result<(), String> {
    let mut peer = connect(dir, id).await?;
    let ask = peer.call(Request::Snapshot, |_| {});
    let now = answered(tokio::time::timeout(ANSWER_WAIT, ask).await)?.unwrap_or_default();
    if json {
        println!("{now}");
        return Ok(());
    }
    let field = |k: &str| now[k].as_str().unwrap_or("-").to_string();
    println!("session   {}", field("session"));
    println!("model     {}", field("model"));
    if let Some(effort) = now["effort"].as_str() {
        println!("effort    {effort}");
    }
    println!("state     {}", field("state"));
    println!("messages  {}", now["messages"]);
    println!("queued    {}", now["queued"]);
    if let Some(usd) = now["usd"].as_f64() {
        println!("cost      ${usd:.4}");
    }
    Ok(())
}

/// Its stream: what the ring holds after `after`, then with `follow` every
/// new frame until the conversation ends. Shown as the terminal shows it,
/// or one JSON line per frame.
async fn events(dir: &Path, id: &str, after: u64, follow: bool, json: bool) -> Result<(), String> {
    let mut peer = connect(dir, id).await?;
    let mut term = Renderer::terminal(json).showing_input();
    let ask = Request::Events { after, follow };
    peer.call(ask, |frame| show(&mut term, frame))
        .await
        .map_err(|e| report(&e))?;
    if !follow {
        return Ok(());
    }
    while let Some(frame) = peer.recv().await {
        match frame {
            Ok(frame) => show(&mut term, &frame),
            Err(TransportError::NotAFrame(_)) => {}
            Err(e) => return Err(report(&e)),
        }
    }
    Ok(())
}

fn show(term: &mut Renderer<std::io::Stdout, std::io::Stderr>, frame: &Frame) {
    // A connection's own frames are not the conversation's.
    if !matches!(
        frame.body,
        Body::Wire(Outbound::Hello { .. } | Outbound::Reply { .. })
    ) {
        term.frame(frame);
    }
}

/// Input for it, as another program's; waits for the turn it joins to end.
async fn send(dir: &Path, id: &str, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("nothing to send".into());
    }
    let mut peer = connect(dir, id).await?;
    let turn = Request::Turn { text: text.into() };
    let end = answered(Ok(peer.call(turn, |_| {}).await))?.unwrap_or_default();
    let stop = end["stopReason"].as_str().unwrap_or("-");
    match end["error"].as_str() {
        Some(error) => eprintln!("The turn ended ({stop}): {error}"),
        None => eprintln!("The turn ended ({stop})."),
    }
    Ok(())
}

/// Stop its running turn, if one runs.
async fn interrupt(dir: &Path, id: &str) -> Result<(), String> {
    let mut peer = connect(dir, id).await?;
    let stop = Request::Control {
        verb: Verb::Interrupt,
        model_spec: None,
        effort: None,
    };
    answered(tokio::time::timeout(ANSWER_WAIT, peer.call(stop, |_| {})).await)?;
    eprintln!("Interrupted.");
    Ok(())
}

async fn connect(dir: &Path, id: &str) -> Result<UnixPeer, String> {
    connect_session(dir, id).await.map_err(|e| match e {
        TransportError::NoSession(_) => format!("{e} (haest ctl list lists them)"),
        e => report(&e),
    })
}

/// A reply's result, or why there is none.
fn answered(
    reply: Result<Result<Option<Value>, TransportError>, tokio::time::error::Elapsed>,
) -> Result<Option<Value>, String> {
    match reply {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(e)) => Err(report(&e)),
        Err(_) => Err(format!("no answer in {} s", ANSWER_WAIT.as_secs())),
    }
}
