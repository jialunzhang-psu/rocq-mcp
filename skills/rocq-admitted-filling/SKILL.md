---
name: rocq-admitted-filling
description: >
  Use when the task is to find, triage, or fill Rocq/Coq Admitted/admit holes.
  Atomic knowledge point covering hole inventory, header fence, candidate
  generation, stuck handoff, and validation. Extracted from LLM4Rocq
  rocq-skills admitted-filling and cycle-engine references.
---

# Rocq Admitted Filling

Use this skill only for `Admitted.` or local `admit.` work. Compose it with
`rocq-repl-workflow`.

## Rules

- Start by seeing the goal interactively.
- Do not change theorem statements or declaration headers while filling a hole.
- Search before proving from scratch.
- Generate 2-3 distinct candidates, test them, then commit the winner.
- If the statement looks false or too weak, stop and report a redraft need.

## Stuck Signal

Mark a hole stuck when:

- the same failure repeats after different strategies,
- search gives no useful premise,
- a helper lemma is missing,
- proof needs statement generalization,
- proof needs multi-file refactoring beyond the current scope.

## Handoff Record

When stuck, record:

- target declaration and location,
- current goal and key hypotheses,
- attempted tactics,
- search queries/results,
- proposed helper or redraft.

## Source Basis

Extracted from `rocq-skills` references:

- `admitted-filling.md`
- `cycle-engine.md`
- `agents/admitted-filler-deep.md`
