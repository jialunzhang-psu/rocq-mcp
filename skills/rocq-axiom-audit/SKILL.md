---
name: rocq-axiom-audit
description: >
  Facts about Rocq assumptions, axioms, admitted declarations, and trust
  inspection.
---

# Rocq Assumption Facts

- `Print Assumptions theorem_name.` prints the logical assumptions on which a
  declaration depends.
- The connected `rocq-mcp` interface exposes the same focused operation through
  `query` with `kind: "assumptions"` and a structured declaration target.
- `Axiom`, `Parameter`, and `Conjecture` declarations introduce constants
  without proof bodies.
- `Admitted.` introduces a declaration whose proof is accepted as an
  assumption.
- `Abort.` cancels the declaration currently being constructed and does not
  introduce a global theorem.
- A successful file or project build does not imply that the resulting theorem
  has no axioms or admitted dependencies.
- If the connected `rocq-mcp` assumptions query prints `Axioms:` followed by
  declaration names, the target depends on each listed assumption. Successful
  expression type checking does not change that result.
- A `rocq-mcp` proof state with status `Completed` is not necessarily
  axiom-free: publication permits explicit `Axiom` declarations inside the
  selected Dune project, while rejecting admitted/unfinished project
  dependencies, out-of-project axioms, other kernel assumption kinds, and
  unsafe theory flags. Require Rocq's closed-under-the-global-context result
  for an axiom-free claim.
- The connected `rocq-mcp` interface has no operation named `rocq_verify`.
