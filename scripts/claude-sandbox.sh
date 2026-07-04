#!/usr/bin/env bash
#
# Run an autonomous Claude Code session inside the read-only `claude-sandbox`
# container (see docker/Dockerfile.sandbox). This fixes the three things a naive
# `docker run --read-only --mount src="$PWD"` gets wrong for this repo:
#
#   1. Mounts the PARENT projects/ dir (not just pigeon-mobile) so the core's
#      `../pigeon` path-dependencies resolve — a $PWD-only mount fails to build.
#   2. Redirects every writable path off the read-only rootfs: tmpfs for /tmp and
#      $HOME (/root, holds ~/.claude), named volumes for the cargo/gradle caches
#      and the core's target/ (kept out of the host tree to avoid host/container
#      rustc clashes). The bind-mounted repo stays writable so edits persist.
#   3. Passes ANTHROPIC_API_KEY through; the image's IS_SANDBOX=1 lets
#      skip-permissions run as root.
#
# Build the image first (the dev base must exist):
#   docker compose build dev
#   docker build -f docker/Dockerfile.sandbox -t claude-sandbox .
#
# Usage:
#   ANTHROPIC_API_KEY=sk-… ./scripts/claude-sandbox.sh [extra claude args]
set -euo pipefail

: "${ANTHROPIC_API_KEY:?set ANTHROPIC_API_KEY in your environment first}"

# This repo is .../projects/pigeon-mobile; mount the parent so ../pigeon is seen.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
projects_dir="$(dirname "$repo_root")"

exec docker run --rm -it \
  --mount type=bind,src="$projects_dir",dst=/work \
  --workdir /work/pigeon-mobile \
  --read-only \
  --tmpfs /tmp \
  --tmpfs /root \
  -v claude-sandbox-cargo:/cargo \
  -v claude-sandbox-gradle:/gradle \
  -v claude-sandbox-core-target:/work/pigeon-mobile/core/target \
  -e ANTHROPIC_API_KEY \
  claude-sandbox \
  claude --dangerously-skip-permissions "$@"
