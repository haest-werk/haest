# haest-http

The one HTTP seam of [haest](https://github.com/haest-werk/haest), an agent
loop and tool harness you embed in a Rust program.

Every request a haest part sends goes through an `HttpClient`: the model calls
([haest-providers](https://crates.io/crates/haest-providers)), WebFetch and
GenerateImage ([haest-tools](https://crates.io/crates/haest-tools)). One trait
with one method, one `Request`, one `Response` whose body streams, one
`HttpError`.

- `ReqwestClient` is the default: reqwest over rustls (ring), no proxy from
  the environment, one connection pool shared by the process, built on the
  first request. `ReqwestClient::configured` is a client of its own, with
  your proxy, certificates or timeouts.
- `build_reqwest` is the one place haest builds a reqwest client. It installs
  ring as the process's rustls provider when none is installed, so building a
  client never panics for want of one; a provider you installed is kept. The
  MCP client ([haest-mcp](https://crates.io/crates/haest-mcp)), whose SDK
  runs only on reqwest, gets its client from here too.
- Implement `HttpClient` to send everything somewhere else: a gateway, a
  recorder, a fake for tests.

```rust
use haest_http::{HttpClient, HttpError, Request, Response, async_trait};
use serde_json::json;

struct Fake;

#[async_trait]
impl HttpClient for Fake {
    async fn send(&self, _req: Request) -> Result<Response, HttpError> {
        Ok(Response::json(200, &json!({"ok": true})))
    }
}
```

Through the facade this crate is `haest::http`. The docs on
[docs.rs](https://docs.rs/haest-http/) have the details; the repository's
[README](https://github.com/haest-werk/haest/blob/main/README.md) has the rest
of haest.

License: MIT.
