# Rocq Typeclass Facts

An unresolved `_` can be inspected against an expected class:

```coq
Check (_ : SomeClass X).
Search SomeClass.
```

Local instances have the form:

```coq
#[local] Instance my_inst : SomeClass X := ...
```

Typeclass-search loops can result from implicit-argument ambiguity, recursive
instance paths, or broad global instances. A `setoid_rewrite` relation has an
`Equivalence` instance, and functions occurring under the relation have
`Proper` instances.
