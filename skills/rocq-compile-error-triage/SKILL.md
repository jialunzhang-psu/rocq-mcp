---
name: rocq-compile-error-triage
description: >
  Facts about Rocq parser, kernel, tactic, universe, name-resolution,
  termination, and proof-closing diagnostics.
---

# Rocq Diagnostic Facts

- A syntax or parse error occurs before kernel type checking of the affected
  command.
- `The term ... has type A while it is expected to have type B` reports a
  mismatch between an inferred type and an expected type.
- `Unable to unify` reports a failure of the unification constraints generated
  by the current terms and goal.
- `reference ... not found` reports unsuccessful name resolution; missing
  imports, an incorrect logical path, a spelling error, or a missing scope can
  produce this diagnostic.
- `No such goal` reports a tactic invocation when the addressed goal has
  already been closed or is not the current focused goal.
- `Wrong bullet` reports a mismatch between a bullet and the current focused
  goal nesting.
- `Universe inconsistency` reports an unsatisfiable set of universe
  constraints.
- `Cannot guess decreasing argument` reports a failure of recursive-definition
  termination or guard checking.
- An error reported at `Qed` or `Defined` can result from goals left open by
  earlier proof commands.
