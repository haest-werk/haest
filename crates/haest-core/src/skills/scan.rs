//! Read every `SKILL.md` a conversation can see, and say which ones the model is
//! allowed to pick on its own.
//!
//! Roots, in the order Claude Code resolves them:
//! - user: `<claude dir>/skills/<name>/SKILL.md`
//! - project: `<dir>/.claude/skills/<name>/SKILL.md` for the cwd and every
//!   ancestor below the home directory (or up to the root, outside it)
//! - plugin: `<plugin path>/skills/<name>/SKILL.md`, named `plugin:name`
//!
//! Unreadable or malformed files are skipped, never an error: one broken skill
//! must not blank the whole list.
//!
//! The `agents` source reads the cross-agent `.agents/skills` convention
//! (`~/.agents/skills` + each project level); `claude` reads Claude Code's
//! (`<claude dir>/skills` + each project level's `.claude/skills`).

use std::path::{Path, PathBuf};

use super::{
    Skill, SkillOptions, SkillSource,
    frontmatter::{Meta, MetaMap, read_meta},
    read_installed_plugins,
};
use crate::{
    context::project_chain,
    frontmatter::split_frontmatter,
    fs::{Fs, read_lossy},
};

/// A plugin whose `skills/` dir is scanned, its skills named `name:skill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRoot {
    /// The plugin's name, without `@marketplace`.
    pub name: String,
    /// Where it is installed; its skills are in `<path>/skills`.
    pub path: PathBuf,
}

/// Where a skill scan reads: the host's [`SkillOptions`] with Claude Code's
/// plugin list read once ([`find`](Self::find)). [`scan_skills`] and
/// [`skills_in`](super::skills_in) scan it, [`dirs`](Self::dirs) lists the
/// dirs a sandbox mounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillScanRoots {
    /// The dirs to start from and the sources that are on.
    pub opts: SkillOptions,
    /// The plugins whose `skills/` dirs are read; empty = none.
    pub plugins: Vec<PluginRoot>,
}

impl SkillScanRoots {
    /// Where `opts`' skills live: with the `plugins` source on, the installed
    /// and enabled plugins in Claude Code's list, read now.
    pub async fn find(fs: &dyn Fs, opts: &SkillOptions) -> Self {
        let plugins = match opts.sources.plugins {
            true => read_installed_plugins(fs, &opts.claude_dir).await,
            false => Vec::new(),
        };
        Self {
            opts: opts.clone(),
            plugins,
        }
    }

    /// Every directory a skill scan may read, for a host that must grant
    /// access to them (a sandbox's mounts). Lists them whether or not they
    /// exist.
    pub fn dirs(&self) -> Vec<PathBuf> {
        scan_plan(self).into_iter().map(|s| s.dir).collect()
    }
}

/// What a SKILL.md's frontmatter says, before it is placed (source, path,
/// plugin). The fields mean what [`Skill`]'s do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMeta {
    /// `name`, else the fallback name (the skill's dir).
    pub name: String,
    /// `description`, else `when_to_use`, else empty.
    pub description: String,
    /// `user-invocable`, default true.
    pub user_invocable: bool,
    /// Not `disable-model-invocation`, default true.
    pub model_invocable: bool,
    /// `argument-hint`.
    pub argument_hint: Option<String>,
    /// `allowed-tools`: a YAML list, or one comma-separated string.
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
///
/// The frontmatter is read as strict YAML first; YAML that refuses (an
/// unquoted `description: ... e.g: "x"`, which Claude Code loads anyway) is
/// read line by line instead. A frontmatter whose
/// YAML aliases would grow it by more than about 64K nodes and bytes when
/// expanded is refused whole, before anything is built: a project's skill
/// must not be able to exhaust memory. Blank strings count as absent; `\r\n`
/// line ends are fine.
///
/// # Examples
///
/// ```
/// use haest_core::skills::parse_skill_file;
///
/// let text = "---\ndescription: Cut a release\nallowed-tools: Bash, Read\n---\nSteps...\n";
/// let meta = parse_skill_file(text, "release").unwrap();
/// assert_eq!(meta.name, "release"); // no `name:`, so the dir's name
/// assert_eq!(meta.allowed_tools.unwrap(), ["Bash", "Read"]);
/// assert!(meta.model_invocable);
/// assert!(parse_skill_file("no frontmatter", "x").is_none());
/// ```
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
    let opts = &roots.opts;
    let (agents, claude) = (opts.sources.agents, opts.sources.claude);
    let mut plan = Vec::new();
    if agents {
        plan.push(step(opts.home.join(".agents/skills"), SkillSource::User));
    }
    if claude {
        plan.push(step(opts.claude_dir.join("skills"), SkillSource::User));
    }
    // At one level `.claude` is read last, so it wins a tie.
    let dot_dirs: Vec<&str> = [(agents, ".agents"), (claude, ".claude")]
        .into_iter()
        .filter_map(|(on, d)| on.then_some(d))
        .collect();
    let project = project_skill_dirs(&opts.cwd, &opts.home, &dot_dirs);
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

/// Every skill under `roots`, one per name. Nearest definition wins: project
/// over user, the cwd's level over the levels above it, like Claude Code.
/// Plugins are namespaced (`plugin:name`), so they never collide.
/// Order: where each name was first seen; [`super::skills_in`] sorts by name.
///
/// Never fails: a missing dir, or a SKILL.md that cannot be read or parsed,
/// is skipped. Every subdirectory of a skill dir that holds a `SKILL.md` is
/// one skill.
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

#[cfg(test)]
mod tests;
