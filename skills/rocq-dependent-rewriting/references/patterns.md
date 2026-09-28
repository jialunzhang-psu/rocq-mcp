# Dependent Rewriting Patterns

## Failure Signature

Typical errors:

```text
Abstracting over the term ... leads to a term which is ill-typed
```

```text
The term ... has type ... while it is expected to have type ...
```

These usually mean the rewritten term appears in a dependent hypothesis or
indexed type.

## Generalize First

If `h : P x` and `Heq : x = y`, avoid rewriting while `h` is still in context:

```coq
generalize dependent h.
rewrite Heq.
intro h.
```

For multiple hypotheses:

```coq
revert h1 h2 h3.
rewrite Heq.
intros h1 h2 h3.
```

## subst When Possible

When the equality is variable-shaped:

```coq
Heq : x = t
```

or:

```coq
Heq : t = x
```

and `x` does not occur in `t`, prefer:

```coq
subst x.
```

This removes the variable and avoids many motive problems.

## Generalize the Statement

If rewriting keeps failing because the theorem is too specific, prove a more
general intermediate statement and instantiate it:

```coq
suffices H : forall x, P x -> Q x.
{ exact (H target proof_of_P). }
intros x HP.
```

## Indexed Relations

For hypotheses from indexed inductive relations:

```coq
inversion H; subst.
```

may work, but can leave hard transports or erase useful equations. If the index
refinement is central, try:

```coq
dependent destruction H.
```

or use dependent induction from `Program.Equality` when induction over an
indexed derivation is required.
