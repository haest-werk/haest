use std::path::{Path, PathBuf};

use super::*;
use crate::fs::MemFs;

#[tokio::test]
async fn user_files_first_then_outermost_project_dir_down_to_cwd_empty_files_skipped() {
    let fs = MemFs::with_files([
        ("/r/home/.claude/CLAUDE.md", "user rules"),
        ("/r/home/repo/AGENTS.md", "repo agents"),
        ("/r/home/repo/pkg/.claude/CLAUDE.md", "pkg rules"),
        ("/r/home/repo/pkg/CLAUDE.md", "   "),
    ]);
    let opts = InstructionOpts {
        cwd: "/r/home/repo/pkg".into(),
        home: "/r/home".into(),
        config_dir: Some("/r/home/.claude".into()),
    };
    let files = find_instruction_files(&fs, &opts).await;
    let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
    assert_eq!(contents, ["user rules", "repo agents", "pkg rules"]);
    assert_eq!(files[1].path, PathBuf::from("/r/home/repo/AGENTS.md"));
}

#[tokio::test]
async fn outside_home_the_chain_runs_from_the_root_and_long_files_are_cut() {
    let long = "x".repeat(MAX_FILE_CHARS + 10);
    let fs = MemFs::with_files([
        ("/AGENTS.md", "root rules"),
        ("/w/CLAUDE.md", long.as_str()),
    ]);
    let opts = InstructionOpts {
        cwd: "/w".into(),
        home: "/home/u".into(),
        config_dir: None,
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
