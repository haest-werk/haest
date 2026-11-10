use std::{collections::HashMap, path::Path};

use super::*;
use crate::fs::MemFs;

fn skill_file(name: &str, extra: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: does {name}\n{extra}---\n\n{body}\n")
}

/// `(dir, name, extra frontmatter, body)` -> a MemFs with `<dir>/<name>/SKILL.md`.
fn fs_with(skills: &[(&str, &str, &str, &str)]) -> MemFs {
    MemFs::with_files(skills.iter().map(|(dir, name, extra, body)| {
        (
            format!("{dir}/{name}/SKILL.md"),
            skill_file(name, extra, body),
        )
    }))
}

fn opts() -> SkillOpts {
    SkillOpts {
        cwd: "/r/proj".into(),
        home: "/r/home".into(),
        config_dir: "/r/home/.claude".into(),
        sources: SkillSources::ALL,
    }
}

#[tokio::test]
async fn each_skill_source_is_read_only_when_asked_for() {
    let fixture = || {
        fs_with(&[
            ("/r/home/.claude/skills", "u1", "", "Body text."),
            ("/r/home/.agents/skills", "u2", "", "Body text."),
            ("/r/proj/.agents/skills", "p1", "", "Body text."),
            ("/r/proj/.claude/skills", "p2", "", "Body text."),
        ])
    };
    let found = |sources| async move {
        let skills = discover_skills(&fixture(), &SkillOpts { sources, ..opts() }).await;
        names(&skills).join(",")
    };
    let agents = SkillSources {
        agents: true,
        ..Default::default()
    };
    let claude = SkillSources {
        claude: true,
        ..Default::default()
    };
    assert_eq!(found(SkillSources::default()).await, "");
    assert_eq!(found(agents).await, "p1,u2");
    assert_eq!(found(claude).await, "p2,u1");
}

fn names(skills: &[Skill]) -> Vec<&str> {
    skills.iter().map(|s| s.name.as_str()).collect()
}

#[tokio::test]
async fn finds_claude_and_agents_skills_at_user_and_project_level() {
    let fs = fs_with(&[
        ("/r/home/.claude/skills", "u1", "", "Body text."),
        ("/r/home/.agents/skills", "u2", "", "Body text."),
        ("/r/proj/.agents/skills", "p1", "", "Body text."),
    ]);
    assert_eq!(
        names(&discover_skills(&fs, &opts()).await),
        ["p1", "u1", "u2"]
    );
}

#[tokio::test]
async fn the_prompt_lists_model_invocable_skills_only_never_bodies() {
    let fs = fs_with(&[
        ("/r/home/.claude/skills", "visible", "", "SECRET BODY"),
        (
            "/r/home/.claude/skills",
            "hidden",
            "disable-model-invocation: true\n",
            "Body text.",
        ),
    ]);
    let block = skills_prompt_block(&discover_skills(&fs, &opts()).await);
    assert!(block.contains("visible"));
    assert!(!block.contains("hidden"));
    assert!(!block.contains("SECRET BODY"));
    assert_eq!(skills_prompt_block(&[]), "");
}

#[tokio::test]
async fn the_loaded_body_drops_frontmatter_and_carries_base_dir_and_arguments() {
    let fs = fs_with(&[("/r/home/.claude/skills", "s", "", "THE BODY")]);
    let skills = discover_skills(&fs, &opts()).await;
    let body = load_skill_body(&fs, &skills[0], Some("arg1 arg2"))
        .await
        .unwrap();
    assert!(body.contains("/r/home/.claude/skills/s"));
    assert!(body.contains("THE BODY"));
    assert!(body.contains("arg1 arg2"));
    assert!(!body.contains("description:"));
}

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
