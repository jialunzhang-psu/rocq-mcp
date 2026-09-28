#!/usr/bin/env bash
set -euo pipefail

# This is the MCP-owned build hook.  mcp-manager invokes it as an opaque
# lifecycle command; all Rocq/PET dependency knowledge stays in this checkout.
repository=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
cd "$repository"

git submodule update --init --checkout -- third_party/coq-lsp
scripts/build-pet.sh
cargo build --release --locked --package rocq-mcp
