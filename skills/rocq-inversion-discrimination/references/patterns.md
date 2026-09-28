# Constructor Reasoning Facts

```coq
H : Some x = None
```

is contradictory because `Some` and `None` are distinct constructors, so
`discriminate` can close the contradiction.

```coq
H : Some x = Some y
```

implies `x = y`, so `injection H` can expose that equality.

`inversion H` decomposes evidence from an inductive proposition according to
the constructors that can produce it. For indexed propositions, the resulting
index equalities can require dependent transport.

```coq
constructor.
```

applies a constructor of the inductive proposition at the current goal.
