---
name: rocq-induction-generalization
description: >
  Use when a Rocq/Coq proof is stuck around induction: weak induction
  hypotheses, wrong induction variable, need to revert/generalize dependent
  variables before induction, strong induction, dependent induction, or
  statement strengthening. Atomic, composable knowledge-point skill.
---

# Rocq Induction Generalization

Use this skill when induction gives an unusable IH or the proof needs a stronger
statement. Compose with `rocq-repl-workflow`.

## Signals

- IH is too specific.
- An index/parameter was introduced before induction and blocks the IH.
- Goal has `forall x`, but induction was done after `intros x`.
- Need `revert`, `generalize dependent`, strong induction, or dependent
  induction.

## Patterns

Generalize before induction:

```coq
revert y z.
induction x as [| x IH]; intros y z.
```

For dependent variables:

```coq
generalize dependent y.
induction x as [| x IH]; intros y.
```

If structural induction is too weak, prove a stronger lemma and instantiate it.

Read [references/patterns.md](references/patterns.md) for selection rules.
