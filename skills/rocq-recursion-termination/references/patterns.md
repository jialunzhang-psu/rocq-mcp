# Rocq Recursion Facts

- Reordering function arguments can make a structural decreasing argument
  syntactically visible to the guard checker.
- `{struct x}` selects `x` as the structural argument when the recursive calls
  are structurally decreasing in `x`.
- A fuel parameter supplies a natural-number structural measure for executable
  checkers.
- Well-founded recursion uses a decrease proof for a relation rather than a
  constructor subterm.
- A relation or induction lemma can express the recursive property without
  introducing a corresponding executable `Fixpoint`.
