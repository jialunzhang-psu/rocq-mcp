#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
for tool in cargo rocq dune; do command -v "$tool" >/dev/null; done
if [[ -z "${ROCQ_PET_BIN:-}" ]]; then
  if [[ -x "$PWD/target/pet/bin/rocq-mcp-pet" ]]; then
    export ROCQ_PET_BIN="$PWD/target/pet/bin/rocq-mcp-pet"
  else
    export ROCQ_PET_BIN="$(command -v pet)"
  fi
fi
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
