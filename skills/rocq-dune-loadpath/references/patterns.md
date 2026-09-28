# Dune and Rocq Load-Path Facts

A typical theory stanza has the form:

```lisp
(rocq.theory
 (name MyTheory)
 (theories Stdlib other_theory)
 (mode vo))
```

The value of `(name ...)` is the logical prefix used by `Require` commands.

Dune-generated `_CoqProject` or `_RocqProject` files can contain mappings such
as:

```text
-Q path Logical
-R path Logical
```

The diagnostic

```text
Cannot find a physical path bound to logical path ...
```

has possible sources in the theory name, generated project file, theory
dependency list, Dune source selection, or stale build metadata.
