---
name: rocq-library-search
description: >
  Use when a Rocq proof needs existing lemmas, imports, Search/Check/Print/About,
  premise discovery, or local/library lemma lookup. Atomic knowledge point
  extracted from LLM4Rocq rocq-skills search guidance and rocq-mcp query API.
---

# Rocq Library Search

Use this skill when the next proof step may already exist as a local or library
lemma.

## Query Forms

- Pattern search: `Search (_ + _ = _ + _).`
- Type-shape search: `Search (_ -> _ -> _).`
- Name-fragment search: `Search "add" "comm".`
- Inspect a candidate: `Check name.`
- Inspect definition/body: `Print name.`
- Summary and arguments: `About name.`

## Search Discipline

- Search local context/project declarations before broad library search.
- Query from the live proof state when possible, so local hypotheses, opened
  scopes, and imports are visible.
- If notation may be ambiguous, use `rocq-notation-disambiguation` before
  interpreting results.
- Add imports only after confirming the lemma/module is actually needed.

## Source Basis

Extracted from:

- `rocq-skills/.../references/admitted-filling.md`
- `rocq-skills/.../references/coq-stdlib-guide.md`
- The connected `rocq-mcp` `query` interface.
