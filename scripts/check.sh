#!/usr/bin/env bash
# Merge gate — run before every commit (docs/roadmap.md, Guardrails).
# Inside the flake devshell:  ./scripts/check.sh
# Outside:                    nix develop --command ./scripts/check.sh
set -euo pipefail
cd "$(dirname "$0")/.."

say() { printf '\n── %s\n' "$*"; }

say "cargo fmt --check"
cargo fmt --check

say "cargo clippy -- -D warnings"
cargo clippy --all-targets -- -D warnings

say "cargo test"
cargo test

say "check.sh: all green"
