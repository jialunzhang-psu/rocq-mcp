---
name: rocq-dependent-rewriting
description: >
  Facts about equality rewriting, transport, and elimination in dependent and
  indexed Rocq/Coq types.
---

# Dependent Rewriting Facts

- If a hypothesis has type `P x`, replacing `x` by an equal term also changes
  the type of that hypothesis; the change is an equality transport.
- `rewrite` constructs an equality-elimination motive. A motive that leaves a
  dependent hypothesis ill-typed causes an abstraction or motive error.
- `generalize dependent h` and `revert h` remove a dependent hypothesis from
  the local context and reintroduce it after the equality elimination.
- `subst x` eliminates a variable when the context contains an equality that
  identifies `x` with a term in which `x` does not occur.
- `dependent destruction H` performs constructor elimination while refining
  the indices constrained by `H`.
- Indexed inductive relations and dependent records can require these
  transport operations after `inversion` or `destruct`.

## Examples

```coq
generalize dependent h.
rewrite Heq.
intro h.
```

```coq
revert h1 h2.
rewrite Heq.
intros h1 h2.
```

```coq
subst x.
```

```coq
dependent destruction H.
```
