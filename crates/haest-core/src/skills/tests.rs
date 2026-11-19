use super::*;
use crate::fs::MemFs;

fn skill_file(name: &str, extra: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: does {name}\n{extra}---\n\n{body}\n")
}

/// `(dir, name, extra frontmatter, body)` -> a MemFs with `<dir>/<name>/SKILL.md`.
fn fs_with(skills: &[(&str, &str, &str, &str)]) -> MemFs {
    MemFs::from_files(skills.iter().map(|(dir, name, extra, body)| {
        (
            format!("{dir}/{name}/SKILL.md"),
            skill_file(name, extra, body),
        )
    }))
}

fn opts() -> SkillOptions {
    SkillOptions {
        cwd: "/r/proj".into(),
        home: "/r/home".into(),
        claude_dir: "/r/home/.claude".into(),
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
        let skills = discover_skills(&fixture(), &SkillOptions { sources, ..opts() }).await;
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
    assert!(block.starts_with(DEFAULT_SKILLS_INTRO), "{block}");
    let listed: Vec<&str> = block.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(listed, ["- visible: does visible"]);
    assert!(!block.contains("SECRET BODY"), "{block}");
    assert_eq!(skills_prompt_block(&[]), "");
}

fn card(name: &str, origin: Option<&str>, when: &str) -> SkillCard {
    let description = format!("{}'s {name}", origin.unwrap_or("nobody"));
    let card = SkillCard::new(name, description).with_when(when);
    match origin {
        Some(origin) => card.with_origin(origin),
        None => card,
    }
}

fn inline(name: &str, origin: &str, body: &str) -> InlineSkill {
    InlineSkill::new(card(name, Some(origin), ""), body)
}

#[test]
fn a_hosts_cards_list_their_origin_and_when_under_its_own_intro() {
    let cards = [
        card("standup", Some("from /team/"), "a standup"),
        card("typst", None, ""),
    ];
    assert_eq!(
        skill_cards_block("Skills: pick one.", &cards),
        "Skills: pick one.\n- standup (from /team/): from /team/'s standup Use when: a standup\n- typst: nobody's typst"
    );
    assert_eq!(skill_cards_block("Skills: pick one.", &[]), "");
}

#[tokio::test]
async fn inline_skills_keep_the_first_of_a_name_and_load_their_body() {
    let set = InlineSkills::merged([
        vec![inline("standup", "from /team/", "TEAM")],
        vec![
            inline("standup", "personal", "MINE"),
            inline("notes", "personal", "MY NOTES"),
        ],
        vec![
            inline("board", "built in", "BOARD"),
            inline("notes", "built in", "THEIRS"),
        ],
    ]);
    let offered: Vec<(String, Option<String>)> = set
        .offered()
        .into_iter()
        .map(|c| (c.name, c.origin))
        .collect();
    let want = |n: &str, o: &str| (n.to_string(), Some(o.to_string()));
    assert_eq!(
        offered,
        [
            want("board", "built in"),
            want("notes", "personal"),
            want("standup", "from /team/")
        ]
    );
    assert_eq!(
        set.load("notes", None).await.unwrap(),
        "Skill notes (personal): personal's notes\n\nMY NOTES"
    );
    assert_eq!(
        set.load("standup", Some(" friday ")).await.unwrap(),
        "Skill standup (from /team/): from /team/'s standup\n\nARGUMENTS: friday\n\nTEAM"
    );
    let missing = set.load("nope", None).await.unwrap_err();
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
}

/// A list already merged is one list: collected or converted, it is the
/// set `merged` makes of it.
#[test]
fn one_list_of_inline_skills_is_the_merged_set_of_it() {
    let list = || {
        vec![
            inline("standup", "personal", "MINE"),
            inline("board", "built in", "BOARD"),
            inline("standup", "built in", "THEIRS"),
        ]
    };
    let merged = InlineSkills::merged([list()]);
    assert_eq!(InlineSkills::from(list()), merged);
    assert_eq!(list().into_iter().collect::<InlineSkills>(), merged);
}

#[tokio::test]
async fn disk_skills_offer_what_the_model_may_invoke_and_load_like_before() {
    let fs = fs_with(&[
        ("/r/home/.claude/skills", "visible", "", "THE BODY"),
        (
            "/r/home/.claude/skills",
            "hidden",
            "disable-model-invocation: true\n",
            "NOPE",
        ),
    ]);
    let skills = discover_skills(&fs, &opts()).await;
    let set = DiskSkills::new(fs, move || skills.clone());
    let offered: Vec<String> = set.offered().into_iter().map(|c| c.name).collect();
    assert_eq!(offered, ["visible"]);
    let body = set.load("visible", Some("a b")).await.unwrap();
    assert!(
        body.contains("THE BODY") && body.contains("ARGUMENTS: a b"),
        "{body}"
    );
    assert!(set.load("hidden", None).await.is_err());
}

#[tokio::test]
async fn every_clone_of_skills_sees_a_replaced_list_and_offers_only_what_the_model_may_pick() {
    let fs = fs_with(&[
        ("/r/home/.claude/skills", "visible", "", "THE BODY"),
        (
            "/r/home/.claude/skills",
            "hidden",
            "disable-model-invocation: true\n",
            "NOPE",
        ),
    ]);
    let found = discover_skills(&fs, &opts()).await;
    let skills = Skills::new(fs);
    let tool_side = skills.clone();
    assert!(tool_side.offered().is_empty());
    skills.replace(found);
    let offered: Vec<String> = tool_side.offered().into_iter().map(|c| c.name).collect();
    assert_eq!(offered, ["visible"]);
    assert_eq!(tool_side.list().len(), 2, "the list keeps every skill");
    assert!(
        tool_side
            .load("visible", None)
            .await
            .unwrap()
            .contains("THE BODY")
    );
    let hidden = tool_side.load("hidden", None).await;
    assert_eq!(hidden.map_err(|e| e.kind()), Err(io::ErrorKind::NotFound));
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
