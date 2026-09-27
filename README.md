# Rocq MCP

An MCP server for interactive Rocq proofs. The workspace has four crates:

| Crate | Role |
|---|---|
| `trace-forest` | Concurrent immutable proof prefixes |
| `rocq-engine` | Thin Dune/PET orchestration and atomic proof writeback |
| `rocq-mcp` | MCP transport and JSON adapter built on `rmcp` |
| `rocq-e2e` | External MCP trace runner |

## Run

Requires Rust, Rocq, opam, and Dune. The tested versions are Rust 1.97, Rocq
9.1.1, the pinned PET 0.2.5 fork in `third_party/coq-lsp`, and Dune 3.22.
The pinned PET adds the document-level declaration endpoint used by
`list_decls`; an unmodified PET 0.2.5 does not implement that endpoint.

```sh
git submodule update --init third_party/coq-lsp
scripts/build-pet.sh
export ROCQ_PET_BIN="$PWD/target/pet/bin/rocq-mcp-pet"
cargo install --locked --path crates/rocq-mcp
rocq-mcp --stdio
# or: rocq-mcp --http 127.0.0.1:6278
```

The HTTP endpoint is `/mcp` and accepts loopback addresses only. The server has
no authentication. Use trusted clients and projects; proof publication edits
project source files. Set `ROCQ_NEW_STATE_DIR` to choose the engine state directory
and `ROCQ_MAX_PET_PROCESSES` to set the PET process limit (default: 4).
`ROCQ_PET_BIN` selects the PET executable once when the engine starts; when it
is unset, the engine resolves `pet` through `PATH`. The build script installs
to `target/pet` by default (override with `ROCQ_PET_PREFIX`) and emits a
launcher that binds PET to the matching coq-lsp plugin tree. It never depends
on Dune's private `_build` path.

The ten tools and their JSON results are specified in
[`COMMANDS.md`](crates/rocq-mcp/COMMANDS.md).

## Test

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

The E2E test replays 1,118 checked-in traces by default (1,157 with the
`fault-injection` feature). Set `ROCQ_E2E_CONCURRENCY` for parallel replay
(default: 4). Normal tool calls have no test-harness deadline because a valid
native Dune build has no production deadline; fault fixtures opt into an
explicit call watchdog. Connection setup and teardown remain bounded. The case matrix and
69 excluded, unreachable combinations are documented in
[`MATRIX.md`](crates/rocq-e2e/MATRIX.md). Regenerating compressed cases requires
`zstd` and Python 3.

Apache-2.0 licensed. See [LICENSE](LICENSE).
