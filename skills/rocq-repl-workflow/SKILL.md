---
name: rocq-repl-workflow
description: >
  Use for Rocq/Coq tasks that open proof goals or design invariants. Use MCP
  for interactive goal inspection and tactic experiments. For delegated
  proofs, assign one temporary admitted theorem per worker. Source review and post-write
  validation may use the repository's build tools.
# Disabled Gatekeeper requirements from the original description:
# Test candidate invariants with a Gatekeeper record before building the lemma
# dependency graph. Preserve every rejected invariant verbatim as uncompiled
# text in FailedInvariants. Only a candidate that passed the previous Gatekeeper
# suite and was later refuted also receives a self-contained spec-only
# FailureHistory and a new Gatekeeper.
---

# Rocq REPL Workflow

Use this as the coordinating workflow. Load a focused Rocq skill only when its
problem class appears.

Gate-related instructions retained in comments are inactive; they impose no
prerequisite on proof search or delegation.

## Interactive Proof Search

1. Read the complete theorem statement and the definitions it uses. Identify
   the likely induction, recursion, case split, invariant, and helper lemmas.
2. Through MCP, execute one small command, read every resulting goal, and then
   choose the next command. Preserve the focus structure expressed by bullets
   and braces.
3. Test the subgoal most likely to invalidate the plan first: one with a weak
   induction hypothesis, dependent indices, nontrivial side conditions,
   impossible equalities, or a missing helper.
4. If that subgoal exposes a false statement, inadequate invariant, wrong
   induction variable, or missing lemma, revise the global plan before closing
   sibling goals. Do not edit a fixed specification without authorization.
5. Close routine goals after the difficult cases have working tactic sequences
   or stated helper lemmas.
6. After each accepted `Qed` or `Defined`, follow the immediate persistence
   rule below before starting another lemma.

## Source Persistence

The wrapper owns the close transaction: a PET-complete proof is persisted as a
durable candidate, audited with PET assumptions, and published through the
Dune-selected source target only after native validation. A successful `check`
therefore may update the authorized `.v` file; inspect the resulting source and
diff before beginning another lemma. Never write a proof merely because a
source terminator is present or because a local status cache says `Completed`.

<!-- Gatekeeper workflow disabled by user request; original text retained below.

## Invariant Gatekeepers

If the gate is not ready in the current project, you should first set it up
following the instructions below.

Use Gatekeepers to reject an inadequate invariant before building the lemma
dependency graph. Schematically, every Gatekeeper has type:

```rocq
G : invariant_candidate -> Prop
```

A candidate `Inv` passes Gatekeeper `G` when Rocq proves `G Inv`. A
Gatekeeper's logical shape does not create a subcategory.

Keep these three classifications:

```text
Gate/
├── Gatekeeper/
├── FailedInvariants/
└── FailureHistory/
```

- **Gatekeeper:** a reusable invariant requirement generalized from a closed
  counterexample to a candidate that had passed every older Gatekeeper. It is
  parameterized by `Inv` and must be proved for every new candidate.
- **FailedInvariants:** the uncompiled textual ledger of every rejected
  invariant candidate, preserved verbatim whether or not it passed the older
  Gatekeepers.
- **FailureHistory:** the exact, compiled, spec-only closed counterexample to a
  named rejected candidate that had first passed every older Gatekeeper.

### FailedInvariants

As soon as an invariant candidate is formally rejected, save its exact source
unchanged under `FailedInvariants/<name>`. This applies both when an existing
Gatekeeper rejects it and when a later proof attempt produces a closed
counterexample. Do not repair, rename, simplify, or retrospectively strengthen
the saved candidate.

Store Rocq source snapshots as text, for example `Candidate.v.txt`, so they are
not compiled by Dune or Rocq. A separate README or manifest may record the
candidate's name, provenance, rejection reason, and relevant Gatekeeper, but
the verbatim snapshot itself must remain unchanged. Never import a
FailedInvariants artifact into a candidate, Gatekeeper, FailureHistory, or
proof. It has no compilation, dependency-closure, or trust-audit requirement.

Recording a candidate in FailedInvariants does not by itself justify a new
Gatekeeper. If the candidate fails any already-registered Gatekeeper, stop
after the textual record: do not create a duplicate FailureHistory and do not
add a Gatekeeper for that failure.

### Dependency boundary

Keep the entire transitive dependency closure of Gatekeepers and
`FailureHistory/<name>` artifacts `spec-only`: import only language, machine,
typing, and soundness specification modules plus base libraries. Do not import
`paper.Proof`, the current proof candidate, a `Proof` helper, or a textual
FailedInvariants snapshot. Audit transitive imports rather than directory
names; the physical `Core/Gate` directory is only a layout and does not
establish this dependency category. Keep the dependency direction one-way:
the candidate may import the gate library, but the gate library and its
history must not import the candidate.

Register every formal Gatekeeper as a field of one project record:

```rocq
Record Gatekeeper (Inv : invariant_candidate) : Prop := {
  gate_name : G_name Inv
  (* one field per registered Gatekeeper *)
}.
```

FailureHistory is not a field. Submit a candidate through this interface:

```rocq
Module Type GatekeeperSubmission.
  Parameter Inv : invariant_candidate.
  Parameter passes : Gatekeeper Inv.
End GatekeeperSubmission.
```

A concrete submission defines `Inv` and proves `passes`. The project should provide:

```sh
dune build @gatekeeper
```

The alias builds and trust-audits the submission, failing if either step fails.
Use only this alias for normal acceptance; inspect individual fields only after
it fails.

Keep positive acceptance separate from negative history replay. The positive
`@gatekeeper` alias must build and trust-audit only the current candidate
submission. Give each historical submission a distinct expected-failure
target; do not include it in the positive alias or treat an expected failure
as a successful submission.

### Rejecting a Candidate and Deriving a Gatekeeper

Always test a candidate against the complete currently registered Gatekeeper
record before building its lemma dependency graph.

If an existing Gatekeeper rejects the candidate:

1. Preserve the candidate verbatim in FailedInvariants.
2. Record which existing field rejected it in the accompanying manifest.
3. Stop. Do not create FailureHistory and do not add a new Gatekeeper.

Create a FailureHistory and derive a new Gatekeeper only after the candidate
has passed every previously registered Gatekeeper and a subsequent actual
proof attempt produces a closed counterexample. The empty initial Gatekeeper
suite counts as passed vacuously. In that case:

1. Preserve the rejected candidate verbatim in FailedInvariants.
2. Prove the concrete counterexample and save it unchanged in FailureHistory.
3. Identify the missing invariant requirement responsible for that failure.
4. Generalize that requirement into `G : invariant_candidate -> Prop`, removing
   details specific to the concrete witness if necessary.
5. Prove that the rejected candidate does not satisfy `G`.
6. Add `G Inv` as a field of `Gatekeeper`. Every later candidate must prove it.

Do not derive a Gatekeeper or create FailureHistory from a candidate that
already fails an existing Gatekeeper. Likewise, do not derive either artifact
from a stuck proof, timeout, missing helper, or tool failure; none of these
establishes that the invariant is wrong. If such an inconclusive candidate is
later formally rejected for a proved reason, record it in FailedInvariants at
that point.

When the counterexample depends on the existence of a state or transition,
retain that requirement with existential quantification and conjunction.
Package the witness with its typing, well-formedness, execution, step, `Inv`,
observation, and effect claims. An implication with a false premise would fail
to preserve the counterexample's non-vacuity.

### FailureHistory

FailureHistory is reserved for a candidate that passed the complete older
Gatekeeper suite and was later refuted. Make each `FailureHistory/<name>`
self-contained. Freeze the rejected candidate under a stable, unique old-`Inv`
name and bundle all of the following in that artifact:

- the exact closed witness;
- closed proofs of every necessary side condition, including typing,
  well-formedness, reachability, each operational step, enabled effect,
  observation, and effect claims as applicable;
- a refutation of that frozen `Inv` (or of the derived required observation);
- the historical `GatekeeperSubmission` used to replay that old candidate.

Do not import the current candidate or any `paper.Proof` helper into this
closure, and do not hide a side condition behind an assumed premise. Close
the witness and refutation with `Qed` or `Defined`, then compile the artifact
on its own and run an assumption audit with no admission or custom axiom.

After deriving and registering `G` from the refutation, replay the bundled
historical submission against the current Gatekeeper only in its
expected-failure regression. Require the failure to identify the registered
field derived from this counterexample; reject failures caused by imports,
build wiring, timeouts, or other incidental errors. Keep this negative replay
distinct from the positive current-candidate `@gatekeeper` alias.

-->

## Lemma Dependency Graph and Delegation

<!-- Disabled Gatekeeper prerequisite:
Start this stage only after every Gatekeeper passes and every remaining proof
step is represented by a stated lemma.
-->

Start this stage once every remaining proof step is represented by a stated
lemma.

The dependency graph has one lemma at each node. An edge from one lemma to
another means that the first lemma uses the second.

1. List the required lemmas and their dependencies. Give each lemma its own
   file and proof task. The coordinating agent fixes its imports and statement,
   leaves only that proof temporarily `Admitted`, wires the top-level theorem
   through the lemmas, and compiles the files to check all imports, statements,
   and dependencies before delegation.
2. Give each worker exclusive ownership of one file and one theorem.
   Forbid changes to fixed specifications, assigned statements, and files owned
   by other workers. If a reusable helper is missing, add it as a separate
   dependency instead of silently enlarging the assigned task.
3. Schedule a lemma only after its dependencies have been accepted. Within each
   independent group, first test the lemma most likely to expose a false
   statement or inadequate invariant. Use explorers only for bounded read-only
   questions. Match worker effort to the proof: routine proofs need low or
   medium effort, difficult proofs need high effort, and dependent or
   core-invariant proofs may need max effort.
4. Require every proof worker to follow this workflow. While the proof is open,
   the worker must use MCP for goal inspection and tactic search. The worker
   replaces only the assigned `Admitted`, then compiles the written file and
   audits the theorem's assumptions. The report includes the selected theorem, the initial goals, and the final
   MCP result showing no remaining goals.
5. If the statement is false, too weak, or lacks an essential premise, stop
   that task and return the concrete failing case. The coordinating agent must
   revisit the <!-- Gatekeeper record, --> dependency graph or statement before
   assigning the proof again.
6. Accept a worker result only after reviewing its diff, confirming that its
   assigned file has no admission, compiling that file, running the repository
   target used to validate the top-level theorem, and auditing the assigned
   theorem. An imported temporary admission prevents every dependent theorem
   from passing the trust audit.

## Tool Boundary

Use the connected `mcp__rocq_mcp__*` server for live goals and tactic
experiments: `start` attaches to an unambiguous project, `prove` selects a
structured declaration returned by `list_decls`, `query` inspects goals or
declarations, `try` tests alternatives without advancing, and `check` commits
the first fully accepted alternative. Use `declare` only when adding a new
theorem is authorized. Consult `rocq-mcp-tools` for current signatures.
Do not use the separate `mcp__codex_apps__rocq_*` plugin as evidence that this
connected server is unavailable.

If MCP fails, report the failing operation and exact error. Do not replace it
with a terminal REPL or direct language-server calls for live goal inspection.
Source reading remains independent of MCP. After writing a completed proof to
its `.v` file, run the repository's normal build and audit assumptions when
needed. Post-write compilation is validation, not interactive tactic search.

## Focused Skill Dispatch

Load only the focused skills needed by the current problem:

- Tools and project setup: `rocq-mcp-tools`, `rocq-dune-loadpath`,
  `rocq-spec-review`.
- Search and libraries: `rocq-library-search`, `rocq-stdlib-lemmas`,
  `rocq-stdpp`, `rocq-notation-disambiguation`.
- Proof structure: `rocq-induction-generalization`,
  `rocq-dependent-rewriting`, `rocq-inversion-discrimination`,
  `rocq-subgoal-focusing`.
- Definitions and errors: `rocq-typeclasses`, `rocq-recursion-termination`,
  `rocq-compile-error-triage`.
- Holes and trust: `rocq-admitted-filling`, `rocq-axiom-audit`.
