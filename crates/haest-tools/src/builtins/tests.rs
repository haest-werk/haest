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
    let offline = builtin_tools(BuiltinOptions {
        web_fetch: None,
        ..Default::default()
    });
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
    let tools = builtin_tools(BuiltinOptions {
        bash: BashOptions::default().with_jobs(hub),
        web_fetch: None,
        ..Default::default()
    });
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
    let pick = |words: &'static [&str], jobs: Option<JobHub>| {
        let bash = jobs
            .into_iter()
            .fold(BashOptions::default(), BashOptions::with_jobs);
        let opts = BuiltinOptions {
            bash,
            ..Default::default()
        };
        names(&builtin_tools_where(|w| words.contains(&w), opts).unwrap())
    };
    assert!(pick(&[], None).is_empty());
    assert_eq!(pick(&["web_fetch", "read"], None), ["Read", "WebFetch"]);
    let hub = JobHub::new(inbox::NoInbox);
    assert_eq!(
        pick(&["bash"], Some(hub)),
        ["Bash", "BashOutput", "KillShell"]
    );
    let every = builtin_tools(BuiltinOptions::default());
    assert!(every.iter().all(|t| builtin_word(t.name()).is_some()));
}

#[test]
fn a_word_that_is_on_is_never_silently_dropped() {
    let skills = || Vec::new();
    let fs = Arc::new(haest_core::fs::MemFs::new());
    let given = BuiltinOptions {
        tasks: Some(Arc::new(TaskBoard::default())),
        skill: Some(SkillToolOptions::new(skills, fs)),
        ..Default::default()
    };
    let pick = |word: &str, opts: BuiltinOptions| builtin_tools_where(|w| w == word, opts);
    let tasks = pick("tasks", given.clone()).unwrap();
    assert_eq!(
        names(&tasks),
        ["TaskCreate", "TaskUpdate", "TaskList", "TaskGet"]
    );
    assert_eq!(names(&pick("skill", given).unwrap()), ["Skill"]);
    for word in ["web_fetch", "tasks", "skill"] {
        let none = BuiltinOptions {
            web_fetch: None,
            ..Default::default()
        };
        let err = pick(word, none).err().unwrap();
        assert_eq!(err.word, word);
    }
}
