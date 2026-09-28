# stdpp Map Facts

Common insertion declarations include:

```text
lookup_insert
lookup_insert_eq
lookup_insert_ne
lookup_insert_Some
lookup_insert_None
lookup_insert_rev
insert_id
insert_insert
insert_insert_eq
insert_insert_ne
```

Common deletion declarations include:

```text
lookup_delete
lookup_delete_eq
lookup_delete_ne
lookup_delete_Some
lookup_delete_None
delete_id
delete_insert
delete_insert_eq
delete_insert_ne
insert_delete
insert_delete_eq
insert_delete_ne
```

Map extensionality declarations include `map_eq` and `map_eq_iff`.

Union lookup declarations include `lookup_union`, `lookup_union_l`,
`lookup_union_r`, `lookup_union_Some`, and `lookup_union_None`.
