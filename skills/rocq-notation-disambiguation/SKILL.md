---
name: rocq-notation-disambiguation
description: >
  Facts about notation declarations, scopes, imports, and elaboration in
  Rocq/Coq.
---

# Rocq Notation Facts

- Notation declarations are resolved in the current parser and elaboration
  environment.
- Imports can add notation declarations and scope declarations.
- `Open Scope` changes the notation scope used by subsequent expressions.
- Surface syntax such as numerals, `+`, and list brackets can elaborate to
  different constants under different types or scopes.
- A search pattern and a theorem statement can therefore denote different
  constants under different preambles.
- The connected MCP `notations` query reports notation information for an
  expression in a selected proof or declaration context.
