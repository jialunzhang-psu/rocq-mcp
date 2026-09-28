---
name: rocq-recursion-termination
description: >
  Facts about Rocq structural recursion, guard checking, well-founded
  recursion, fuel parameters, and coinductive productivity.
---

# Rocq Recursion Facts

- `Fixpoint` definitions are accepted only when recursive calls satisfy the
  structural guard condition or an explicitly supported well-founded scheme.
- A structural recursive call is made on a syntactic subterm of the selected
  decreasing argument.
- `{struct x}` identifies the structural argument used by the guard checker.
- A fuel argument changes a recursion driven by an external measure into a
  recursion structurally decreasing on the fuel.
- Well-founded recursion requires a well-founded relation and proofs that
  recursive calls decrease in that relation.
- `Program Fixpoint` can generate obligations for measure-based recursion.
- `CoFixpoint` definitions are checked for guardedness/productivity rather than
  ordinary structural termination.
