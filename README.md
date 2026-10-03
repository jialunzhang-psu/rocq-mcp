# Rocq MCP

An MCP server for interactive Rocq proofs. The workspace has three crates:

| Crate | Role |
|---|---|
| `rocq-engine` | Thin Dune/PET orchestration and atomic proof writeback |
| `rocq-mcp` | MCP transport and JSON adapter built on `rmcp` |
| `rocq-e2e` | External MCP trace runner |

## MCP proof workflow

The authoritative catalog contains ten tools: `start`, `list_files`,
`list_decls`, `query`, `declare`, `prove`, `abandon`, `check`, `try`, and
`rewind`. To edit an existing unfinished declaration, use:

```text
start -> list_files -> list_decls -> prove -> try/check/query/rewind
```

`prove` enters and selects the exact declaration returned by `list_decls`.
`try` only evaluates hypothetical fragments and requires that selection;
`invalid_request: call prove first` means no proof is active on that MCP
connection. `check` commits an accepted fragment and, when it closes the proof,
performs atomic source writeback, the Dune build, PET refresh, and the trust
audit. There is no separate save command and no Desktop or shell/file tool is
needed. For a new declaration, use `declare` instead of `prove`.

Read-only `query` operations—including expression type checking and
`assumptions`—do not enter or modify a proof. An assumptions result containing
`Axioms:` means the target depends on the listed declarations; neither a
successful type query nor `Completed` means axiom-free. `Completed` permits
explicit in-project `Axiom` dependencies under the documented trust policy.

The server returns all ten entries from uncursored MCP `tools/list`. If a
client tool picker or semantic tool search displays only a subset, refresh or
reconnect that connector and inspect `tools/list`; do not treat the filtered or
stale client catalog as the server's capability set. The complete wire
contract and lifecycle details are in
[`COMMANDS.md`](crates/rocq-mcp/COMMANDS.md).

## Run

Requires Rust, Rocq, opam, and Dune. The tested versions are Rust 1.97, Rocq
9.1.1, the pinned PET 0.2.5 fork in `third_party/coq-lsp`, and Dune 3.22.
The pinned PET adds canonical document declarations, exact state release,
nested-module insertion anchors, parser-aware traced speculative runs, and
authoritative workspace refresh. An
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
project source files. Each attached Dune project has at most one shared PET
child; it is started lazily on the first PET-backed operation, and connections
attached to the same project share it. `ROCQ_PET_BIN` selects the PET
executable once when the engine starts; when it
is unset, the engine resolves `pet` through `PATH`. The build script installs
to `target/pet` by default (override with `ROCQ_PET_PREFIX`) and emits a
launcher that binds PET to the matching coq-lsp plugin tree. It never depends
on Dune's private `_build` path. Dune commands have no default correctness
deadline; operators may set `ROCQ_COMMAND_TIMEOUT_SECS` explicitly.
Semantic `query` calls have a 240-second watchdog so an abandoned project
refresh or prover query cannot outlive its client indefinitely; the lightweight
`query(kind:"progress")` poll is exempt. Override the semantic-query deadline
with a positive `ROCQ_QUERY_TIMEOUT_SECS` value.

The ten tools and their JSON results are specified in
[`COMMANDS.md`](crates/rocq-mcp/COMMANDS.md).
Multi-sentence `check`/`try` failures can opt into parser-derived sentence
traces and pre-failure goal snapshots; project symbol lookup, structured
goals/diffs, and publication/PET replay status are exposed through documented
`query` variants. Progress is strictly client-polled with
`query(kind:"progress")`; the server does not emit progress notifications.
For `start`, pass an absolute path to the project on the MCP server when using
HTTP/Funnel. Relative paths work only when the client supplies an explicit
working directory or a single local MCP workspace root.

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
