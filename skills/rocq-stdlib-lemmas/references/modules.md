# Stdlib Module Hints

## Arithmetic

```coq
From Stdlib Require Import Lia PeanoNat ZArith.
```

Useful names often start with:

- `Nat.`
- `Z.`
- `N.`

## Lists

```coq
From Stdlib Require Import List.
Import ListNotations.
```

Search for:

- `In`
- `Forall`
- `Forall2`
- `map`
- `fold_left`
- `app`
- `length`

## Permutations

```coq
From Stdlib Require Import Sorting.Permutation.
```

Search for `Permutation`, `Permutation_app`, `Permutation_cons`, and
`Permutation_in`.

## Equality and Dependent Elimination

```coq
From Stdlib Require Import Program.Equality Logic.Eqdep_dec.
```

Use only when ordinary destruct/inversion/rewrite is insufficient.
