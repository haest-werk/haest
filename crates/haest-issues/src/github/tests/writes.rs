//! Creating, changing and deleting issues: the requests GitHub gets.

use serde_json::json;

use super::*;

#[tokio::test]
async fn a_closed_create_is_a_post_then_a_close() {
    let closed = with(issue(7, "t"), "state", json!("closed"));
    let http = FakeHttp::new([ok(issue(7, "t")), ok(closed)]);
    let new = NewIssue {
        title: "t".into(),
        body: "b".into(),
        status: Some("closed".into()),
        labels: vec!["bug".into()],
        ..Default::default()
    };
    let made = github(http.clone()).create(new).await.unwrap();
    assert_eq!((made.id.as_str(), made.status.as_str()), ("7", "closed"));
    assert_eq!(
        http.sent(),
        [
            format!("POST {API}/repos/o/r/issues"),
            format!("PATCH {API}/repos/o/r/issues/7"),
        ]
    );
    let calls = http.calls();
    assert_eq!(
        calls[0].json_body(),
        json!({"title": "t", "body": "b", "labels": ["bug"]})
    );
    assert_eq!(calls[0].header("content-type"), Some("application/json"));
    assert_eq!(
        calls[1].json_body(),
        json!({"state": "closed", "state_reason": "completed"})
    );
}

#[tokio::test]
async fn adding_and_removing_labels_starts_from_the_ones_it_has() {
    let current = with(
        issue(3, "t"),
        "labels",
        json!([{"name": "a"}, {"name": "b"}]),
    );
    let http = FakeHttp::new([ok(current), ok(issue(3, "new"))]);
    let patch = IssuePatch {
        title: Some("new".into()),
        status: Some("not_planned".into()),
        add_labels: vec!["c".into()],
        remove_labels: vec!["b".into()],
        ..Default::default()
    };
    github(http.clone()).update("3", patch).await.unwrap();
    assert_eq!(
        http.sent(),
        [
            format!("GET {API}/repos/o/r/issues/3"),
            format!("PATCH {API}/repos/o/r/issues/3"),
        ]
    );
    assert_eq!(
        http.calls()[1].json_body(),
        json!({
            "state": "closed",
            "state_reason": "not_planned",
            "labels": ["a", "c"],
            "title": "new"
        })
    );
}

#[tokio::test]
async fn an_update_that_changes_nothing_is_refused_unsent() {
    let http = FakeHttp::new([]);
    let nothing = github(http.clone())
        .update("3", IssuePatch::default())
        .await;
    assert!(matches!(nothing, Err(TrackerError::Invalid(_))));
    assert!(http.calls().is_empty());
}

#[tokio::test]
async fn a_delete_is_graphqls_delete_issue_on_the_node_id() {
    let http = FakeHttp::new([
        ok(issue(4, "t")),
        ok(json!({"data": {"deleteIssue": {"clientMutationId": null}}})),
    ]);
    github(http.clone()).delete("4").await.unwrap();
    assert_eq!(
        http.sent(),
        [
            format!("GET {API}/repos/o/r/issues/4"),
            format!("POST {API}/graphql"),
        ]
    );
    let body = http.calls()[1].json_body();
    assert_eq!(body["variables"], json!({"id": "I_node4"}));
    assert!(body["query"].as_str().unwrap().contains("deleteIssue"));
    assert_eq!(
        http.calls()[1].header("authorization"),
        Some("Bearer t0k3n")
    );
}

#[tokio::test]
async fn a_graphql_error_is_a_failure_in_githubs_words() {
    let http = FakeHttp::new([
        ok(issue(4, "t")),
        ok(json!({"data": null, "errors": [{"message": "must have admin rights to Repository"}]})),
    ]);
    let Err(TrackerError::Failed(why)) = github(http).delete("4").await else {
        panic!("a GraphQL error must fail the delete");
    };
    assert!(why.contains("admin rights"), "{why}");
}

/// The tracker is one repository's: a model that names another repository's
/// issue (with the host's token) gets it refused, and nothing is sent.
#[tokio::test]
async fn an_issue_of_another_repository_is_never_read_changed_or_deleted() {
    let http = FakeHttp::new([]);
    let gh = github(http.clone());
    let other = "victim/secret#1";
    let patch = IssuePatch {
        title: Some("pwned".into()),
        ..Default::default()
    };
    let child = Link::new(Relation::Child, "2");
    let refused = [
        ("get", gh.get(other).await.map(drop)),
        ("update", gh.update(other, patch).await.map(drop)),
        ("delete", gh.delete(other).await),
        ("link", gh.link(other, &child, LinkChange::Add).await),
    ];
    for (call, result) in refused {
        let named = matches!(&result, Err(TrackerError::Invalid(m)) if m.contains(other) && m.contains("o/r"));
        assert!(named, "{call}: {result:?}");
    }
    assert!(http.calls().is_empty(), "{:?}", http.sent());
}

/// Its own repository named in full, in any case, is its own: the request
/// goes to the repository as the tracker names it.
#[tokio::test]
async fn its_own_repository_named_in_full_is_its_own() {
    let http = FakeHttp::new([ok(issue(5, "five"))]);
    let got = github(http.clone()).get("O/R#5").await.unwrap();
    assert_eq!(got.id, "5");
    assert_eq!(http.sent(), [format!("GET {API}/repos/o/r/issues/5")]);
}

#[tokio::test]
async fn an_enterprise_root_moves_rest_and_graphql_alike() {
    let http = FakeHttp::new([ok(json!([])), ok(issue(4, "t")), ok(json!({"data": {}}))]);
    let root = String::from("https://ghe.example/api/v3/");
    let gh = github(http.clone()).with_api_url(root);
    assert_eq!(gh.api_url(), "https://ghe.example/api/v3");
    assert_eq!((gh.owner(), gh.repo()), ("o", "r"));
    assert!(gh.has_token() && !GithubTracker::new("o", "r").has_token());
    let handed = haest_http::HttpClientRef::from(http.clone());
    assert!(haest_http::HttpClientRef::ptr_eq(gh.http(), &handed));
    gh.list(&filter(1)).await.unwrap();
    gh.delete("4").await.unwrap();
    let sent = http.sent();
    assert!(sent[0].starts_with("GET https://ghe.example/api/v3/repos/o/r/issues?"));
    assert_eq!(sent[2], "POST https://ghe.example/api/graphql");
}
