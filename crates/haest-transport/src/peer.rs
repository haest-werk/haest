//! [`Peer`]: the other end of a connection, the side that asks: requests
//! out, frames in. What `haest ctl` drives a session with; a parent process
//! or a test can use it over any line source and sink.

use haest_session::{Body, Frame};
use haest_wire::{Access, Inbound, Outbound, Request};
use serde_json::Value;

use crate::{LineSink, LineSource, TransportError};

/// A connection from the asking side. [`start`](Self::start) reads the
/// `hello`; then [`call`](Self::call) sends a request and reads up to its
/// reply, or [`send`](Self::send) and [`recv`](Self::recv) do it a frame at
/// a time.
///
/// A line it cannot read as a frame (a newer wire's frame type, say) is
/// [`TransportError::NotAFrame`] from `recv`, and skipped by `call`: the
/// connection goes on.
pub struct Peer<S, K> {
    source: S,
    sink: K,
    hello: Frame,
    next_id: u64,
}

impl<S: LineSource, K: LineSink> Peer<S, K> {
    /// The connection over `source` and `sink`, once its `hello` is read.
    ///
    /// # Errors
    ///
    /// [`TransportError::Ended`] when it ends first, [`TransportError::NotAFrame`]
    /// when the first line is not a `hello`, or a read error.
    pub async fn start(mut source: S, sink: K) -> Result<Self, TransportError> {
        let line = source.next_line().await.ok_or(TransportError::Ended)??;
        let hello = frame(&line)?;
        if !matches!(hello.body, Body::Wire(Outbound::Hello { .. })) {
            return Err(TransportError::NotHello);
        }
        Ok(Self {
            source,
            sink,
            hello,
            next_id: 0,
        })
    }

    /// The `hello` frame: the session, its model, its process, this
    /// connection's access.
    pub fn hello(&self) -> &Frame {
        &self.hello
    }

    /// What this connection may do, as the `hello` said.
    pub fn access(&self) -> Access {
        match &self.hello.body {
            Body::Wire(Outbound::Hello { access, .. }) => *access,
            _ => Access::Watch,
        }
    }

    /// Send `request` with an id of its own; that id.
    ///
    /// # Errors
    ///
    /// The write failed.
    pub async fn send(&mut self, request: Request) -> Result<String, TransportError> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        let line = serde_json::to_string(&Inbound::new(id.clone(), request))
            .map_err(TransportError::NotAFrame)?;
        self.sink.send_line(line).await?;
        Ok(id)
    }

    /// The next frame; None once the connection ended.
    pub async fn recv(&mut self) -> Option<Result<Frame, TransportError>> {
        Some(self.source.next_line().await?.and_then(|line| frame(&line)))
    }

    /// Send `request` and read up to its reply, each frame before it handed
    /// to `each`; the reply's result.
    ///
    /// # Errors
    ///
    /// [`TransportError::Refused`] with the other end's reason,
    /// [`TransportError::Ended`] when the connection ended first, or a read
    /// or write error.
    pub async fn call(
        &mut self,
        request: Request,
        mut each: impl FnMut(&Frame),
    ) -> Result<Option<Value>, TransportError> {
        let id = self.send(request).await?;
        loop {
            let frame = match self.recv().await.ok_or(TransportError::Ended)? {
                Ok(frame) => frame,
                Err(TransportError::NotAFrame(_)) => continue,
                Err(e) => return Err(e),
            };
            match &frame.body {
                Body::Wire(Outbound::Reply {
                    id: of,
                    ok,
                    result,
                    error,
                }) if *of == id => {
                    return match ok {
                        true => Ok(result.clone()),
                        false => Err(TransportError::Refused(error.clone().unwrap_or_default())),
                    };
                }
                _ => each(&frame),
            }
        }
    }
}

fn frame(line: &str) -> Result<Frame, TransportError> {
    serde_json::from_str(line).map_err(TransportError::NotAFrame)
}
