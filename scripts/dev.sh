#!/usr/bin/env bash
# Usage: scripts/dev.sh <cargo args>
# Runs cargo inside the dev container. With no args, cross-builds the x64 release binary.
# ARM64 build: scripts/dev.sh xwin build --release --target aarch64-pc-windows-msvc
# All tests, end-to-end included: scripts/dev.sh test --features fake-device
set -euo pipefail

cd "$(dirname "$0")/.."

if [ "$#" -eq 0 ]; then
    set -- xwin build --release --target x86_64-pc-windows-msvc
fi

exec docker compose run --rm dev cargo "$@"
