# Inversion and Discrimination Patterns

## discriminate

Use when the context contains impossible equality between distinct constructors:

```coq
H : Some x = None
```

or:

```coq
H : S n = 0
```

## injection

Use when equal constructors imply equal arguments:

```coq
H : Some x = Some y
injection H as Hxy.
```

## inversion

Use on evidence from an inductive proposition to expose which constructor
created it:

```coq
inversion H; subst; clear H.
```

Avoid destructing/inverting too aggressively if it destroys useful hypotheses or
creates dependent transports. In that case use `rocq-dependent-rewriting`.

## constructor

When the goal is an inductive proposition and the constructor is clear:

```coq
constructor.
```

If multiple constructors apply, use explicit constructor names for readability.
