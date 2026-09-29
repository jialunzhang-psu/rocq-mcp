#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
rounds="${1:-1}"
[[ "$rounds" =~ ^[1-9][0-9]*$ ]] || { echo 'rounds must be positive' >&2; exit 2; }
if [[ -z "${ROCQ_PET_BIN:-}" ]]; then
  if [[ -x "$PWD/target/pet/bin/rocq-mcp-pet" ]]; then
    export ROCQ_PET_BIN="$PWD/target/pet/bin/rocq-mcp-pet"
  else
    export ROCQ_PET_BIN="$(command -v pet)"
  fi
fi
for ((round=1; round<=rounds; round++)); do
  printf 'MCP lifecycle round %d/%d\n' "$round" "$rounds"
  # The process-boundary suite tests the current public lifecycle directly;
  # rocq-e2e separately checks the strict external trace contract.
  cargo test --locked -p rocq-mcp --test stdio_protocol
  cargo test --locked -p rocq-e2e
  cargo build --locked -p rocq-mcp -p rocq-e2e
  target/debug/rocq-trace target/debug/rocq-mcp \
    crates/rocq-e2e/traces/current_protocol.jsonl
done
