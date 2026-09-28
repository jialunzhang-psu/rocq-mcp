---
name: rocq-subgoal-focusing
description: >
  Use when Rocq/Coq proofs create multiple subgoals, bullets/braces are needed,
  or errors like "No such goal", "Wrong bullet", unfocused goals, or tactics
  applying to the wrong branch appear. Atomic, composable knowledge-point skill.
---

# Rocq Subgoal Focusing

Use this skill for goal focus and bullet discipline. Compose with
`rocq-repl-workflow`.

## Rules

- As soon as a tactic creates multiple goals, use bullets or braces.
- Do not mix unfocused tactics with bullets at the same level.
- Use `-`, `+`, `*` for nested bullet levels.
- Use `{ ... }` for local focus when it is clearer than bullet nesting.

## Pattern

```coq
destruct H as [H1 | H2].
- ...
- ...
```

Nested:

```coq
split.
- ...
- destruct H.
  + ...
  + ...
```

Read [references/errors.md](references/errors.md) for common focus errors.
