//! Reading SKILL.md files and the dirs a scan reads, in order.

use std::{collections::HashMap, path::Path};

use super::*;
use crate::fs::MemFs;

#[test]
fn parse_reads_invocation_flags_defaulting_both_to_on() {
    let plain = parse_skill_file("---\nname: a\ndescription: x\n---\nbody", "a").unwrap();
    assert_eq!((plain.user_invocable, plain.model_invocable), (true, true));
    let hidden = parse_skill_file(
        "---\nname: b\nuser-invocable: false\ndisable-model-invocation: true\n---\n",
        "b",
    )
    .unwrap();
    assert_eq!(
        (hidden.user_invocable, hidden.model_invocable),
        (false, false)
    );
}

#[test]
fn a_block_scalar_description_is_read_whole() {
    let parsed = parse_skill_file(
        "---\nname: c\ndescription: |\n  line one\n  line two\n---\n",
        "c",
    )
    .unwrap();
    assert_eq!(parsed.description.split('\n').count(), 2);
}

#[test]
fn no_frontmatter_is_skipped_not_an_error() {
    assert!(parse_skill_file("# just markdown", "d").is_none());
}

#[test]
fn an_unquoted_description_containing_colon_space_is_read_anyway() {
    let parsed = parse_skill_file(
        "---\nname: e\ndescription: Do X. Triggers: \"a\", \"b\"\n---\n",
        "e",
    )
    .unwrap();
    assert_eq!(parsed.name, "e");
    assert!(parsed.description.starts_with("Do X."));
}

#[test]
fn lists_and_fallbacks() {
    let parsed = parse_skill_file(
        "---\nwhen_to_use: later\nallowed-tools: Read, Grep\n---\n",
        "dir-name",
    )
    .unwrap();
    assert_eq!(parsed.name, "dir-name");
    assert_eq!(parsed.description, "later");
    assert_eq!(parsed.allowed_tools.unwrap(), ["Read", "Grep"]);
}

const HOME: &str = "/r/home";
const CONFIG: &str = "/r/home/.claude";
const REPO: &str = "/r/home/code/repo";
const CWD: &str = "/r/home/code/repo/packages/app";
const PLUGIN: &str = "/r/plugins/toolkit";

fn scan_fixture() -> MemFs {
    let f = |fm: &str| format!("---\n{fm}\n---\n\n# body\n");
    MemFs::with_files([
        (
            format!("{CONFIG}/skills/shared/SKILL.md"),
            f("name: shared\ndescription: user copy"),
        ),
        (
            format!("{CONFIG}/skills/only-user/SKILL.md"),
            f("name: only-user\ndescription: u"),
        ),
        (
            format!("{REPO}/.claude/skills/shared/SKILL.md"),
            f("name: shared\ndescription: repo copy"),
        ),
        (
            format!("{CWD}/.claude/skills/local/SKILL.md"),
            f("name: local\ndescription: l"),
        ),
        (
            format!("{PLUGIN}/skills/lint/SKILL.md"),
            f("name: lint\ndescription: p"),
        ),
        (
            format!("{HOME}/.claude/skills-not-scanned/x/SKILL.md"),
            f("name: x"),
        ),
        (
            format!("{HOME}/.agents/skills/agents-user/SKILL.md"),
            f("name: agents-user\ndescription: a"),
        ),
        (
            format!("{CWD}/.agents/skills/local/SKILL.md"),
            f("name: local\ndescription: agents copy"),
        ),
    ])
}

fn roots(plugins: bool, agents_dirs: bool) -> SkillScanRoots {
    SkillScanRoots {
        config_dir: CONFIG.into(),
        cwd: CWD.into(),
        home: HOME.into(),
        agents: agents_dirs,
        claude: true,
        plugins: if plugins {
            vec![PluginRoot {
                name: "toolkit".into(),
                path: PLUGIN.into(),
            }]
        } else {
            vec![]
        },
    }
}

async fn scanned(plugins: bool, agents_dirs: bool) -> HashMap<String, Skill> {
    scan_skills(&scan_fixture(), &roots(plugins, agents_dirs))
        .await
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect()
}

#[test]
fn scan_roots_name_every_directory_the_scan_reads_plugin_roots_included() {
    let dirs = scan_roots(&roots(true, false));
    assert!(dirs.contains(&Path::new(CONFIG).join("skills")));
    assert!(dirs.contains(&Path::new(REPO).join(".claude/skills")));
    assert!(dirs.contains(&Path::new(PLUGIN).join("skills")));
}

#[test]
fn outside_home_every_level_up_to_the_root_is_a_project_level() {
    let outside = SkillScanRoots {
        cwd: "/srv/app".into(),
        ..roots(false, false)
    };
    let project: Vec<_> = scan_roots(&outside)
        .into_iter()
        .filter(|d| !d.starts_with(CONFIG))
        .collect();
    let want = [
        "/.claude/skills",
        "/srv/.claude/skills",
        "/srv/app/.claude/skills",
    ];
    assert_eq!(project, want.map(std::path::PathBuf::from));
}

#[tokio::test]
async fn collects_user_ancestor_project_and_plugin_roots() {
    let by_name = scanned(true, false).await;
    let mut found: Vec<&str> = by_name.keys().map(String::as_str).collect();
    found.sort();
    assert_eq!(found, ["local", "only-user", "shared", "toolkit:lint"]);
}

#[tokio::test]
async fn a_project_skill_overrides_the_user_skill_of_the_same_name() {
    assert_eq!(
        scanned(true, false).await["shared"].source,
        SkillSource::Project
    );
}

#[tokio::test]
async fn plugin_skills_are_namespaced_and_carry_their_plugin() {
    let by_name = scanned(true, false).await;
    let lint = &by_name["toolkit:lint"];
    assert_eq!(lint.source, SkillSource::Plugin);
    assert_eq!(lint.plugin.as_deref(), Some("toolkit"));
}

#[tokio::test]
async fn agents_dirs_are_read_only_on_opt_in_and_claude_wins_a_tie_at_one_level() {
    assert!(!scanned(false, false).await.contains_key("agents-user"));
    let opted = scanned(false, true).await;
    assert_eq!(opted["agents-user"].source, SkillSource::User);
    assert_eq!(opted["local"].description, "l");
}
