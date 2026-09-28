# Subgoal Focus Errors

## `No such goal`

Usually means a tactic solved more goals than expected or a bullet branch is
already closed. Inspect the current goals before adding more tactics.

## `Wrong bullet`

Usually means the current bullet level does not match the open focused goal.
Check nesting:

- level 1: `-`
- level 2: `+`
- level 3: `*`

For deeper nesting, prefer braces:

```coq
{ ... }
```

## Hidden Branches

If a proof starts solving only the first branch repeatedly, add bullets
immediately after the branching tactic.

## Cleanup

After a proof works, keep bullets if they document semantically distinct cases.
Do not collapse branches into one-liners when it obscures the case split.
