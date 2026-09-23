# Rocq MCP

An MCP server for interactive Rocq proofs. The workspace has four crates:

| Crate | Role |
|---|---|
| `trace-forest` | Concurrent immutable proof prefixes |
| `rocq-engine` | PET interaction, proof recovery, verification, publication |
| `rocq-mcp` | MCP transport and JSON adapter built on `rmcp` |
| `rocq-e2e` | External MCP trace runner |

## Run

Requires Rust, Rocq, PET, and Dune on `PATH`. The tested versions are Rust 1.97,
Rocq 9.1.1, PET 0.2.5, and Dune 3.22.

```sh
cargo install --locked --path crates/rocq-mcp
rocq-mcp --stdio
# or: rocq-mcp --http 127.0.0.1:6278
```

The HTTP endpoint is `/mcp` and accepts loopback addresses only. The server has
no authentication. Use trusted clients and projects; proof publication edits
project source files. Set `ROCQ_NEW_STATE_DIR` to choose the engine state directory
and `ROCQ_MAX_PET_PROCESSES` to set the PET process limit (default: 4).

The six tools and their JSON results are specified in
[`COMMANDS.md`](crates/rocq-mcp/COMMANDS.md).

## Test

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

The E2E test replays 1,957 checked-in traces against an external server. Set
`ROCQ_E2E_CONCURRENCY` for parallel replay (default: 4). The case matrix and
114 excluded, unreachable combinations are documented in
[`MATRIX.md`](crates/rocq-e2e/MATRIX.md). Regenerating compressed cases requires
`zstd` and Python 3.

Apache-2.0 licensed. See [LICENSE](LICENSE).
