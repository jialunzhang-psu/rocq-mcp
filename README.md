# Rocq MCP

An MCP server for interactive Rocq proofs. The workspace has three crates:

| Crate | Role |
|---|---|
| `rocq-engine` | Thin Dune/PET orchestration and atomic proof writeback |
| `rocq-mcp` | MCP transport and JSON adapter built on `rmcp` |
| `rocq-e2e` | External MCP trace runner |

## Run

Requires Rust, Rocq, opam, and Dune. The tested versions are Rust 1.97, Rocq
9.1.1, the pinned PET 0.2.5 fork in `third_party/coq-lsp`, and Dune 3.22.
The pinned PET adds canonical document declarations, exact state release,
nested-module insertion anchors, and authoritative workspace refresh. An
unmodified PET 0.2.5 does not implement those lifecycle endpoints.

```sh
git submodule update --init third_party/coq-lsp
scripts/build-pet.sh
export ROCQ_PET_BIN="$PWD/target/pet/bin/rocq-mcp-pet"
cargo install --locked --path crates/rocq-mcp
rocq-mcp --stdio
# or: rocq-mcp --http 127.0.0.1:6278
```

The repository also exposes `scripts/build.sh` as its manager integration
hook. It performs the submodule initialization, pinned PET build, and MCP
build as one server-owned operation; `mcp-manager` only invokes that hook.

The HTTP endpoint is `/mcp` and accepts loopback addresses only. The server has
no authentication. Use trusted clients and projects; proof publication edits
project source files. Each active Dune project owns one long-lived PET child;
connections attached to the same project share it. `ROCQ_PET_BIN` selects the
PET executable once when the engine starts; when it
is unset, the engine resolves `pet` through `PATH`. The build script installs
to `target/pet` by default (override with `ROCQ_PET_PREFIX`) and emits a
launcher that binds PET to the matching coq-lsp plugin tree. It never depends
on Dune's private `_build` path. Dune commands have no default correctness
deadline; operators may set `ROCQ_COMMAND_TIMEOUT_SECS` explicitly.

The ten tools and their JSON results are specified in
[`COMMANDS.md`](crates/rocq-mcp/COMMANDS.md).

## Test

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The maintained stdio suite exercises the current public lifecycle across a
real process boundary. `rocq-e2e` is a small external HTTP trace runner; its
checked-in current-protocol trace is documented in
[`TRACE_FORMAT.md`](crates/rocq-e2e/TRACE_FORMAT.md). Tool calls have no default
harness deadline because a valid native Dune build has no production deadline.
Connection setup and teardown remain bounded.

Apache-2.0 licensed. See [LICENSE](LICENSE).
