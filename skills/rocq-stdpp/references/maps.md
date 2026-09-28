# stdpp Map Lemmas

Common `gmap`/finite-map facts.

## Insert

```coq
rewrite lookup_insert_eq.
rewrite lookup_insert_ne by congruence.
apply lookup_insert_Some in H.
destruct H as [[Heq Hval] | [Hneq Hlookup]].
```

Names to try:

- `lookup_insert`
- `lookup_insert_eq`
- `lookup_insert_ne`
- `lookup_insert_Some`
- `lookup_insert_None`
- `lookup_insert_rev`
- `insert_id`
- `insert_insert`
- `insert_insert_eq`
- `insert_insert_ne`

## Delete

```coq
rewrite lookup_delete_eq in H.
rewrite lookup_delete_ne in H by congruence.
apply lookup_delete_Some in H.
destruct H as [Hneq Hlookup].
```

Names to try:

- `lookup_delete`
- `lookup_delete_eq`
- `lookup_delete_ne`
- `lookup_delete_Some`
- `lookup_delete_None`
- `delete_id`
- `delete_insert`
- `delete_insert_eq`
- `delete_insert_ne`
- `insert_delete`
- `insert_delete_eq`
- `insert_delete_ne`

## Map Equality

For `m1 = m2`:

```coq
apply map_eq.
intro k.
```

Then solve lookup equality with insert/delete/union rewrites.

Names to try:

- `map_eq`
- `map_eq_iff`

## Union

Names to try:

- `lookup_union`
- `lookup_union_l`
- `lookup_union_r`
- `lookup_union_Some`
- `lookup_union_None`
- `insert_union_singleton_l`
- `insert_union_singleton_r`
- `delete_union`
