---
name: rocq-dune-loadpath
description: >
  Use when Rocq/Coq development involves Dune integration, rocq.theory stanzas,
  dune-project, generated _CoqProject/_RocqProject files, logical load paths,
  -Q/-R/-I flags, Require/From import failures, or starting a REPL/LSP/MCP
  session with the project environment. Atomic, composable knowledge-point
  skill.
---

# Rocq Dune Load Paths

Use this skill when imports or REPL startup fail because the project load path is
wrong. Compose with `rocq-repl-workflow`.

## Concepts

- Dune `rocq.theory` defines the logical theory name and dependencies.
- `_CoqProject` / `_RocqProject` may be generated for editor and REPL tooling.
- `-Q dir Logical.Name` maps a physical directory to a logical prefix.
- `-R dir Logical.Name` is recursive and allows shorter `Require` forms.
- `From X Require Import Y.` depends on the logical path.

## Triage

- Check `dune-project` and `dune` for `(using rocq ...)` and `rocq.theory`.
- Check generated `_CoqProject` or `_RocqProject` if present.
- Prefer `dune coq top` or generated project flags over hand-written guesses.
- If REPL/LSP cannot find a module, first fix load paths/imports, not tactics.

Read [references/patterns.md](references/patterns.md) for common layouts.
