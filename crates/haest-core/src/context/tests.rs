use std::path::{Path, PathBuf};

use super::*;
use crate::fs::MemFs;

fn contents(files: &[InstructionFile]) -> Vec<&str> {
    files.iter().map(|f| f.content.as_str()).collect()
}

#[tokio::test]
async fn each_kind_of_instruction_file_is_read_only_when_asked_for() {
    let fs = MemFs::with_files([
        ("/r/home/.claude/CLAUDE.md", "claude user"),
        ("/r/home/.agents/AGENTS.md", "agents user"),
        ("/r/home/repo/AGENTS.md", "repo agents"),
        ("/r/home/repo/CLAUDE.md", "repo claude"),
        ("/r/home/repo/.claude/CLAUDE.md", "repo dot claude"),
        ("/r/home/repo/CLAUDE.local.md", "repo local"),
    ]);
    let mut opts = InstructionOptions {
        cwd: "/r/home/repo".into(),
        home: "/r/home".into(),
        config_dir: None,
        sources: ContextSources::NONE,
    };
    assert!(find_instruction_files(&fs, &opts).await.is_empty());
    opts.sources.claude = true;
    let files = find_instruction_files(&fs, &opts).await;
    let claude_only = [
        "claude user",
        "repo claude",
        "repo dot claude",
        "repo local",
    ];
    assert_eq!(contents(&files), claude_only);
    (opts.sources.agents, opts.sources.claude) = (true, false);
    let files = find_instruction_files(&fs, &opts).await;
    assert_eq!(contents(&files), ["agents user", "repo agents"]);
    opts.sources.claude = true;
    let files = find_instruction_files(&fs, &opts).await;
    assert_eq!(
        contents(&files),
        [
            "claude user",
            "agents user",
            "repo agents",
            "repo claude",
            "repo dot claude",
            "repo local"
        ]
    );
}

#[tokio::test]
async fn user_files_first_then_outermost_project_dir_down_to_cwd_empty_files_skipped() {
    let fs = MemFs::with_files([
        ("/r/home/.claude/CLAUDE.md", "user rules"),
        ("/r/home/repo/AGENTS.md", "repo agents"),
        ("/r/home/repo/pkg/.claude/CLAUDE.md", "pkg rules"),
        ("/r/home/repo/pkg/CLAUDE.md", "   "),
    ]);
    let opts = InstructionOptions {
        cwd: "/r/home/repo/pkg".into(),
        home: "/r/home".into(),
        config_dir: Some("/r/home/.claude".into()),
        sources: ContextSources::ALL,
    };
    let files = find_instruction_files(&fs, &opts).await;
    assert_eq!(contents(&files), ["user rules", "repo agents", "pkg rules"]);
    assert_eq!(files[1].path, PathBuf::from("/r/home/repo/AGENTS.md"));
}

#[tokio::test]
async fn outside_home_the_chain_runs_from_the_root_and_long_files_are_cut() {
    let long = "x".repeat(MAX_FILE_CHARS + 10);
    let fs = MemFs::with_files([
        ("/AGENTS.md", "root rules"),
        ("/w/AGENTS.md", long.as_str()),
    ]);
    let opts = InstructionOptions {
        cwd: "/w".into(),
        home: "/home/u".into(),
        config_dir: None,
        sources: ContextSources {
            agents: true,
            claude: false,
        },
    };
    let files = find_instruction_files(&fs, &opts).await;
    assert_eq!(files[0].content, "root rules");
    assert!(files[1].content.ends_with("[truncated]"));
    let kept = files[1].content.chars().filter(|c| *c == 'x').count();
    assert_eq!(kept, MAX_FILE_CHARS);
}

#[test]
fn the_prompt_carries_environment_instructions_and_skills_and_omits_empty_sections() {
    let prompt = build_system_prompt(&PromptInputs {
        cwd: "/w".into(),
        model: "openrouter:x".into(),
        platform: "darwin".into(),
        date: "2026-09-28".into(),
        git_branch: None,
        instructions: vec![InstructionFile {
            path: "/w/AGENTS.md".into(),
            content: "be nice".into(),
        }],
        skills: vec![],
        tool_names: vec!["Read".into()],
        ..Default::default()
    });
    assert!(prompt.contains("/w"));
    assert!(prompt.contains("be nice"));
    assert!(prompt.contains("2026-09-28"));
    assert!(!prompt.contains("# Skills"));
    let bare = build_system_prompt(&PromptInputs::default());
    assert!(!bare.contains("# Instructions"));
    assert!(!bare.contains("\n\n\n"));
}

#[test]
fn the_prompt_teaches_only_the_tools_the_call_has_and_the_identity_can_be_replaced() {
    let with = |tools: &[&str]| {
        build_system_prompt(&PromptInputs {
            tool_names: tools.iter().map(|t| t.to_string()).collect(),
            ..Default::default()
        })
    };
    let none = with(&[]);
    assert!(!none.contains("Tool discipline") && none.contains("- Tools: none"));
    let all = with(&["Read", "Write", "Edit", "Bash", "Grep", "Glob"]);
    assert!(all.contains("- Use Read, Grep and Glob to look at code; use Bash for builds"));
    assert!(all.contains("Write only for new files"));
    let read = with(&["Read"]);
    assert!(read.contains("- Use Read to look at code.\n- Paths may be"));
    assert!(!read.contains("Edit") && !read.contains("Bash for"));
    let own = build_system_prompt(&PromptInputs {
        identity: Some("You are brain's reader.".into()),
        ..Default::default()
    });
    assert!(own.starts_with("You are brain's reader.") && !own.contains("You are haest"));
}

#[test]
fn the_workspace_note_goes_only_to_a_call_that_can_touch_the_workspace() {
    const NOTE: &str = "SANDBOX LIMITS";
    let with = |tools: &[&str]| {
        build_system_prompt(&PromptInputs {
            tool_names: tools.iter().map(|t| t.to_string()).collect(),
            workspace_note: Some(NOTE.into()),
            ..Default::default()
        })
    };
    for touches in [&["Bash"][..], &["Glob"], &["WebFetch", "Edit"]] {
        assert!(with(touches).contains(NOTE), "{touches:?}");
    }
    for not in [&[][..], &["WebFetch", "mcp__gh__search"]] {
        assert!(!with(not).contains(NOTE), "{not:?}");
    }
}

#[tokio::test]
async fn a_repo_on_a_branch_from_a_subdirectory() {
    let fs = MemFs::with_files([
        ("/r/repo/.git/HEAD", "ref: refs/heads/feat/x\n"),
        ("/r/repo/src/a.rs", ""),
    ]);
    let branch = current_git_branch(&fs, Path::new("/r/repo/src")).await;
    assert_eq!(branch.as_deref(), Some("feat/x"));
}

#[tokio::test]
async fn a_worktree_whose_git_is_a_file_pointing_at_its_git_dir() {
    let fs = MemFs::with_files([
        ("/r/gitdirs/wt/HEAD", "ref: refs/heads/wt-branch\n"),
        ("/r/wt/.git", "gitdir: /r/gitdirs/wt\n"),
        ("/r/rel/.git", "gitdir: ../gitdirs/wt\n"),
    ]);
    let branch = current_git_branch(&fs, Path::new("/r/wt")).await;
    assert_eq!(branch.as_deref(), Some("wt-branch"));
    let relative = current_git_branch(&fs, Path::new("/r/rel")).await;
    assert_eq!(relative.as_deref(), Some("wt-branch"));
}

#[tokio::test]
async fn detached_head_gives_a_short_sha_and_no_repo_gives_none() {
    let fs = MemFs::with_files([
        ("/r/d/.git/HEAD", "a1b2c3d4e5f6a7b8c9d0\n"),
        ("/r/plain/a.txt", ""),
    ]);
    let sha = current_git_branch(&fs, Path::new("/r/d")).await;
    assert_eq!(sha.as_deref(), Some("a1b2c3d4e5f6"));
    assert_eq!(current_git_branch(&fs, Path::new("/r/plain")).await, None);
}
