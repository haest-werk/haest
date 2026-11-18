//! The API root a host hands in (`with_api_url`): never a credential shown,
//! never a request to a host other than the one written.

use serde_json::json;

use super::*;

#[test]
fn the_token_never_shows_in_debug_output() {
    let shown = format!("{:?}", github(FakeHttp::new([])));
    assert!(!shown.contains("t0k3n"), "{shown}");
    assert!(shown.contains("o") && shown.contains("handed in"));

    let enterprise =
        github(FakeHttp::new([])).with_api_url("https://ci:URL-SECRET@ghe.example.com/api/v3");
    let shown = format!("{enterprise:?}");
    assert!(!shown.contains("URL-SECRET"), "{shown}");
    assert!(shown.contains("ghe.example.com/api/v3"), "{shown}");
}

/// An API root whose password holds an unescaped `/` reads to a URL parser
/// as another host (`ci`, port 1234), which would be sent the token and the
/// rest of the password: no call goes out, and each says why and how to
/// write the root. Percent-encoded, it is sent to.
#[tokio::test]
async fn an_api_root_a_parser_misreads_is_never_sent_to() {
    let http = FakeHttp::new([ok(issue(1, "one")), ok(issue(1, "one"))]);
    let gh = github(http.clone()).with_api_url("https://ci:1234/s3cret@ghe.example/api/v3");
    for call in [
        gh.get("1").await.map(|_| ()),
        gh.list(&filter(5)).await.map(|_| ()),
    ] {
        let Err(TrackerError::Failed(why)) = call else {
            panic!("{call:?}");
        };
        assert!(!why.contains("s3cret") && !why.contains("1234"), "{why}");
        assert!(
            why.contains("%2F") && why.contains("ghe.example/api/v3"),
            "{why}"
        );
    }
    assert!(http.calls().is_empty(), "{:?}", http.calls());

    let encoded = github(http.clone()).with_api_url("https://ci:1234%2Fs3cret@ghe.example/api/v3");
    assert!(encoded.get("1").await.is_ok());
    assert_eq!(http.calls().len(), 1);
}

/// An API root with no host (`https://`, what `https://${GHE_HOST}` makes of
/// an empty variable) lost the `//` after its scheme to the trim of its
/// trailing `/`: `https:` passed the check, and the request went to
/// `https:/repos/...`, which a URL parser dials as the host `repos`, with
/// the token. Every call, GraphQL's too, now fails and sends nothing.
#[tokio::test]
async fn an_api_root_with_no_host_is_never_sent_to() {
    for root in ["https://", "http://", "https:///", "https:///api/v3"] {
        let http = FakeHttp::new([ok(json!([])), ok(json!({"data": {}}))]);
        let gh = github(http.clone()).with_api_url(root);
        let listed = gh.list(&filter(5)).await.map(|_| ());
        let asked = gh.graphql("query { viewer { login } }", json!({})).await;
        for call in [listed, asked.map(|_| ())] {
            assert!(
                matches!(call, Err(TrackerError::Failed(_))),
                "{root}: {call:?}"
            );
        }
        assert!(http.calls().is_empty(), "{root}: {:?}", http.sent());
    }
}

/// A root that is a scheme with only slashes after it (`https:`, `https:/`)
/// passed: a request URL under it (`https:/repos/...`) reads to a parser as
/// the host `repos`, which was sent the token, and GraphQL went to the host
/// `graphql`. A request URL under it cannot be told from one under a root
/// whose host is `repos`, so the root is checked too: every call fails and
/// sends nothing.
#[tokio::test]
async fn an_api_root_of_a_scheme_alone_is_never_sent_to() {
    for root in ["https:", "https:/", "https:\\", "http:"] {
        let http = FakeHttp::new([ok(json!([])), ok(json!({"data": {}}))]);
        let gh = github(http.clone()).with_api_url(root);
        let listed = gh.list(&filter(5)).await.map(|_| ());
        let asked = gh.graphql("query { viewer { login } }", json!({})).await;
        for call in [listed, asked.map(|_| ())] {
            assert!(
                matches!(call, Err(TrackerError::Failed(_))),
                "{root}: {call:?}"
            );
        }
        assert!(http.calls().is_empty(), "{root}: {:?}", http.sent());
    }
}

/// An Enterprise Server root with a CR after it (a CRLF `.env`), a space
/// before it, or written in upper case kept its `/v3` for GraphQL
/// (`.../api/v3/graphql`, a 404 on the same host). The root is read as a
/// parser reads it, and the `/v3` cut in any case.
#[tokio::test]
async fn an_enterprise_root_with_a_cr_or_in_upper_case_cuts_v3_for_graphql() {
    for (root, graphql) in [
        (
            "https://ghe.example/api/v3\r",
            "https://ghe.example/api/graphql",
        ),
        (
            "https://ghe.example/api/v3/\r\n",
            "https://ghe.example/api/graphql",
        ),
        (
            " https://ghe.example/api/v3",
            "https://ghe.example/api/graphql",
        ),
        (
            "HTTPS://GHE.EXAMPLE/API/V3",
            "HTTPS://GHE.EXAMPLE/API/graphql",
        ),
    ] {
        let http = FakeHttp::new([ok(json!({"data": {}}))]);
        let gh = github(http.clone()).with_api_url(root);
        gh.graphql("query { viewer { login } }", json!({}))
            .await
            .unwrap();
        assert_eq!(http.sent(), [format!("POST {graphql}")], "{root:?}");
    }
}

/// A root whose host is `v3` (no path) lost its host to the cut of an
/// Enterprise Server root's `/v3`: its GraphQL endpoint was
/// `https://graphql`, another host, sent the token. Only a `/v3` in the
/// path is cut.
#[tokio::test]
async fn only_a_v3_in_the_roots_path_is_cut_for_graphql() {
    let http = FakeHttp::new([ok(json!({"data": {}}))]);
    let gh = github(http.clone()).with_api_url("https://v3");
    gh.graphql("query { viewer { login } }", json!({}))
        .await
        .unwrap();
    assert_eq!(http.sent(), ["POST https://v3/graphql"]);
}
