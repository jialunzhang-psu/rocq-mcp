# E2E matrix

`tools/trace_cases/matrix.py` is the executable matrix definition and
`TRACE_CASES.jsonl.zst` is its generated mapping. The matrix has one project
authority: Dune. `_CoqProject` layouts and source/catalog status filters are not
test dimensions because the wrapper no longer implements those semantics.

The current generated manifest contains **16,172 cases**, **69 explicit
exclusions**, and **1,157 physical trace files** (the latter share many cases).
The default replay covers **1,118 files / 28,290 events**; enabling
`fault-injection` covers all **1,157 files / 28,831 events**.
All non-excluded cases are mapped to a trace. Search cases exercise Rocq
`Search` patterns, optional explicit PET source contexts (`at`), malformed
patterns, and output-boundary behavior; they do not search catalog strings.
`Open` and `Completed` are the only proof lifecycle values: synchronous
writeback failures remain open attempts, so the matrix does not manufacture
durable `Pending` or `Rejected` targets.

## Authority and exclusions

- Dune `describe` selects sources and logical libraries; malformed Dune
  configuration is an invalid-configuration process case.
- PET owns declaration headers, AST ranges, command acceptance, goals,
  completion, and every semantic query.
- Local code is limited to source byte anchors, atomic persistence, and MCP
  envelope validation.
- There is no wrapper-owned project lock or exclusive-project failure axis.
  Concurrent build metadata is Dune's responsibility, and a competing Dune
  build is covered as a successful `start` case.
- Three duplicate-theory location combinations are excluded because Dune
  rejects the workspace before declaration location can be reached; their Dune
  configuration failures are covered separately.
- Context-free semantic queries are excluded unless a selected proof or
  explicit `at` declaration supplies a PET state.

Regenerate after matrix changes with:

```bash
python3 tools/generate_trace_cases.py
```
