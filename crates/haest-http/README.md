# haest-http

The HTTP seam of [haest](https://github.com/haest-werk/haest), an agent
loop and tool harness you embed in a Rust program.

The model calls ([haest-providers](https://crates.io/crates/haest-providers)),
GenerateImage ([haest-tools](https://crates.io/crates/haest-tools)) and the
GitHub tracker send every request through the `HttpClient` they are handed.
One trait with one method, one `Request`, one `Response` whose body streams,
one `HttpError`. Two parts do not take that client: WebFetch sends through an
`HttpClient` of its own that checks every address it connects to, and an MCP
server's streamable HTTP runs on rmcp's reqwest client (built here, but not
an `HttpClient`).

- `ReqwestClient` is the default: reqwest over rustls (ring), no proxy from
  the environment, a redirect followed only within its origin (a 307 or
  308), one connection pool shared by the process, built on the first
  request. `ReqwestClient::configured` is a client of its own, with your
  proxy, certificates or timeouts.
- `build_reqwest` is the one place haest builds a reqwest client. It installs
  ring as the process's rustls provider when none is installed, so building a
  client never panics for want of one; a provider you installed is kept. That
  install is process-wide and happens at the first request, so install your
  own before it. The MCP client
  ([haest-mcp](https://crates.io/crates/haest-mcp)), whose SDK runs only on
  reqwest, gets its client from here too.
- Implement `HttpClient` to send everything somewhere else: a gateway, a
  recorder, a fake for tests. A part that sends takes
  `impl Into<HttpClientRef>`, so it is handed the client itself.
- The `reqwest` feature (on by default) is `ReqwestClient` and
  `build_reqwest`. With `default-features = false` this crate is the trait
  and its types only: no reqwest, rustls or ring is built, and the default
  client (`HttpClientRef::default()`) refuses every request until you hand
  one in.
- The `http` feature (off by default) converts to and from the `http`
  crate: a `Request` into an `http::Request<Bytes>`, an
  `http::Response<B>` into a `Response`, for a client that is an axum
  router or a hyper client underneath.
- The `testing` feature (off by default) is `testing::Server`, an HTTP
  server on 127.0.0.1 for a test: it writes down each request and answers
  from a script, or as a closure that reads the request decides
  (`Server::answering`). For `dev-dependencies` only: like every `testing`
  module and feature in haest, it is outside semver.

```rust
use haest_http::{HttpClient, HttpError, Request, Response, async_trait};
use serde_json::json;

struct Fake;

#[async_trait]
impl HttpClient for Fake {
    async fn send(&self, _req: Request) -> Result<Response, HttpError> {
        Ok(Response::new(200, "").with_json(&json!({"ok": true})))
    }
}
```

Through the facade this crate is `haest::http`. The docs on
[docs.rs](https://docs.rs/haest-http/) have the details; the repository's
[README](https://github.com/haest-werk/haest/blob/main/README.md) has the rest
of haest.

License: MIT.
