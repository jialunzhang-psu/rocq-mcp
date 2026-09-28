---
name: rocq-mcp-tools
description: >
  Use when a Rocq task needs the connected rocq-mcp interface for
  attaching to a project, selecting or declaring a theorem, submitting proof
  commands, testing candidates, or querying declarations and goals.
---

# Rocq MCP Tools

Inspect the complete tool registry, not just an abbreviated top-level list.
The server exposes `start`, `list_files`, `list_decls`, `query`, `declare`,
`prove`, `abandon`, `check`, `try`, and `rewind` under the connected
`mcp__rocq_mcp__*` namespace. Do not confuse it with the separate
`mcp__codex_apps__rocq_*` plugin: that plugin may be present yet unhealthy.
Test the connected server directly before reporting Rocq MCP unavailable.

## Current interface

- `start({project_path})` attaches to one Dune workspace. Use `list_files({})`,
  then `list_decls({file})`; the returned
  `{file, qualified_path}` declaration ID is the input to later operations.
- `query` reads a theorem's statement, proof, definition, assumptions, or
  dependencies; queries also support goals, search, type, and notations. Call
  `start` first. Use the exact schema advertised by the tool registry.
- `prove({declaration})` selects the exact structured declaration ID.
- `declare({name, statement, kind?, library, file})` creates an in-memory proof
  at a Dune-selected source when the task authorizes adding one.
- `abandon({declaration})` discards that unpublished proof and never deletes
  source.
- `check({attempts})` takes 1–20 ordered, possibly multi-sentence fragments,
  commits the first fragment PET accepts completely, and does not evaluate
  later fragments. Rejected fragments leave no accepted prefix.
- `try({attempts})` evaluates every fragment independently from the same base
  without advancing the trace, checkpoint, or source.
- `rewind({})`, `rewind({steps})`, or `rewind({checkpoint})` selects an earlier
  successful-check boundary. Inspect `isError` and `structuredContent` for
  every tool result.

Proof status is not inferred from a source terminator. Interactive goals and
completion come from PET; publication additionally requires Dune/Rocq build
and the PET assumptions audit. For `query type` and `notations`, a selected
open proof provides the live context; without one, pass an explicit `at`
declaration to select the PET source context.

The wrapper retains proof branches internally; cursors are not part of the MCP
wire protocol. Open states expose connection-local integer checkpoints.
Successful solved checks publish through source CAS and Dune validation. For finished work,
inspect the actual source diff and run the project's normal build and trust
checks.

If the server fails, report its exact error and the operation tried.
Source inspection and post-write project builds may use repository tools; live
goal inspection and tactic experiments should use the connected MCP interface.
