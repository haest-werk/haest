//! What a card means as an [`Issue`]: its lane, its tags, its relations.

use super::front::{Front, Val};
use crate::tracker::{Issue, Link, Relation};

/// Every lane, in the order a board shows them; a new card starts in the first.
pub(crate) const STATUSES: [&str; 6] = [
    "inbox",
    "open",
    "in-progress",
    "in-review",
    "done",
    "archived",
];
/// The lanes of work not finished: what a list shows when it names none.
pub(crate) const OPEN_STATUSES: [&str; 4] = ["inbox", "open", "in-progress", "in-review"];
pub(crate) const PRIORITIES: [&str; 3] = ["low", "medium", "high"];

/// The keys an [`Issue`] already shows in its own fields; any other flat key is an extra.
const SHOWN: [&str; 8] = [
    "title",
    "status",
    "priority",
    "tags",
    "epic",
    "depends_on",
    "relates_to",
    "created",
];

/// The longest a card id derived from a title is.
pub(crate) const ID_MAX: usize = 60;

pub(crate) fn as_strings<const N: usize>(words: [&str; N]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

/// The card's lane: its `status:`, or `inbox` when that is missing or unknown.
pub(crate) fn status_of(f: &Front) -> &str {
    f.str("status")
        .filter(|s| STATUSES.contains(s))
        .unwrap_or(STATUSES[0])
}

/// A card's relations as it stores them: epic, depends_on, relates_to.
pub(crate) fn own_links(f: &Front) -> Vec<Link> {
    let mut links = Vec::new();
    if let Some(epic) = f.list("epic").into_iter().next() {
        links.push(Link::new(Relation::Parent, epic));
    }
    for (key, relation) in [
        ("depends_on", Relation::DependsOn),
        ("relates_to", Relation::RelatesTo),
    ] {
        links.extend(f.list(key).into_iter().map(|t| Link::new(relation, t)));
    }
    links
}

pub(crate) fn issue_of(id: &str, f: &Front, url: String) -> Issue {
    let extra = (f.keys.iter())
        .filter(|(k, _)| !SHOWN.contains(&k.as_str()))
        .filter_map(|(k, v)| {
            let value = match v {
                Val::Str(s) if s.is_empty() => return None,
                Val::Str(s) => s.clone(),
                Val::List(items) => items.join(", "),
                Val::Block(lines) => lines[1..].join("\n"),
            };
            Some((k.clone(), value))
        })
        .collect();
    Issue {
        id: id.to_string(),
        title: f.str("title").unwrap_or(id).to_string(),
        status: status_of(f).to_string(),
        priority: f
            .str("priority")
            .filter(|p| PRIORITIES.contains(p))
            .map(str::to_string),
        labels: match f.get("tags") {
            Some(Val::List(tags)) => tags.clone(),
            _ => Vec::new(),
        },
        body: f.body.clone(),
        url: Some(url),
        created: f.str("created").map(str::to_string),
        updated: None,
        links: own_links(f),
        extra,
    }
}

/// Every word of `query` (any case) is somewhere in the card's id, title, tags or body.
pub(crate) fn found_by(issue: &Issue, query: &str) -> bool {
    let hay = format!(
        "{}\n{}\n{}\n{}",
        issue.id,
        issue.title,
        issue.labels.join(" "),
        issue.body
    )
    .to_lowercase();
    query
        .split_whitespace()
        .all(|word| hay.contains(&word.to_lowercase()))
}

/// The card id a tool was given, or why it is not one: a name in the card
/// directory, never a path out of it. A trailing `.md` is the file, not the id.
pub(crate) fn check_id(id: &str) -> Result<&str, String> {
    let id = id.trim();
    let id = id.strip_suffix(".md").unwrap_or(id);
    let bad = id.is_empty()
        || id.starts_with('.')
        || id.contains(['/', '\\', '\0'])
        || id.chars().any(char::is_control);
    if bad {
        return Err(format!(
            "\"{id}\" is not a card id: give the card's file name without .md"
        ));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_a_name_in_the_directory_never_a_path() {
        assert_eq!(check_id("fix-login.md"), Ok("fix-login"));
        assert_eq!(check_id(" fix-login "), Ok("fix-login"));
        for bad in ["", "../x", "a/b", ".hidden", "a\\b", "a\nb"] {
            assert!(check_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_card_with_no_or_an_unknown_status_is_in_the_inbox() {
        let card = |status: &str| Front::parse(&format!("---\ntitle: t\n{status}\n---\n")).unwrap();
        assert_eq!(status_of(&card("status: done")), "done");
        assert_eq!(status_of(&card("status: someday")), "inbox");
        assert_eq!(status_of(&card("other: x")), "inbox");
    }
}
