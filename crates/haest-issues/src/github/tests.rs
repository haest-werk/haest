//! The GitHub backend against a scripted GitHub: what it asks for, and what
//! it makes of the answers. No network (the one live check is `#[ignore]`d).

use serde_json::json;

use super::*;
use crate::testkit::{API, FakeHttp, github, issue, ok, with};

mod writes;

fn filter(limit: usize) -> IssueFilter {
    IssueFilter {
        limit,
        ..Default::default()
    }
}

fn ids(issues: &[Issue]) -> Vec<&str> {
    issues.iter().map(|i| i.id.as_str()).collect()
}

#[tokio::test]
async fn a_list_asks_for_open_issues_with_the_token_and_skips_pull_requests() {
    let pr = with(issue(2, "a PR"), "pull_request", json!({"url": "x"}));
    let http = FakeHttp::new([ok(json!([issue(3, "c"), pr, issue(1, "a")]))]);
    let found = github(http.clone()).list(&filter(30)).await.unwrap();
    assert_eq!(ids(&found), ["3", "1"]);
    assert_eq!(found[0].status, "open");
    assert_eq!(
        http.sent(),
        [format!(
            "GET {API}/repos/o/r/issues?state=open&per_page=30&page=1"
        )]
    );
    let call = &http.calls()[0];
    assert_eq!(call.header("authorization"), Some("Bearer t0k3n"));
    assert_eq!(call.header("accept"), Some("application/vnd.github+json"));
    assert!(call.header("x-github-api-version").is_some());
    assert!(call.header("user-agent").is_some());
    assert_eq!(call.body, None);
}

#[tokio::test]
async fn a_list_reads_more_pages_until_it_has_enough() {
    let pr = with(issue(4, "a PR"), "pull_request", json!({"url": "x"}));
    let http = FakeHttp::new([
        ok(json!([issue(5, "e"), pr])),
        ok(json!([issue(3, "c"), issue(2, "b")])),
    ]);
    let found = github(http.clone()).list(&filter(2)).await.unwrap();
    assert_eq!(ids(&found), ["5", "3"]);
    assert_eq!(http.calls().len(), 2);
    assert!(http.sent()[1].ends_with("&page=2"));
}

#[tokio::test]
async fn not_planned_is_picked_out_of_the_closed_issues() {
    let closed = |n, reason| {
        let v = with(issue(n, "x"), "state", json!("closed"));
        with(v, "state_reason", json!(reason))
    };
    let http = FakeHttp::new([ok(json!([
        closed(2, "completed"),
        closed(1, "not_planned")
    ]))]);
    let wanted = IssueFilter {
        statuses: Some(vec!["not_planned".into()]),
        labels: vec!["bug".into(), "good first issue".into()],
        ..filter(10)
    };
    let found = github(http.clone()).list(&wanted).await.unwrap();
    assert_eq!(ids(&found), ["1"]);
    assert_eq!(found[0].status, "not_planned");
    assert_eq!(
        http.sent()[0],
        format!(
            "GET {API}/repos/o/r/issues?state=closed&per_page=10&labels=bug%2Cgood%20first%20issue&page=1"
        )
    );
}

#[tokio::test]
async fn github_has_no_priority_and_says_so_before_asking() {
    let http = FakeHttp::new([]);
    let gh = github(http.clone());
    assert!(gh.info().priorities.is_empty());
    let by_priority = IssueFilter {
        priority: Some("high".into()),
        ..filter(5)
    };
    assert!(matches!(
        gh.list(&by_priority).await,
        Err(TrackerError::Invalid(_))
    ));
    let new = NewIssue {
        title: "t".into(),
        priority: Some("high".into()),
        ..Default::default()
    };
    assert!(matches!(
        gh.create(new).await,
        Err(TrackerError::Invalid(_))
    ));
    let unknown = IssueFilter {
        statuses: Some(vec!["done".into()]),
        ..filter(5)
    };
    assert!(matches!(
        gh.list(&unknown).await,
        Err(TrackerError::Invalid(_))
    ));
    assert!(http.calls().is_empty());
}

#[tokio::test]
async fn a_search_stays_in_the_repository_and_its_issues() {
    let http = FakeHttp::new([ok(
        json!({"total_count": 1, "items": [issue(8, "login bug")]}),
    )]);
    let found = github(http.clone()).search("login bug", 5).await.unwrap();
    assert_eq!(ids(&found), ["8"]);
    assert_eq!(
        http.sent(),
        [format!(
            "GET {API}/search/issues?q=repo%3Ao%2Fr%20is%3Aissue%20login%20bug&per_page=5"
        )]
    );
}

#[tokio::test]
async fn a_get_reads_the_parent_children_and_what_blocks_it() {
    let mut v = issue(7, "seven");
    v["state"] = json!("closed");
    v["state_reason"] = json!("not_planned");
    v["labels"] = json!([{"name": "bug"}, {"name": "ui"}]);
    v["assignees"] = json!([{"login": "ann"}, {"login": "bo"}]);
    v["parent_issue_url"] = json!(format!("{API}/repos/o/r/issues/1"));
    v["sub_issues_summary"]["total"] = json!(2);
    v["issue_dependencies_summary"]["total_blocked_by"] = json!(1);
    let elsewhere = with(
        issue(9, "nine"),
        "repository_url",
        json!(format!("{API}/repos/x/y")),
    );
    let http = FakeHttp::new([
        ok(v),
        ok(json!([issue(5, "five"), issue(6, "six")])),
        ok(json!([elsewhere])),
    ]);
    let got = github(http.clone()).get("#7").await.unwrap();
    assert_eq!(
        (got.id.as_str(), got.status.as_str(), got.labels.clone()),
        (
            "7",
            "not_planned",
            vec!["bug".to_string(), "ui".to_string()]
        )
    );
    let links: Vec<(Relation, &str)> = (got.links.iter())
        .map(|l| (l.relation, l.target.as_str()))
        .collect();
    assert_eq!(
        links,
        [
            (Relation::Parent, "1"),
            (Relation::Child, "5"),
            (Relation::Child, "6"),
            (Relation::DependsOn, "x/y#9")
        ]
    );
    assert!(got.extra.contains(&("assignees".into(), "ann, bo".into())));
    assert_eq!(got.url.as_deref(), Some("https://github.com/o/r/issues/7"));
    // Nothing blocks on it, so its blocking list is never asked for.
    assert_eq!(
        http.sent(),
        [
            format!("GET {API}/repos/o/r/issues/7"),
            format!("GET {API}/repos/o/r/issues/7/sub_issues?per_page=100"),
            format!("GET {API}/repos/o/r/issues/7/dependencies/blocked_by?per_page=100"),
        ]
    );
}

#[tokio::test]
async fn a_pull_request_is_not_an_issue_and_a_bad_id_asks_nothing() {
    let pr = with(issue(2, "a PR"), "pull_request", json!({"url": "x"}));
    let http = FakeHttp::new([ok(pr)]);
    let gh = github(http.clone());
    assert!(matches!(gh.get("2").await, Err(TrackerError::NotFound(_))));
    for bad in ["two", "o/#3", "a/b/c#3", "#"] {
        assert!(
            matches!(gh.get(bad).await, Err(TrackerError::Invalid(_))),
            "{bad}"
        );
    }
    assert_eq!(http.calls().len(), 1);
}

#[tokio::test]
async fn refusals_carry_githubs_own_words() {
    let said = |status, message: &str| {
        crate::github::http::HttpReply::json(status, &json!({"message": message}))
    };
    let http = FakeHttp::new([
        said(404, "Not Found"),
        said(422, "Validation Failed"),
        said(500, "Server Error"),
    ]);
    let gh = github(http);
    assert!(matches!(gh.get("1").await, Err(TrackerError::NotFound(_))));
    let patch = IssuePatch {
        title: Some("x".into()),
        ..Default::default()
    };
    let Err(TrackerError::Invalid(why)) = gh.update("1", patch).await else {
        panic!("a 422 is a change GitHub refused");
    };
    assert!(why.contains("Validation Failed"), "{why}");
    let Err(TrackerError::Failed(why)) = gh.get("1").await else {
        panic!("a 500 is a failure");
    };
    assert!(why.contains("500") && why.contains("Server Error"), "{why}");

    let anonymous = GithubTracker::new("o", "r")
        .with_http(FakeHttp::new([said(401, "Requires authentication")]));
    let Err(TrackerError::Failed(why)) = anonymous.get("1").await else {
        panic!("a 401 is a failure");
    };
    assert!(why.contains("token"), "{why}");
    let down = github(FakeHttp::down("connection refused"));
    assert!(matches!(down.get("1").await, Err(TrackerError::Failed(_))));
}

#[test]
fn the_token_never_shows_in_debug_output() {
    let shown = format!("{:?}", github(FakeHttp::new([])));
    assert!(!shown.contains("t0k3n"), "{shown}");
    assert!(shown.contains("o") && shown.contains("handed in"));
}

/// The one live check: reads haest's own public repository with no token.
/// `cargo test -p haest-issues -- --ignored` runs it.
#[tokio::test]
#[ignore = "reaches api.github.com"]
async fn a_public_repository_lists_without_a_token() {
    let gh = GithubTracker::new("haest-werk", "haest");
    let every = IssueFilter {
        statuses: Some(gh.info().statuses),
        ..filter(5)
    };
    let found = gh.list(&every).await.expect("GitHub answered");
    assert!(found.len() <= 5);
    let searched = gh.search("haest", 5).await.expect("GitHub searched");
    assert!(searched.len() <= 5);
}
