# stdpp Set and dom Lemmas

## dom and Lookup

Move between domain membership and lookup:

```coq
apply elem_of_dom_2 in Hlookup.
rewrite elem_of_dom in H.
apply not_elem_of_dom in Hnotin.
```

Names to try:

- `elem_of_dom`
- `elem_of_dom_2`
- `not_elem_of_dom`
- `not_elem_of_dom_1`
- `not_elem_of_dom_2`
- `dom_empty`
- `dom_insert`
- `dom_delete`
- `dom_union`

## Set Algebra

For pure set goals:

```coq
set_solver.
```

If membership is hidden:

```coq
set_unfold.
```

Then use propositional reasoning or `set_solver`.

## Caution

`set_solver` solves set algebra, not project-specific semantic invariants. First
rewrite custom definitions to membership facts.
