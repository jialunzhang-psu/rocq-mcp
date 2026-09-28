---
name: rocq-stdpp
description: >
  Facts about stdpp maps, sets, finite sets, lookup, insertion, deletion,
  domains, and set automation.
---

# stdpp Map and Set Facts

- `m !! k` denotes a stdpp map lookup.
- `<[k:=v]> m` denotes insertion or replacement at key `k`.
- `delete k m` denotes deletion of `k` from a map.
- `dom m` denotes the domain of a map.
- `map_eq` changes map equality into equality of lookups at each key when
  applied to a map equality goal.
- `lookup_insert_eq` describes lookup at the inserted key.
- `lookup_insert_ne` describes lookup at a key different from the inserted key.
- `lookup_delete_eq` and `lookup_delete_ne` describe lookup after deletion.
- `lookup_insert_Some` decomposes a successful lookup in an inserted map.
- `elem_of_dom_2` connects a successful lookup with domain membership.
- `not_elem_of_dom` connects absence from a domain with lookup failure.
- `set_unfold` exposes set membership expressions.
- `set_solver` solves supported pure set-algebra goals.

```coq
apply map_eq; intro k.
rewrite lookup_insert_eq.
rewrite lookup_insert_ne by congruence.
rewrite lookup_delete_eq.
rewrite lookup_delete_ne by congruence.
```
