---
name: rocq-notation-disambiguation
description: >
  Use when a Rocq proof or search depends on notation/scope resolution, such as
  ambiguous arithmetic, list notation, numeric scopes, imported scopes, or
  notation queries in the connected Rocq MCP interface.
---

# Rocq Notation Disambiguation

Use this skill before interpreting a goal, statement, or search result whose
notations may resolve to different constants.

## When

- Arithmetic notation may be `nat`, `Z`, `Q`, `R`, or algebraic.
- A search pattern gives irrelevant results.
- A statement copied from a goal behaves differently under another preamble.
- `Open Scope` or imported modules may affect parsing.

## Method

- Inspect notations in the same context as the file/proof.
- Put imports and `Open Scope` commands in the preamble, not as isolated query
  body commands.
- Prefer file/live-state context over manually reconstructing imports.

## Source Basis

Extracted from:

- The connected `rocq-mcp` `query({kind:"notations", expression:...})` interface.
- `rocq-skills` search workflow notes.
