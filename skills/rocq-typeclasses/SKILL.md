---
name: rocq-typeclasses
description: >
  Use when Rocq/Coq proof or definition fails because of typeclass or canonical
  instance resolution: missing instance, Existing Instance, #[local] Instance,
  typeclasses eauto, setoid rewriting instances, Proper, Equivalence, Decision,
  EqDecision, or resolution performance/timeouts. Atomic, composable
  knowledge-point skill.
---

# Rocq Typeclasses

Use this skill for typeclass resolution and instance problems.

## Signals

- "Unable to satisfy the following constraints".
- "Cannot infer this placeholder".
- `typeclasses eauto` timeout.
- Missing `Decision`, `EqDecision`, `Countable`, `Proper`, or `Equivalence`.
- Setoid rewriting fails due to missing `Proper`.

## Patterns

Provide a local instance:

```coq
#[local] Instance my_inst : SomeClass X := ...
```

Expose an existing instance:

```coq
Existing Instance my_inst.
```

Ask Rocq to show resolution:

```coq
Set Typeclasses Debug.
```

Read [references/patterns.md](references/patterns.md) for debugging rules.
