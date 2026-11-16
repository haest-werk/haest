use super::*;

#[test]
fn the_builders_set_what_the_struct_would() {
    let built = McpServerConfig::stdio("s", "srv")
        .with_args(["a"])
        .with_args(["b"])
        .with_env("K", "v")
        .with_cwd("/p");
    let written = McpServerConfig {
        name: "s".into(),
        transport: McpTransport::Stdio {
            command: "srv".into(),
            args: vec!["a".into(), "b".into()],
            env: BTreeMap::from([("K".into(), "v".into())]),
            cwd: Some("/p".into()),
        },
    };
    assert_eq!(built, written);
    let http = McpServerConfig::http("h", "https://h.test/mcp").with_header("A", "1");
    let McpTransport::Http { headers, .. } = http.transport else {
        panic!("http() makes an HTTP server");
    };
    assert_eq!(headers, BTreeMap::from([("A".into(), "1".into())]));
}

#[test]
#[should_panic(expected = "with_header on stdio")]
fn a_header_on_a_stdio_server_is_a_bug_said_at_once() {
    let _ = McpServerConfig::stdio("s", "srv").with_header("A", "1");
}
