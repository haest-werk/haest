//! Ported from sandbox-js.test.ts: `js <file>` over a workspace [`Fs`] (here a
//! [`MemFs`], standing in for the sandbox's overlay), the way a sandbox shell
//! builtin calls it.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use haest_core::{fs::MemFs, workspace::ExecResult};
use tokio_util::sync::CancellationToken;

use super::{JsEnv, NOT_HERE, js_command, not_here};

fn env(fs: &Arc<MemFs>) -> JsEnv {
    JsEnv {
        fs: fs.clone(),
        cwd: "/p".into(),
        env: Default::default(),
        stdin: None,
        cancel: CancellationToken::new(),
        ends_at: Some(Instant::now() + Duration::from_secs(20)),
    }
}

/// A script INTO the workspace, the way the model would write it with Write.
fn with_script(name: &str, body: &str) -> Arc<MemFs> {
    Arc::new(MemFs::with_files([(format!("/p/{name}"), body)]))
}

async fn js(fs: &Arc<MemFs>, line: &str) -> ExecResult {
    let args: Vec<String> = line.split_whitespace().map(String::from).collect();
    js_command(&args, &env(fs)).await
}

#[cfg(feature = "typescript")]
#[tokio::test]
async fn a_typescript_module_reads_project_files_and_writes_through_the_workspace() {
    use std::path::Path;

    use haest_core::fs::Fs;

    let fs = with_script(
        "temp/sum.ts",
        "import { readFile, writeFile } from 'fs/promises'
import path from 'node:path'
const nums: number[] = (await readFile('a.txt', 'utf8')).trim().split('\\n').map(Number)
const total = nums.reduce((a, b) => a + b, 0)
await writeFile(path.join('temp', 'out.txt'), String(total))
console.log(total)",
    );
    fs.write(Path::new("/p/a.txt"), b"1\n2\n3\n").await.unwrap();
    let r = js(&fs, "temp/sum.ts").await;
    assert_eq!(r.exit_code, 0, "{}", r.stderr);
    assert_eq!(r.stdout.trim(), "6");
    let out = fs
        .read_to_string(Path::new("/p/temp/out.txt"))
        .await
        .unwrap();
    assert_eq!(out, "6");
}

#[tokio::test]
async fn require_works_for_the_same_modules_and_argv_and_exit_codes_behave_like_a_cli() {
    let fs = with_script(
        "args.js",
        "const path = require('path'); console.log(process.argv.slice(2).join(','), path.basename('/x/y.js')); process.exit(3)",
    );
    let r = js(&fs, "args.js one two").await;
    assert_eq!(r.stdout.trim(), "one,two y.js");
    assert_eq!(r.exit_code, 3);
}

#[tokio::test]
async fn a_sync_fs_call_fails_with_the_async_call_to_use_instead() {
    let fs = with_script("sync.js", "require('fs').readFileSync('a.txt', 'utf8')");
    let r = js(&fs, "sync.js").await;
    assert_eq!(r.exit_code, 1);
    assert!(r.stderr.contains("fs.promises.readFile"), "{}", r.stderr);
}

#[tokio::test]
async fn an_uncaught_error_exits_1_and_names_the_line() {
    let fs = with_script("boom.js", "const a = 1\nnull.boom()");
    let r = js(&fs, "boom.js").await;
    assert_eq!(r.exit_code, 1);
    assert!(r.stderr.trim_end().ends_with("(line 2)"), "{}", r.stderr);
}

#[cfg(feature = "typescript")]
#[tokio::test]
async fn a_typescript_error_names_the_line_in_the_ts_file() {
    let fs = with_script(
        "boom.ts",
        "const a: number = 1\nconst b: string = 'x'\nnull.boom()",
    );
    let r = js(&fs, "boom.ts").await;
    assert_eq!(r.exit_code, 1);
    assert!(r.stderr.trim_end().ends_with("(line 3)"), "{}", r.stderr);
}

#[tokio::test]
async fn a_module_that_does_not_exist_here_fails_the_run() {
    let fs = with_script("pkg.js", "import _ from 'lodash'");
    assert_eq!(js(&fs, "pkg.js").await.exit_code, 1);
}

#[tokio::test]
async fn a_missing_file_or_no_file_is_a_cli_error() {
    let fs = with_script("x.js", "");
    assert_eq!(js(&fs, "nope.js").await.exit_code, 1);
    assert_eq!(js(&fs, "").await.exit_code, 2);
    let help = js(&fs, "--help").await;
    assert_eq!(help.exit_code, 0);
    assert!(!help.stdout.is_empty());
}

#[tokio::test]
async fn a_runaway_loop_is_stopped_by_the_shell_call_deadline() {
    let fs = with_script("loop.js", "while (true) {}");
    let mut e = env(&fs);
    e.ends_at = Some(Instant::now() + Duration::from_millis(1500));
    let started = Instant::now();
    let r = js_command(&["loop.js".to_string()], &e).await;
    assert_ne!(r.exit_code, 0);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn other_runtime_names_point_at_js_instead_of_pretending() {
    for name in ["bun", "node", "npx", "tsx"] {
        assert!(NOT_HERE.contains(&name));
        let r = not_here(name);
        assert_eq!(r.exit_code, 127);
        assert!(r.stdout.is_empty() && !r.stderr.is_empty());
    }
}

/// The engine runs no JSX, and the type stripper leaves JSX as it is: a JSX
/// file is refused before anything in it runs.
#[tokio::test]
async fn a_jsx_file_is_refused_before_it_runs() {
    for name in ["app.tsx", "app.mtsx", "app.ctsx", "app.jsx"] {
        let fs = with_script(name, "console.log('ran')");
        let r = js(&fs, name).await;
        assert_eq!((r.stdout.as_str(), r.exit_code), ("", 1), "{name}");
        assert!(r.stderr.contains(name), "{}", r.stderr);
    }
}
