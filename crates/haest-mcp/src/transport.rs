//! The two ways to reach a configured server: a child process on stdio, or
//! streamable HTTP. Building one can fail (no such program, a header HTTP cannot
//! carry); that failure is the server's, never the hub's.

mod scrubbed;

use std::{collections::BTreeMap, io, path::Path, process::Stdio, sync::OnceLock};

use haest_core::ChildEnv;
use haest_http::{
    build_reqwest,
    reqwest::{
        self,
        header::{HeaderName, HeaderValue},
    },
    reqwest_builder,
};
use rmcp::transport::{
    StreamableHttpClientTransport, TokioChildProcess,
    streamable_http_client::StreamableHttpClientTransportConfig,
};

use crate::McpStartError;
use scrubbed::ScrubbedClient;

/// `command args...` with the host's handed env plus the server's own `env`
/// over it -- not this process's environment -- in `cwd`. A bare command is
/// found on that env's PATH, or with none in the system's default directories;
/// a bare command not found there with no PATH given fails saying so. Its
/// stderr is ignored; it is killed when the connection drops.
pub(crate) fn stdio(
    command: &str,
    args: &[String],
    handed: &ChildEnv,
    env: &BTreeMap<String, String>,
    cwd: Option<&Path>,
) -> Result<TokioChildProcess, McpStartError> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .env_clear()
        .envs(handed.vars().iter().map(|(k, v)| (k, v)))
        .envs(env)
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let no_path = handed.get("PATH").is_none() && !env.contains_key("PATH");
    let (child, _stderr) = TokioChildProcess::builder(cmd)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| {
            let command = command.to_string();
            let unsearched = no_path && !command.contains('/');
            match source.kind() {
                io::ErrorKind::NotFound if unsearched => {
                    McpStartError::NotOnPath { command, source }
                }
                _ => McpStartError::Spawn { command, source },
            }
        })?;
    Ok(child)
}

/// Header names and values as HTTP takes them, or the one that cannot be sent.
pub(crate) fn header_map(
    headers: &BTreeMap<String, String>,
) -> Result<std::collections::HashMap<HeaderName, HeaderValue>, McpStartError> {
    headers
        .iter()
        .map(|(k, v)| {
            let name = HeaderName::try_from(k.as_str()).map_err(|e| McpStartError::HeaderName {
                name: k.clone(),
                source: e.into(),
            })?;
            let value =
                HeaderValue::try_from(v.as_str()).map_err(|e| McpStartError::HeaderValue {
                    name: k.clone(),
                    source: e.into(),
                })?;
            Ok((name, value))
        })
        .collect()
}

/// Streamable HTTP to `url`, `headers` on every request. An error names the
/// URL without its query, fragment or userinfo ([`ScrubbedClient`]): a key
/// may ride in one.
pub(crate) fn http(
    url: &str,
    headers: &BTreeMap<String, String>,
) -> Result<StreamableHttpClientTransport<ScrubbedClient>, McpStartError> {
    let headers = header_map(headers)?;
    let config = StreamableHttpClientTransportConfig::with_uri(url).custom_headers(headers);
    Ok(StreamableHttpClientTransport::with_client(
        ScrubbedClient(http_client()?),
        config,
    ))
}

/// haest's reqwest client ([`reqwest_builder`]: no proxy from the
/// environment, which would be sent the server's headers (D15); a redirect
/// followed only as a same-origin 307 or 308, so another origin is never sent
/// the session id and the headers, and a POST never turns into a GET without
/// the message). Like rmcp's own, it keeps no idle connections (a reused one
/// can stall ~40 ms). Built once (by haest-http, which sets up TLS) and shared
/// by every HTTP server.
fn http_client() -> Result<reqwest::Client, McpStartError> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let builder = reqwest_builder().pool_max_idle_per_host(0);
    let built = build_reqwest(builder).map_err(McpStartError::Client)?;
    Ok(CLIENT.get_or_init(|| built).clone())
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
        let e = header_map(&bad).unwrap_err();
        assert!(matches!(e, McpStartError::HeaderValue { name, .. } if name == "X-Bad"));
        let bad = BTreeMap::from([("X Bad".to_string(), "v".to_string())]);
        let e = header_map(&bad).unwrap_err();
        assert!(matches!(e, McpStartError::HeaderName { name, .. } if name == "X Bad"));
    }

    #[tokio::test]
    async fn an_http_transport_builds_its_tls_client_without_dialling() {
        let headers = BTreeMap::from([("Authorization".to_string(), "Bearer t".to_string())]);
        assert!(http("https://example.invalid/mcp", &headers).is_ok());
    }
}
