//! Read every `SKILL.md` a conversation can see, and say which ones the model is
//! allowed to pick on its own.
//!
//! Roots, in the order Claude Code resolves them:
//! - user: `<config dir>/skills/<name>/SKILL.md`
//! - project: `<dir>/.claude/skills/<name>/SKILL.md` for the cwd and every
//!   ancestor below the home directory
//! - plugin: `<plugin path>/skills/<name>/SKILL.md`, named `plugin:name`
//!
//! Unreadable or malformed files are skipped, never an error: one broken skill
//! must not blank the whole list.
//!
//! `agents_dirs` reads the cross-agent `.agents/skills` convention
//! (`~/.agents/skills` + each project level); `claude_dirs` reads Claude
//! Code's (`<config dir>/skills` + each project level's `.claude/skills`).

use std::path::{Path, PathBuf};

use super::{
    Skill, SkillSource,
    frontmatter::{Meta, MetaMap, read_meta, split_frontmatter},
};
use crate::fs::{Fs, normalize};

/// A plugin whose `skills/` dir is scanned, its skills named `name:skill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRoot {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillScanRoots {
    pub config_dir: PathBuf,
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub plugins: Vec<PluginRoot>,
    /// Read `.agents/skills` at user and project level.
    pub agents_dirs: bool,
    /// Read `<config dir>/skills` and `.claude/skills` at project level.
    pub claude_dirs: bool,
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

/// Project skill dirs, nearest first; within one level in REVERSE `dot_dirs` order,
/// so after the caller's reverse the LAST dot dir is read last and wins.
fn project_skill_dirs(cwd: &Path, home: &Path, dot_dirs: &[&str]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let stop = normalize(home);
    let mut dir = normalize(cwd);
    while dir != stop {
        let Some(parent) = dir.parent().map(Path::to_path_buf) else {
            break;
        };
        for d in dot_dirs.iter().rev() {
            dirs.push(dir.join(d).join("skills"));
        }
        dir = parent;
    }
    dirs
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
    let mut plan = Vec::new();
    if roots.agents_dirs {
        plan.push(step(roots.home.join(".agents/skills"), SkillSource::User));
    }
    if roots.claude_dirs {
        plan.push(step(roots.config_dir.join("skills"), SkillSource::User));
    }
    // At one level `.claude` is read last, so it wins a tie.
    let dot_dirs: Vec<&str> = [
        (roots.agents_dirs, ".agents"),
        (roots.claude_dirs, ".claude"),
    ]
    .into_iter()
    .filter_map(|(on, d)| on.then_some(d))
    .collect();
    // Farthest ancestor first so the cwd's own skills overwrite.
    let project = project_skill_dirs(&roots.cwd, &roots.home, &dot_dirs);
    plan.extend(
        project
            .into_iter()
            .rev()
            .map(|d| step(d, SkillSource::Project)),
    );
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
        let Ok(bytes) = fs.read(&path).await else {
            continue;
        };
        let Some(meta) = parse_skill_file(&String::from_utf8_lossy(&bytes), &entry.name) else {
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
pub async fn scan_skills(roots: &SkillScanRoots, fs: &dyn Fs) -> Vec<Skill> {
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
