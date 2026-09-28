# Rocq Stdlib Module Facts

Arithmetic imports include:

```coq
From Stdlib Require Import Lia PeanoNat ZArith.
```

Common namespaces include `Nat.`, `Z.`, and `N.`.

List imports include:

```coq
From Stdlib Require Import List.
Import ListNotations.
```

List declarations include `In`, `Forall`, `Forall2`, `map`, `fold_left`,
`app`, and `length`.

Permutation imports include:

```coq
From Stdlib Require Import Sorting.Permutation.
```

Declarations include `Permutation`, `Permutation_app`, `Permutation_cons`, and
`Permutation_in`.

Dependent equality imports include:

```coq
From Stdlib Require Import Program.Equality Logic.Eqdep_dec.
```
