# E2E matrix

`tools/trace_cases/matrix.py` defines the Cartesian products and
`TRACE_CASES.jsonl.zst` maps each case to a trace and command position. The
runner invokes an external `rocq-mcp` process over MCP; engine unit tests do not
count as E2E coverage.

| Family | Cases | Trace files |
|---|---:|---:|
| `start_parameters`, `start_paths`, `start_failures` | 363 | 42 |
| `search_valid`, `search_invalid`, `search_boundaries` | 120,336 | 84 |
| `query_kind`, `query_target`, `query_expression`, `query_failures` | 2,706 | 180 |
| `declare_parameters`, `declare_boundaries`, `declare_failures` | 11,076 | 57 |
| `prove`, `prove_failures` | 1,557 | 1,224 |
| `check`, `check_escaping_heads`, `check_publication` | 9,954 | 186 |
| `check_multi_parameters`, `check_multi_order`, `check_multi_failures` | 924 | 33 |
| `two_users`, `three_users` | 51 | 51 |
| `environment_change` | 84 | 84 |
| `publication_fault`, `simultaneous_requests` | 40 | 40 |
| **Total** | **147,091** | **1,981 family slots** |

Some families share trace files: the corpus has **1,957 files**, with **169,255
events** in the fault-injection build. The manifest marks **146,977 cases
implemented**, **114 excluded**, and **none unmapped**. The structural tests
check case-to-command mapping and file/event counts; the process test compares
complete responses. Forty-two fault and declaration-race traces run only with
`--all-features`.

## Excluded combinations

These outputs cannot occur through the current public route. They remain in the
manifest with an `exclusion.code` and reason; they are not reported as tested.

| Code | Cases | Reason |
|---|---:|---|
| `query_without_target` | 24 | `goals`, `search`, `type`, and `notations` have no target to resolve. |
| `query_without_proof_anchor` | 24 | Only `goals` compares an existing proof anchor. |
| `query_without_pet` | 12 | Catalog/source-only queries cannot have a PET timeout. |
| `attached_project_query` | 27 | Query reuses the project attached by `start`. |
| `attached_project_prove` | 3 | Prove reuses that attachment. |
| `attached_project_check` | 18 | Check reuses the attachment held since start/declare. |
| `attached_project_check_multi` | 3 | Candidate checks use the selected proof's attachment. |
| `open_has_no_axiom_baseline` | 3 | An unsolved proof has no frozen candidate baseline. |

The suite covers `_CoqProject` and Dune layouts, all six tools, connection and
server restarts, multi-user branches, source and toolchain changes, PET faults,
trust rejection, publication crash windows, and recovery. It does **not** test a
network intermediary dropping a response while the server stays alive, or
arbitrary large-scale schedules beyond the checked-in simultaneous requests.
