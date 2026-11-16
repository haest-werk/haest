use super::*;

#[test]
fn the_builders_set_what_the_struct_would() {
    let built = McpServerConfig::new(
        "s",
        McpTransport::stdio("srv")
            .with_args(["a"])
            .with_args(["b"])
            .with_env("K", "v")
            .with_cwd("/p"),
    );
    let written = McpServerConfig {
        name: "s".into(),
        transport: McpTransport::Stdio(StdioTransport {
            command: "srv".into(),
            args: vec!["a".into(), "b".into()],
            env: BTreeMap::from([("K".into(), "v".into())]),
            cwd: Some("/p".into()),
        }),
    };
    assert_eq!(built, written);
    let http = McpTransport::http("https://h.test/mcp").with_header("A", "1");
    assert_eq!(http.headers, BTreeMap::from([("A".into(), "1".into())]));
}
