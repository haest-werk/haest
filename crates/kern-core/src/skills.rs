//! Agent Skills for kern: discovery (Claude Code's skill dirs, the cross-agent
//! `.agents/skills` dirs and Claude Code's installed plugins), and progressive
//! disclosure -- only name + description reach the system prompt; the body loads
//! on demand (the `Skill` tool).
//!
//! A host's skills plug in as a [`SkillSet`]: SKILL.md files on disk
//! ([`DiskSkills`]) or skills it holds itself ([`InlineSkills`]).
//!
//! Every read goes through a kern [`Fs`]; home and config dirs are passed in.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    User,
    Project,
    Plugin,
}

/// A `SKILL.md` read off disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    pub source: SkillSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    pub description: String,
    pub user_invocable: bool,
    /// The model may invoke it on its own when a request matches the description.
    pub model_invocable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    pub path: PathBuf,
}

crate::word_set! {
    /// Which skill dirs kern reads (`skills`). None of them is read unless the
    /// host says so.
    pub SkillSources {
        /// `~/.agents/skills` and every project level's `.agents/skills`.
        agents,
        /// `<config dir>/skills` and every project level's `.claude/skills`.
        claude,
        /// Claude Code's installed, enabled plugins.
        plugins,
    }
}

/// Where kern looks for skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillOpts {
    pub cwd: PathBuf,
    pub home: PathBuf,
    /// The user-level config dir (`~/.claude` unless the host says otherwise).
    pub config_dir: PathBuf,
    pub sources: SkillSources,
}

/// Where `opts`' skills live, worked out once (it reads Claude Code's plugin
/// list): [`skills_in`] scans them, [`scan_roots`] lists the dirs a sandbox mounts.
pub async fn skill_scan_roots(fs: &dyn Fs, opts: &SkillOpts) -> SkillScanRoots {
    let plugins = match opts.sources.plugins {
        true => read_installed_plugins(fs, &opts.config_dir).await,
        false => Vec::new(),
    };
    SkillScanRoots {
        config_dir: opts.config_dir.clone(),
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

/// Every skill kern can offer, sorted by name.
pub async fn discover_skills(fs: &dyn Fs, opts: &SkillOpts) -> Vec<Skill> {
    skills_in(fs, &skill_scan_roots(fs, opts).await).await
}

/// Every skill under `roots`, sorted by name.
pub async fn skills_in(fs: &dyn Fs, roots: &SkillScanRoots) -> Vec<Skill> {
    let mut skills = scan_skills(fs, roots).await;
    skills.sort_by(|a, b| by_name(&a.name, &b.name));
    skills
}

/// The skill body the model receives when it invokes a skill.
pub async fn load_skill_body(fs: &dyn Fs, skill: &Skill, args: Option<&str>) -> io::Result<String> {
    let text = String::from_utf8_lossy(&fs.read(&skill.path).await?).replace("\r\n", "\n");
    let body = frontmatter::split_frontmatter(&text).map_or(text.as_str(), |(_, b)| b);
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
pub fn skills_prompt_block(skills: &[Skill]) -> String {
    let offered: Vec<SkillCard> = skills
        .iter()
        .filter(|s| s.model_invocable)
        .map(Skill::card)
        .collect();
    skill_cards_block(DEFAULT_SKILLS_INTRO, &offered)
}
