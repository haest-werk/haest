//! What a sandbox `js` script sees ([`super`]): Node-shaped `fs`, `fs/promises`,
//! `path` and `process`, importable or `require`-able, over the SANDBOX's file
//! system (the workspace [`Fs`] -- in the sandbox, the copy-on-write overlay, so
//! writes stay in memory until they are applied).
//!
//! The file system is async, and the engine can only answer a native call later, so
//! only the promise API is real. Every `*Sync` name exists and throws, naming the
//! async call to use instead: a script that reaches for `readFileSync` learns the
//! fix from the error, not from a missing-function crash.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};

use futures::FutureExt;
use haest_core::fs::{Fs, ensure_dir, resolve_path};
use serde_json::{Value, json};

use crate::vm::{NativeFn, VmSpec};

const PROMISE_API: [&str; 11] = [
    "readFile",
    "writeFile",
    "appendFile",
    "readdir",
    "mkdir",
    "stat",
    "rm",
    "unlink",
    "rename",
    "copyFile",
    "access",
];
const PATH_API: [&str; 8] = [
    "join",
    "resolve",
    "dirname",
    "basename",
    "extname",
    "relative",
    "normalize",
    "isAbsolute",
];
/// The modules a `js` script can import or `require`, by bare name (with a
/// `node:` prefix too).
pub(crate) const JS_MODULES: [&str; 3] = ["fs", "fs/promises", "path"];

fn sync_api() -> Vec<String> {
    let mut names: Vec<String> = PROMISE_API
        .iter()
        .filter(|n| **n != "access")
        .map(|n| format!("{n}Sync"))
        .collect();
    names.push("existsSync".into());
    names
}

fn exports(module: &str) -> Vec<String> {
    match module {
        "fs" => std::iter::once("promises".to_string())
            .chain(sync_api())
            .collect(),
        "fs/promises" => PROMISE_API.iter().map(|s| s.to_string()).collect(),
        _ => PATH_API
            .iter()
            .chain(&["sep", "delimiter", "posix"])
            .map(|s| s.to_string())
            .collect(),
    }
}

const PRELUDE: &str = r#"{
  const call = async (op, ...args) => JSON.parse(await __fs(op, JSON.stringify(args)));
  const asyncName = n => n === 'existsSync' ? 'access' : n.replace(/Sync$/, '');
  const noSync = n => () => { throw new Error('fs.' + n + ' is not available in the sandbox: its file system is async. Use await fs.promises.' + asyncName(n) + '(...) or import { ' + asyncName(n) + " } from 'fs/promises'."); };
  const stat = async p => { const s = await call('stat', p); return { ...s, mtime: new Date(s.mtime), isFile: () => s.isFile, isDirectory: () => s.isDirectory }; };
  const access = async p => { if (!(await call('exists', p))) throw Object.assign(new Error("ENOENT: no such file or directory, access '" + p + "'"), { code: 'ENOENT' }); };
  const stdinText = __stdin === null ? '' : __stdin;
  const promises = {
    readFile: p => p === '/dev/stdin' ? Promise.resolve(stdinText) : call('readFile', p), writeFile: (p, d) => call('writeFile', p, String(d)), appendFile: (p, d) => call('appendFile', p, String(d)),
    readdir: p => call('readdir', p), mkdir: (p, o) => call('mkdir', p, !!(o && o.recursive)), stat,
    rm: (p, o) => call('rm', p, !!(o && o.recursive), !!(o && o.force)), unlink: p => call('rm', p, false, false),
    rename: (a, b) => call('mv', a, b), copyFile: (a, b) => call('cp', a, b), access,
  };
  const fs = { promises, ...Object.fromEntries(__SYNC_API__.map(n => [n, noSync(n)])) };
  const path = Object.fromEntries(__PATH_API__.map(op => [op, (...a) => JSON.parse(__path(op, JSON.stringify(op === 'resolve' ? [__cwd, ...a] : a)))]));
  Object.assign(path, { sep: '/', delimiter: ':' });
  path.posix = path;
  globalThis.__modules = { fs, 'fs/promises': promises, path };
  globalThis.require = name => {
    const m = __modules[String(name).replace(/^node:/, '')];
    if (!m) throw new Error('module "' + name + '" is not available in the sandbox (js has: __JS_MODULES__)');
    return m;
  };
  const write = level => ({ write: s => { __log(level, String(s).replace(/\n$/, '')); return true; } });
  // What was piped in, all there at once: one chunk to iterate, or one 'data' then 'end'.
  const listeners = { data: [], end: [] };
  let emitted = false;
  const stdin = {
    isTTY: false,
    setEncoding: () => stdin,
    on: (event, f) => {
      if (listeners[event]) listeners[event].push(f);
      if (!emitted) {
        emitted = true;
        Promise.resolve().then(() => {
          if (stdinText) listeners.data.forEach(g => g(stdinText));
          listeners.end.forEach(g => g());
        });
      }
      return stdin;
    },
    [Symbol.asyncIterator]: async function* () { if (stdinText) yield stdinText; },
  };
  globalThis.process = {
    argv: __argv, env: __env, cwd: () => __cwd,
    exit: code => { __exit(String(code ?? 0)); throw new Error('process.exit(' + (code ?? 0) + ')'); },
    stdin, stdout: write('log'), stderr: write('error'),
  };
}"#;

fn prelude() -> String {
    PRELUDE
        .replace("__SYNC_API__", &json!(sync_api()).to_string())
        .replace("__PATH_API__", &json!(PATH_API).to_string())
        .replace("__JS_MODULES__", &JS_MODULES.join(", "))
}

/// Every importable name (bare and `node:`) -> its source: the prelude's object, default and named exports.
fn module_sources() -> HashMap<String, String> {
    let mut out = HashMap::new();
    for key in JS_MODULES {
        let named: Vec<String> = exports(key)
            .iter()
            .map(|n| format!("{n} = m.{n}"))
            .collect();
        let source = format!(
            "const m = globalThis.__modules[{}];\nexport default m;\nexport const {};",
            json!(key),
            named.join(", ")
        );
        out.insert(format!("node:{key}"), source.clone());
        out.insert(key.to_string(), source);
    }
    out
}

/// One script's world: its files, directory, argv and environment.
pub(crate) struct JsScript {
    pub fs: Arc<dyn Fs>,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// What was piped in; None = nothing.
    pub stdin: Option<String>,
    /// What the script passed to process.exit(), once it did.
    pub exit_code: Arc<Mutex<Option<i32>>>,
}

/// `String(v)` for a JSON value.
fn s(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "undefined".into(),
    }
}

fn flag(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Bool(true)))
}

/// One fs operation the guest asked for; the answer as JSON.
async fn fs_op(fs: &dyn Fs, cwd: &Path, op: &str, args: &[Value]) -> Result<Value, String> {
    let abs = |i: usize| resolve_path(cwd, &s(args.get(i)));
    let io = |e: std::io::Error| e.to_string();
    Ok(match op {
        "readFile" => json!(fs.read_to_string(&abs(0)).await.map_err(io)?),
        "writeFile" => {
            fs.write(&abs(0), s(args.get(1)).as_bytes())
                .await
                .map_err(io)?;
            Value::Null
        }
        "appendFile" => {
            fs.append(&abs(0), s(args.get(1)).as_bytes())
                .await
                .map_err(io)?;
            Value::Null
        }
        "readdir" => {
            let names: Vec<String> = fs
                .read_dir(&abs(0))
                .await
                .map_err(io)?
                .into_iter()
                .map(|e| e.name)
                .collect();
            json!(names)
        }
        "mkdir" if flag(args.get(1)) => {
            ensure_dir(fs, &abs(0)).await.map_err(io)?;
            Value::Null
        }
        "mkdir" => {
            fs.mkdir(&abs(0), false).await.map_err(io)?;
            Value::Null
        }
        "stat" => {
            let st = fs.stat(&abs(0)).await.map_err(io)?;
            let mtime = st
                .modified
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64);
            json!({"isFile": st.is_file(), "isDirectory": st.is_dir(), "size": st.size, "mtime": mtime})
        }
        "rm" => {
            let p = abs(0);
            if !(flag(args.get(2)) && !fs.exists(&p).await) {
                fs.remove(&p, flag(args.get(1))).await.map_err(io)?;
            }
            Value::Null
        }
        "mv" => {
            fs.rename(&abs(0), &abs(1)).await.map_err(io)?;
            Value::Null
        }
        "cp" => {
            let data = fs.read(&abs(0)).await.map_err(io)?;
            fs.write(&abs(1), &data).await.map_err(io)?;
            Value::Null
        }
        "exists" => json!(fs.exists(&abs(0)).await),
        other => return Err(format!("unknown fs operation {other}")),
    })
}

/// The engine spec for one script: the prelude above, wired to this script's fs, cwd and argv.
pub(crate) fn js_script_spec(script: JsScript, filename: &str) -> VmSpec {
    let mut values = serde_json::Map::new();
    values.insert("__argv".into(), json!(script.argv));
    values.insert("__env".into(), json!(script.env));
    values.insert("__stdin".into(), json!(script.stdin));
    values.insert("__cwd".into(), json!(script.cwd.display().to_string()));
    let (fs, cwd) = (script.fs.clone(), script.cwd.clone());
    let fs_fn: NativeFn = Arc::new(move |args| {
        let (fs, cwd) = (fs.clone(), cwd.clone());
        async move {
            let op = args.first().cloned().unwrap_or_default();
            let parsed: Vec<Value> =
                serde_json::from_str(args.get(1).map_or("[]", String::as_str))?;
            Ok(fs_op(fs.as_ref(), &cwd, &op, &parsed).await?.to_string())
        }
        .boxed()
    });
    // The guest throws right after calling it; the code lands before the run's result does.
    let exit = script.exit_code.clone();
    let exit_fn: NativeFn = Arc::new(move |args| {
        let code = args
            .first()
            .and_then(|c| c.trim().parse::<f64>().ok())
            .filter(|c| c.is_finite());
        *exit.lock().unwrap() = Some(code.map_or(0, |c| c as i32));
        async { Ok(String::new()) }.boxed()
    });
    VmSpec {
        prelude: prelude(),
        values,
        native_fns: [("__fs".to_string(), fs_fn), ("__exit".to_string(), exit_fn)].into(),
        modules: Some(module_sources()),
        filename: Some(filename.to_string()),
    }
}
