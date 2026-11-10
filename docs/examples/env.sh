# KERN_* variables: the settings of config.toml, for one shell.
# Source it (`. docs/examples/env.sh`) or copy the lines you want. A variable
# wins over the config files; a flag wins over the variable. Provider keys
# (OPENROUTER_API_KEY, ...) come from your own environment: ../environment.md.

export KERN_MODEL=openrouter:anthropic/claude-sonnet-5.5
export KERN_TOOLS=read,grep,glob,bash
export KERN_WORKSPACE=real
export KERN_ENV_PASSTHROUGH=core
export KERN_ENV='GH_TOKEN=${GITHUB_TOKEN}'
export KERN_MAX_TURN_USD=3
export KERN_TRUSTED_DIRS='~/projects'
