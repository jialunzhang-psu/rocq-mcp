# Rocq Goal-Focus Diagnostics

- `No such goal` can result when an earlier tactic has already closed the
  addressed branch or consumed more goals than expected.
- `Wrong bullet` indicates that the current bullet level does not match the
  open focused goal.
- Bullet levels conventionally use `-` at level 1, `+` at level 2, and `*` at
  level 3; braces represent an explicit local focus block.
- A tactic applied repeatedly to only the first branch indicates that the
  remaining branches are not currently focused.
