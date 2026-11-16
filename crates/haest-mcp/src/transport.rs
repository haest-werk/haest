//! The two ways to reach a configured server: a child process on stdio, or
//! streamable HTTP. Building one can fail (no such program, a header HTTP cannot
//! carry); that failure is the server's, never the hub's.

use std::{collections::BTreeMap, path::Path, process::Stdio};

use haest_core::ChildEnv;
use http::{HeaderName, HeaderValue};
use rmcp::transport::{
    StreamableHttpClientTransport, TokioChildProcess,
    streamable_http_client::StreamableHttpClientTransportConfig,
};

/// `command args...` with the host's handed env plus the server's own `env`
/// over it -- not this process's environment -- in `cwd`. A bare command is
/// found on that env's PATH. Its stderr is ignored; it is killed when the
/// connection drops.
pub(crate) fn stdio(
    command: &str,
    args: &[String],
    handed: &ChildEnv,
    env: &BTreeMap<String, String>,
    cwd: Option<&Path>,
) -> Result<TokioChildProcess, String> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .env_clear()
        .envs(handed.vars().iter().map(|(k, v)| (k, v)))
        .envs(env)
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let (child, _stderr) = TokioChildProcess::builder(cmd)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {command}: {e}"))?;
    Ok(child)
}

/// Header names and values as HTTP takes them, or the one that cannot be sent.
pub(crate) fn header_map(
    headers: &BTreeMap<String, String>,
) -> Result<std::collections::HashMap<HeaderName, HeaderValue>, String> {
    headers
        .iter()
        .map(|(k, v)| {
            let name = HeaderName::try_from(k.as_str())
                .map_err(|e| format!("header \"{k}\" is not a valid name: {e}"))?;
            let value = HeaderValue::try_from(v.as_str())
                .map_err(|e| format!("header \"{k}\" has a value HTTP cannot carry: {e}"))?;
            Ok((name, value))
        })
        .collect()
}

/// Streamable HTTP to `url`, `headers` on every request.
pub(crate) fn http(
    url: &str,
    headers: &BTreeMap<String, String>,
) -> Result<StreamableHttpClientTransport<reqwest::Client>, String> {
    let headers = header_map(headers)?;
    let config = StreamableHttpClientTransportConfig::with_uri(url).custom_headers(headers);
    Ok(StreamableHttpClientTransport::with_client(
        http_client()?,
        config,
    ))
}

/// How many redirects one request may follow.
const MAX_REDIRECTS: usize = 10;

/// rmcp's default client, minus the proxy it would take from the environment:
/// HTTP(S)_PROXY / ALL_PROXY would be sent the server's headers (D15). Like
/// rmcp's, it keeps no idle connections (a reused one can stall ~40 ms) and
/// follows only a same-origin 307 or 308: a 301-303 turns the POST into a GET
/// without the message, and another origin would be sent the session id and
/// the headers.
fn http_client() -> Result<reqwest::Client, String> {
    // reqwest takes the process-wide TLS provider when one is installed; this
    // installs ring only when nobody has installed one yet.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let redirects = reqwest::redirect::Policy::custom(|attempt| {
        let keeps_body = matches!(
            attempt.status(),
            reqwest::StatusCode::TEMPORARY_REDIRECT | reqwest::StatusCode::PERMANENT_REDIRECT
        );
        let same_origin = attempt
            .previous()
            .first()
            .is_some_and(|first| first.origin() == attempt.url().origin());
        if !keeps_body || !same_origin {
            attempt.stop()
        } else if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error(format!("exceeded {MAX_REDIRECTS} redirects"))
        } else {
            attempt.follow()
        }
    });
    reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(0)
        .redirect(redirects)
        .build()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_pass_through_and_one_http_cannot_carry_is_named() {
        let ok = BTreeMap::from([("Authorization".to_string(), "Bearer t".to_string())]);
        let map = header_map(&ok).unwrap();
        assert_eq!(map[&HeaderName::from_static("authorization")], "Bearer t");
        let bad = BTreeMap::from([("X-Bad".to_string(), "line\nbreak".to_string())]);
        assert!(header_map(&bad).unwrap_err().contains("X-Bad"));
    }

    #[tokio::test]
    async fn an_http_transport_builds_its_tls_client_without_dialling() {
        let headers = BTreeMap::from([("Authorization".to_string(), "Bearer t".to_string())]);
        assert!(http("https://example.invalid/mcp", &headers).is_ok());
    }
}
