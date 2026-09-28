---
name: rocq-dependent-rewriting
description: >
  Use when Rocq/Coq rewrite/subst/inversion fails because a term occurs in
  dependent hypotheses or indexed types, especially errors like "Abstracting
  over the term ... leads to a term which is ill-typed", motive errors,
  equality transport issues, dependent destruction, generalize dependent, or
  reverting hypotheses before rewriting. Atomic, composable knowledge-point
  skill; does not prescribe the proof workflow.
---

# Rocq Dependent Rewriting

This is a knowledge-point skill for rewriting under dependent types. Compose it
with `rocq-repl-workflow` for interactive proof development.

## When To Use

- `rewrite H` fails with an ill-typed abstraction/motive error.
- A variable appears in the type of a hypothesis.
- An indexed relation or dependent record carries equalities between indices.
- `inversion` or `destruct` creates equalities that do not rewrite cleanly.
- You need `generalize dependent`, `revert`, `subst`, or `dependent destruction`.

## Core Diagnosis

If a hypothesis has type `P x`, and you rewrite `x`, the hypothesis type must
also be transported. Rocq may fail to infer the correct dependent motive.

Do not keep trying random `rewrite` variants. Change the context shape first.

## Patterns

Move dependent hypotheses back into the goal:

```coq
generalize dependent h.
rewrite Heq.
intro h.
```

Or revert several dependencies:

```coq
revert h1 h2.
rewrite Heq.
intros h1 h2.
```

Use `subst` for variable equalities:

```coq
subst x.
```

Use dependent destruction when indexed constructors need their indices refined:

```coq
dependent destruction H.
```

Read [references/patterns.md](references/patterns.md) for examples and failure
signatures.
