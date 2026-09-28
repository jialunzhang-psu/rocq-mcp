# Typeclass Debugging Patterns

## Missing Instance

If `_` cannot be inferred, inspect the expected class:

```coq
Check (_ : SomeClass X).
```

Then search:

```coq
Search SomeClass.
```

## Local vs Global Instances

Prefer local instances inside proof files unless the instance is part of the
public interface:

```coq
#[local] Instance ...
```

Use exported/global instances only intentionally.

## Resolution Loops

If typeclass search times out:

- make implicit arguments explicit,
- provide the instance manually,
- lower the search depth,
- avoid broad global instances.

## Setoid Rewriting

For `setoid_rewrite`, check that the relation has:

- `Equivalence`,
- `Proper` instances for functions under the rewrite.
