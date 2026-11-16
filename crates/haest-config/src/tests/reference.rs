//! `docs/settings.md`, written from the registry: every setting in its three
//! spellings with its example, what it takes, its default and where it may be
//! set; then the built-in profiles. The doc on disk must be what this writes,
//! and each example must read the same in all three spellings.
//! `HAEST_BLESS=1 cargo test -p haest-config reference` rewrites the doc.

use std::fmt::Write;

use toml::Value;

use super::*;
use crate::{
    fields::whole_up_to,
    profiles::BUILTIN,
    registry::{Def, Scope},
    value::var_parts,
};

const DOC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/settings.md");

const INTRO: &str = "# haest settings

<!-- Written from the settings registry (crates/haest-config/src/registry.rs).
     Edit the registry, then rewrite this file:
     HAEST_BLESS=1 cargo test -p haest-config reference -->

haest starts bare: no tools, no instruction files, no skills, no MCP, no mods,
in the sandbox, with every brake on. Each setting below turns one thing on or
sets one value: a model, a cap, a path.

A setting has one name, written three ways: a key in a TOML config file, a
`HAEST_*` variable, a flag. Each layer wins over the ones before it:

1. the defaults below
2. your `~/.config/haest/config.toml` (`$XDG_CONFIG_HOME/haest`, or `config_dir`)
3. the nearest `.haest/config.toml` from the working dir up, inside `trusted_dirs` only
4. the profile `profile` names
5. `HAEST_*` variables
6. flags

`haest settings` shows what a run gets and where each value came from; it takes
the same flags. [environment.md](environment.md) covers what haest reads from
its environment and what a process it starts gets. [examples/](examples/) has
whole config files.
";

/// What a value of this kind looks like.
fn takes(def: &Def) -> String {
    let words = |w: &[&str]| {
        w.iter()
            .map(|w| format!("`{w}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match def.kind {
        Kind::Bool => {
            "`true` or `false` (also `1`/`0`, `on`/`off`, `yes`/`no`); a flag alone is `true`"
                .into()
        }
        Kind::Number if whole_up_to(def.key).is_some() => "a whole number, 0 or more".into(),
        Kind::Number => "a number, 0 or more".into(),
        Kind::Text => "text".into(),
        Kind::Path => "a directory: `~/x` is under your home dir, `x` under the working dir".into(),
        Kind::Choice(w) => format!("one of {}", words(w)),
        Kind::List(Some(w)) => format!("a list of {}; `all` (every one) or `none`", words(w)),
        Kind::List(None) => "a list: comma-separated, or a TOML array; `none` is empty".into(),
        Kind::Vars => {
            "`NAME=VALUE`, or `NAME` for haest's own value; repeat the flag for more".into()
        }
    }
}

fn set_in(def: &Def) -> &'static str {
    match (def.scope, def.profile_may_set()) {
        (Scope::Any, true) => {
            "a config file (yours, or a trusted project's), a profile, its variable, its flag"
        }
        (Scope::Any, false) => {
            "a config file (yours, or a trusted project's), its variable, its flag; not inside a profile"
        }
        (Scope::User, _) => {
            "your own config file, its variable, its flag; never a project's file or a profile"
        }
        (Scope::Process, _) => "its variable or its flag only: it says where the config file is",
    }
}

fn default_of(def: &Def) -> String {
    match Config::bare("<cwd>", "~").get(def.key).as_deref() {
        None if def.kind == Kind::Vars => "none".into(),
        None => match tool_default(def.key) {
            Some(own) => format!("unset: the tool's own, {own}"),
            None => "unset".into(),
        },
        Some("") => "none".into(),
        Some(v) => format!("`{v}`"),
    }
}

/// What a key left unset means when the tool, not the config, holds the
/// default: the reader sees the real value, read from the tool itself.
fn tool_default(key: &str) -> Option<String> {
    use haest_tools::{DEFAULT_IMAGE_MODEL, DEFAULT_IMAGE_MODELS};
    match key {
        "image_model" => Some(format!("`{DEFAULT_IMAGE_MODEL}`")),
        "image_models" => Some(DEFAULT_IMAGE_MODELS.map(|m| format!("`{m}`")).join(", ")),
        _ => None,
    }
}

/// The help as a sentence of markdown (a `<name>` must not read as a tag),
/// then the reference's own detail in the same paragraph.
fn help(def: &Def) -> String {
    let mut text = def.help.replace('<', "&lt;");
    if !text.ends_with('.') {
        text.push('.');
    }
    if let Some(detail) = def.detail {
        text = format!("{text}\n{}", detail.replace('<', "&lt;"));
    }
    text
}

/// The example as a config file says it; None when no file may set it.
fn toml_line(def: &Def) -> Option<String> {
    let raw = def.example;
    let string = |s: &str| Value::String(s.into()).to_string();
    Some(match def.kind {
        _ if def.scope == Scope::Process => return None,
        Kind::Bool | Kind::Number => format!("{} = {raw}", def.key),
        Kind::List(_) => {
            let items = raw.split(',').map(|s| Value::String(s.into())).collect();
            format!("{} = {}", def.key, Value::Array(items))
        }
        Kind::Vars => {
            let (name, value) = var_parts(raw);
            format!("[{}]\n{name} = {}", def.key, string(value))
        }
        Kind::Text | Kind::Path | Kind::Choice(_) => format!("{} = {}", def.key, string(raw)),
    })
}

/// `raw` as a shell word: bare when it is safe, else single-quoted.
fn sh(raw: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_-./:,=@%+".contains(c);
    if !raw.is_empty() && raw.chars().all(safe) {
        return raw.into();
    }
    format!("'{}'", raw.replace('\'', r"'\''"))
}

fn flag_line(def: &Def) -> String {
    let long = format!("--{}", def.flag());
    match (def.kind, def.short) {
        // A switch alone is on; off needs the `=`.
        (Kind::Bool, _) if def.example == "true" => long,
        (Kind::Bool, _) => format!("{long}={}", def.example),
        (_, Some(c)) => format!("{long} {0}   or   -{c} {0}", sh(def.example)),
        (_, None) => format!("{long} {}", sh(def.example)),
    }
}

fn setting(out: &mut String, def: &Def) {
    let file = toml_line(def)
        .map(|l| l.replace('\n', "\n              "))
        .unwrap_or_else(|| "(not in a file)".into());
    let _ = write!(
        out,
        "\n### `{}`\n\n{}\n\n- Takes: {}\n- Default: {}\n- Set in: {}\n\n```text\nconfig.toml   {file}\nvariable      {}={}\nflag          {}\n```\n",
        def.key,
        help(def),
        takes(def),
        default_of(def),
        set_in(def),
        def.env(),
        sh(def.example),
        flag_line(def),
    );
}

fn profiles(out: &mut String) {
    out.push_str("\n## Profiles\n\n`profile` picks a named set of settings. It sits over the config files and\nunder the variables and flags. A `[profiles.<name>]` table in a config file\nadds to a built-in profile (its keys win) or makes a new one:\n\n```toml\nprofile = \"review\"\n\n[profiles.review]\ntools = [\"read\", \"grep\", \"glob\"]\n```\n");
    for (name, values) in BUILTIN {
        let _ = write!(
            out,
            "\n### `{name}` (built in)\n\n| Setting | Value |\n|---|---|\n"
        );
        for (key, value) in *values {
            let _ = writeln!(out, "| [`{key}`](#{key}) | `{value}` |");
        }
    }
}

/// The whole doc.
fn reference() -> String {
    let mut groups: Vec<&str> = DEFS.iter().map(|d| d.group).collect();
    groups.dedup();
    let mut out = INTRO.to_string();
    out.push_str("\n| Group | Settings |\n|---|---|\n");
    for group in &groups {
        let keys: Vec<String> = DEFS
            .iter()
            .filter(|d| d.group == *group)
            .map(|d| format!("[`{0}`](#{0})", d.key))
            .collect();
        let _ = writeln!(out, "| {group} | {} |", keys.join(", "));
    }
    out.push_str("| [Profiles](#profiles) | the built-in ones, and your own |\n");
    for group in &groups {
        let _ = writeln!(out, "\n## {group}");
        DEFS.iter()
            .filter(|d| d.group == *group)
            .for_each(|d| setting(&mut out, d));
    }
    profiles(&mut out);
    out
}

#[test]
fn the_settings_reference_is_what_the_registry_writes() {
    let doc = reference();
    if std::env::var_os("HAEST_BLESS").is_some() {
        std::fs::write(DOC, &doc).unwrap();
    }
    let on_disk = std::fs::read_to_string(DOC).unwrap_or_default();
    assert!(
        on_disk == doc,
        "docs/settings.md is not what the registry writes: HAEST_BLESS=1 cargo test -p haest-config reference"
    );
}

/// What the reference shows for a setting is one value, however it is spelled.
#[tokio::test]
async fn each_example_reads_the_same_from_a_file_a_variable_and_a_flag() {
    for def in DEFS {
        let flag = run("/w", &[], &[], &[(def.key, def.example)]).await;
        let flag = flag.unwrap_or_else(|e| panic!("{}: {e}", def.key)).config;
        let var = run("/w", &[], &[(&def.env(), def.example)], &[]).await;
        assert_eq!(var.unwrap().config, flag, "{}", def.key);
        if let Some(line) = toml_line(def) {
            let file = run("/w", &[(USER_FILE, &line)], &[], &[]).await;
            let file = file.unwrap_or_else(|e| panic!("{}: {line}: {e}", def.key));
            assert_eq!(file.config, flag, "{}: {line}", def.key);
        }
    }
}

/// A heading is words a person reads, never a Rust type's name: a type
/// rename once turned the "Caps" heading into "CapSettings".
#[test]
fn every_heading_reads_as_words_not_a_type_name() {
    for def in crate::registry::DEFS {
        let camel = def
            .group
            .as_bytes()
            .windows(2)
            .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
        assert!(!camel, "`{}` is listed under `{}`", def.key, def.group);
    }
}
