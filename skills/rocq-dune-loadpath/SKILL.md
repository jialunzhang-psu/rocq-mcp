---
name: rocq-dune-loadpath
description: >
  Facts about Dune Rocq theories, logical load paths, project files, and
  physical-to-logical module mappings.
---

# Dune and Rocq Load-Path Facts

- A Dune `rocq.theory` stanza declares a logical theory name and its theory
  dependencies.
- `_CoqProject` and `_RocqProject` files can contain Rocq load-path flags for
  editor, REPL, LSP, and MCP processes.
- `-Q physical Logical` establishes a physical-directory to logical-prefix
  mapping.
- `-R physical Logical` establishes a recursive physical-directory to
  logical-prefix mapping.
- `-I physical` adds a physical directory to the load-path search set.
- `From X Require Import Y.` resolves the logical module path `X.Y`.
- `Cannot find a physical path bound to logical path ...` indicates that the
  current load-path metadata has no usable physical mapping for the requested
  logical path, or that the corresponding project source/build information is
  unavailable.
