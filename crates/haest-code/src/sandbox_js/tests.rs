//! Ported from sandbox-js.test.ts: `js <file>` over a workspace [`Fs`] (here a
//! [`MemFs`], standing in for the sandbox's overlay), the way a sandbox shell
//! builtin calls it.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use haest_core::{ChildEnv, fs::MemFs, workspace::ExecResult};

use super::{JsEnv, js_command};
use crate::VmCaps;

/// The env's caps hold the script: its heap (8 MB of text fits the default,
/// not 4 MiB), and its output, cut at `max_log_chars` with a line saying so.
#[tokio::test]
async fn the_envs_caps_hold_the_scripts_memory_and_output() {
    let fs = with_script("big.js", "console.log('x'.repeat(8 << 20).length)");
    let roomy = js_command(&["big.js"], &env(&fs)).await;
    assert_eq!((roomy.stdout.trim(), roomy.exit_code), ("8388608", 0));
    let small = VmCaps::sandbox().with_memory_bytes(4 << 20);
    let run = js_command(&["big.js"], &env(&fs).with_caps(small)).await;
    assert_eq!(run.exit_code, 1, "{}", run.stderr);
    assert!(run.stderr.contains("Uncaught"), "{}", run.stderr);

    let fs = with_script("loud.js", "console.log('x'.repeat(50))");
    let short = VmCaps::sandbox().with_max_log_chars(10);
    let run = js_command(&["loud.js"], &env(&fs).with_caps(short)).await;
    assert!(run.stdout.len() <= 11, "{:?}", run.stdout);
    assert!(
        run.stderr.contains("output truncated at 10 characters"),
        "{}",
        run.stderr
    );
}

fn env(fs: &Arc<MemFs>) -> JsEnv {
    JsEnv::new(fs.clone(), "/p").with_ends_at(Instant::now() + Duration::from_secs(20))
}

/// A script INTO the workspace, the way the model would write it with Write.
fn with_script(name: &str, body: &str) -> Arc<MemFs> {
    Arc::new(MemFs::from_files([(format!("/p/{name}"), body)]))
}

async fn js(fs: &Arc<MemFs>, line: &str) -> ExecResult {
    let args: Vec<String> = line.split_whitespace().map(String::from).collect();
    js_command(&args, &env(fs)).await
}

#[tokio::test]
async fn process_env_is_the_envs_variables_the_last_of_a_name_winning() {
    let fs = with_script("env.js", "console.log(process.env.WHO, process.env.LANG)");
    let vars = ChildEnv::new([("WHO", "first"), ("LANG", "sv"), ("WHO", "last")]);
    let e = JsEnv::new(fs.clone(), "/p").with_env(vars);
    let r = js_command(&["env.js"], &e).await;
    assert_eq!(r.stdout.trim(), "last sv", "{}", r.stderr);
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
async fn a_missing_file_or_no_file_is_a_cli_error() {
    let fs = with_script("x.js", "");
    assert_eq!(js(&fs, "nope.js").await.exit_code, 1);
    assert_eq!(js(&fs, "").await.exit_code, 2);
    let help = js(&fs, "--help").await;
    assert_eq!(help.exit_code, 0);
    assert!(!help.stdout.is_empty());
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
