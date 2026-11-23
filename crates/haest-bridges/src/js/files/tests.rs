//! Finding and loading a conversation's mod files: which directories, which
//! files, in what order, and a file that fails.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use haest_core::{Tools, fs::MemFs, workspace::WorkspaceMode};
use haest_mods::{ModEnv, ModOptions, load_mods};
use serde_json::json;

use super::*;
use crate::testkit::*;

const HOME: &str = "/home/cfg";
const HOME_MODS: &str = "/home/cfg/mods";

fn opts() -> ModFileOptions {
    ModFileOptions::new(HOME)
        .with_user_mods(true)
        .with_caps(test_caps())
}

fn sandbox(ui: &Arc<TestUi>) -> ModEnv {
    env(ui.clone(), WorkspaceMode::Sandbox)
}

#[test]
fn no_mod_dir_is_read_unless_asked_for() {
    let cwd = Path::new(CWD);
    assert!(ModFileOptions::new(HOME).mod_dirs(cwd).is_empty());
    assert_eq!(opts().mod_dirs(cwd), [PathBuf::from(HOME_MODS)]);
    let both = opts().with_project_mods(true).mod_dirs(cwd);
    assert_eq!(both, [HOME_MODS, "/p/.haest/mods"].map(PathBuf::from));
}

#[tokio::test]
async fn a_mod_in_the_user_mods_dir_loads_and_a_broken_one_is_reported_and_skipped() {
    let fs = MemFs::from_files([
        (
            format!("{HOME_MODS}/a-good.ts"),
            "export default function (haest) { haest.tool({ name: 'Hello', kind: 'agent.unknown', description: 'hi', parameters: {}, execute: async () => ({ content: 'hello' }) }) }",
        ),
        (format!("{HOME_MODS}/b-broken.ts"), "export default 42"),
    ]);
    let ui = TestUi::new();
    let files = ModFiles::load(opts(), fs, sandbox(&ui)).await;
    let tools = wrapped(Mods::new(sandbox(&ui), [files]), Tools::new());
    let hello = find(&tools, "Hello");
    let out = hello.execute(input(json!({})), &ctx()).await.unwrap();
    assert_eq!(out.content, "hello");
    let errors = ui.errors();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("b-broken.ts"));
}

#[tokio::test]
async fn the_hosts_limits_bound_every_mod_file_it_loads() {
    let fs = MemFs::from_files([(
        format!("{HOME_MODS}/hangs.js"),
        "export default () => new Promise(() => {})",
    )]);
    let ui = TestUi::new();
    let caps = ModCaps {
        load_timeout: Duration::from_millis(200),
        ..test_caps()
    };
    let started = Instant::now();
    let files = ModFiles::load(opts().with_caps(caps), fs, sandbox(&ui)).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(files.names().is_empty());
    assert!(ui.errors()[0].contains("hangs.js"), "{:?}", ui.errors());
}

#[tokio::test]
async fn mod_files_are_code_files_in_name_order_and_declarations_are_not_mods() {
    let fs = MemFs::from_files([
        ("/m/b.js", "x"),
        ("/m/a.ts", "x"),
        ("/m/c.mjs", "x"),
        ("/m/types.d.ts", "x"),
        ("/m/notes.md", "x"),
    ]);
    let names: Vec<PathBuf> = mod_files(&fs, Path::new("/m")).await;
    assert_eq!(names, ["/m/a.ts", "/m/b.js", "/m/c.mjs"].map(PathBuf::from));
    assert!(mod_files(&fs, Path::new("/missing")).await.is_empty());
}

#[tokio::test]
async fn project_mods_load_only_when_the_host_turns_them_on() {
    let source =
        "export default function (haest) { haest.on('system.prompt', () => 'from the repo') }";
    let fs = MemFs::from_files([(format!("{CWD}/.haest/mods/repo.js"), source)]);
    let ui = TestUi::new();
    let off = ModFiles::load(opts(), fs.clone(), sandbox(&ui)).await;
    assert_eq!(Mods::new(sandbox(&ui), [off]).system_append().await, "");
    let on = ModFiles::load(opts().with_project_mods(true), fs, sandbox(&ui)).await;
    let on = Mods::new(sandbox(&ui), [on]);
    assert_eq!(on.system_append().await, "from the repo");
}

/// A host's own mod, known by its name, that adds it to the human's text.
struct Hosts(&'static str);

#[async_trait::async_trait]
impl Mod for Hosts {
    fn name(&self) -> &str {
        self.0
    }
    async fn prompt_submit(&self, text: &str, _: &ModEnv) -> Result<Option<String>, BoxError> {
        Ok(Some(format!("{text} {}", self.0)))
    }
}

#[tokio::test]
async fn the_files_run_after_the_built_ins_and_before_the_hosts_other_mods() {
    let files = [
        (
            format!("{HOME_MODS}/a.js"),
            "export default (haest) => haest.on('prompt.submit', t => t + ' a')",
        ),
        (
            format!("{HOME_MODS}/b.js"),
            "export default (haest) => haest.on('prompt.submit', t => t + ' b')",
        ),
    ];
    let ui = TestUi::new();
    let files = ModFiles::load(opts(), MemFs::from_files(files), sandbox(&ui)).await;
    let cfg = ModOptions::new(CWD, WorkspaceMode::Sandbox)
        .with_mod(files)
        .with_mod(Hosts("host"));
    let mods = load_mods(cfg, ui);
    assert_eq!(mods.names(), ["approval", "plan", "mod files", "host"]);
    assert_eq!(mods.rewrite_prompt("hi").await, "hi a b host");
}
