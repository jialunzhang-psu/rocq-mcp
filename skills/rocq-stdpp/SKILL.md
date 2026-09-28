---
name: rocq-stdpp
description: >
  Use when a Rocq/Coq proof involves the stdpp library: gmap, gset, fin_sets,
  lookup notation `m !! k`, insert notation `<[k:=v]> m`, delete, dom,
  elem_of, map equality, set_solver, set_unfold, or stdpp list/set/map lemmas.
  Atomic, composable knowledge-point skill; does not prescribe the proof
  workflow.
---

# Rocq stdpp

This is a knowledge-point skill for `stdpp` maps and sets. Compose it with
`rocq-repl-workflow` for interactive proof development.

## When To Use

- Goals or hypotheses contain `m !! k`.
- Goals or hypotheses contain `<[k:=v]> m`, `delete k m`, `dom m`, or `∈`.
- A proof needs `lookup_insert_*`, `lookup_delete_*`, `elem_of_dom`, or
  `set_solver`.
- A map equality should be reduced to pointwise lookup equality.

## Core Patterns

- Map equality: `apply map_eq; intro k.`
- Insert split: `apply lookup_insert_Some in H`.
- Insert same key: `rewrite lookup_insert_eq`.
- Insert different key: `rewrite lookup_insert_ne by congruence`.
- Delete same key: `rewrite lookup_delete_eq`.
- Delete different key: `rewrite lookup_delete_ne by congruence`.
- Domain from lookup: `apply elem_of_dom_2 in Hlookup`.
- Lookup from not-in-domain: `apply not_elem_of_dom in Hnotin`.
- Pure set algebra: `set_solver`, after `set_unfold` if necessary.

Read [references/maps.md](references/maps.md) for common map lemmas.
Read [references/sets.md](references/sets.md) for common set/domain lemmas.
