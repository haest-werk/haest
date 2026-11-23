# haest and its environment

haest reads a few variables of its own environment, and passes on only what
you allow. Nothing else of your environment reaches a model, a tool or a
process haest starts.

1. [Variables haest reads](#variables-haest-reads)
2. [What a process haest starts gets](#what-a-process-haest-starts-gets)
3. [Passing a variable on purpose: `env`](#passing-a-variable-on-purpose-env)
4. [`${NAME}` in config files](#name-in-config-files)
5. [Recipes](#recipes)

Every setting, with its three spellings, is in [settings.md](settings.md).
Whole example files are in [examples/](examples/).

## Variables haest reads

### Settings: `HAEST_*`

Every setting is also a variable: `HAEST_` and the setting's name in upper
case (`tools` is `HAEST_TOOLS`, `max_turn_usd` is `HAEST_MAX_TURN_USD`). A
variable wins over the config files and loses to a flag. An empty variable
counts as unset. [settings.md](settings.md) lists them all.

```sh
HAEST_TOOLS=read,grep,glob haest "what does this repo do?"
export HAEST_MAX_TURN_USD=1   # every run in this shell
```

### Where haest keeps its files

| Variable | What it changes |
|---|---|
| `HOME` | your home dir: `~` in a path setting, and the default dirs below |
| `XDG_CONFIG_HOME` | `config_dir` becomes `$XDG_CONFIG_HOME/haest` (else `~/.config/haest`) |
| `XDG_DATA_HOME` | `data_dir` becomes `$XDG_DATA_HOME/haest` (else `~/.local/share/haest`) |
| `HAEST_CONFIG_DIR` | `config_dir` itself: where `config.toml`, `providers.json` and your mods are |
| `HAEST_DATA_DIR` | `data_dir` itself: sessions, history and spill files |

### Provider keys

haest reads a provider's key when a run uses that provider (`-m
provider:model`). The keys stay with haest: a process haest starts gets one only
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

### The broker link's token

`HAEST_BROKER_TOKEN`: the token a link to a broker (`broker_url`) sends in
its first frame. It is a key, not a setting: no file, flag or profile sets it,
and like a provider's key it stays with haest.

### Variables a config file names

`${NAME}` in an `[env]` value or in `.mcp.json` is read from haest's
environment: see [`${NAME}` in config files](#name-in-config-files).

### Proxy variables: not read for model requests

A model request goes straight to the provider's endpoint, and so does
WebFetch. For them `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY` (in
either case) and the operating system's proxy settings are not read, so your
shell never sends a provider key through a proxy. To reach a provider
through a gateway, point its endpoint override (`ANTHROPIC_BASE_URL`, a
`providers.json` entry's `baseURL`) at it. A program that embeds haest and
wants a proxy hands in an HTTP client configured with one.

## What a process haest starts gets

haest starts a process for real bash (`workspace = real`), each background job
of it, each stdio MCP server, and each [`hooks`](settings.md#hooks) command
(on the host, in either workspace). Such a process gets an environment built
from scratch, in this order, a later one winning for the same name:

1. what `env_passthrough` lets through of haest's environment
2. the variables in `env`
3. for an MCP server, its own `env` in `.mcp.json`; for a hook in the
   `claude` dialect, `CLAUDE_PROJECT_DIR`

| `env_passthrough` | A process gets |
|---|---|
| `none` | nothing of haest's environment (bash is still found on `/usr/bin:/bin`) |
| `core` (the default) | `PATH`, `SHELL`, `TMPDIR`, `TEMP`, `TMP`, `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `LOGNAME`, `USER` |
| `all` | every variable, API keys included (the `claude` profile sets this, as Claude Code does) |

The words and the `core` list are Codex's (`shell_environment_policy.inherit`).

The sandbox (`workspace = sandbox`, the default) starts no process: its bash
is simulated. Its shell has its own `HOME` (`/home/user`) and `USER` (`user`),
nothing of your environment and nothing `env_passthrough` lets through. It
gets the variables in `env`, and only those.

## Passing a variable on purpose: `env`

`env` hands a variable to every process haest starts and to the sandbox's
shell, the way `docker run -e` does. It is a flag you repeat, a variable, and
a table in a config file:

```sh
haest -E GH_TOKEN='${GITHUB_TOKEN}'   # under another name, filled in by haest
haest -E GITHUB_TOKEN                 # a bare name: haest's own value, same name
haest -E NODE_ENV=test -E CI=1        # repeat it for more
HAEST_ENV='PAGER=cat' haest            # the variable holds one
```

```toml
# config.toml, or a trusted project's .haest/config.toml
[env]
GH_TOKEN = "${GITHUB_TOKEN}"
NODE_ENV = "${NODE_ENV:-development}"
PAGER = "cat"
```

`-e` is `--effort`, so `env` is `-E`.

Each layer adds to the ones before it name by name: your `config.toml`, a
trusted project's, the profile, `HAEST_ENV`, then the flags. A name set twice
keeps the later value. `env` lands on top of `env_passthrough`, so
`-E HOME=/tmp/h` beats the `HOME` that `core` passes.

Single quotes around `'${GITHUB_TOKEN}'` keep your shell from filling it in:
haest does it instead, so the key never sits in haest's command line, where
`ps` would show it.

`haest settings` lists each name and where it came from, never its value:

```text
env                      GH_TOKEN                                 /home/me/.config/haest/config.toml
env                      NODE_ENV                                 --env
```

## `${NAME}` in config files

Three places fill in variables from haest's environment, with Claude Code's
syntax:

| Written | Becomes |
|---|---|
| `${NAME}` | `NAME`'s value |
| `${NAME:-default}` | `NAME`'s value, else `default` |
| `$NAME`, `${not a name}` | itself: only a braced name is filled in |

- `[env]` values (and `-E`, `HAEST_ENV`). A variable whose `${NAME}` has no
  value and no default is not passed, and haest says which.
- `.mcp.json`: a server's `command`, `args`, `env`, `url` and `headers`
  values. One with no value and no default stays as written, as in Claude
  Code, and haest says which server names it. A key goes in `headers`
  (`"Authorization": "Bearer ${TOKEN}"`) or the URL's query, never its
  path: haest keeps a header's value and the query out of its logs and
  errors, and shows the path as it is.
- A [`hooks`](settings.md#hooks) entry's `headers` values
  (`authorization = "Bearer ${REVIEW_TOKEN}"`). One with no value and no
  default is sent as written, and haest says which entry and header name
  it. `haest settings` shows a header by its name only.

A variable set to an empty value counts as set: `${EMPTY:-x}` is empty.

## Recipes

Give bash in the real workspace a GitHub token and nothing else of yours:

```sh
haest --workspace real --tools bash --env-passthrough none \
  -E PATH=/usr/bin:/bin -E GH_TOKEN='${GITHUB_TOKEN}'
```

Run like Claude Code, whole environment included:

```sh
haest --profile claude
```

Keep the default `core`, and add what this project's tools need, in its
`.haest/config.toml` (read only inside your `trusted_dirs`):

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
haest settings --profile claude -E GH_TOKEN='${GITHUB_TOKEN}'
```
