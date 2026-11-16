//! Read every `SKILL.md` a conversation can see, and say which ones the model is
//! allowed to pick on its own.
//!
//! Roots, in the order Claude Code resolves them:
//! - user: `<config dir>/skills/<name>/SKILL.md`
//! - project: `<dir>/.claude/skills/<name>/SKILL.md` for the cwd and every
//!   ancestor below the home directory (or up to the root, outside it)
//! - plugin: `<plugin path>/skills/<name>/SKILL.md`, named `plugin:name`
//!
//! Unreadable or malformed files are skipped, never an error: one broken skill
//! must not blank the whole list.
//!
//! The `agents` source reads the cross-agent `.agents/skills` convention
//! (`~/.agents/skills` + each project level); `claude` reads Claude Code's
//! (`<config dir>/skills` + each project level's `.claude/skills`).

use std::path::{Path, PathBuf};

use super::{
    Skill, SkillSource,
    frontmatter::{Meta, MetaMap, read_meta, split_frontmatter},
};
use crate::{
    context::project_chain,
    fs::{Fs, read_lossy},
};

/// A plugin whose `skills/` dir is scanned, its skills named `name:skill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRoot {
    pub name: String,
    pub path: PathBuf,
}

/// Where a skill scan reads, worked out: [`super::skill_scan_roots`] makes one
/// from [`super::SkillOpts`]. Each field is one fact the scan reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillScanRoots {
    pub config_dir: PathBuf,
    pub cwd: PathBuf,
    pub home: PathBuf,
    /// `.agents/skills`: `~/.agents/skills` and every project level's.
    pub agents: bool,
    /// Claude Code's: `<config dir>/skills` and every project level's `.claude/skills`.
    pub claude: bool,
    /// The plugins whose `skills/` dirs are read; empty = none.
    pub plugins: Vec<PluginRoot>,
}

/// What a SKILL.md's frontmatter says, before it is placed (source, path, plugin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub user_invocable: bool,
    pub model_invocable: bool,
    pub argument_hint: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
}

fn as_string(v: Option<&Meta>) -> Option<String> {
    match v {
        Some(Meta::Str(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

fn as_flag(v: Option<&Meta>, fallback: bool) -> bool {
    match v {
        Some(Meta::Bool(b)) => *b,
        Some(Meta::Str(s)) if s == "true" => true,
        Some(Meta::Str(s)) if s == "false" => false,
        _ => fallback,
    }
}

fn as_list(v: Option<&Meta>) -> Option<Vec<String>> {
    match v {
        Some(Meta::List(l)) => Some(l.clone()),
        Some(Meta::Str(s)) if !s.trim().is_empty() => Some(
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
        ),
        _ => None,
    }
}

/// Parse one SKILL.md; None for anything unusable (no frontmatter, no readable keys).
pub fn parse_skill_file(content: &str, fallback_name: &str) -> Option<SkillMeta> {
    let content = content.replace("\r\n", "\n");
    let (head, _) = split_frontmatter(&content)?;
    let meta: MetaMap = read_meta(&head)?;
    Some(SkillMeta {
        name: as_string(meta.get("name")).unwrap_or_else(|| fallback_name.into()),
        description: as_string(meta.get("description"))
            .or_else(|| as_string(meta.get("when_to_use")))
            .unwrap_or_default(),
        user_invocable: as_flag(meta.get("user-invocable"), true),
        model_invocable: !as_flag(meta.get("disable-model-invocation"), false),
        argument_hint: as_string(meta.get("argument-hint")),
        allowed_tools: as_list(meta.get("allowed-tools")),
    })
}

/// Project skill dirs, the farthest level first so the cwd's own are read
/// last and win; at one level in `dot_dirs` order, so the last one wins a tie.
/// The levels are [`project_chain`]'s, the same as the instruction files'.
fn project_skill_dirs(cwd: &Path, home: &Path, dot_dirs: &[&str]) -> Vec<PathBuf> {
    project_chain(cwd, home)
        .into_iter()
        .flat_map(|level| dot_dirs.iter().map(move |d| level.join(d).join("skills")))
        .collect()
}

struct ScanStep {
    dir: PathBuf,
    source: SkillSource,
    plugin: Option<String>,
}

/// Every skill directory, in the order later steps overwrite earlier ones.
fn scan_plan(roots: &SkillScanRoots) -> Vec<ScanStep> {
    let step = |dir: PathBuf, source| ScanStep {
        dir,
        source,
        plugin: None,
    };
    let (agents, claude) = (roots.agents, roots.claude);
    let mut plan = Vec::new();
    if agents {
        plan.push(step(roots.home.join(".agents/skills"), SkillSource::User));
    }
    if claude {
        plan.push(step(roots.config_dir.join("skills"), SkillSource::User));
    }
    // At one level `.claude` is read last, so it wins a tie.
    let dot_dirs: Vec<&str> = [(agents, ".agents"), (claude, ".claude")]
        .into_iter()
        .filter_map(|(on, d)| on.then_some(d))
        .collect();
    let project = project_skill_dirs(&roots.cwd, &roots.home, &dot_dirs);
    plan.extend(project.into_iter().map(|d| step(d, SkillSource::Project)));
    plan.extend(roots.plugins.iter().map(|p| ScanStep {
        dir: p.path.join("skills"),
        source: SkillSource::Plugin,
        plugin: Some(p.name.clone()),
    }));
    plan
}

async fn scan_dir(fs: &dyn Fs, step: &ScanStep) -> Vec<Skill> {
    let Ok(entries) = fs.read_dir(&step.dir).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = step.dir.join(&entry.name).join("SKILL.md");
        let Some(text) = read_lossy(fs, &path).await else {
            continue;
        };
        let Some(meta) = parse_skill_file(&text, &entry.name) else {
            continue;
        };
        let name = match &step.plugin {
            Some(p) => format!("{p}:{}", meta.name),
            None => meta.name,
        };
        out.push(Skill {
            name,
            source: step.source,
            plugin: step.plugin.clone(),
            description: meta.description,
            user_invocable: meta.user_invocable,
            model_invocable: meta.model_invocable,
            argument_hint: meta.argument_hint,
            allowed_tools: meta.allowed_tools,
            path,
        });
    }
    out
}

/// Nearest definition wins: project over user, like Claude Code. Plugins are namespaced.
/// Order: where each name was first seen.
pub async fn scan_skills(fs: &dyn Fs, roots: &SkillScanRoots) -> Vec<Skill> {
    let mut by_name: Vec<Skill> = Vec::new();
    for step in scan_plan(roots) {
        for skill in scan_dir(fs, &step).await {
            match by_name.iter_mut().find(|s| s.name == skill.name) {
                Some(slot) => *slot = skill,
                None => by_name.push(skill),
            }
        }
    }
    by_name
}

/// Every directory a skill scan may read, for a host that must grant access to them.
pub fn scan_roots(roots: &SkillScanRoots) -> Vec<PathBuf> {
    scan_plan(roots).into_iter().map(|s| s.dir).collect()
}
