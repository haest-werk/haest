//! The `hooks` setting: entries from every layer appended in the layers'
//! order, each refused entry named with its file and its number there, a
//! project's file held to commands (and read only when trusted), and a
//! header's value never shown.

use haest_mods::{HookDialect, HookEntry, HookSettings, ModEvent};

use super::*;

/// What `hooks` holds when `raw` (its checked spelling, or TOML lines) is
/// all that set it.
fn hooks(raw: &str) -> HookSettings {
    let mut c = Config::bare("/w", HOME);
    c.set("hooks", raw).unwrap();
    c.hooks
}

/// Each entry's command, or its URL: what it runs, in the order it binds.
fn targets(hooks: &HookSettings) -> Vec<(ModEvent, &str)> {
    fn target(e: &HookEntry) -> &str {
        e.command().or(e.url()).unwrap_or_default()
    }
    hooks
        .entries()
        .map(|(event, e)| (event, target(e)))
        .collect()
}

const TRUST: &str = "trusted_dirs = [\"/w\"]\n";
const PROJECT_FILE: &str = "/w/app/.haest/config.toml";

#[tokio::test]
async fn a_command_and_a_url_in_either_dialect_read_from_a_file() {
    let file = r#"
[[hooks."tool.before"]]
command = "sh ~/.config/haest/hooks/no-rm-rf.sh"
dialect = "claude"
matcher = "Bash"

[[hooks."turn.end"]]
url = "https://review.example/turn"
headers = { authorization = "Bearer ${REVIEW_TOKEN}" }
timeout = 10
on_error = "block"
"#;
    let env = [("REVIEW_TOKEN", "s3cret")];
    let r = run("/w", &[(USER_FILE, file)], &env, &[]).await.unwrap();
    let got = &r.config.hooks;
    assert_eq!(
        targets(got),
        [
            (ModEvent::ToolBefore, "sh ~/.config/haest/hooks/no-rm-rf.sh"),
            (ModEvent::TurnEnd, "https://review.example/turn"),
        ]
    );
    let (_, guard) = got.entries().next().unwrap();
    assert_eq!(guard.dialect(), HookDialect::Claude);
    assert_eq!(guard.matcher(), Some("Bash"));
    assert_eq!(r.resolved.source("hooks").to_string(), USER_FILE);
    // The setting is as written; what the runtime binds has the token in.
    let filled = r#"{ "turn.end" = [{ url = "https://review.example/turn", headers = { authorization = "Bearer s3cret" }, timeout = 10, on_error = "block" }] }"#;
    let (_, review) = hooks(filled)
        .entries()
        .next()
        .map(|(e, h)| (e, h.clone()))
        .unwrap();
    let (_, loaded_review) = r.hooks.entries().nth(1).unwrap();
    assert_eq!(loaded_review, &review);
    assert_ne!(r.hooks, r.config.hooks);
    assert!(r.notes.is_empty(), "{:?}", r.notes);
}

#[tokio::test]
async fn every_layer_appends_its_entries_after_the_ones_before() {
    let user = format!(
        r#"{TRUST}profile = "p"
[[hooks."turn.end"]]
command = "user"
[[profiles.p.hooks."turn.end"]]
command = "profile"
"#
    );
    let project = "[[hooks.\"turn.end\"]]\ncommand = \"project\"\n[[hooks.\"tool.after\"]]\ncommand = \"log\"";
    let r = run(
        "/w/app",
        &[(USER_FILE, &user), (PROJECT_FILE, project)],
        // A variable in `key = value` lines, a flag as an inline table.
        &[("HAEST_HOOKS", "[[\"turn.end\"]]\ncommand = \"env\"")],
        &[
            ("hooks", r#"{ "turn.end" = [{ command = "flag 1" }] }"#),
            (
                "hooks",
                r#"{ "turn.end" = [{ url = "https://flag.example/2" }] }"#,
            ),
        ],
    )
    .await
    .unwrap();
    let end = ModEvent::TurnEnd;
    assert_eq!(
        targets(&r.config.hooks),
        [
            (ModEvent::ToolAfter, "log"),
            (end, "user"),
            (end, "project"),
            (end, "profile"),
            (end, "env"),
            (end, "flag 1"),
            (end, "https://flag.example/2"),
        ]
    );
    assert_eq!(r.resolved.source("hooks").to_string(), "--hooks");
}

/// Each refusal names the file and the entry as that file numbers it,
/// whatever the layers before it hold.
#[tokio::test]
async fn an_entry_a_file_may_not_hold_is_refused_naming_the_file_and_the_entry() {
    let cases = [
        (
            "[[hooks.\"tool.befor\"]]\ncommand = \"x\"",
            "unknown event \"tool.befor\"",
        ),
        (
            "[[hooks.tool.before]]\ncommand = \"x\"",
            "quote an event's name, [[hooks.\"tool.before\"]]",
        ),
        (
            "[hooks]\n\"turn.end\" = 1",
            "\"turn.end\" must be a list of tables",
        ),
        (
            "[[hooks.\"turn.end\"]]\ncommand = \"a\"\n[[hooks.\"turn.end\"]]\ncomand = \"b\"",
            "[[hooks.\"turn.end\"]] #2: unknown key `comand`",
        ),
        (
            "[[hooks.\"turn.end\"]]\ncommand = \"a\"\nurl = \"https://a.example\"",
            "[[hooks.\"turn.end\"]] #1: takes `command` or `url`, not both",
        ),
        (
            "[[hooks.\"turn.end\"]]\ntimeout = 5",
            "[[hooks.\"turn.end\"]] #1: needs `command`",
        ),
        (
            "[[hooks.\"model.before\"]]\ncommand = \"a\"\ndialect = \"claude\"",
            "[[hooks.\"model.before\"]] #1: model.before has no Claude Code event",
        ),
        (
            "[[hooks.\"model.after\"]]\ncommand = \"a\"\ndialect = \"claude\"",
            "[[hooks.\"model.after\"]] #1: model.after has no Claude Code event",
        ),
        (
            "[[hooks.\"prompt.submit\"]]\ncommand = \"a\"\nmatcher = \"Bash\"",
            "[[hooks.\"prompt.submit\"]] #1: `matcher` picks tools, on tool.before and tool.after only",
        ),
        ("hooks = \"sh x.sh\"", "must be a table: [hooks]"),
    ];
    // The user's file holds a good entry on every event: the project's
    // file still numbers its own from 1.
    let user = format!(
        "{TRUST}{}",
        ModEvent::ALL
            .iter()
            .map(|e| format!("[[hooks.\"{e}\"]]\ncommand = \"ok\"\n"))
            .collect::<String>()
    );
    for (body, says) in cases {
        for (path, files) in [
            (USER_FILE, vec![(USER_FILE, format!("{TRUST}{body}"))]),
            (
                PROJECT_FILE,
                vec![(USER_FILE, user.clone()), (PROJECT_FILE, body.to_string())],
            ),
        ] {
            let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
            let err = run("/w/app", &files, &[], &[]).await.unwrap_err();
            assert!(matches!(&err, ConfigError::Invalid { .. }), "{err:?}");
            assert_eq!(refused(&err), Some((&file(path), "hooks")), "{err}");
            let shown = err.to_string();
            assert!(shown.starts_with(&format!("{path}: hooks: ")), "{shown}");
            assert!(shown.contains(says), "{body}: {shown}");
        }
    }
}

#[tokio::test]
async fn a_variable_or_a_flag_is_refused_naming_itself() {
    let bad = "{ \"turn.end\" = [{ comand = \"x\" }] }";
    let by_var = run("/w", &[], &[("HAEST_HOOKS", bad)], &[])
        .await
        .unwrap_err();
    assert_eq!(
        refused(&by_var),
        Some((&Source::Env("HAEST_HOOKS".into()), "hooks"))
    );
    let by_flag = run("/w", &[], &[], &[("hooks", bad)]).await.unwrap_err();
    assert_eq!(
        refused(&by_flag),
        Some((&Source::Flag("hooks".into()), "hooks"))
    );
    assert!(
        by_flag
            .to_string()
            .contains("[[hooks.\"turn.end\"]] #1: unknown key `comand`")
    );
}

#[tokio::test]
async fn an_untrusted_projects_hooks_are_not_read() {
    let project = (
        PROJECT_FILE,
        "[[hooks.\"tool.before\"]]\ncommand = \"sh evil.sh\"",
    );
    let r = run("/w/app", &[project], &[], &[]).await.unwrap();
    assert!(r.config.hooks.is_empty() && r.hooks.is_empty());
    assert!(matches!(r.resolved.project_file, ProjectFile::Untrusted(_)));
    assert_eq!(r.notes.len(), 1, "{:?}", r.notes);
}

/// A POST sends the conversation's events off the machine: a project's file
/// (and a profile it defines) may bind a command, never a URL.
#[tokio::test]
async fn a_url_hook_comes_from_your_own_config_only() {
    let url = "[[hooks.\"tool.before\"]]\ncommand = \"sh ok.sh\"\n[[hooks.\"turn.end\"]]\nurl = \"https://repo.example/hook\"";
    let in_profile = "[[profiles.p.hooks.\"turn.end\"]]\nurl = \"https://repo.example/hook\"";
    for (body, key) in [(url, "hooks"), (in_profile, "profiles.p.hooks")] {
        let files = [(USER_FILE, TRUST), (PROJECT_FILE, body)];
        let err = run("/w/app", &files, &[], &[]).await.unwrap_err();
        assert_eq!(refused(&err), Some((&file(PROJECT_FILE), key)), "{err}");
        let shown = err.to_string();
        assert!(
            shown.contains(
                "[[hooks.\"turn.end\"]] #1: a `url` hook can't be set in a project's file"
            ),
            "{shown}"
        );
        assert!(!shown.contains("repo.example"), "{shown}");
    }
    // A command from the project, a URL from your own file: both bind.
    let user = format!("{TRUST}[[hooks.\"turn.end\"]]\nurl = \"https://mine.example/hook\"");
    let project = "[[hooks.\"turn.end\"]]\ncommand = \"sh ok.sh\"";
    let r = run(
        "/w/app",
        &[(USER_FILE, &user), (PROJECT_FILE, project)],
        &[],
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        targets(&r.config.hooks),
        [
            (ModEvent::TurnEnd, "https://mine.example/hook"),
            (ModEvent::TurnEnd, "sh ok.sh")
        ]
    );
}

#[tokio::test]
async fn a_headers_value_is_never_shown_and_an_unset_variable_is_said() {
    let file = r#"[[hooks."turn.end"]]
url = "https://review.example/turn"
headers = { authorization = "Bearer l1teral-t0ken", x-team = "${TEAM}" }
"#;
    let r = run("/w", &[(USER_FILE, file)], &[], &[]).await.unwrap();
    let shown = r.config.get("hooks").expect("set");
    assert!(
        shown.contains("authorization") && !shown.contains("l1teral-t0ken"),
        "{shown}"
    );
    let debug = format!("{r:?}");
    assert!(!debug.contains("l1teral-t0ken"), "{debug}");
    assert!(debug.contains("review.example"), "{debug}");
    assert_eq!(
        r.notes,
        [
            "[[hooks.\"turn.end\"]] #1: header x-team names ${TEAM}, which is not set and has no default: sent as written"
        ]
    );
}

#[tokio::test]
async fn nothing_set_binds_nothing() {
    let r = run("/w", &[], &[], &[]).await.unwrap();
    assert!(r.config.hooks.is_empty() && r.hooks.is_empty());
    assert_eq!(r.config.get("hooks"), None);
    assert_eq!(r.resolved.source("hooks"), Source::Default);
}
