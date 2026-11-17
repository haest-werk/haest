//! The host's own skills (not SKILL.md files on disk) through haest's Skill
//! tool and prompt list; the host's own tools deferred behind ToolSearch.

use haest::{
    core::deferred_tools::{DeferMode, ToolDeferral},
    core::skills::{InlineSkill, InlineSkills, SkillCard, SkillSet, skill_cards_block},
    tools::{DeferredTools, SkillTool, SkillToolOptions},
};

use super::*;

fn skill(name: &str, origin: &str, description: &str, when: &str, body: &str) -> InlineSkill {
    InlineSkill {
        card: SkillCard {
            name: name.into(),
            description: description.into(),
            when: when.into(),
            origin: Some(origin.into()),
        },
        body: body.into(),
    }
}

#[tokio::test]
async fn the_hosts_own_skills_load_through_haests_skill_tool() {
    let mut r = main_rig(vec![
        vec![call("c1", "Skill", json!({"skill": "standup"}))],
        say("Did it your way."),
    ]);
    // The person's own standup wins over the built-in one of that name.
    let set = InlineSkills::merged([
        vec![skill(
            "standup",
            "yours",
            "How you run a standup",
            "a standup",
            "1. What I did.",
        )],
        vec![
            skill("standup", "built in", "A standup", "", "THE DEFAULT"),
            skill("board", "built in", "Keep a board", "", "Columns."),
        ],
    ]);
    let intro = "Skills: load one with Skill before you start.";
    let mut tools = r.app.base_tools();
    tools.push(SkillTool::new(
        SkillToolOptions::new(set.clone()).with_name_enum(true),
    ));
    let prompt = format!(
        "{}\n\n{}",
        r.app.prompt(),
        skill_cards_block(intro, &set.offered())
    );
    r.reseat(|deps| deps.with_tools(tools).with_system_prompt(prompt));

    r.answered("Standup time.").await;
    let first = &r.app.provider.requests(false)[0];
    assert!(
        first.system.contains(
            "Skills: load one with Skill before you start.\n- board (built in): Keep a board\n- standup (yours): How you run a standup Use when: a standup"
        ),
        "{}",
        first.system
    );
    assert!(
        !first.system.contains("1. What I did."),
        "bodies never ride in the prompt"
    );
    assert_eq!(
        r.output("c1").content,
        "Skill standup (yours): How you run a standup\n\n1. What I did."
    );
}

/// The rig's agent over the host's tools deferred, all but Bash.
fn defer_all_but_bash(r: &mut Rig, mode: DeferMode) {
    let deferral = ToolDeferral::new(mode).deferring_all_except(["Bash"]);
    let source = DeferredTools::new(r.app.base_tools(), deferral);
    r.reseat(|deps| deps.with_tools(source));
}

#[tokio::test]
async fn a_host_defers_its_own_tools_and_a_loaded_one_joins_the_list() {
    let mut r = main_rig(vec![
        vec![call("c1", "ToolSearch", json!({"query": "figure records"}))],
        vec![call("c2", "lookup", json!({"key": "q3"}))],
        // Never searched for: called by name, it runs anyway.
        vec![call(
            "c3",
            "Write",
            json!({"file_path": "/work/q3.md", "content": "up"}),
        )],
        say("Q3 is up 12%, written down."),
    ]);
    defer_all_but_bash(&mut r, DeferMode::Grow);
    r.answered("How was Q3?").await;

    let seen = r.app.provider.requests(false);
    assert_eq!(
        seen[0].tools,
        ["Bash", "ToolSearch"],
        "only Bash and the search go out"
    );
    let found = &r.output("c1").content;
    assert!(found.contains("\"name\":\"lookup\""), "{found}");
    assert_eq!(
        seen[1].tools,
        ["Bash", "ToolSearch", "lookup"],
        "a loaded tool joins"
    );
    assert_eq!(r.output("c2").content, "up 12%");
    assert!(!r.output("c3").is_error, "{:?}", r.output("c3"));
    assert_eq!(r.file("/work/q3.md").await, "up");
    // Loaded tools keep the host's order: the list depends on what is loaded,
    // never on the order it loaded in.
    assert_eq!(
        seen[3].tools,
        ["Bash", "ToolSearch", "Write", "lookup"],
        "a tool called by name counts as loaded"
    );
}

#[tokio::test]
async fn in_proxy_mode_the_list_never_changes_and_call_tool_runs_a_deferred_one() {
    let mut r = main_rig(vec![
        vec![call("c1", "ToolSearch", json!({"query": "select:lookup"}))],
        vec![call(
            "c2",
            "CallTool",
            json!({"name": "lookup", "input": {"key": "q2"}}),
        )],
        say("Q2 was flat."),
    ]);
    defer_all_but_bash(&mut r, DeferMode::Proxy);
    r.answered("And Q2?").await;
    let lists: Vec<Vec<String>> = r
        .app
        .provider
        .requests(false)
        .into_iter()
        .map(|s| s.tools)
        .collect();
    assert!(
        lists
            .iter()
            .all(|l| l == &["Bash", "ToolSearch", "CallTool"]),
        "the cached prefix holds: {lists:?}"
    );
    let c2 = r.output("c2");
    assert_eq!(
        (c2.content.as_str(), c2.title.as_deref()),
        ("flat", Some("Looked up q2"))
    );
}
