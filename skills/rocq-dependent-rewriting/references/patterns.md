# Dependent Rewriting Facts

- `Abstracting over the term ... leads to a term which is ill-typed` is a
  diagnostic associated with an ill-typed dependent abstraction during
  equality elimination.
- `The term ... has type ... while it is expected to have type ...` can occur
  when a dependent hypothesis has not been transported along an equality.
- If `h : P x` and `Heq : x = y`, moving `h` out of the context before
  `rewrite Heq` permits `h` to be reintroduced at the transported type.

```coq
generalize dependent h.
rewrite Heq.
intro h.
```

- `subst x` removes `x` when the equality has the variable-shaped form `x = t`
  or `t = x` and `x` is not contained in `t`.
- `inversion H; subst` can refine indexed constructors but can also leave
  equality transports.
- `dependent destruction H` refines indices during dependent constructor
  elimination.
- `dependent induction` from `Program.Equality` supplies an induction
  principle for indexed derivations.
