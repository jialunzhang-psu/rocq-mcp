# Dune and Rocq Load Path Patterns

## rocq.theory

Typical shape:

```lisp
(rocq.theory
 (name MyTheory)
 (theories Stdlib other_theory)
 (mode vo))
```

The `(name ...)` is the logical prefix used in `Require`.

## Generated Project Files

Dune may generate `_CoqProject` or `_RocqProject` containing flags such as:

```text
-Q path Logical
-R path Logical
```

Use these flags for `rocq top`, `rocq repl`, LSP, or MCP if tooling does not
derive them automatically.

## Import Failures

If:

```text
Cannot find a physical path bound to logical path ...
```

then inspect:

- theory name,
- generated project file,
- dependency theory list,
- whether the target file is in the Dune stanza,
- whether build artifacts are stale.

## Validation

After fixing load paths, run the project build once. Do not use build output as
the tactic search loop.
