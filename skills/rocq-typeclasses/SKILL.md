---
name: rocq-typeclasses
description: >
  Facts about Rocq typeclass and canonical-instance resolution, setoid
  rewriting, and instance debugging.
---

# Rocq Typeclass Facts

- Typeclass resolution searches registered instances for a class constraint.
- `#[local] Instance` registers an instance with local visibility.
- `Existing Instance x` registers an existing declaration as an instance.
- `Set Typeclasses Debug.` exposes typeclass search steps.
- `Decision`, `EqDecision`, `Countable`, `Proper`, and `Equivalence` are
  typeclass interfaces used by libraries including stdpp and setoid rewriting.
- `setoid_rewrite` requires an `Equivalence` for the relation and suitable
  `Proper` instances for functions under the relation.
- A typeclass-search timeout can result from a loop, a large search space, or a
  missing instance.

```coq
#[local] Instance my_inst : SomeClass X := ...
Existing Instance my_inst.
Set Typeclasses Debug.
```
