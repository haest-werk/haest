use super::*;
use crate::{JobHub, inbox};
use haest_core::ToolRef;

fn names(tools: &[ToolRef]) -> Vec<String> {
    tools.iter().map(|t| t.name().to_string()).collect()
}

#[test]
fn the_builtin_list_has_every_tool_with_its_kind_and_web_fetch_is_optional() {
    let all = builtin_tools(BuiltinOptions::default());
    assert_eq!(
        names(&all),
        ["Read", "Write", "Edit", "Bash", "Grep", "Glob", "WebFetch"]
    );
    let kinds: Vec<_> = all.iter().map(|t| t.kind()).collect();
    assert_eq!(
        kinds,
        [
            "file.read",
            "file.write",
            "file.edit",
            "shell.exec",
            "text.search",
            "file.glob",
            "web.fetch"
        ]
    );
    let read_only: Vec<_> = all
        .iter()
        .filter(|t| t.read_only())
        .map(|t| t.name())
        .collect();
    assert_eq!(read_only, ["Read", "Grep", "Glob", "WebFetch"]);
    let offline = builtin_tools(BuiltinOptions::new().without_web_fetch());
    assert!(!names(&offline).contains(&"WebFetch".to_string()));
}

/// The tool guide and haest-runtime's Code policy go by FILE_TOOLS: a
/// renamed tool here would drop out of both silently.
#[test]
fn every_file_tool_is_a_builtin() {
    let all = names(&builtin_tools(BuiltinOptions::default()));
    for name in crate::FILE_TOOLS {
        assert!(all.iter().any(|t| t == name), "{name} is not a built-in");
    }
}

#[tokio::test]
async fn a_job_hub_adds_the_job_tools_after_bash() {
    let hub = JobHub::new(inbox::NoInbox);
    let opts = BuiltinOptions::new()
        .with_bash(BashOptions::default().with_jobs(hub))
        .without_web_fetch();
    let tools = builtin_tools(opts);
    assert_eq!(
        names(&tools),
        [
            "Read",
            "Write",
            "Edit",
            "Bash",
            "BashOutput",
            "KillShell",
            "Grep",
            "Glob"
        ]
    );
    let bash = &tools[3];
    assert!(bash.parameters()["properties"]["run_in_background"].is_object());
}

#[tokio::test]
async fn a_host_picks_built_ins_by_word_and_bash_brings_its_job_tools() {
    let pick = |words: &'static [BuiltinTool], jobs: Option<JobHub>| {
        let bash = jobs
            .into_iter()
            .fold(BashOptions::default(), BashOptions::with_jobs);
        let opts = BuiltinOptions::new().with_bash(bash);
        names(&builtin_tools_where(|t| words.contains(&t), opts).unwrap())
    };
    assert!(pick(&[], None).is_empty());
    let read_and_fetch = &[BuiltinTool::WebFetch, BuiltinTool::Read];
    assert_eq!(pick(read_and_fetch, None), ["Read", "WebFetch"]);
    let hub = JobHub::new(inbox::NoInbox);
    assert_eq!(
        pick(&[BuiltinTool::Bash], Some(hub)),
        ["Bash", "BashOutput", "KillShell"]
    );
    let every = builtin_tools(BuiltinOptions::default());
    assert!(every.iter().all(|t| BuiltinTool::of(t.name()).is_some()));
}

#[test]
fn a_word_that_is_on_is_never_silently_dropped() {
    let skills = haest_core::skills::InlineSkills::default();
    let given = BuiltinOptions::new()
        .with_tasks(TaskBoard::default())
        .with_skill(SkillToolOptions::new(skills));
    let pick = |tool: BuiltinTool, opts: BuiltinOptions| builtin_tools_where(|t| t == tool, opts);
    let tasks = pick(BuiltinTool::Tasks, given.clone()).unwrap();
    assert_eq!(
        names(&tasks),
        ["TaskCreate", "TaskUpdate", "TaskList", "TaskGet"]
    );
    assert_eq!(names(&pick(BuiltinTool::Skill, given).unwrap()), ["Skill"]);
    for tool in [
        BuiltinTool::WebFetch,
        BuiltinTool::Tasks,
        BuiltinTool::Skill,
    ] {
        let none = BuiltinOptions::new().without_web_fetch();
        let err = pick(tool, none).err().unwrap();
        assert_eq!(err.tool, tool);
    }
}

#[test]
fn every_built_in_reads_and_parses_as_its_word() {
    for &tool in BuiltinTool::ALL {
        assert_eq!(tool.to_string().parse(), Ok(tool));
    }
    let err = "fetch".parse::<BuiltinTool>().unwrap_err();
    assert_eq!(err.word(), "fetch");
}
