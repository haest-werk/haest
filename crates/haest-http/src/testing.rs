//! For tests that send HTTP, in this crate and in any other (the `testing`
//! feature; turn it on in `dev-dependencies` only): [`Server`], an HTTP/1.1
//! server on 127.0.0.1 for one test, written by hand over a socket. It
//! writes down each request and answers it, from a script in order
//! ([`Server::start`]) or as a closure that reads the request decides
//! ([`Server::answering`]).
//!
//! Not part of the API and not covered by semver, like every `testing`
//! module and feature in haest: it changes in any release.
//!
//! The server runs on threads of its own, so it serves a test on any
//! runtime, a synchronous test, or a whole test binary from a `static`.
//!
//! ```
//! use std::io::{Read, Write};
//!
//! use haest_http::testing::{Respond, Server};
//!
//! # fn main() -> std::io::Result<()> {
//! let server = Server::answering(|seen| match seen.path() {
//!     "/ping" => Respond::With(200, r#"{"pong":true}"#.into()),
//!     _ => Respond::With(404, String::new()),
//! });
//! let mut conn = std::net::TcpStream::connect(server.addr())?;
//! conn.write_all(b"GET /ping HTTP/1.1\r\nhost: here\r\n\r\n")?;
//! let mut answer = String::new();
//! conn.read_to_string(&mut answer)?;
//! assert!(answer.starts_with("HTTP/1.1 200 "));
//! assert!(answer.ends_with(r#"{"pong":true}"#));
//! assert_eq!(server.seen()[0].line, "GET /ping HTTP/1.1");
//! assert_eq!(server.seen()[0].header("host"), Some("here"));
//! # Ok(())
//! # }
//! ```

use std::{
    collections::VecDeque,
    fmt::Write as _,
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

/// What a [`Server`] answers a request.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Respond {
    /// This status and body, as `content-type: application/json`.
    With(u16, String),
    /// This status, these headers and this body. `content-length` and
    /// `connection: close` are added; nothing else is (no `content-type`
    /// unless the headers have one).
    WithHeaders(u16, Vec<(String, String)>, String),
    /// No answer: the connection closes as soon as the request is in.
    Close,
    /// Nothing, ever: the request hangs until its client gives up.
    Never,
}

/// One request a [`Server`] got.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Seen {
    /// The request line: `POST /hook HTTP/1.1`.
    pub line: String,
    /// Its headers, names in lower case, in order.
    pub headers: Vec<(String, String)>,
    /// Its body: as many bytes as its `content-length` says, none without one.
    pub body: Vec<u8>,
}

impl Seen {
    /// The value of the header `name` (lower case), when it was sent once.
    ///
    /// # Panics
    ///
    /// When it was sent more than once.
    pub fn header(&self, name: &str) -> Option<&str> {
        let mut values = self.headers.iter().filter(|(n, _)| n == name);
        let value = values.next().map(|(_, v)| v.as_str());
        assert!(values.next().is_none(), "{name} was sent twice");
        value
    }

    /// The request line's target: `/hook` of `POST /hook HTTP/1.1` (a
    /// whole URL when the request was sent to a proxy).
    pub fn path(&self) -> &str {
        self.line.split(' ').nth(1).unwrap_or_default()
    }
}

/// What a [`Server`] calls to answer each request, one call at a time.
type Responder = Box<dyn FnMut(&Seen) -> Respond + Send>;

/// A server on a free port of 127.0.0.1. One request per connection, each
/// written down before it is answered. Dropped, it stops listening.
#[derive(Debug)]
pub struct Server {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl Server {
    /// A server answering each request with the next of `responses`, then
    /// `200` with an empty body.
    ///
    /// # Panics
    ///
    /// When no port of 127.0.0.1 can be bound.
    pub fn start(responses: impl IntoIterator<Item = Respond>) -> Self {
        let mut responses: VecDeque<Respond> = responses.into_iter().collect();
        Self::answering(move |_| {
            responses
                .pop_front()
                .unwrap_or_else(|| Respond::With(200, String::new()))
        })
    }

    /// A server answering each request with what `respond` makes of it,
    /// called in the order the requests arrive.
    ///
    /// # Panics
    ///
    /// When no port of 127.0.0.1 can be bound.
    pub fn answering(respond: impl FnMut(&Seen) -> Respond + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port on 127.0.0.1");
        let addr = listener.local_addr().expect("the bound port");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let respond: Arc<Mutex<Responder>> = Arc::new(Mutex::new(Box::new(respond)));
        let (log, stopping) = (seen.clone(), stop.clone());
        let accept = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                let (log, respond) = (log.clone(), respond.clone());
                std::thread::spawn(move || serve(stream, &log, &respond));
            }
        });
        Self {
            addr,
            seen,
            stop,
            accept: Some(accept),
        }
    }

    /// Where it listens.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `path` on it: `url("/hook")` is `http://127.0.0.1:<port>/hook`.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Every request it got, in order.
    ///
    /// # Panics
    ///
    /// When a thread serving a request panicked while it wrote one down.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("the server's log").clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // The accept loop reads the flag at its next connection: this one.
        // Joined, the port is free again when the drop returns.
        if TcpStream::connect(self.addr).is_ok()
            && let Some(accept) = self.accept.take()
        {
            let _ = accept.join();
        }
    }
}

/// One request on `stream`: written down, answered.
fn serve(mut stream: TcpStream, log: &Mutex<Vec<Seen>>, respond: &Mutex<Responder>) {
    let Some(seen) = read_request(&mut stream) else {
        return;
    };
    // A responder that panicked (a test's assert) poisons its lock: every
    // later request is closed unanswered.
    let Ok(mut respond) = respond.lock() else {
        return;
    };
    if let Ok(mut log) = log.lock() {
        log.push(seen.clone());
    }
    let answer = respond(&seen);
    drop(respond);
    match answer {
        Respond::With(status, body) => {
            let json = [("content-type".to_string(), "application/json".to_string())];
            write_answer(stream, status, &json, &body);
        }
        Respond::WithHeaders(status, headers, body) => {
            write_answer(stream, status, &headers, &body);
        }
        Respond::Close => {}
        Respond::Never => {
            // Held open until the client hangs up.
            let mut sink = [0u8; 512];
            while let Ok(1..) = stream.read(&mut sink) {}
        }
    }
}

/// The request on `stream`: its head, and its body by its `content-length`.
/// None when the client hung up first.
fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    let mut read = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(at) = read.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => read.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&read[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let line = lines.next().unwrap_or_default().to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = read[head_end + 4..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(length);
    Some(Seen {
        line,
        headers,
        body,
    })
}

/// `status`, `headers` and `body` on `stream`, then the end of it.
fn write_answer(mut stream: TcpStream, status: u16, headers: &[(String, String)], body: &str) {
    let mut head = format!("HTTP/1.1 {status} X\r\n");
    for (name, value) in headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    let _ = write!(
        head,
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.shutdown(Shutdown::Write);
}
