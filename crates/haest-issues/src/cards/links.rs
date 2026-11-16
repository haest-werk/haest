//! Relations between cards, stored on ONE side as the board stores them:
//! `epic:` on the child, `depends_on:` on the one that waits, `relates_to:` on
//! the card it was added to (and read from both). The other side is computed.

use super::{
    CardTracker,
    card::check_id,
    front::{Front, Val},
};
use crate::tracker::{Link, LinkChange, Relation, TrackerError};

/// The relations other cards hold toward `id`: children (their `epic:`), the
/// cards waiting on it (their `depends_on:`), and `relates_to:` from their side.
pub(super) fn inverse_links(id: &str, own: &Front, all: &[(String, Front)]) -> Vec<Link> {
    let mut links = Vec::new();
    let own_related = own.list("relates_to");
    for (other, f) in all.iter().filter(|(other, _)| other != id) {
        let says = |key: &str| f.list(key).iter().any(|t| t == id);
        if says("epic") {
            links.push(Link::new(Relation::Child, other));
        }
        if says("depends_on") {
            links.push(Link::new(Relation::Blocks, other));
        }
        if says("relates_to") && !own_related.contains(other) {
            links.push(Link::new(Relation::RelatesTo, other));
        }
    }
    links
}

/// Following `key` from `from` reaches `to` (`epic` and `depends_on` must not loop).
fn reaches(all: &[(String, Front)], key: &str, from: &str, to: &str) -> bool {
    let mut seen: Vec<String> = Vec::new();
    let mut next = vec![from.to_string()];
    while let Some(at) = next.pop() {
        if at == to {
            return true;
        }
        if seen.contains(&at) {
            continue;
        }
        if let Some((_, f)) = all.iter().find(|(id, _)| *id == at) {
            next.extend(f.list(key));
        }
        seen.push(at);
    }
    false
}

impl CardTracker {
    pub(super) async fn change_link(
        &self,
        id: &str,
        link: &Link,
        change: LinkChange,
    ) -> Result<(), TrackerError> {
        let id = check_id(id).map_err(TrackerError::Invalid)?;
        let target = check_id(&link.target).map_err(TrackerError::Invalid)?;
        if id == target {
            return Err(TrackerError::Invalid(
                "a card cannot be linked to itself".into(),
            ));
        }
        let _one_writer = self.writes.lock().await;
        // Both must exist: a link to a card nobody wrote is a typo here.
        self.read(id).await?;
        self.read(target).await?;
        // Every relation as the side that stores it: (holder, key, the id it names).
        let (holder, key, named) = match link.relation {
            Relation::Parent => (id, "epic", target),
            Relation::Child => (target, "epic", id),
            Relation::DependsOn => (id, "depends_on", target),
            Relation::Blocks => (target, "depends_on", id),
            Relation::RelatesTo => (id, "relates_to", target),
        };
        if change == LinkChange::Add && key != "relates_to" {
            let all = self.all().await?;
            if reaches(&all, key, named, holder) {
                return Err(TrackerError::Invalid(format!(
                    "linking {id} {} {target} would make a loop",
                    link.relation.as_str()
                )));
            }
        }
        let mut front = self.read(holder).await?;
        let changed = match (key, change) {
            ("epic", LinkChange::Add) => {
                front.set("epic", Val::Str(named.into()));
                true
            }
            ("epic", LinkChange::Remove) => {
                let is = front.str("epic") == Some(named);
                if is {
                    front.remove("epic");
                }
                is
            }
            (_, LinkChange::Add) => {
                let mut items = front.list(key);
                if !items.iter().any(|t| t == named) {
                    items.push(named.into());
                }
                front.set_list(key, items);
                true
            }
            (_, LinkChange::Remove) => {
                let mut items = front.list(key);
                let before = items.len();
                items.retain(|t| t != named);
                let removed = items.len() < before;
                front.set_list(key, items);
                removed
            }
        };
        if changed {
            return self.write(holder, &front).await;
        }
        // relates_to reads both ways, so it may be stored on the other card.
        if key == "relates_to" {
            let mut other = self.read(target).await?;
            let mut items = other.list(key);
            if items.iter().any(|t| t == id) {
                items.retain(|t| t != id);
                other.set_list(key, items);
                return self.write(target, &other).await;
            }
        }
        Err(TrackerError::NotFound(format!(
            "{id} and {target} are not linked as {}",
            link.relation.as_str()
        )))
    }
}
