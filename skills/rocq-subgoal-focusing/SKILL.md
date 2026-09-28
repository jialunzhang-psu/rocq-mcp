---
name: rocq-subgoal-focusing
description: >
  Facts about Rocq goal focus, bullets, braces, and multiple-goal diagnostics.
---

# Rocq Goal-Focus Facts

- A tactic can create multiple goals, and Rocq tracks their focus and nesting.
- `-`, `+`, and `*` are bullet tokens for successive focus levels.
- `{ ... }` creates a local focus block.
- `No such goal` occurs when a tactic or bullet addresses a goal that is no
  longer open.
- `Wrong bullet` occurs when a bullet does not match the current focus nesting.

```coq
destruct H as [H1 | H2].
- ...
- ...
```

```coq
split.
- ...
- destruct H.
  + ...
  + ...
```
