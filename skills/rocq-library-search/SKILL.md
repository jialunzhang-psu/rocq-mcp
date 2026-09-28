---
name: rocq-library-search
description: >
  Facts about Rocq declaration search and inspection commands, local contexts,
  and the connected MCP query forms.
---

# Rocq Search Facts

- `Search pattern.` returns declarations whose types match a pattern in the
  current environment.
- `Check name.` prints the type of a declaration or expression.
- `Print name.` prints the body or proof term of a declaration.
- `About name.` prints declaration metadata such as arguments and implicit
  parameters.
- Name-fragment search accepts string patterns such as `Search "add" "comm".`
- Search results depend on the imported environment, opened scopes, and local
  hypotheses.
- The connected `rocq-mcp` query interface exposes corresponding `search`,
  `type`, `print`, and `about` operations.
