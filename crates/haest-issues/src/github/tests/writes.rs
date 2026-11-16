//! Creating, changing, deleting and linking issues: the requests GitHub gets.

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
        calls[0].body,
        Some(json!({"title": "t", "body": "b", "labels": ["bug"]}))
    );
    assert_eq!(calls[0].header("content-type"), Some("application/json"));
    assert_eq!(
        calls[1].body,
        Some(json!({"state": "closed", "state_reason": "completed"}))
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
        http.calls()[1].body,
        Some(json!({
            "state": "closed",
            "state_reason": "not_planned",
            "labels": ["a", "c"],
            "title": "new"
        }))
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
    let body = http.calls()[1].body.clone().unwrap();
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

#[tokio::test]
async fn each_relation_is_stored_where_github_keeps_it() {
    let added = || ok(json!({}));
    // parent: this issue becomes a sub-issue of the target.
    let http = FakeHttp::new([ok(issue(5, "five")), added()]);
    let gh = github(http.clone());
    gh.link("5", &Link::new(Relation::Parent, "1"), LinkChange::Add)
        .await
        .unwrap();
    // child: the target becomes this issue's sub-issue.
    let http2 = FakeHttp::new([ok(issue(5, "five")), added()]);
    github(http2.clone())
        .link("1", &Link::new(Relation::Child, "5"), LinkChange::Add)
        .await
        .unwrap();
    for h in [&http, &http2] {
        assert_eq!(
            h.sent(),
            [
                format!("GET {API}/repos/o/r/issues/5"),
                format!("POST {API}/repos/o/r/issues/1/sub_issues"),
            ]
        );
        assert_eq!(
            h.calls()[1].body,
            Some(json!({"sub_issue_id": 1005, "replace_parent": true}))
        );
    }
    // blocks: the target is blocked by this issue.
    let http = FakeHttp::new([ok(issue(9, "nine")), added()]);
    github(http.clone())
        .link("9", &Link::new(Relation::Blocks, "5"), LinkChange::Add)
        .await
        .unwrap();
    assert_eq!(
        http.sent()[1],
        format!("POST {API}/repos/o/r/issues/5/dependencies/blocked_by")
    );
    assert_eq!(http.calls()[1].body, Some(json!({"issue_id": 1009})));
}

#[tokio::test]
async fn unlinking_names_the_other_issue_by_its_id_in_its_own_repository() {
    let elsewhere = with(
        issue(9, "nine"),
        "repository_url",
        json!(format!("{API}/repos/x/y")),
    );
    let http = FakeHttp::new([ok(elsewhere), ok(json!({}))]);
    github(http.clone())
        .link(
            "5",
            &Link::new(Relation::DependsOn, "x/y#9"),
            LinkChange::Remove,
        )
        .await
        .unwrap();
    assert_eq!(
        http.sent(),
        [
            format!("GET {API}/repos/x/y/issues/9"),
            format!("DELETE {API}/repos/o/r/issues/5/dependencies/blocked_by/1009"),
        ]
    );
    let http = FakeHttp::new([ok(issue(5, "five")), ok(json!({}))]);
    github(http.clone())
        .link("5", &Link::new(Relation::Parent, "1"), LinkChange::Remove)
        .await
        .unwrap();
    assert_eq!(
        http.sent()[1],
        format!("DELETE {API}/repos/o/r/issues/1/sub_issue")
    );
    assert_eq!(http.calls()[1].body, Some(json!({"sub_issue_id": 1005})));
}

#[tokio::test]
async fn relates_to_and_a_link_to_itself_are_refused_unsent() {
    let http = FakeHttp::new([]);
    let gh = github(http.clone());
    assert!(!gh.info().relations.contains(&Relation::RelatesTo));
    for link in [
        Link::new(Relation::RelatesTo, "2"),
        Link::new(Relation::DependsOn, "#1"),
    ] {
        let refused = gh.link("1", &link, LinkChange::Add).await;
        assert!(matches!(refused, Err(TrackerError::Invalid(_))), "{link:?}");
    }
    assert!(http.calls().is_empty());
}

#[tokio::test]
async fn an_enterprise_root_moves_rest_and_graphql_alike() {
    let http = FakeHttp::new([ok(json!([])), ok(issue(4, "t")), ok(json!({"data": {}}))]);
    let gh = github(http.clone()).with_api_url("https://ghe.example/api/v3/");
    gh.list(&filter(1)).await.unwrap();
    gh.delete("4").await.unwrap();
    let sent = http.sent();
    assert!(sent[0].starts_with("GET https://ghe.example/api/v3/repos/o/r/issues?"));
    assert_eq!(sent[2], "POST https://ghe.example/api/graphql");
}
