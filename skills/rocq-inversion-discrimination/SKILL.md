---
name: rocq-inversion-discrimination
description: >
  Facts about constructor disjointness, constructor injectivity, inversion,
  discrimination, and case analysis for Rocq inductive types.
---

# Constructor Reasoning Facts

- Distinct constructors of an inductive type are disjoint.
- Equal applications of the same constructor have equal corresponding
  arguments.
- `discriminate` derives contradiction from an equality between distinct
  constructors.
- `injection` derives equalities of constructor arguments.
- `inversion H` exposes the constructors that can have produced evidence `H`
  and derives their parameter and index equalities.
- `destruct x` performs constructor case analysis on `x`.
- `constructor` applies a constructor of an inductive proposition to the
  current goal when its premises can be generated.

```coq
discriminate.
injection H as H1 H2.
inversion H; subst.
destruct x as [| x xs].
```
