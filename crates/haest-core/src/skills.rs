//! Agent Skills for haest: discovery (Claude Code's skill dirs, the cross-agent
//! `.agents/skills` dirs and Claude Code's installed plugins), and progressive
//! disclosure -- only name + description reach the system prompt; the body loads
//! on demand (the `Skill` tool).
//!
//! A host's skills plug in as a [`SkillSet`]: SKILL.md files on disk
//! ([`DiskSkills`]) or skills it holds itself ([`InlineSkills`]).
//!
//! Every read goes through a haest [`Fs`]; home and Claude Code's dir are passed in.
//! Nothing is read unless the host turns a source on ([`SkillSources`]).
//!
//! The pieces, in the order a host meets them:
//! - [`discover_skills`] with a [`SkillOptions`]: every [`Skill`] in the dirs the
//!   sources name, sorted by name. ([`skill_scan_roots`] + [`skills_in`] is
//!   the same in two steps, for a host that also needs the dirs: [`scan_roots`].)
//! - [`skills_prompt_block`]: the system prompt's list of the skills the model
//!   may pick ([`skill_cards_block`] for a host's own intro and cards).
//! - [`DiskSkills`] / [`InlineSkills`]: the [`SkillSet`] the `Skill` tool
//!   (`haest-tools`) loads bodies from ([`load_skill_body`] for a disk skill).
//!
//! # Examples
//!
//! ```
//! use std::sync::Arc;
//!
//! use haest_core::{
//!     Fs, MemFs,
//!     skills::{DiskSkills, SkillOptions, SkillSet, SkillSources, discover_skills, skills_prompt_block},
//! };
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> std::io::Result<()> {
//! let fs: Arc<dyn Fs> = Arc::new(MemFs::with_files([(
//!     "/home/me/.claude/skills/release/SKILL.md",
//!     "---\nname: release\ndescription: Cut a release\n---\nBump the version, tag, push.\n",
//! )]));
//! let opts = SkillOptions {
//!     cwd: "/home/me/project".into(),
//!     home: "/home/me".into(),
//!     claude_dir: "/home/me/.claude".into(),
//!     sources: SkillSources { claude: true, ..SkillSources::NONE },
//! };
//!
//! let skills = discover_skills(fs.as_ref(), &opts).await;
//! assert!(skills_prompt_block(&skills).ends_with("\n- release: Cut a release"));
//!
//! let set = DiskSkills::new(move || skills.clone(), fs);
//! let body = set.load("release", Some("v2.0")).await?;
//! assert!(body.ends_with("ARGUMENTS: v2.0\n\nBump the version, tag, push."));
//! # Ok(())
//! # }
//! ```

mod frontmatter;
mod plugins;
mod scan;
mod set;
#[cfg(test)]
mod tests;

use std::{
    cmp::Ordering,
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub use plugins::read_installed_plugins;
pub use scan::{PluginRoot, SkillMeta, SkillScanRoots, parse_skill_file, scan_roots, scan_skills};
pub use set::{
    DEFAULT_SKILLS_INTRO, DiskSkills, InlineSkill, InlineSkills, SkillCard, SkillSet,
    first_of_each_name, skill_cards_block,
};

use crate::fs::Fs;

/// Which kind of skill dir a skill was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    /// The user's own: `<claude dir>/skills` or `~/.agents/skills`.
    User,
    /// A project's: `.claude/skills` or `.agents/skills` in the cwd or a dir above it.
    Project,
    /// An installed Claude Code plugin's `skills/` dir.
    Plugin,
}

/// A `SKILL.md` read off disk: its frontmatter and where it is. The body is
/// not kept; [`load_skill_body`] reads it when the model asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    /// The name the model loads it by: the frontmatter's `name`, else its
    /// dir's name; `plugin:name` for a plugin's.
    pub name: String,
    /// The kind of dir it came from.
    pub source: SkillSource,
    /// The plugin's name, for a plugin's skill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// What it is for: `description`, else `when_to_use`; may be empty.
    pub description: String,
    /// A human may run it as a command (`user-invocable`, default true).
    /// Recorded for the host; haest itself does not read it.
    pub user_invocable: bool,
    /// The model may invoke it on its own when a request matches the description
    /// (false when `disable-model-invocation: true`).
    pub model_invocable: bool,
    /// How to pass it arguments (`argument-hint`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    /// The tools it says it needs (`allowed-tools`). Recorded, not enforced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    /// The SKILL.md file itself; its dir is the skill's base directory.
    pub path: PathBuf,
}

crate::word_set! {
    /// Which skill dirs haest reads (`skills`). None of them is read unless the
    /// host says so.
    pub SkillSources ("skill source") {
        /// `~/.agents/skills` and every project level's `.agents/skills`.
        agents,
        /// `<claude dir>/skills` and every project level's `.claude/skills`.
        claude,
        /// Claude Code's installed, enabled plugins.
        plugins,
    }
}

/// Where haest looks for skills: the dirs to start from and the sources that
/// are on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillOptions {
    /// The project dir. Its skill dirs count, and every level's above it up
    /// to `home` (not `home` itself; up to `/` for a dir outside `home`).
    pub cwd: PathBuf,
    /// The user's home dir (for `~/.agents/skills`, and where the project
    /// levels stop).
    pub home: PathBuf,
    /// Claude Code's user dir (`<home>/.claude` unless the host says
    /// otherwise): its `skills` and its plugin list.
    pub claude_dir: PathBuf,
    /// Which skill dirs are read; [`SkillSources::NONE`] reads nothing.
    pub sources: SkillSources,
}

/// Where `opts`' skills live, worked out once (it reads Claude Code's plugin
/// list): [`skills_in`] scans them, [`scan_roots`] lists the dirs a sandbox mounts.
pub async fn skill_scan_roots(fs: &dyn Fs, opts: &SkillOptions) -> SkillScanRoots {
    let plugins = match opts.sources.plugins {
        true => read_installed_plugins(fs, &opts.claude_dir).await,
        false => Vec::new(),
    };
    SkillScanRoots {
        claude_dir: opts.claude_dir.clone(),
        cwd: opts.cwd.clone(),
        home: opts.home.clone(),
        agents: opts.sources.agents,
        claude: opts.sources.claude,
        plugins,
    }
}

impl Skill {
    /// Its line in the prompt's list.
    pub fn card(&self) -> SkillCard {
        SkillCard {
            name: self.name.clone(),
            description: self.description.clone(),
            when: String::new(),
            origin: None,
        }
    }
}

/// Case-insensitive first, the way a locale sort reads names.
fn by_name(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// Every skill haest can offer for `opts`, sorted by name (case-insensitive
/// first). One name, one skill: the nearest definition wins ([`scan_skills`]).
///
/// Never fails: a dir that is missing, or a SKILL.md that cannot be read or
/// parsed, is skipped.
pub async fn discover_skills(fs: &dyn Fs, opts: &SkillOptions) -> Vec<Skill> {
    skills_in(fs, &skill_scan_roots(fs, opts).await).await
}

/// Every skill under `roots`, sorted by name: [`scan_skills`], sorted.
pub async fn skills_in(fs: &dyn Fs, roots: &SkillScanRoots) -> Vec<Skill> {
    let mut skills = scan_skills(fs, roots).await;
    skills.sort_by(|a, b| by_name(&a.name, &b.name));
    skills
}

/// The skill body the model receives when it invokes a skill: a header naming
/// the skill and its base directory (relative paths in the skill resolve
/// against it), `ARGUMENTS: ...` when `args` has any, then the SKILL.md body
/// without its frontmatter. The file is read fresh, so an edit shows at once.
///
/// # Errors
///
/// The read error from `fs` (the file was moved or deleted since the scan).
/// Invalid UTF-8 is replaced, not an error.
pub async fn load_skill_body(fs: &dyn Fs, skill: &Skill, args: Option<&str>) -> io::Result<String> {
    let text = String::from_utf8_lossy(&fs.read(&skill.path).await?).replace("\r\n", "\n");
    let body = crate::frontmatter::split_frontmatter(&text).map_or(text.as_str(), |(_, b)| b);
    let dir = skill.path.parent().unwrap_or(Path::new("/"));
    let header = format!(
        "Skill \"{}\" loaded. Base directory: {}\nRelative paths in this skill resolve against that directory.",
        skill.name,
        dir.display()
    );
    let arg_line = set::argument_line(args);
    Ok(format!("{header}{arg_line}\n\n{}", body.trim()))
}

/// The system-prompt block that advertises model-invocable skills; empty when there are none.
/// haest's own intro ([`DEFAULT_SKILLS_INTRO`]), then one line per skill,
/// never a body ([`skill_cards_block`]).
pub fn skills_prompt_block(skills: &[Skill]) -> String {
    let offered: Vec<SkillCard> = skills
        .iter()
        .filter(|s| s.model_invocable)
        .map(Skill::card)
        .collect();
    skill_cards_block(DEFAULT_SKILLS_INTRO, &offered)
}
