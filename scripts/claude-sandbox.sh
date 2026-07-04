#!/usr/bin/env bash
#
# Run an autonomous Claude Code session inside the read-only `claude-sandbox`
# container (see docker/Dockerfile.sandbox). This fixes the things a naive
# `docker run --read-only --mount src="$PWD"` gets wrong for this repo:
#
#   1. Mounts the PARENT projects/ dir (not just pigeon-mobile) so the core's
#      `../pigeon` path-dependencies resolve — a $PWD-only mount fails to build.
#   2. Redirects every writable path off the read-only rootfs: tmpfs for /tmp and
#      $HOME (/root), named volumes for the cargo/gradle caches and the core's
#      target/ (kept out of the host tree to avoid host/container rustc clashes).
#      The bind-mounted repo stays writable so edits persist.
#   3. Auth: reuses your host Claude subscription login — no API key needed on
#      Pro/Max. Your ~/.claude is mounted READ-ONLY at /seed and copied into the
#      container's ephemeral $HOME at start (see docker/sandbox-entrypoint.sh),
#      so the session is logged in but in-container writes/token-refreshes never
#      touch your host credentials. The image's IS_SANDBOX=1 lets
#      skip-permissions run as root.
#
# Build the image first (the dev base must exist):
#   docker compose build dev
#   docker build -f docker/Dockerfile.sandbox -t claude-sandbox .
#
# Usage:
#   ./scripts/claude-sandbox.sh [extra claude args]
#
# (API-key billing instead of a subscription? Drop the ~/.claude mounts below and
#  add `-e ANTHROPIC_API_KEY` with the key set in your environment.)
set -euo pipefail

# This repo is .../projects/pigeon-mobile; mount the parent so ../pigeon is seen.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
projects_dir="$(dirname "$repo_root")"

# Host Claude credentials/config to reuse (subscription login lives here).
claude_home="${HOME}/.claude"
claude_json="${HOME}/.claude.json"
if [ ! -d "$claude_home" ]; then
  echo "error: $claude_home not found — run 'claude' on the host and log in first" >&2
  exit 1
fi

# Mount the host login READ-ONLY at /seed; the entrypoint copies it into the
# writable tmpfs $HOME. ~/.claude.json (config/onboarding) is optional.
args=(
  --rm -it
  --mount "type=bind,src=${projects_dir},dst=/work"
  --workdir /work/pigeon-mobile
  --read-only
  --tmpfs /tmp
  --tmpfs /root
  -v "${claude_home}:/seed/claude:ro"
  -v claude-sandbox-cargo:/cargo
  -v claude-sandbox-gradle:/gradle
  -v claude-sandbox-core-target:/work/pigeon-mobile/core/target
)
[ -f "$claude_json" ] && args+=(-v "${claude_json}:/seed/claude.json:ro")

exec docker run "${args[@]}" \
  claude-sandbox \
  claude --dangerously-skip-permissions "$@"
