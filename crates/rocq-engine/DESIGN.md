# rocq-engine lazy PET-wrapper redesign

This note records the agreed target architecture. It is a design contract,
not a claim that the migration is complete.

## Responsibilities

The engine has four narrow responsibilities:

* **Dune** supplies the canonical workspace, selected source files, logical
  libraries, load paths, and build targets. It is not a declaration database.
* **PET** supplies Rocq semantics: loading a source document, resolving its
  `Require` dependencies (normally through existing `.vo` files), declaration
  and AST information, proof goals, queries, and proof-step results. The wrapper may
  kill a PET lane that stops answering its JSON-RPC pipe; that watchdog is a
  process-safety boundary, not a deadline for a valid Dune build or proof.
* **TraceForest** stores only proof attempts and their branching tactic traces.
  It is not a project catalogue and does not infer Rocq meaning.
* **Writeback** uses a PET source range plus a compare-and-swap source digest
  to publish one selected declaration. It does not discover declarations by
  scanning source text.

## Lazy discovery

Attaching a project must not ask PET to inspect every source file. The old
workspace-wide declaration refresh is intentionally removed from the normal
startup path.

The discovery flow is:

```text
start(project)
  -> attach Dune workspace and PET runtime
list_files()
  -> return Dune-selected workspace-relative source paths
list_decls(file)
  -> ask PET for document declarations from exactly that file
prove(declaration id)
  -> ask PET to open exactly that source/declaration
```

Opening one file causes PET/Fleche to resolve its direct and transitive
`Require` dependencies. Existing `.vo` files are used when available. Files
that are not imported by the target are not eagerly loaded merely because they
belong to the same Dune theory.

PET must run at the source-mapping root reported by the file's Dune Rocq rule.
When the theory exposes Dune's `(generate_project_file)` target, the Dune
wrapper builds that reported target before PET starts and PET consumes the
resulting `_RocqProject`. The wrapper does not synthesize `-Q`/`-R` flags,
guess a theory directory, or assume that the server's current working
directory is the project. A project without a Dune-generated project target
may use PET's native root behavior, but the wrapper must not claim that missing
dependency load paths were successfully loaded.

## Stable identities

### FileId

The external file identity is a normalized path relative to the canonical Dune
workspace root, for example:

```text
Library/Holes/pt_generated_consistent_preserved.v
```

Absolute paths and `_build` paths are internal implementation details. Every
file supplied by a caller is canonicalized and checked against Dune's selected
source set before PET is invoked.

### DeclarationId

The authoritative declaration key is:

```text
DeclarationId {
    file: FileId,
    qualified_path: Vec<String>,
}
```

For `NLLProof.A.B.foo`, `qualified_path` is
`["NLLProof", "A", "B", "foo"]`. The pinned PET exposes
`petanque/document_declarations`, which traverses Flèche's checked document
once and returns an ordered record for every supported declaration: its full
module-relative path, byte range, kind, and source statement. An ordered list
preserves same-leaf declarations such as `A.foo` and `B.foo`; the wrapper does
not reconstruct scopes, split the file into sentences, or issue per-sentence
AST requests. It only prefixes PET's path with the logical library reported by
Dune and stores that complete path once in `DeclarationId`.

`modules` and `constant` are derived views used only at API boundaries that
need to print or construct a Rocq command. They are not separate sources of
truth. A source range is also not identity: edits can move line and byte
offsets without changing the declaration.

The file component remains part of the identity even when the qualified name
is unique in a current environment. It permits lazy reopening and distinguishes
same-named declarations in different source documents.

## State ownership

There is no authoritative workspace-wide declaration catalogue. Runtime state
is retained only for declarations actually touched by the connection:

```text
ProjectRuntime
  └── touched DeclarationId -> proof attempt / trace root
```

PET remains the authority for whether a declaration exists, its statement,
kind, goals, proof term, and assumptions. The trace forest is the authority for
the user's accepted tactic prefixes and branches. A PET process may be evicted
and recreated; the trace is replayed into a fresh PET state rather than copied
into a second semantic representation.

A trace-root key contains the complete `DeclarationId` (including `FileId`)
and the anchored source digest. Distinct files or source snapshots can never
alias one root. Because one touched declaration can temporarily retain attempts
from several source digests, `abandon` retires every such root family before it
removes the declaration's attempt records; retiring only an arbitrary first
cursor would leave unreachable live traces.

## Queries and completion

`statement`, `proof`, `definition`, `assumptions`, `dependencies`, `search`,
`type`, and `notations` are PET operations in the declaration's loaded
context. The wrapper only validates the request, selects the PET source/state,
and returns typed transport errors. Completion is reported only after PET's
terminal proof result, the native Dune build, and the configured trust audit
all succeed.

## Trust audit identity and scope

Trust comparison uses PET-resolved constant identity, never a wrapper-composed
short name. `Print Assumptions` may print `ax`, `M.ax`, or `File.M.ax`
depending on the current context. The PET wrapper must run `Locate` for every
reported assumption in that same checked state and retain the unique constant
path. When PET AST identifies an explicit source `Axiom` or an `Admitted`
declaration, its temporary source-context spelling is likewise resolved with
`Locate` in that source's final PET state. The audit compares these canonical
paths and PET-checked types.

`Print Assumptions` is also the dependency oracle for the audit. Dune maps the
canonical constant prefix to its selected compilation-unit source; only source
files that can own assumptions actually returned by PET are inspected for
explicit axioms versus unfinished proofs. An assumption-free proof scans no
unrelated source. A close must not enumerate or PET-load every Dune source,
and a broken file outside the target's assumption set must not affect it.

## Rewind and request-boundary selection

`check` accepts 1--20 ordered proof fragments, each containing one or more Rocq
sentences. Every fragment PET reaches starts from the same immutable base. The
first fragment whose every sentence succeeds is appended to TraceForest and
later fragments are not evaluated. Rejected fragments are atomic and append no
accepted prefix. `try` evaluates every fragment from that same base and never
appends. A successful `check` records at most one new request checkpoint: an
accepted fragment that remains open creates one, while a solved fragment closes
the proof. An all-rejected `check` and every `try` create none.
Every operation that consumes an attempt obtains PET state through one engine
gateway. That gateway first checks the anchored source digest and reloads
Dune's current selected-source view, then reuses or replays PET. Consequently
`check`, `try`, queries, inspection, and rewind cannot disagree merely because
one path held a live PET handle while the project configuration changed.
If another connection retires the shared trace between selection and close, or
if a source CAS invalidates its immutable snapshot, the losing response exposes
no attempt/checkpoint; a checkpoint must never name an unusable cursor.

The public forms are `rewind()`, `rewind(steps=N)`, and
`rewind(checkpoint=ID)`. With no arguments the operation goes back one request.
Checkpoint IDs are session-local monotonically increasing integers and are
never reused. Beginning another proof clears the active checkpoint graph but
does not reset its allocator, so stale IDs cannot alias the new proof.

The ownership split is strict:

1. `rocq-mcp::checkpoint` alone owns request-boundary parent links, the current
   checkpoint, integer encoding, and the private `CheckpointId -> AttemptId`
   mapping. Rewinding then checking creates a new graph branch; old branch IDs
   remain selectable while that proof is active.
2. TraceForest continues to own immutable sentence-level actions and cursors.
   It knows nothing about MCP request boundaries or public checkpoint IDs.
3. The engine exposes only `checkout(current, target)`. It verifies that both
   opaque attempts belong to the same project and exact declaration root,
   validates the source snapshot and Dune source selection, and restores the
   target through PET. It does not count actions or select a connection
   checkpoint.
4. The MCP layer updates selection only after engine checkout succeeds. A
   replay, source-digest, or declaration failure leaves the original checkpoint
   selected. Completion, `abandon`, `start`, or selecting another proof clears
   the active graph.

Rewind never invokes native Dune build or writeback; it does query Dune's
selected-source metadata through the shared attempt gateway. Engine tests cover exact
attempt checkout, branch retention, PET eviction/replay, cross-proof rejection,
source changes, and failed-checkout atomicity. MCP tests cover all three public
forms, atomic multi-sentence alternatives, all-rejected checks, branching,
monotonically allocated IDs, and stale/invalid targets.

## Migration order

1. Add `list_files` as a Dune-only operation.
2. Add `list_decls(file)` as a one-file PET document operation.
3. Replace global declaration resolution with `DeclarationId`-based lazy
   resolution and remove workspace-wide refresh from `start`/`prove`.
4. Add immutable engine checkout plus MCP-owned request checkpoints and make
   all query and writeback paths consume the same PET-backed declaration
   record; delete duplicate catalog/index bookkeeping.
5. Validate the resulting flow on the large real project with prebuilt `.vo`
   dependencies, multiple interleaved attempts, PET eviction/replay, and
   native writeback.

TODO: Finish the full e2e replay and real-project completion audit. The
production workspace-wide refresh symbols named below have already been
removed; they are listed explicitly because reintroducing an adapter or
compatibility implementation for them would violate this design.

## Direct-deletion list

The items in this section are not extension points and must not be adapted,
wrapped, deprecated, or retained behind a compatibility branch. Delete the
implementation, all callers, its tests, and any public re-export together.
Where an item has already been deleted, keep it deleted.

### `crates/rocq-engine/src/engine.rs`

Delete these methods and fields outright:

* `Engine::declarations` as a workspace-wide listing operation;
* `Engine::refresh_project_state` and every call to it from `start`, `prove`,
  and named query operations;
* `Engine::identity_for_name` and name resolution by scanning a global map;
* `open_named`, `abandon_named`, and every other operation whose identity input
  is only a declaration-name string;
* global-map lookup branches in `source_for_identity`, open, abandon, query,
  close, and writeback code;
* any code that replaces the whole declaration map after PET document refresh.

Those call sites are replaced by one-file PET resolution using `DeclarationId`.
Do **not** replace these methods with a lazy cache having the same catalogue
semantics. The touched-attempt map is the only declaration map that may remain,
and it contains only declarations actually opened or declared during this
connection/project runtime.

### `crates/rocq-engine/src/pet/runtime.rs`

Delete the workspace batch method `PetRuntime::index_toc` and its loop over
`dune.sources`. Keep only a one-file PET declaration operation used by
`list_decls`.
No replacement may iterate every source merely to attach a project.

Delete any wrapper-side source walker used to discover declarations, classify
declaration kinds, detect `Admitted`, or manufacture names independently of
PET. In particular, `declaration_nodes_locked`, its scope stack, and the
TOC/per-sentence-AST merge are superseded by PET's single
`petanque/document_declarations` request and must not remain as a fallback.
Wrapper code may read source bytes only for PET transport, digest/CAS
validation, trust auditing that PET has not yet exposed as a document record,
and PET-range-based writeback.

### `crates/rocq-engine/src/types.rs`

Delete `ProjectState::declarations` itself, not merely its documentation, and
delete the invariant that every Dune-selected declaration is present. The only
replacement is `ProjectState::touched`, keyed by `DeclarationId`, with entries
created on `prove`/`declare`; do not keep a second complete source index under
another name.

Delete authoritative identity storage split across `library`, `modules`, and
`constant`. Public identity is exactly
`DeclarationId { file, qualified_path }`. A Dune logical library may remain on
an internal loaded-source record solely because Dune needs it to construct a
build target; it is not a second declaration key. `constant()` and module
prefixes may be computed from `qualified_path` when emitting a command, but
must not be stored as independent identity state.

Delete any catalogue/repository type whose job is to persist declaration
metadata, proof lifecycle, source spans, or PET semantic results independently
of PET. In particular, do not recreate the removed `Catalog`,
`ProofRepository`, or `SourceIndex`. TraceForest owns proof traces; the
in-memory touched record owns only the PET source anchor and attempt handle
required to continue that trace. No complete document declaration index or
scope reconstruction may survive the single PET document request.

### `crates/rocq-mcp/src/adapter.rs` and protocol docs

Delete the `start` response branch that calls the workspace declaration listing
and returns all declarations. `start` returns attachment success only. Delete
the compatibility path that resolves a `prove`, `abandon`, or declaration query
target from `"theorem": "A.B.t"`, `"target": "A.B.t"`, or any other bare
string by searching project state. These operations consume the structured
`DeclarationId` returned by `list_decls(file)`.

Delete the old seven-tool schema assertion and old request/response variants;
there is one ten-tool protocol, with `list_files`, `list_decls`, and
`rewind`, rather than old and new protocols selected at runtime. `rewind`
must use the selected connection's monotonic request checkpoint graph; it must
not introduce a wire cursor, action-count contract, or name-only fallback.

Update `crates/rocq-mcp/COMMANDS.md`, schema tests, and protocol fixtures. Do
not retain deserializers, aliases, fallback fields, or response shims solely to
make old fixtures pass.

### Tests and generated traces

Directly delete:

* unit/integration tests whose sole assertion is eager project indexing,
  catalogue refresh, name-only lookup, or wrapper-guessed declaration identity;
* generated traces whose `start` response embeds a complete declaration list;
* generated traces whose requests use the removed `theorem` string or a string
  `target`/`at` field and test no behavior beyond that removed protocol;
* fixtures used only by one of those deleted tests.

Trace serialization is owned by the e2e generator, not the MCP server. The
generator writes only the current protocol; no runtime compatibility parser or
old-trace rewrite pass may be added.

Do not hand-edit generated trace payloads one by one and do not add a legacy
protocol parser. Update the trace generator to emit the new flow, delete its
old generated output, and regenerate it. If an old test covers a still-valid
semantic property (PET crash recovery, trust auditing, CAS failure, Dune
errors, and so on), keep the property but rewrite its setup as
`start -> list_files/list_decls -> DeclarationId`.

## Code that must remain

The deletion rule does not apply to infrastructure required by the frozen
architecture:

* Dune-selected file discovery, logical-library/build-target lookup, and native
  `dune build` invocation;
* one-file PET document-declaration requests, PET process pooling/eviction,
  state replay from TraceForest, PET queries, and PET AST/range decoding needed
  outside declaration listing;
* `ProjectState::touched` and attempt-to-project routing needed to associate an
  MCP attempt handle with its project and PET state;
* TraceForest branching/cursor operations;
* PET-range-based writeback, source digest CAS, atomic replacement, native Dune
  validation, and trust auditing.

These components may be simplified, but deleting them would remove required
semantics rather than remove the legacy catalogue.

## Deletion completion checks

The deletion is complete only when all of the following searches return no
production-code matches (test names may quote a removed symbol only to assert
its absence):

```sh
grep -R -nE 'refresh_project_state|identity_for_name|open_named|abandon_named' \
  crates/rocq-engine/src crates/rocq-mcp/src
grep -R -n 'index_toc(' crates/rocq-engine/src
grep -R -nE 'Catalog|ProofRepository|SourceIndex' crates/rocq-engine/src
```

In addition, attaching a project must issue no PET declaration request;
`list_decls` must issue exactly one `petanque/document_declarations` request for
the requested Dune-selected file and no per-sentence AST requests; and no MCP
request parser may accept the removed name-only declaration forms.
