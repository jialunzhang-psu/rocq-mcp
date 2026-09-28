---
name: rocq-stdlib-lemmas
description: >
  Facts about Rocq Stdlib modules for arithmetic, booleans, options, lists,
  strings, relations, permutations, and equality.
---

# Rocq Stdlib Module Facts

- `PeanoNat` and `Arith` contain natural-number results; `Lia` contains linear
  arithmetic procedures and results; `ZArith` contains integer arithmetic.
- `Bool` contains boolean definitions and results.
- `List` and `ListNotations` contain list definitions, notation, and results.
- `Sorting.Permutation` contains permutation relations and their results.
- `Relations`, `RelationClasses`, and `Morphisms` contain relation and
  morphism infrastructure.
- `String` contains string definitions and results.
- `Program.Equality`, `Logic.Eqdep_dec`, and `Logic.JMeq` contain dependent
  equality and dependent elimination infrastructure.

```coq
From Stdlib Require Import Lia PeanoNat ZArith.
From Stdlib Require Import List.
Import ListNotations.
From Stdlib Require Import Sorting.Permutation.
From Stdlib Require Import Program.Equality Logic.Eqdep_dec.
```
