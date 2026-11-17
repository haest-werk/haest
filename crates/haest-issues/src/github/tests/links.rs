//! Linking issues: where GitHub keeps each relation, and the links this
//! tracker refuses before it sends anything.

use serde_json::{Value, json};

use super::*;

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
        assert_eq!(h.calls()[1].json_body(), json!({"sub_issue_id": 1005}));
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
    assert_eq!(http.calls()[1].json_body(), json!({"issue_id": 1009}));
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
    assert_eq!(http.calls()[1].json_body(), json!({"sub_issue_id": 1005}));
}

/// GitHub keeps a parent link on the parent and a "blocks" link on the issue
/// that waits: with the other end in another repository, that is the issue
/// the write would go to. Refused, adding and removing alike, before anything
/// is sent (not even the GET of this repository's issue).
#[tokio::test]
async fn a_link_github_keeps_on_another_repositorys_issue_is_refused_unsent() {
    let other = "victim/secret#9";
    for relation in [Relation::Parent, Relation::Blocks] {
        for change in [LinkChange::Add, LinkChange::Remove] {
            // A GitHub that would take the write, so only the tracker can stop it.
            let http = FakeHttp::new([ok(issue(1, "one")), ok(json!({}))]);
            let link = Link::new(relation, other);
            let result = github(http.clone()).link("1", &link, change).await;
            let refused = matches!(&result, Err(TrackerError::Invalid(m)) if m.contains(other) && m.contains("o/r"));
            assert!(refused, "{relation:?} {change:?}: {result:?}");
            assert!(http.calls().is_empty(), "{:?}", http.sent());
        }
    }
}

/// A new parent takes the child off its old one, a link GitHub keeps on that
/// old parent. With the old parent in another repository, the add is refused
/// once the child is read, before any write: as a `child` of this
/// repository's issue (the child elsewhere) and as a `parent` (the child
/// here). An old parent in this repository is replaced; with none, nothing
/// is asked to be replaced.
#[tokio::test]
async fn a_new_parent_never_takes_a_child_off_another_repositorys_issue() {
    let old_parent = json!(format!("{API}/repos/o/third/issues/3"));
    let other9 = with(
        with(
            issue(9, "nine"),
            "repository_url",
            json!(format!("{API}/repos/o/other")),
        ),
        "parent_issue_url",
        old_parent.clone(),
    );
    let home5 = with(issue(5, "five"), "parent_issue_url", old_parent);
    for (id, link, child) in [
        ("1", Link::new(Relation::Child, "o/other#9"), other9),
        ("5", Link::new(Relation::Parent, "7"), home5),
    ] {
        // A GitHub that would take the write, so only the tracker can stop it.
        let http = FakeHttp::new([ok(child), ok(json!({}))]);
        let result = github(http.clone()).link(id, &link, LinkChange::Add).await;
        let refused = matches!(&result, Err(TrackerError::Invalid(m)) if m.contains("o/third#3"));
        assert!(refused, "{link:?}: {result:?}");
        assert_eq!(http.calls().len(), 1, "only the read: {:?}", http.sent());
    }

    let parent_here = json!(format!("{API}/repos/o/r/issues/2"));
    for (old, replace) in [(parent_here, Some(true)), (Value::Null, None)] {
        let http = FakeHttp::new([
            ok(with(issue(5, "five"), "parent_issue_url", old)),
            ok(json!({})),
        ]);
        github(http.clone())
            .link("5", &Link::new(Relation::Parent, "7"), LinkChange::Add)
            .await
            .unwrap();
        let body = http.calls()[1].json_body();
        assert_eq!(body["sub_issue_id"], 1005);
        assert_eq!(
            body.get("replace_parent").and_then(Value::as_bool),
            replace,
            "{body}"
        );
    }
}

/// A name that is not a GitHub owner or repository (`.` and `..` above all,
/// which would climb out of `/repos/` and out of an Enterprise root) is
/// refused as a link's other end, unsent; real names, a managed user's
/// `_shortcode` and a dotted repository among them, are taken.
#[tokio::test]
async fn a_link_to_a_name_github_cannot_have_is_refused_unsent() {
    let http = FakeHttp::new([]);
    let gh = github(http.clone()).with_api_url("https://ghe.example/api/v3");
    let long_owner = format!("{}/r#9", "o".repeat(40));
    let long_repo = format!("o/{}#9", "r".repeat(101));
    for target in [
        "../..#9",
        "./r#9",
        "o/.#9",
        "o/..#9",
        "a.b/r#9",
        "o/r r#9",
        "o%2F../r#9",
        "o/r%2e%2e#9",
        "ö/r#9",
        long_owner.as_str(),
        long_repo.as_str(),
    ] {
        let link = Link::new(Relation::DependsOn, target);
        let refused = gh.link("1", &link, LinkChange::Add).await;
        assert!(
            matches!(&refused, Err(TrackerError::Invalid(m)) if m.contains(target)),
            "{target}: {refused:?}"
        );
    }
    assert!(http.calls().is_empty(), "{:?}", http.sent());

    for (target, repo) in [
        ("octo_acme/r#9", "octo_acme/r"),
        ("o/.github#9", "o/.github"),
        ("O-1/My.repo_x-2#9", "O-1/My.repo_x-2"),
    ] {
        let nine = with(
            issue(9, "nine"),
            "repository_url",
            json!(format!("{API}/repos/{repo}")),
        );
        let http = FakeHttp::new([ok(nine), ok(json!({}))]);
        let link = Link::new(Relation::DependsOn, target);
        let got = github(http.clone()).link("1", &link, LinkChange::Add).await;
        assert!(got.is_ok(), "{target}: {got:?}");
        assert_eq!(http.sent()[0], format!("GET {API}/repos/{repo}/issues/9"));
    }
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
