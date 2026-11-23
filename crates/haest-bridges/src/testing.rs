//! For tests of HTTP hooks, in this crate and in a host's (the `testing`
//! feature; turn it on in `dev-dependencies` only): [`Server`], an
//! HTTP/1.1 server on 127.0.0.1 for one test, written by hand over a socket.
//! It writes down each request and answers it as scripted.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

/// What a [`Server`] answers a request.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Respond {
    /// This status and body.
    With(u16, String),
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
    /// Its body.
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
}

/// A server that answers each request with the next of its responses, then
/// `200` with an empty body. One request per connection. Dropped, it stops.
#[derive(Debug)]
pub struct Server {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    task: JoinHandle<()>,
}

impl Server {
    /// A server on a free port of 127.0.0.1, answering `responses` in order.
    /// Needs a tokio runtime.
    ///
    /// # Panics
    ///
    /// When no port of 127.0.0.1 can be bound.
    pub async fn start(responses: impl IntoIterator<Item = Respond>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a free port on 127.0.0.1");
        let addr = listener.local_addr().expect("the bound port");
        let url = format!("http://{addr}/hook");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let responses = Arc::new(Mutex::new(responses.into_iter().collect::<VecDeque<_>>()));
        let log = seen.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (log, responses) = (log.clone(), responses.clone());
                tokio::spawn(async move { serve(stream, &log, &responses).await });
            }
        });
        Self { url, seen, task }
    }

    /// Its URL: `http://127.0.0.1:<port>/hook`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Every request it got, in order.
    ///
    /// # Panics
    ///
    /// When a task serving a request panicked while it wrote one down.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("the server's log").clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One request on `stream`: written down, answered.
async fn serve(
    mut stream: TcpStream,
    log: &Mutex<Vec<Seen>>,
    responses: &Mutex<VecDeque<Respond>>,
) {
    let mut read = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(at) = read.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
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
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    if let Ok(mut log) = log.lock() {
        log.push(Seen {
            line,
            headers,
            body,
        });
    }
    let respond = responses.lock().ok().and_then(|mut r| r.pop_front());
    let (status, text) = match respond {
        Some(Respond::With(status, text)) => (status, text),
        Some(Respond::Never) => return std::future::pending().await,
        None => (200, String::new()),
    };
    let response = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
    stream.write_all(response.as_bytes()).await.ok();
    stream.shutdown().await.ok();
}
