//! The card backend over a real directory on disk.

use std::{fs, path::Path};

use haest_workspace::RealFs;

use super::{card::STATUSES, *};
use crate::{IssueFilter, IssuePatch, Link, LinkChange, NewIssue, Relation, Tracker};

mod links;

/// 2026-11-16T12:00:00.000Z
const T: u64 = 1_794_830_400_000;

pub(super) fn board(dir: &Path) -> CardTracker {
    CardTracker::new(Arc::new(RealFs), dir).with_clock(|| T)
}

pub(super) fn write_card(dir: &Path, id: &str, text: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(format!("{id}.md")), text).unwrap();
}

pub(super) fn read_card(dir: &Path, id: &str) -> String {
    fs::read_to_string(dir.join(format!("{id}.md"))).unwrap()
}

fn new(title: &str) -> NewIssue {
    NewIssue {
        title: title.into(),
        ..Default::default()
    }
}

fn ids(issues: &[Issue]) -> Vec<&str> {
    issues.iter().map(|i| i.id.as_str()).collect()
}

#[tokio::test]
async fn a_new_card_gets_the_boards_keys_and_an_id_from_its_title() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cards");
    let b = board(&dir);
    let made = b
        .create(NewIssue {
            title: "Fix the login: Safari!".into(),
            body: "Passkeys fail.\n".into(),
            priority: Some("high".into()),
            labels: vec!["auth".into(), "web".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(made.id, "fix-the-login-safari");
    assert_eq!(
        read_card(&dir, "fix-the-login-safari"),
        "---
title: \"Fix the login: Safari!\"
status: inbox
priority: high
tags: [auth, web]
created: 2026-11-16T12:00:00.000Z
---

Passkeys fail.
"
    );
    // The id is taken: the next card of that title gets the next free one.
    let again = b.create(new("Fix the login: Safari!")).await.unwrap();
    assert_eq!(again.id, "fix-the-login-safari-2");
    let wordless = b.create(new("!!!")).await.unwrap();
    assert_eq!(wordless.id, format!("task-{T}"));
    assert!(matches!(
        b.create(new("  ")).await,
        Err(TrackerError::Invalid(_))
    ));
}

#[tokio::test]
async fn a_lane_change_rewrites_the_status_and_keeps_every_other_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let card = "---
title: Ship it
status: in-review
priority: medium
tags: [release]
epic: v2
created: 2026-08-19T12:00:00Z
gate: strict
evidence_tests: 374 pass
promise:
  closes:
    - one
---

# Ship it

Done when green.
";
    write_card(tmp.path(), "ship-it", card);
    let b = board(tmp.path());
    let patch = IssuePatch {
        status: Some("done".into()),
        ..Default::default()
    };
    let moved = b.update("ship-it", patch).await.unwrap();
    assert_eq!(moved.status, "done");
    let after = read_card(tmp.path(), "ship-it");
    assert_eq!(
        after,
        card.replace("status: in-review", "status: done"),
        "the file stays where it is, and only its status changed"
    );
    assert_eq!(
        fs::read_dir(tmp.path()).unwrap().count(),
        1,
        "no temp file left"
    );
}

#[tokio::test]
async fn an_update_changes_title_body_priority_and_tags_but_never_the_id() {
    let tmp = tempfile::tempdir().unwrap();
    let b = board(tmp.path());
    let made = b
        .create(NewIssue {
            labels: vec!["a".into(), "b".into()],
            ..new("Old title")
        })
        .await
        .unwrap();
    let patch = IssuePatch {
        title: Some("New title".into()),
        body: Some("New body.".into()),
        priority: Some("low".into()),
        add_labels: vec!["c".into()],
        remove_labels: vec!["a".into()],
        ..Default::default()
    };
    let changed = b.update(&made.id, patch).await.unwrap();
    assert_eq!(changed.id, "old-title");
    let got = b.get("old-title").await.unwrap();
    assert_eq!(
        (
            got.title.as_str(),
            got.body.as_str(),
            got.priority.as_deref()
        ),
        ("New title", "New body.", Some("low"))
    );
    assert_eq!(got.labels, ["b", "c"]);
    let clear = IssuePatch {
        labels: Some(Vec::new()),
        ..Default::default()
    };
    b.update("old-title", clear).await.unwrap();
    assert!(!read_card(tmp.path(), "old-title").contains("tags"));
    let bad = IssuePatch {
        status: Some("doing".into()),
        ..Default::default()
    };
    assert!(matches!(
        b.update("old-title", bad).await,
        Err(TrackerError::Invalid(_))
    ));
}

#[tokio::test]
async fn a_list_shows_the_open_lanes_newest_first_and_filters() {
    let tmp = tempfile::tempdir().unwrap();
    let card = |id: &str, status: &str, created: &str, extra: &str| {
        let text =
            format!("---\ntitle: {id}\nstatus: {status}\n{extra}created: {created}\n---\n\nbody\n");
        write_card(tmp.path(), id, &text);
    };
    card("old", "open", "2026-01-01T00:00:00Z", "tags: [ui]\n");
    card(
        "new",
        "in-progress",
        "2026-03-01T00:00:00Z",
        "priority: high\ntags: [ui, api]\n",
    );
    card("mid", "inbox", "2026-02-01T00:00:00Z", "");
    card("shipped", "done", "2026-04-01T00:00:00Z", "tags: [ui]\n");
    card("dropped", "archived", "2026-05-01T00:00:00Z", "");
    let b = board(tmp.path());
    let list = |statuses: Option<Vec<&str>>, labels: &[&str], priority: Option<&str>| {
        let filter = IssueFilter {
            statuses: statuses.map(|s| s.iter().map(|w| w.to_string()).collect()),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            priority: priority.map(str::to_string),
            limit: 10,
        };
        let b = &b;
        async move { b.list(&filter).await.unwrap() }
    };
    assert_eq!(ids(&list(None, &[], None).await), ["new", "mid", "old"]);
    assert_eq!(ids(&list(Some(vec!["done"]), &[], None).await), ["shipped"]);
    let every = Some(STATUSES.to_vec());
    assert_eq!(
        ids(&list(every.clone(), &["ui"], None).await),
        ["shipped", "new", "old"]
    );
    assert_eq!(ids(&list(None, &["ui", "api"], None).await), ["new"]);
    assert_eq!(ids(&list(every, &[], Some("high")).await), ["new"]);
    let one = IssueFilter {
        limit: 1,
        ..Default::default()
    };
    assert_eq!(ids(&b.list(&one).await.unwrap()), ["new"]);
}

#[tokio::test]
async fn a_search_finds_every_word_in_any_lane() {
    let tmp = tempfile::tempdir().unwrap();
    write_card(
        tmp.path(),
        "login",
        "---\ntitle: Login breaks\nstatus: done\ntags: [auth]\n---\n\nOn Safari only.\n",
    );
    write_card(tmp.path(), "other", "---\ntitle: Other\n---\n\nNothing.\n");
    let b = board(tmp.path());
    assert_eq!(ids(&b.search("safari AUTH", 10).await.unwrap()), ["login"]);
    assert!(b.search("safari chrome", 10).await.unwrap().is_empty());
    assert!(matches!(
        b.search(" ", 10).await,
        Err(TrackerError::Invalid(_))
    ));
}

#[tokio::test]
async fn a_delete_removes_the_file_and_paths_never_reach_outside() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cards");
    write_card(&dir, "gone", "---\ntitle: Gone\n---\n");
    fs::write(tmp.path().join("outside.md"), "keep").unwrap();
    let b = board(&dir);
    b.delete("gone.md").await.unwrap();
    assert!(!dir.join("gone.md").exists());
    assert!(matches!(
        b.delete("gone").await,
        Err(TrackerError::NotFound(_))
    ));
    for path in ["../outside", "/etc/hosts", ".hidden"] {
        assert!(
            matches!(b.get(path).await, Err(TrackerError::Invalid(_))),
            "{path}"
        );
        assert!(
            matches!(b.delete(path).await, Err(TrackerError::Invalid(_))),
            "{path}"
        );
    }
    assert!(tmp.path().join("outside.md").exists());
}

#[tokio::test]
async fn no_directory_is_no_cards_and_a_plain_file_is_a_card() {
    let tmp = tempfile::tempdir().unwrap();
    let b = board(&tmp.path().join("none-yet"));
    let all = IssueFilter {
        limit: 10,
        ..Default::default()
    };
    assert!(b.list(&all).await.unwrap().is_empty());
    write_card(tmp.path(), "note", "Just text.\n");
    write_card(tmp.path(), ".draft", "---\ntitle: hidden\n---\n");
    fs::write(tmp.path().join(".note.md.tmp"), "half a card").unwrap();
    let b = board(tmp.path());
    let found = b.list(&all).await.unwrap();
    assert_eq!(ids(&found), ["note"]);
    assert_eq!(
        (found[0].title.as_str(), found[0].status.as_str()),
        ("note", "inbox")
    );
    let patch = IssuePatch {
        priority: Some("low".into()),
        ..Default::default()
    };
    b.update("note", patch).await.unwrap();
    assert_eq!(
        read_card(tmp.path(), "note"),
        "---\nstatus: inbox\npriority: low\n---\n\nJust text.\n"
    );
}
