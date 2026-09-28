---
name: rocq-recursion-termination
description: >
  Use when Rocq/Coq rejects Fixpoint/CoFixpoint/Program Fixpoint definitions
  because it cannot guess or verify a decreasing argument, structural recursion
  is not evident, a fuel parameter is needed, or well-founded recursion/induction
  is required. Atomic, composable knowledge-point skill.
---

# Rocq Recursion Termination

Use this skill when the guard checker rejects recursion.

## Signals

- "Cannot guess decreasing argument of fix".
- Recursive call is not on a syntactic subterm.
- Recursion follows a measure rather than a constructor.
- A function needs fuel or well-founded recursion.

## Options

- Reorder arguments so the decreasing argument is structurally visible.
- Add `{struct x}` when the structural argument is clear.
- Introduce a fuel parameter for executable checkers.
- Use well-founded recursion when recursion follows a measure.
- Prove a relation/induction lemma instead of forcing a complicated Fixpoint.

Read [references/patterns.md](references/patterns.md) for tradeoffs.
