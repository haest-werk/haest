# HAEST_* variables: the settings of config.toml, for one shell.
# Source it (`. docs/examples/env.sh`) or copy the lines you want. A variable
# wins over the config files; a flag wins over the variable. Provider keys
# (OPENROUTER_API_KEY, ...) come from your own environment: ../environment.md.

export HAEST_MODEL=openrouter:anthropic/claude-sonnet-5.5
export HAEST_TOOLS=read,grep,glob,bash
export HAEST_WORKSPACE=real
export HAEST_ENV_PASSTHROUGH=core
export HAEST_ENV='GH_TOKEN=${GITHUB_TOKEN}'
export HAEST_MAX_TURN_USD=3
export HAEST_TRUSTED_DIRS='~/projects'

# A parent process that drives haest over its stdin and stdout sets this in
# the child's environment only, never in a shell you type in (it is --stdio):
#   HAEST_STDIO=true haest
