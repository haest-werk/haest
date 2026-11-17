//! A host's skills as haest's Skill tool and prompt block read them. Skills on
//! disk (SKILL.md, [`DiskSkills`]) are one source; a host whose skills live
//! elsewhere (a database, a folder of its own, built into the binary) hands
//! them over as [`InlineSkills`] or implements [`SkillSet`] itself.

use std::{
    fmt, io,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;

use super::{Skill, load_skill_body};
use crate::fs::FsRef;

/// One skill as the model is offered it: a line of the prompt's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCard {
    /// The name the model loads it by.
    pub name: String,
    /// What it is for, in a line.
    pub description: String,
    /// When to load it ("Use when: ..."); empty = not said.
    pub when: String,
    /// Where it comes from, as the list names it ("built in", "from /team/"); None = not named.
    pub origin: Option<String>,
}

/// Where the Skill tool and the prompt's list get a host's skills.
///
/// haest has two: [`DiskSkills`] (SKILL.md files) and [`InlineSkills`] (held
/// in memory). A host whose skills live elsewhere (a database) implements it.
///
/// # Examples
///
/// ```
/// use haest_core::skills::{InlineSkill, InlineSkills, SkillCard, SkillSet};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> std::io::Result<()> {
/// let card = SkillCard {
///     name: "standup".into(),
///     description: "Write the daily standup".into(),
///     when: "the user asks for a standup".into(),
///     origin: Some("built in".into()),
/// };
/// let set = InlineSkills::merged([vec![InlineSkill { card, body: "Yesterday, today, blockers.".into() }]]);
///
/// assert_eq!(set.offered()[0].name, "standup");
/// let loaded = set.load("standup", None).await?;
/// assert_eq!(loaded, "Skill standup (built in): Write the daily standup\n\nYesterday, today, blockers.");
/// assert!(set.load("nope", None).await.is_err());
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait SkillSet: Send + Sync {
    /// The skills the model may load now, in the order the list shows them
    /// (asked on every call: a set may change mid-conversation).
    fn offered(&self) -> Vec<SkillCard>;
    /// What the model reads when it loads `name`, one of [`Self::offered`];
    /// `args` is what the model passed along, if anything.
    ///
    /// # Errors
    ///
    /// `NotFound` for a name not offered; whatever reading the body fails with.
    async fn load(&self, name: &str, args: Option<&str>) -> io::Result<String>;
}

crate::plug_ref! {
    /// A [`SkillSet`], shared: what the Skill tool reads from.
    pub struct SkillSetRef(dyn SkillSet);
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

/// SKILL.md files (haest's discovery): the model-invocable ones, read from `fs` when loaded.
///
/// Offers only the skills the model may pick; a skill with
/// `disable-model-invocation` cannot be loaded through it either. A load
/// is [`load_skill_body`]. [`Skills`] is the same over a list the host
/// replaces.
pub struct DiskSkills {
    fs: FsRef,
    list: Box<dyn Fn() -> Vec<Skill> + Send + Sync>,
}

impl DiskSkills {
    /// `fs` reads the bodies (the host's own: a user skill lives outside any
    /// sandbox mount); `list` is asked on every call (a rescan shows at once).
    pub fn new(
        fs: impl Into<FsRef>,
        list: impl Fn() -> Vec<Skill> + Send + Sync + 'static,
    ) -> Self {
        Self {
            fs: fs.into(),
            list: Box::new(list),
        }
    }
}

impl fmt::Debug for DiskSkills {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiskSkills").finish_non_exhaustive()
    }
}

/// The model-invocable skills of `skills`, as the model is offered them.
fn offered_cards(skills: &[Skill]) -> Vec<SkillCard> {
    skills
        .iter()
        .filter(|s| s.model_invocable)
        .map(Skill::card)
        .collect()
}

/// `name`'s body, read from `fs`, when `skills` offers it to the model.
async fn load_offered(
    fs: &FsRef,
    skills: &[Skill],
    name: &str,
    args: Option<&str>,
) -> io::Result<String> {
    let skill = skills
        .iter()
        .find(|s| s.model_invocable && s.name == name)
        .ok_or_else(|| not_found(name))?;
    load_skill_body(&**fs, skill, args).await
}

#[async_trait]
impl SkillSet for DiskSkills {
    fn offered(&self) -> Vec<SkillCard> {
        offered_cards(&(self.list)())
    }

    async fn load(&self, name: &str, args: Option<&str>) -> io::Result<String> {
        load_offered(&self.fs, &(self.list)(), name, args).await
    }
}

/// A host's SKILL.md skills, held where every clone sees them: the host
/// replaces the list after a rescan ([`Skills::replace`]), and the Skill tool
/// and the prompt read it on their next call. A [`SkillSet`] over `fs`, like
/// [`DiskSkills`]; cheap to clone.
///
/// # Examples
///
/// ```
/// use haest_core::{
///     MemFs,
///     skills::{SkillOptions, SkillSet, SkillSources, Skills, discover_skills},
/// };
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> std::io::Result<()> {
/// let fs = MemFs::with_files([(
///     "/home/me/.claude/skills/release/SKILL.md",
///     "---\nname: release\ndescription: Cut a release\n---\nTag and push.\n",
/// )]);
/// let opts = SkillOptions {
///     cwd: "/home/me/project".into(),
///     home: "/home/me".into(),
///     claude_dir: "/home/me/.claude".into(),
///     sources: SkillSources { claude: true, ..SkillSources::NONE },
/// };
/// let found = discover_skills(&fs, &opts).await;
///
/// let skills = Skills::new(fs);
/// let for_the_tool = skills.clone();
/// assert!(for_the_tool.offered().is_empty());
///
/// // A rescan: every clone (the Skill tool's included) sees the new list.
/// skills.replace(found);
/// assert_eq!(for_the_tool.offered()[0].name, "release");
/// assert!(for_the_tool.load("release", None).await?.ends_with("Tag and push."));
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Skills {
    fs: FsRef,
    list: Arc<RwLock<Vec<Skill>>>,
}

impl Skills {
    /// No skills yet, their bodies read from `fs` once there are.
    pub fn new(fs: impl Into<FsRef>) -> Self {
        Self {
            fs: fs.into(),
            list: Arc::default(),
        }
    }

    /// These skills, with `skills` in place of what they held.
    pub fn with_skills(self, skills: Vec<Skill>) -> Self {
        self.replace(skills);
        self
    }

    /// Every skill held, the ones the model may not pick included.
    pub fn list(&self) -> Vec<Skill> {
        self.list
            .read()
            .expect("the skill list is never poisoned")
            .clone()
    }

    /// Hold `skills` in place of the list: every clone sees them at once.
    pub fn replace(&self, skills: Vec<Skill>) {
        *self.list.write().expect("the skill list is never poisoned") = skills;
    }
}

impl fmt::Debug for Skills {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<String> = self.list().into_iter().map(|s| s.name).collect();
        f.debug_struct("Skills")
            .field("names", &names)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl SkillSet for Skills {
    fn offered(&self) -> Vec<SkillCard> {
        offered_cards(&self.list())
    }

    async fn load(&self, name: &str, args: Option<&str>) -> io::Result<String> {
        load_offered(&self.fs, &self.list(), name, args).await
    }
}

/// One skill a host holds in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSkill {
    /// How it is listed.
    pub card: SkillCard,
    /// What the model reads when it loads it.
    pub body: String,
}

/// A host's skills held in memory, one per name, listed by name. Build it
/// with [`InlineSkills::merged`]; [`Default`] is the empty set.
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

    /// The skills, sorted by name.
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

/// haest's own intro to the list (`# Skills`): what a skill is, and to load
/// a matching one with the `Skill` tool BEFORE doing the task.
pub const DEFAULT_SKILLS_INTRO: &str = "# Skills\nSkills are packaged instructions for specific tasks. When a task matches a skill below, call the `Skill` tool with its name BEFORE doing the task, then follow what it returns.\n";

/// The system prompt's list: `intro`, then one line per skill, never a body.
/// Empty when there are none.
///
/// A line is `- <name> (<origin>): <description> Use when: <when>`, the
/// origin and the `Use when` part only when the card has them.
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
