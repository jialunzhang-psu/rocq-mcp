---
name: rocq-compile-error-triage
description: >
  Use when a Rocq/Coq command or build reports syntax errors, type mismatch,
  unable to unify, unknown identifiers, universe inconsistency, no such goal,
  termination, timeout, or misleading Qed/unsolved-goal errors. Atomic
  knowledge point extracted from LLM4Rocq rocq-skills compilation-errors and
  proof-repair references.
---

# Rocq Compile Error Triage

Use this skill to classify errors, not to drive proof search. Compose with
`rocq-repl-workflow`.

## Priority

Fix in this order:

1. Syntax/parse errors.
2. Kernel typing or universe errors.
3. Tactic failures and unsolved goals.
4. Warnings/deprecations.

Later diagnostics are unreliable while earlier errors remain.

## Common Classifications

- `The term ... has type ... expected ...`: type mismatch; inspect expected
  type, use `change`, annotation, rewrite, or a more precise lemma.
- `Unable to unify`: wrong goal shape; unfold/simplify/rewrite only after
  inspecting the current goal.
- `reference ... not found`: missing import, wrong module path, typo, or scope.
- `No such goal`: tactic consumed more goals than expected; fix bullet/focusing.
- `Universe inconsistency`: do not patch tactics blindly; inspect definitions.
- `Cannot guess decreasing argument`: termination/structural recursion issue.
- Error at `Qed`: often means an earlier tactic left subgoals open.

## Source Basis

Extracted from:

- `rocq-skills/.../references/compilation-errors.md`
- `rocq-skills/.../references/compiler-guided-repair.md`
- `rocq-skills/.../lib/scripts/parse_rocq_errors.py`
