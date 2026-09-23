# rocq-engine

`rocq-engine` owns proof semantics. `rocq-mcp` translates JSON and keeps a
connection's private selection; `trace-forest` stores immutable prefixes. The
engine owns PET processes, project layout, native verification, trust checks,
source publication, and recovery. No user-facing cursor or source path crosses
the MCP boundary.

## Proof state

A root contains an `OpenDeclaration`: structured logical identity, declaration
kind, statement, lexical context, and source anchor. Each edge contains one
canonical tactic. Several callers may hold branches of the same root. Equal
actions at one parent share an edge; no branch is a mutable theorem head.

Open, unsolved traces live only for the engine process, including its spill tier.
PET state is a disposable cache reconstructed by replaying a selected prefix.
A PET failure or eviction never triggers a replay of every trace. Inspection and
candidate evaluation use PET without appending; `check_multi` evaluates 1–20
one-sentence candidates from the same parent in input order.

A goals-clear branch wins the forest's per-root close arbitration. It is written
as a checksummed, fsynced `SolvedCandidate` under the project-owned
`.rocq-engine/<attached-scope>/proofs/` for Dune workspaces, or
`_build/.rocq-engine/proofs/` for non-Dune projects, before validation begins. This is the first
crash-durable proof state. No second branch can replace it. Native validation
and trust audit promote it atomically to `ClosedProof`, which includes an
ordered file-replacement plan. Recovery reads either complete phase. A rejected
candidate retains its tactics and diagnostic; it is not discarded.

## Logical placement

The effective `_CoqProject` or Dune layout maps each logical library to a
canonical `.v` file. Identity keeps the compilation unit separate from nested
Modules and Sections. Short-name lookup must be unique. An existing theorem's
proof body is replaced in place. A new declaration is inserted in its mapped
library and requested existing lexical context; a uniquely mapped new library
creates a `.v` file at close. Declare resolves placement but edits no source.
No fallback output file or caller-supplied filesystem path exists.

Theorem-family declarations close with `Qed`; `Definition` closes with
`Defined`. The terminator is not a user option.

## Commit and recovery

Close is the only source commit boundary. The engine serializes publication per
project, rereads the latest source, validates the declaration anchor, stages
source changes, checks the proof with native Rocq, audits assumptions, and
builds the affected project. It then publishes source/metadata replacements,
asks Dune to refresh its build tree (or compiles the non-Dune project), detaches all project PET runtimes, and acknowledges
the durable record. A successful solving call waits for this sequence. PET is
restarted lazily on the next operation; it never hot-reloads a changed `.vo`.

The target module and its affected dependency closure must build. Unrelated
modules already known to be broken may remain broken. Dune projects use their
workspace build context, including when attached below its root; otherwise the engine invokes native compilation. A content-keyed
baseline build distinguishes old unrelated failures from regressions caused by
this close.

Failed builds or publication leave the selected proof `Pending` and recoverable.
Automatic retry uses bounded backoff and wakes on relevant project changes.
Recovery is idempotent across candidate promotion, multi-file replacement,
final build, and acknowledgement. An already published proof is recognized
without duplicate insertion. A semantic target change cannot redirect a saved
proof to another declaration; it remains bound to the original identity. A
different external proof may satisfy that binding only after native build and
the saved candidate's trust audit pass. There is no public retry, reset, or
abandon operation.

## Trust

Each solved candidate freezes a content-addressed baseline. It allows only
unchanged explicit project `Axiom`, `Parameter`, or `Conjecture` declarations
and external assumptions backed by the same compiled artifact, load path, and
toolchain identity. Existing `Admitted`, aborted, or open local proofs, including
the target's old admission, are never authorized. Native `Print Assumptions`
provides the transitive roots after `Qed` or `Defined`. Unresolved or new roots
fail closed. A deterministic policy rejection is a `Rejected` theorem with the
saved candidate and a typed diagnostic, not a publication conflict.

## Concurrency and errors

One engine process holds an exclusive lifetime lock per attached project. PET
operations share a project read gate; close holds its write gate and invalidates
PET before releasing it. Independent projects and PET lanes can run in parallel
up to `max_pet_processes`. No global lock covers native work.

The public error vocabulary is `ErrorKind` in `api.rs`; the MCP adapter maps it
one-to-one. Internal races, PET death, and publication recovery do not become
new user-facing error kinds. External processes have bounded I/O, deadlines,
and owned process cleanup. The engine does not claim crash durability for open
unsolved traces, nor does it intercept external deletion of build artifacts. Dune
durable proof records live outside Dune's cleanable build directory.
