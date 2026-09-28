---
name: rocq-inversion-discrimination
description: >
  Use when a Rocq/Coq proof needs constructor reasoning: inversion,
  discriminate, injection, constructor disjointness, constructor injectivity,
  impossible cases, or inversion H; subst. Atomic, composable knowledge-point
  skill.
---

# Rocq Inversion and Discrimination

Use this skill for ordinary inductive constructor reasoning. For indexed
dependent transport issues, combine with `rocq-dependent-rewriting`.

## Patterns

Impossible constructor equality:

```coq
discriminate.
```

Constructor injectivity:

```coq
injection H as H1 H2.
```

Inductive hypothesis inversion:

```coq
inversion H; subst.
```

Case split on a constructor:

```coq
destruct x as [| x xs].
```

Read [references/patterns.md](references/patterns.md) for selection guidance.
