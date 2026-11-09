//! The two ways to reach a configured server: a child process on stdio, or
//! streamable HTTP. Building one can fail (no such program, a header HTTP cannot
//! carry); that failure is the server's, never the hub's.

use std::{collections::BTreeMap, path::PathBuf, process::Stdio};

use http::{HeaderName, HeaderValue};
use rmcp::transport::{
    StreamableHttpClientTransport, TokioChildProcess,
    streamable_http_client::StreamableHttpClientTransportConfig,
};

/// `command args...` with this process's environment plus `env`, in `cwd`. Its
/// stderr is ignored; it is killed when the connection drops.
pub(crate) fn stdio(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: Option<&PathBuf>,
) -> Result<TokioChildProcess, String> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args).envs(env).kill_on_drop(true);
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
    // reqwest takes the process-wide TLS provider when one is installed; this
    // installs ring only when nobody has installed one yet.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = StreamableHttpClientTransportConfig::with_uri(url).custom_headers(headers);
    Ok(StreamableHttpClientTransport::from_config(config))
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
