#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
rounds="${1:-1}"
[[ "$rounds" =~ ^[1-9][0-9]*$ ]] || { echo 'rounds must be positive' >&2; exit 2; }
for ((round=1; round<=rounds; round++)); do
  printf 'E2E round %d/%d\n' "$round" "$rounds"
  cargo test --locked -p rocq-mcp --test e2e_traces --all-features
done
