//! Agent Skills for kern: discovery (Claude Code's skill dirs, the cross-agent
//! `.agents/skills` dirs and Claude Code's installed plugins), and progressive
//! disclosure -- only name + description reach the system prompt; the body loads
//! on demand (the `Skill` tool).
//!
//! Every read goes through a kern [`Fs`]; home and config dirs are passed in.

mod frontmatter;
mod plugins;
mod scan;
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

/// Where kern looks for skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillOpts {
    pub cwd: PathBuf,
    pub home: PathBuf,
    /// The user-level config dir (`~/.claude` unless the host says otherwise).
    pub config_dir: PathBuf,
}

async fn kern_scan_roots(fs: &dyn Fs, opts: &SkillOpts) -> SkillScanRoots {
    SkillScanRoots {
        config_dir: opts.config_dir.clone(),
        cwd: opts.cwd.clone(),
        home: opts.home.clone(),
        plugins: read_installed_plugins(&opts.config_dir, fs).await,
        agents_dirs: true,
    }
}

/// Case-insensitive first, the way a locale sort reads names.
fn by_name(a: &Skill, b: &Skill) -> Ordering {
    a.name
        .to_lowercase()
        .cmp(&b.name.to_lowercase())
        .then_with(|| a.name.cmp(&b.name))
}

/// Every skill kern can offer, sorted by name.
pub async fn discover_skills(fs: &dyn Fs, opts: &SkillOpts) -> Vec<Skill> {
    let mut skills = scan_skills(&kern_scan_roots(fs, opts).await, fs).await;
    skills.sort_by(by_name);
    skills
}

/// Every directory kern's skills can live in -- a sandbox mounts these read-only.
pub async fn skill_roots(fs: &dyn Fs, opts: &SkillOpts) -> Vec<PathBuf> {
    scan_roots(&kern_scan_roots(fs, opts).await)
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
    let arg_line = match args.map(str::trim).filter(|a| !a.is_empty()) {
        Some(a) => format!("\n\nARGUMENTS: {a}"),
        None => String::new(),
    };
    Ok(format!("{header}{arg_line}\n\n{}", body.trim()))
}

/// The system-prompt block that advertises model-invocable skills; empty when there are none.
pub fn skills_prompt_block(skills: &[Skill]) -> String {
    let offered: Vec<String> = skills
        .iter()
        .filter(|s| s.model_invocable)
        .map(|s| format!("- {}: {}", s.name, s.description))
        .collect();
    if offered.is_empty() {
        return String::new();
    }
    [
        "# Skills".to_string(),
        "Skills are packaged instructions for specific tasks. When a task matches a skill below, call the `Skill` tool with its name BEFORE doing the task, then follow what it returns.".into(),
        String::new(),
    ]
    .into_iter()
    .chain(offered)
    .collect::<Vec<_>>()
    .join("\n")
}
