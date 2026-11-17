//! What GitHub answers is checked, not only what the tracker asks: an answer
//! that is another repository's issue is never acted on or shown.

use serde_json::{Value, json};

use super::*;

/// `o/r#5` moved to `o/secret#12`: what a client that follows GitHub's 301
/// for a transferred issue hands back for `o/r#5`.
fn moved() -> Value {
    let v = with(
        issue(12, "secret"),
        "repository_url",
        json!(format!("{API}/repos/o/secret")),
    );
    with(v, "node_id", json!("I_secret12"))
}

/// The read before a delete, a label change or a link is the issue asked
/// for, else nothing more is sent: a delete never reaches the issue where
/// it went.
#[tokio::test]
async fn an_answer_that_is_another_issue_is_never_acted_on() {
    let labels = IssuePatch {
        add_labels: vec!["x".into()],
        ..Default::default()
    };
    let deleted = json!({"data": {"deleteIssue": {"clientMutationId": null}}});
    // A GitHub that would take every write, so only the tracker can stop it.
    let gh = |then: Value| {
        let http = FakeHttp::new([ok(moved()), ok(then)]);
        (github(http.clone()), http)
    };
    let (g, delete) = gh(deleted);
    let deleted = g.delete("5").await;
    let (g, get) = gh(json!([]));
    let got = g.get("5").await.map(drop);
    let (g, update) = gh(issue(5, "five"));
    let updated = g.update("5", labels).await.map(drop);
    let (g, link) = gh(json!({}));
    let linked = g
        .link("5", &Link::new(Relation::Parent, "7"), LinkChange::Add)
        .await;
    for (call, result, http) in [
        ("delete", deleted, delete),
        ("get", got, get),
        ("update", updated, update),
        ("link", linked, link),
    ] {
        assert!(
            matches!(result, Err(TrackerError::NotFound(_))),
            "{call}: {result:?}"
        );
        assert_eq!(http.calls().len(), 1, "{call}: {:?}", http.sent());
    }
}

/// A change GitHub answers with another issue went where the client was
/// sent: the call says it failed, never that it changed this issue.
#[tokio::test]
async fn a_change_answered_with_another_issue_fails() {
    let http = FakeHttp::new([ok(moved())]);
    let title = IssuePatch {
        title: Some("new".into()),
        ..Default::default()
    };
    let updated = github(http.clone()).update("5", title).await;
    assert!(
        matches!(updated, Err(TrackerError::Failed(_))),
        "{updated:?}"
    );
}

/// The words the model searches for stay words: a qualifier, a group, a
/// phrase or an operator in them never widens the search past this
/// repository, and what the answer holds from elsewhere is dropped.
#[tokio::test]
async fn a_search_stays_in_the_repository_whatever_the_model_writes() {
    let elsewhere = with(
        issue(9, "private"),
        "repository_url",
        json!(format!("{API}/repos/victim/secret")),
    );
    let http = FakeHttp::new([ok(json!({"items": [issue(8, "bug"), elsewhere]}))]);
    let query = r#"bug OR repo:victim/secret (org:victim "leak") -label:x and NOT y"#;
    let found = github(http.clone()).search(query, 5).await.unwrap();
    assert_eq!(ids(&found), ["8"]);
    let words = "repo:o/r is:issue bug repo victim/secret org victim leak label x y";
    assert_eq!(
        http.sent(),
        [format!(
            "GET {API}/search/issues?q={}&per_page=5",
            super::super::api::encode(words)
        )]
    );
}
