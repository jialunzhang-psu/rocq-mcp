---
name: rocq-stdlib-lemmas
description: >
  Use when a Rocq/Coq proof needs standard-library facts or imports about nat,
  Z, N, bool, option, lists, strings, relations, permutations, equality,
  arithmetic decision procedures, or common Stdlib module names. Atomic,
  composable knowledge-point skill.
---

# Rocq Stdlib Lemmas

Use this skill to decide where to search in the standard library. Compose with
`rocq-library-search` for actual queries.

## Common Modules

- Natural numbers: `PeanoNat`, `Arith`, `Lia`.
- Integers: `ZArith`, `Lia`.
- Booleans: `Bool`.
- Lists: `List`, `ListNotations`.
- Permutations: `Sorting.Permutation`.
- Equality: `Logic.Eqdep_dec`, `Logic.JMeq`, `Program.Equality`.
- Relations: `Relations`, `RelationClasses`, `Morphisms`.
- Strings: `String`.

## Search Direction

- For arithmetic inequalities/equalities, import/search `Lia`, `PeanoNat`, or
  `ZArith`.
- For list membership/length/map/app, search `List`.
- For permutation-preserving facts, search `Sorting.Permutation`.
- For dependent equality/inversion support, search `Program.Equality` and
  `Logic.Eqdep_dec`.

Read [references/modules.md](references/modules.md) for concrete import hints.
