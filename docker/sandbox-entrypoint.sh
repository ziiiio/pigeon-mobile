#!/usr/bin/env bash
# Sandbox entrypoint: seed the container's ephemeral $HOME with the host's Claude
# login (mounted READ-ONLY at /seed) before exec'ing the command. Claude needs a
# writable ~/.claude (session state, token refresh); copying rather than
# bind-mounting the host dir means in-container writes/refreshes stay ephemeral
# and never modify — or change ownership of — the host's real credentials.
set -e

if [ -d /seed/claude ]; then
  mkdir -p "$HOME/.claude"
  cp -a /seed/claude/. "$HOME/.claude/"
fi
[ -f /seed/claude.json ] && cp -a /seed/claude.json "$HOME/.claude.json"

exec "$@"
