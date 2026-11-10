# kern and its environment

kern reads a few variables of its own environment, and passes on only what
you allow. Nothing else of your environment reaches a model, a tool or a
process kern starts.

1. [Variables kern reads](#variables-kern-reads)
2. [What a process kern starts gets](#what-a-process-kern-starts-gets)
3. [Passing a variable on purpose: `env`](#passing-a-variable-on-purpose-env)
4. [`${NAME}` in config files](#name-in-config-files)
5. [Recipes](#recipes)

Every setting, with its three spellings, is in [settings.md](settings.md).
Whole example files are in [examples/](examples/).

## Variables kern reads

### Settings: `KERN_*`

Every setting is also a variable: `KERN_` and the setting's name in upper
case (`tools` is `KERN_TOOLS`, `max_turn_usd` is `KERN_MAX_TURN_USD`). A
variable wins over the config files and loses to a flag. An empty variable
counts as unset. [settings.md](settings.md) lists them all.

```sh
KERN_TOOLS=read,grep,glob kern "what does this repo do?"
export KERN_MAX_TURN_USD=1   # every run in this shell
```

### Where kern keeps its files

| Variable | What it changes |
|---|---|
| `HOME` | your home dir: `~` in a path setting, and the default dirs below |
| `XDG_CONFIG_HOME` | `config_dir` becomes `$XDG_CONFIG_HOME/kern` (else `~/.config/kern`) |
| `XDG_DATA_HOME` | `data_dir` becomes `$XDG_DATA_HOME/kern` (else `~/.local/share/kern`) |
| `KERN_CONFIG_DIR` | `config_dir` itself: where `config.toml`, `providers.json` and your mods are |
| `KERN_DATA_DIR` | `data_dir` itself: sessions, history and spill files |

### Provider keys

kern reads a provider's key when a run uses that provider (`-m
provider:model`). The keys stay with kern: a process kern starts gets one only
under `env_passthrough = all`, or when you pass it with `env`.

| Provider | Key | Endpoint override |
|---|---|---|
| `openrouter` (the default model's) | `OPENROUTER_API_KEY` | `OPENROUTER_BASE_URL` |
| `anthropic` | `ANTHROPIC_API_KEY` | `ANTHROPIC_BASE_URL` |
| `openai` | `OPENAI_API_KEY` | `OPENAI_BASE_URL` |
| `paperinstruments` | `PAPERINSTRUMENTS_API_KEY` | `PAPERINSTRUMENTS_BASE_URL` |
| a `providers.json` entry | its `apiKey`, else the variable its `apiKeyEnv` names | its `baseURL` |

GenerateImage (`image = true`) also uses `OPENROUTER_API_KEY` and
`OPENROUTER_BASE_URL`. An empty key counts as none. See
[examples/providers.json](examples/providers.json) for a local server and a
gateway.

### Variables a config file names

`${NAME}` in an `[env]` value or in `.mcp.json` is read from kern's
environment: see [`${NAME}` in config files](#name-in-config-files).

## What a process kern starts gets

kern starts a process for real bash (`workspace = real`), each background job
of it, and each stdio MCP server. Such a process gets an environment built
from scratch, in this order, a later one winning for the same name:

1. what `env_passthrough` lets through of kern's environment
2. the variables in `env`
3. for an MCP server, its own `env` in `.mcp.json`

| `env_passthrough` | A process gets |
|---|---|
| `none` | nothing of kern's environment (bash is still found on `/usr/bin:/bin`) |
| `core` (the default) | `PATH`, `SHELL`, `TMPDIR`, `TEMP`, `TMP`, `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `LOGNAME`, `USER` |
| `all` | every variable, API keys included (the `claude` profile sets this, as Claude Code does) |

The words and the `core` list are Codex's (`shell_environment_policy.inherit`).

The sandbox (`workspace = sandbox`, the default) starts no process: its bash
is simulated. Its shell has its own `HOME` (`/home/user`) and `USER` (`user`),
nothing of your environment and nothing `env_passthrough` lets through. It
gets the variables in `env`, and only those.

## Passing a variable on purpose: `env`

`env` hands a variable to every process kern starts and to the sandbox's
shell, the way `docker run -e` does. It is a flag you repeat, a variable, and
a table in a config file:

```sh
kern -E GH_TOKEN='${GITHUB_TOKEN}'   # under another name, filled in by kern
kern -E GITHUB_TOKEN                 # a bare name: kern's own value, same name
kern -E NODE_ENV=test -E CI=1        # repeat it for more
KERN_ENV='PAGER=cat' kern            # the variable holds one
```

```toml
# config.toml, or a trusted project's .kern/config.toml
[env]
GH_TOKEN = "${GITHUB_TOKEN}"
NODE_ENV = "${NODE_ENV:-development}"
PAGER = "cat"
```

`-e` is `--effort`, so `env` is `-E`.

Each layer adds to the ones before it name by name: your `config.toml`, a
trusted project's, the profile, `KERN_ENV`, then the flags. A name set twice
keeps the later value. `env` lands on top of `env_passthrough`, so
`-E HOME=/tmp/h` beats the `HOME` that `core` passes.

Single quotes around `'${GITHUB_TOKEN}'` keep your shell from filling it in:
kern does it instead, so the key never sits in kern's command line, where
`ps` would show it.

`kern settings` lists each name and where it came from, never its value:

```text
env                      GH_TOKEN                                 /home/me/.config/kern/config.toml
env                      NODE_ENV                                 --env
```

## `${NAME}` in config files

Two places fill in variables from kern's environment, with Claude Code's
syntax:

| Written | Becomes |
|---|---|
| `${NAME}` | `NAME`'s value |
| `${NAME:-default}` | `NAME`'s value, else `default` |
| `$NAME`, `${not a name}` | itself: only a braced name is filled in |

- `[env]` values (and `-E`, `KERN_ENV`). A variable whose `${NAME}` has no
  value and no default is not passed, and kern says which.
- `.mcp.json`: a server's `command`, `args`, `env`, `url` and `headers`
  values. One with no value and no default stays as written, as in Claude
  Code, and kern says which server names it.

A variable set to an empty value counts as set: `${EMPTY:-x}` is empty.

## Recipes

Give bash in the real workspace a GitHub token and nothing else of yours:

```sh
kern --workspace real --tools bash --env-passthrough none \
  -E PATH=/usr/bin:/bin -E GH_TOKEN='${GITHUB_TOKEN}'
```

Run like Claude Code, whole environment included:

```sh
kern --profile claude
```

Keep the default `core`, and add what this project's tools need, in its
`.kern/config.toml` (read only inside your `trusted_dirs`):

```toml
[env]
DATABASE_URL = "${DATABASE_URL:-postgres://localhost:5432/app_dev}"
RUST_LOG = "info"
```

Start an MCP server with a key under the name it wants
([examples/mcp.json](examples/mcp.json)):

```json
{ "mcpServers": { "github": {
    "command": "github-mcp-server", "args": ["stdio"],
    "env": { "GITHUB_PERSONAL_ACCESS_TOKEN": "${GITHUB_TOKEN}" } } } }
```

Check what a run would get before running it:

```sh
kern settings --profile claude -E GH_TOKEN='${GITHUB_TOKEN}'
```
