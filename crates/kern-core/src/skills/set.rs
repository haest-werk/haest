//! A host's skills as kern's Skill tool and prompt block read them. Skills on
//! disk (SKILL.md, [`DiskSkills`]) are one source; a host whose skills live
//! elsewhere (a database, a folder of its own, built into the binary) hands
//! them over as [`InlineSkills`] or implements [`SkillSet`] itself.

use std::{io, sync::Arc};

use async_trait::async_trait;

use super::{Skill, load_skill_body};
use crate::fs::Fs;

/// One skill as the model is offered it: a line of the prompt's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCard {
    pub name: String,
    pub description: String,
    /// When to load it ("Use when: ..."); empty = not said.
    pub when: String,
    /// Where it comes from, as the list names it ("built in", "from /team/"); None = not named.
    pub origin: Option<String>,
}

/// Where the Skill tool and the prompt's list get a host's skills.
#[async_trait]
pub trait SkillSet: Send + Sync {
    /// The skills the model may load now, in the order the list shows them
    /// (asked on every call: a set may change mid-conversation).
    fn offered(&self) -> Vec<SkillCard>;
    /// What the model reads when it loads `name`, one of [`Self::offered`];
    /// `NotFound` for any other.
    async fn load(&self, name: &str, args: Option<&str>) -> io::Result<String>;
}

fn not_found(name: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("no skill named {name}"))
}

/// ` (<origin>)`, or nothing when the card names none.
fn origin(c: &SkillCard) -> String {
    c.origin
        .as_ref()
        .map(|o| format!(" ({o})"))
        .unwrap_or_default()
}

/// `\n\nARGUMENTS: a` when the model passed any.
pub(super) fn argument_line(args: Option<&str>) -> String {
    match args.map(str::trim).filter(|a| !a.is_empty()) {
        Some(a) => format!("\n\nARGUMENTS: {a}"),
        None => String::new(),
    }
}

/// SKILL.md files (kern's discovery): the model-invocable ones, read from `fs` when loaded.
pub struct DiskSkills {
    list: Arc<dyn Fn() -> Vec<Skill> + Send + Sync>,
    fs: Arc<dyn Fs>,
}

impl DiskSkills {
    /// `list` is asked on every call (a rescan shows at once); `fs` reads the
    /// bodies (the host's own: a user skill lives outside any sandbox mount).
    pub fn new(list: impl Fn() -> Vec<Skill> + Send + Sync + 'static, fs: Arc<dyn Fs>) -> Self {
        Self {
            list: Arc::new(list),
            fs,
        }
    }

    fn offered_skills(&self) -> Vec<Skill> {
        (self.list)()
            .into_iter()
            .filter(|s| s.model_invocable)
            .collect()
    }
}

#[async_trait]
impl SkillSet for DiskSkills {
    fn offered(&self) -> Vec<SkillCard> {
        self.offered_skills().iter().map(Skill::card).collect()
    }

    async fn load(&self, name: &str, args: Option<&str>) -> io::Result<String> {
        let skills = self.offered_skills();
        let skill = skills
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| not_found(name))?;
        load_skill_body(self.fs.as_ref(), skill, args).await
    }
}

/// One skill a host holds in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSkill {
    pub card: SkillCard,
    pub body: String,
}

/// A host's skills held in memory, one per name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InlineSkills(Vec<InlineSkill>);

/// One per name, the FIRST of a name wins: pass the most specific list first
/// (a folder's, then the person's, then the built-in ones). Listed by name.
/// For a host that keeps skills of its own type ([`InlineSkills::merged`] is this over cards).
pub fn first_of_each_name<T>(
    lists: impl IntoIterator<Item = Vec<T>>,
    name: impl Fn(&T) -> &str,
) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for s in lists.into_iter().flatten() {
        if !out.iter().any(|had| name(had) == name(&s)) {
            out.push(s);
        }
    }
    out.sort_by(|a, b| super::by_name(name(a), name(b)));
    out
}

impl InlineSkills {
    /// One per name, the first of a name wins ([`first_of_each_name`]).
    pub fn merged(lists: impl IntoIterator<Item = Vec<InlineSkill>>) -> Self {
        Self(first_of_each_name(lists, |s| &s.card.name))
    }

    pub fn skills(&self) -> &[InlineSkill] {
        &self.0
    }
}

#[async_trait]
impl SkillSet for InlineSkills {
    fn offered(&self) -> Vec<SkillCard> {
        self.0.iter().map(|s| s.card.clone()).collect()
    }

    /// `Skill <name> (<origin>): <description>`, the arguments, then the body.
    async fn load(&self, name: &str, args: Option<&str>) -> io::Result<String> {
        let s = self
            .0
            .iter()
            .find(|s| s.card.name == name)
            .ok_or_else(|| not_found(name))?;
        let c = &s.card;
        Ok(format!(
            "Skill {}{}: {}{}\n\n{}",
            c.name,
            origin(c),
            c.description,
            argument_line(args),
            s.body
        ))
    }
}

/// Kern's own intro to the list (`# Skills`).
pub const DEFAULT_SKILLS_INTRO: &str = "# Skills\nSkills are packaged instructions for specific tasks. When a task matches a skill below, call the `Skill` tool with its name BEFORE doing the task, then follow what it returns.\n";

/// The system prompt's list: `intro`, then one line per skill, never a body.
/// Empty when there are none.
pub fn skill_cards_block(intro: &str, cards: &[SkillCard]) -> String {
    if cards.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = cards
        .iter()
        .map(|c| {
            let when = if c.when.is_empty() {
                String::new()
            } else {
                format!(" Use when: {}", c.when)
            };
            format!("- {}{}: {}{when}", c.name, origin(c), c.description)
        })
        .collect();
    format!("{intro}\n{}", lines.join("\n"))
}
