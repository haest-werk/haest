# haest-bridges

Mods and hooks written outside Rust for
[haest](https://github.com/haest-werk/haest), an agent loop and tool harness
you embed in a Rust program.

A bridge is a `Hook<E>` or a `Mod` from
[haest-mods](https://crates.io/crates/haest-mods) that does three things for
each event: encodes the event's payload, carries it to code that is not the
host's Rust (a script, a process, a server), and decodes that code's answer as
the event's decision. The payloads and decisions are haest-mods' types, and
their JSON (`{ deny }`, `{ input }`, `{ stop }`, `{ steer }`, `{ continue }`,
`{ compact }`) is what every bridge speaks. What a decision does is
haest-mods' too, so a handler means the same whichever bridge carries it.

Each bridge is a feature of this crate, with the dependencies only it needs.
None is on by default; without one the crate is empty, and a host that writes
its mods in Rust never links an engine.

- `js`: `.js` / `.ts` mod files in embedded QuickJS (`.ts` is type-stripped
  first, in pure Rust).
  - `JsMod` is one file: its default export receives the `haest` API (hook an
    event, add a tool or a slash command, hide any tool, ask the human). It
    runs on its own thread, bounded by `ModCaps` (memory, a time slice, a load
    timeout); `ModLoadError` says why one did not load.
  - `ModFiles` is a conversation's mod files as one mod: the human's
    (`<config dir>/mods`) and, only in a directory the human trusts, the
    project's (`.haest/mods`), loaded in name order and run by haest-mods'
    rules. A host adds it after the built-ins and before its own mods.
  - With `ModFileOptions::with_reload` (the `mod_reload` setting: `off`,
    `ask`, `auto`), `ModFiles` reads the files again before each model call:
    a new or changed file loads, a deleted one unloads, one that fails to
    load keeps the version before and is reported. `ask` puts each change
    before the human first. The model's next request has the new tools.

A shell bridge (a hook as a command, JSON on stdin and stdout) and an HTTP
bridge (a hook behind a URL) come next, as features of this crate.

```rust
use haest_bridges::{ModFileOptions, ModFiles, ModReload};
use haest_core::{MemFs, workspace::WorkspaceMode};
use haest_mods::{ApprovalRequest, Decision, ModEnv, ModOptions, ModUi, load_mods};

struct Terminal;

#[async_trait::async_trait]
impl ModUi for Terminal {
    async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision {
        Decision::Deny
    }
    fn info(&self, text: &str) {
        println!("{text}")
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let disk = MemFs::from_files([(
        "/home/me/.config/haest/mods/short.js",
        "export default (haest) => haest.on('system.prompt', () => 'Answer in one line.')",
    )]);
    let env = ModEnv::new("/project", WorkspaceMode::Real, Terminal);
    let opts = ModFileOptions::new("/home/me/.config/haest")
        .with_user_mods(true)
        .with_reload(ModReload::Ask);
    let files = ModFiles::load(opts, disk, env).await;

    let mods = load_mods(ModOptions::new("/project", WorkspaceMode::Real).with_mod(files), Terminal);
    assert_eq!(mods.system_append().await, "Answer in one line.");
}
```

Through the facade this crate is `haest::bridges` (feature `bridge-js`, on by
default). The docs on [docs.rs](https://docs.rs/haest-bridges/) have the
details; the repository's
[README](https://github.com/haest-werk/haest/blob/main/README.md) has the rest
of haest.

License: MIT.
