---
name: rocq-axiom-audit
description: >
  Use when checking Rocq proof trust: Print Assumptions, custom axioms,
  Admitted/Abort leakage, sandboxed verification, or whether a theorem is closed
  under the global context. Atomic knowledge point extracted from LLM4Rocq
  rocq-mcp verification and rocq-skills axiom-check guidance.
---

# Rocq Axiom Audit

Use this skill when soundness/trust of completed proofs matters.

## Checks

- Use `Print Assumptions theorem_name.` to inspect dependencies.
- Use the connected MCP `query({kind:"assumptions", target:...})` for a
  focused dependency check. Independently compare the exact statement and
  inspect source for `Admitted`, `admit`, `Abort`, and custom axioms; this
  connected interface has no sandboxed `rocq_verify`.
- Treat explicit `Axiom`, `Parameter`, and `Conjecture` declarations as audit
  findings unless the user explicitly accepts them.

## Completion Standard

A proof is clean only when:

- the relevant scope has no `Admitted`/`admit`,
- no unexpected custom axioms appear,
- the checked theorem has the intended statement,
- file/project validation passes.

## Source Basis

Extracted from:

- `rocq-mcp/README.md` verification and assumptions sections.
- `rocq-skills` `check_axioms.sh`, `axiom-elimination.md`, and review guidance.
