//! One guarded GET: every redirect hop checked by the net guard, and a body
//! refused past the byte cap instead of buffered.

use haest_core::{error::report, tool::ToolError};
use haest_http::{HttpError, Request, Response};
use url::Url;

use super::WebFetchNet;
use crate::net_guard::assert_public_url;

/// The largest body read; a longer one is refused, never cut.
const MAX_BYTES: usize = 5 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const USER_AGENT: &str = "Mozilla/5.0 (compatible; haest-webfetch/1)";

/// GET `url`, following redirects by hand so each hop is guarded.
pub(super) async fn guarded_get(
    url: &Url,
    net: &WebFetchNet,
) -> Result<(Response, Url), ToolError> {
    let mut at = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        assert_public_url(&at, &*net.resolve)
            .await
            .map_err(|e| ToolError::Input(report(&e)))?;
        let get = Request::get(at.as_str()).with_header("user-agent", USER_AGENT);
        let res = net
            .http
            .send(get)
            .await
            .map_err(|e| ToolError::Input(format!("could not fetch {at}: {}", report(&e))))?;
        let location = match res.status {
            300..=399 => res.header("location").map(str::to_string),
            _ => None,
        };
        let Some(next) = location else {
            return Ok((res, at));
        };
        drop(res);
        at = at
            .join(&next)
            .map_err(|e| ToolError::Input(format!("bad redirect from {at} to \"{next}\": {e}")))?;
    }
    Err(ToolError::Input(format!(
        "more than {MAX_REDIRECTS} redirects starting at {url}"
    )))
}

/// The body, refusing anything past MAX_BYTES instead of buffering it.
pub(super) async fn capped_body(res: Response) -> Result<Vec<u8>, ToolError> {
    let declared: u64 = res
        .header("content-length")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    if declared > MAX_BYTES as u64 {
        return Err(ToolError::Input(format!(
            "the response is {declared} bytes; the limit is {MAX_BYTES}"
        )));
    }
    match res.body.bytes(MAX_BYTES).await {
        Ok(body) => Ok(body.to_vec()),
        Err(HttpError::TooLarge(_)) => Err(ToolError::Input(format!(
            "the response passed the {MAX_BYTES}-byte limit"
        ))),
        Err(e) => Err(ToolError::Input(report(&e))),
    }
}
