---
name: rocq-induction-generalization
description: >
  Facts about Rocq induction principles, variable generalization, strong
  induction, dependent induction, and statement strengthening.
---

# Induction and Generalization Facts

- Variables introduced before an induction are fixed in the resulting
  induction hypothesis.
- `revert` and `generalize dependent` quantify such variables again before the
  induction, producing an induction hypothesis that is more general in those
  variables.
- Structural induction follows the constructors of an inductive object.
- Induction over an inductive derivation follows the constructors of the
  derivation relation.
- Strong or well-founded induction supplies an induction hypothesis for values
  smaller under a well-founded relation rather than only syntactic subterms.
- A stronger auxiliary statement can be instantiated to recover a more
  specific theorem.

```coq
revert y z.
induction x as [| x IH]; intros y z.
```

```coq
generalize dependent y.
induction x as [| x IH]; intros y.
```
