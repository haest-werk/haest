//! What the `haest` binary's tests run it in: a home of its own, and a model
//! that answers from a script over a local socket.

#![allow(
    dead_code,
    reason = "every test binary includes this module and uses a different part of it"
)]

pub(crate) mod pty;

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Output, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use serde_json::{Value, json};

/// A home of its own (haest's files and data under it) with a project dir in
/// it; gone at the end.
pub(crate) struct Home(pub PathBuf);

impl Home {
    pub(crate) fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("haest-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("w")).unwrap();
        Home(dir)
    }

    /// `haest <args>` in the project, stdin empty, its environment only HOME
    /// and what the caller adds.
    pub(crate) fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_haest"));
        cmd.arg("-C")
            .arg(self.0.join("w"))
            .args(args)
            .env_clear()
            .env("HOME", &self.0)
            .stdin(Stdio::null());
        cmd
    }

    /// [`Self::command`] with `vars`, run to its end.
    pub(crate) fn haest(&self, args: &[&str], vars: &[(&str, &str)]) -> Output {
        let mut cmd = self.command(args);
        cmd.envs(vars.iter().copied()).output().unwrap()
    }

    /// The session files haest saved.
    pub(crate) fn sessions(&self) -> Vec<PathBuf> {
        match std::fs::read_dir(self.0.join(".local/share/haest/sessions")) {
            Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The provider `local` (an OpenAI-compatible server at `model`), its key
    /// read from `LOCAL_KEY`: run with `-m local:m` and [`LOCAL_KEY`].
    pub(crate) fn serve(&self, model: &FakeModel) {
        let dir = self.0.join(".config/haest");
        std::fs::create_dir_all(&dir).unwrap();
        let providers = json!({"local": {
            "type": "openai",
            "baseURL": format!("{}/v1", model.url),
            "apiKeyEnv": "LOCAL_KEY",
        }});
        std::fs::write(dir.join("providers.json"), providers.to_string()).unwrap();
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The variable the `local` provider's key is read from, with a value.
pub(crate) const LOCAL_KEY: (&str, &str) = ("LOCAL_KEY", "k");

pub(crate) fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// One answer of the [`FakeModel`].
#[derive(Debug, Clone)]
pub(crate) enum Reply {
    /// Text, then the end of the answer.
    Text(String),
    /// One tool call: its name and its input.
    Tool(String, Value),
}

/// An OpenAI-compatible model on a local socket: each request gets the next
/// reply of its script, streamed; with the script used up, "out of script".
pub(crate) struct FakeModel {
    pub url: String,
    script: Arc<Mutex<VecDeque<Reply>>>,
    /// Every request's body, in order.
    pub requests: Arc<Mutex<Vec<Value>>>,
}

impl FakeModel {
    pub(crate) fn new(script: impl IntoIterator<Item = Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let script = Arc::new(Mutex::new(script.into_iter().collect::<VecDeque<_>>()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (s, r) = (script.clone(), requests.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { return };
                let (s, r) = (s.clone(), r.clone());
                std::thread::spawn(move || answer(conn, &s, &r));
            }
        });
        Self {
            url,
            script,
            requests,
        }
    }

    /// The bodies of the requests so far.
    pub(crate) fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

fn answer(conn: TcpStream, script: &Mutex<VecDeque<Reply>>, requests: &Mutex<Vec<Value>>) {
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    requests
        .lock()
        .unwrap()
        .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
    let reply = script.lock().unwrap().pop_front();
    let mut conn = conn;
    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
    let _ = conn.write_all(head.as_bytes());
    let _ = conn.write_all(sse(reply).as_bytes());
    let _ = conn.flush();
}

/// A reply as a chat completions stream.
fn sse(reply: Option<Reply>) -> String {
    let chunk = |delta: Value, reason: Option<&str>| {
        json!({"id": "c", "object": "chat.completion.chunk", "created": 0, "model": "m",
               "choices": [{"index": 0, "delta": delta, "finish_reason": reason}]})
    };
    let chunks = match reply {
        Some(Reply::Text(text)) => vec![
            chunk(json!({"content": text}), None),
            chunk(json!({}), Some("stop")),
        ],
        Some(Reply::Tool(name, input)) => {
            let call = json!({"index": 0, "id": "call-1", "type": "function",
                              "function": {"name": name, "arguments": input.to_string()}});
            vec![
                chunk(json!({"tool_calls": [call]}), None),
                chunk(json!({}), Some("tool_calls")),
            ]
        }
        None => vec![
            chunk(json!({"content": "out of script"}), None),
            chunk(json!({}), Some("stop")),
        ],
    };
    let usage = json!({"id": "c", "object": "chat.completion.chunk", "created": 0, "model": "m",
                       "choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 2}});
    let mut body: String = chunks
        .iter()
        .chain([&usage])
        .map(|c| format!("data: {c}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    body
}

/// A running `haest --stdio`, from the parent's side.
pub(crate) struct Parent {
    pub child: Child,
    stdin: Option<ChildStdin>,
    frames: mpsc::Receiver<Value>,
}

impl Parent {
    pub(crate) fn start(home: &Home, args: &[&str]) -> Self {
        Self::start_with(home, args, &[])
    }

    /// [`Self::start`] with `vars` in its environment too.
    pub(crate) fn start_with(home: &Home, args: &[&str], vars: &[(&str, &str)]) -> Self {
        let mut cmd = home.command(&[&["--stdio", "-m", "local:m"], args].concat());
        let mut child = cmd
            .env(LOCAL_KEY.0, LOCAL_KEY.1)
            .envs(vars.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, frames) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                let frame = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
                if tx.send(frame).is_err() {
                    return;
                }
            }
        });
        Self {
            stdin: child.stdin.take(),
            child,
            frames,
        }
    }

    pub(crate) fn send(&mut self, request: Value) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{request}").unwrap();
        stdin.flush().unwrap();
    }

    pub(crate) fn turn(&mut self, id: &str, text: &str) {
        self.send(json!({"v": 1, "id": id, "type": "turn", "text": text}));
    }

    pub(crate) fn control(&mut self, id: &str, verb: &str) {
        self.send(json!({"v": 1, "id": id, "type": "control", "verb": verb}));
    }

    pub(crate) fn answer(&mut self, id: &str, ask: &Value, answer: &str) {
        self.send(
            json!({"v": 1, "id": id, "type": "approval_answer", "ask": ask, "answer": answer}),
        );
    }

    /// Frames up to and including the first `stop` matches.
    pub(crate) fn until(&self, stop: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let frame = match self.frames.recv_timeout(Duration::from_secs(20)) {
                Ok(frame) => frame,
                Err(e) => panic!("{e} waiting; seen: {seen:#?}"),
            };
            let done = stop(&frame);
            seen.push(frame);
            if done {
                return seen;
            }
        }
    }

    /// Frames up to the reply to `id`, which is the last.
    pub(crate) fn reply(&self, id: &str) -> Vec<Value> {
        self.until(|f| ty(f) == "reply" && f["id"] == id)
    }

    /// The next approval ask.
    pub(crate) fn ask(&self) -> Value {
        self.until(|f| ty(f) == "approval_ask").pop().unwrap()
    }

    /// Close stdin: the parent has nothing more to ask.
    pub(crate) fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }

    /// Every frame still to come, until haest closes its stdout.
    pub(crate) fn rest(&self) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            match self.frames.recv_timeout(Duration::from_secs(20)) {
                Ok(frame) => seen.push(frame),
                Err(mpsc::RecvTimeoutError::Disconnected) => return seen,
                Err(e) => panic!("{e} waiting for stdout to close; seen: {seen:#?}"),
            }
        }
    }

    /// Close stdin and wait for the exit: its code and stderr.
    pub(crate) fn finish(mut self) -> (Option<i32>, String) {
        drop(self.stdin.take());
        let out = self.child.wait_with_output().unwrap();
        (out.status.code(), stderr(&out))
    }
}

pub(crate) fn ty(frame: &Value) -> &str {
    frame["type"].as_str().unwrap_or_default()
}

pub(crate) fn bash(command: &str) -> Reply {
    Reply::Tool(
        "Bash".into(),
        json!({"command": command, "description": command}),
    )
}
