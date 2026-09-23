#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
for tool in cargo rocq pet dune; do command -v "$tool" >/dev/null; done
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
