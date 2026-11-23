#!/bin/sh
# A Claude Code PreToolUse hook: refuse a Bash command that runs `rm -rf`.
#
# It runs as it is under Claude Code and under haest, bound to Bash:
#
#   Claude Code (.claude/settings.json):
#     "hooks": { "PreToolUse": [{ "matcher": "Bash",
#       "hooks": [{ "type": "command", "command": "sh docs/examples/hooks/no-rm-rf.sh" }] }] }
#
#   haest (config.toml, copied to ~/.config/haest/hooks/):
#     [[hooks."tool.before"]]
#     command = "sh ~/.config/haest/hooks/no-rm-rf.sh"
#     dialect = "claude"
#     matcher = "Bash"
#
#   haest (Rust):
#     HookSet::new().on_tool_before(
#         Binding::new(ShellHook::new("sh docs/examples/hooks/no-rm-rf.sh").with_dialect(Claude))
#             .with_matcher("Bash"))
#     (its session_id: the one the conversation's ModEnv names, `ModEnv::set_session`)
#
# The call arrives as JSON on stdin ({ "tool_name": "Bash", "tool_input": { "command": .. }, .. }).
# Printing nothing lets it run; this answer refuses it, and the model reads the reason.
# The same refusal as an exit code: print the reason to stderr and `exit 2`.

if grep -q 'rm -rf'; then
  echo '{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "rm -rf is not allowed here"}}'
fi
