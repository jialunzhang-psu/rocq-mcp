# Rocq MCP PET-wrapper architecture

This document is the authoritative design for the whole `rocq-mcp` workspace,
not only for the `rocq-engine` crate. The implementation follows this contract;
the final section records the few intentionally deferred risks rather than an
unimplemented compatibility path.

The public MCP tools and their schemas remain those documented in
`crates/rocq-mcp/COMMANDS.md`. This redesign removes duplicated backend
machinery; it does not expose PET state IDs, trace cursors, publication
transactions, or process-management operations to MCP clients.

## 1. Goals

The implementation is a thin wrapper around three authorities:

1. **Dune** is the authority for project layout, source selection, logical
   libraries, load paths, build contexts, generated project configuration, and
   native builds.
2. **PET** is the authority for Rocq syntax and semantics: document
   declarations, canonical declaration identity, source ranges and AST data,
   proof states, tactic execution, goals, queries, proof completion, and
   assumptions.
3. **The source file** is the durable representation of a published proof.
   Publication uses PET-provided ranges plus compare-and-swap protection.

Everything else exists only to adapt these authorities to the public MCP
interface. In particular:

- there is no generic trace forest;
- there is no workspace-wide declaration catalogue;
- there is no proof repository;
- there is no wrapper-side Rocq parser or source walker;
- there is no pool of disposable or per-file PET processes;
- there is one live PET process per active Dune project, retained across
  non-mutating calls and refreshed at project mutation boundaries (with
  process replacement only as the fail-closed refresh fallback);
- PET state IDs are private, ephemeral handles for one live PET process;
- the only proof history retained by the wrapper is the request-boundary data
  required by `rewind` and writeback.

The preferred implementation is the smallest one satisfying these
invariants. Compatibility adapters for superseded backend representations are
explicitly forbidden.

## 2. Non-goals

This design does not add:

- persistence of unpublished proofs across a `rocq-mcp` server restart;
- a wrapper-owned semantic model of Rocq;
- a global search index over declarations;
- a public PET state-management API;
- an MCP tool for state release or process restart;
- a background publication queue or a `Pending` status;
- support for old internal cursor, attempt, catalogue, or trace formats.

An unexpected PET **child-process** exit is recoverable while the MCP server
remains alive because the accepted proof fragments are already needed for
writeback. A restart of the MCP server itself discards unpublished sessions.

## 3. Crate and module boundaries

### 3.1 `rocq-engine`

`rocq-engine` contains narrow wrappers, not the proof-session state machine.

```text
rocq-engine/
  dune/       Dune process invocation and typed result decoding
  pet/        one PET process client and PET protocol types
  writeback/  PET-range-based CAS publication and validation
  types.rs    small values crossing the three module boundaries
```

The crate may expose a small facade that sequences these wrappers for one MCP
operation, but that facade must not own a second representation of projects,
declarations, attempts, or traces.

### 3.2 `rocq-mcp`

`rocq-mcp` owns:

- MCP schemas and request validation;
- the active-project registry;
- connection attachment and selection;
- one request-boundary checkpoint graph per active proof;
- monotonically increasing public checkpoint IDs;
- translation between typed engine errors and MCP errors.

This is the correct owner of checkpoints because checkpoint semantics are an
MCP-interface property: one checkpoint represents one successful user
`check` request, even when the selected fragment contains several Rocq
sentences.

### 3.3 `trace-forest`

The `trace-forest` crate and its workspace dependency are deleted. Its generic
root/action/cursor model, UUIDs, action hashes, capacity watermarks, spill
paths, replay cache, and persistence hooks are not replaced.

The small MCP checkpoint graph described below is not a generic trace store.
It has exactly the fields forced by the existing `rewind` and writeback
contracts and one node per accepted MCP request, not one node per parsed Rocq
sentence.

## 4. Authority and ownership rules

The following table is normative.

| Information | Sole authority | Wrapper may retain |
|---|---|---|
| Workspace root, selected files, logical build context | Dune | Typed Dune result for the active project |
| Declaration identity, kind, statement, AST, source ranges | PET | The PET result needed by an active proof |
| Rocq environment, goals, proof completion, queries | PET state | Ephemeral PET state ID and PET-reported completion bit |
| Public checkpoint topology and current selection | `rocq-mcp` | Parent link and current checkpoint ID |
| Accepted tactic text | Exact MCP request text | One fragment on the corresponding checkpoint |
| Published proof | Source file | Digest and PET ranges used by the active CAS transaction |
| Build success | Dune | Result of the current validation operation only |
| Assumption/trust result | PET | Result of the current audit operation only |

No module may infer Rocq meaning from source text when PET can provide it. No
module may infer Dune layout from conventional directory names when Dune can
provide it.

## 5. Project and PET process model

### 5.1 One PET process per active project

The server supports multiple projects by creating multiple PET instances:

```rust
struct ServerRuntime {
    projects: HashMap<ProjectId, ProjectRuntime>,
    connections: HashMap<ConnectionId, ConnectionSession>,
}

struct ProjectRuntime {
    dune: DuneProject,
    pet: PetActor,
    publication_lock: Mutex<()>,
}
```

`ProjectId` is Dune's canonical workspace root. It is not a guessed directory
name. Two connections attached to the
same `ProjectId` share the same PET process. Different projects use different
PET processes and may execute concurrently.

The typed Dune view is a cache, not a second project authority. Under the
project operation barrier, every admitted MCP operation re-queries Dune for
the selected files, logical libraries, native targets, and PET load paths. If
that typed view changed, the runtime installs it atomically, replaces the PET
epoch, and clears every session's state IDs before continuing. Calling
`start` again re-queries and installs a fresh Dune view inside the already
shared runtime's barrier by the same rule. A failed Dune query fails the
request without falling back to the previous view, filesystem configuration
scanning, or conventional directory names.

The PET process is started lazily on the first PET-backed operation. A project
with no attached connections and no operation in flight is removed and its
PET process is terminated. There is no lane pool, LRU, capacity eviction, or
per-file PET instance.

### 5.2 The PET actor

`PetActor` is the only owner of the PET subprocess, its stdin/stdout framing,
and request IDs. It serializes RPC operations for that process. No other code
writes to the PET pipe or kills/restarts the process.

Serial execution provides the process-lifecycle invariant needed for crash
recovery. `PetActor` reports process loss but never reaches into MCP session
state; the `rocq-mcp` project coordinator sequences these steps:

1. on EOF, process exit, or an unrecoverable transport error, stop dequeuing
   calls and fail the in-flight call;
2. atomically clear every `pet_state` belonging to that project from the MCP
   sessions;
3. reap the old child and start a fresh PET process;
4. perform the capability handshake;
5. resume queued work, replaying a requested checkpoint if necessary.

No raw state ID may survive step 2. Therefore state IDs do not need a wrapper
generation number. A restarted PET may reuse an integer without aliasing an
old checkpoint because all old integers have already been removed before the
new process accepts work.

Blind retries are forbidden. A semantic Rocq error or non-zero Dune build is
returned as such. PET replacement and state replay occur only after loss of
the PET transport or at an explicit project-mutation boundary.

### 5.3 Capability handshake

The pinned PET build must advertise every extension required by this wrapper.
Startup fails immediately if a required endpoint is absent. It must not run
for minutes and then discover that it accidentally launched an incompatible
system PET.

Required capabilities include:

- one-call document declaration enumeration with canonical qualified names;
- declaration/source ranges required by writeback;
- atomic execution of a complete proof fragment;
- exact batch release of exported state IDs;
- authoritative workspace refresh after source/build mutation;
- the PET queries used by the MCP query variants and trust audit.

If stock PET lacks one of these capabilities, it is added to the pinned PET
submodule. Rust fallback scanners or alternative semantic implementations are
not permitted.

### 5.4 Source/build mutation is a project PET epoch boundary

Writing a source file and rebuilding its `.vo` changes the project environment.
An immutable PET state created before that change may still execute, but it no
longer proves anything about the current on-disk project. Reloading only the
edited document is insufficient: other loaded documents may depend on its old
compiled environment.

Therefore every source publication, and every server-initiated Dune build that
may replace Rocq build artifacts while PET is already live, is an exclusive,
project-wide PET epoch transition:

1. pause new operations for that project's PET actor;
2. wait for the current PET operation to finish;
3. atomically set every checkpoint `pet_state` for that project to `None`;
4. perform the source transaction and Dune validation while the actor is
   exclusively held;
5. call PET's `petanque/refresh_workspace` operation;
6. when applicable, audit the written declaration in the refreshed process;
7. resume queued operations, lazily replaying their checkpoints.

`refresh_workspace` is a PET-owned semantic operation, not a Rust cache guess.
It must atomically:

- clear every JSON-exported state ID;
- rebuild the shell workspace environment from the current Dune-provided root;
- bump Fleche/Coq file state and clear the `.vo` intern cache;
- clear PET's `.glob` and source-content memo tables;
- ensure every later document request reads and checks the current source;
- return success only when no pre-refresh document or compiled environment can
  be selected by a later request.

The pinned code already contains the underlying Fleche workspace-update
mechanism (`Theory.workspace_update`, `Doc.update_env`, `Coq.Files.bump`, and
`Memo.Intern.clear`), while the PET shell already constructs requested
documents from disk. The PET endpoint must compose those native mechanisms and
clear PET-specific memo tables; Rust must not reproduce their dependency
logic.

If PET cannot prove that refresh completed, the actor fails closed by
terminating it and starting a fresh process. Process replacement is therefore
the recovery fallback, not the normal writeback path. No pre-mutation PET state
is usable in the new epoch in either case. Other projects remain usable
throughout the transition.

## 6. Stable public identities

### 6.1 Files

`FileId` is a normalized source path relative to the canonical Dune workspace
root:

```text
Library/Holes/example.v
```

Only Dune-selected source files are accepted. Absolute paths, build-directory
paths, and hard-coded source roots are private implementation details and are
never public identities.

### 6.2 Declarations

The external declaration identity remains:

```rust
struct DeclarationId {
    file: FileId,
    qualified_path: Vec<String>,
}
```

`qualified_path` must be returned canonically by PET. The wrapper must not
construct it from a TOC leaf plus a locally maintained module/section stack,
nor concatenate a guessed Dune library prefix. This distinguishes, for
example, `A.foo` from `B.foo` in the same file and supports nested modules.

Source ranges are anchors, not identity. Editing preceding text may move a
range without changing the declaration identity.

One Rocq command may introduce multiple constants. PET returns that fact as
overlapping declaration ranges. Such declarations remain discoverable and a
completed one may be inspected, but no individual range is publishable: the
wrapper rejects an unfinished compound declaration at open and writeback
checks the same invariant again before touching source.

## 7. Minimal MCP session state

Each connection has at most one selected unpublished proof. Starting or
selecting another proof retires the previous active proof according to the
public command contract.

```rust
struct ConnectionSession {
    attached_project: Option<ProjectId>,
    active_proof: Option<ProofSession>,
    next_checkpoint: u64,
}

struct ProofSession {
    target: DeclarationTarget,
    source_anchor: SourceAnchor,
    current: CheckpointId,
    checkpoints: BTreeMap<CheckpointId, Checkpoint>,
}

struct Checkpoint {
    parent: Option<CheckpointId>,
    pet_state: Option<PetStateId>,
    accepted_input: Option<String>,
    pet_finished: bool,
}
```

Semantic definitions:

- `next_checkpoint` is connection-local, monotonically increasing, and never
  reset or reused while the connection lives.
- the root checkpoint has no parent and no accepted input;
- each non-root checkpoint represents exactly one accepted `check` request;
- `accepted_input` is the exact winning fragment supplied by the caller, not
  a reconstructed or normalized tactic script;
- `pet_state` is an optional cache handle into the currently live PET process;
- `pet_finished` records PET's result for that state. It is not independently
  inferred from goals or source text;
- `current` selects a node; selecting a different node does not delete either
  branch;
- `source_anchor` contains the file digest and PET-provided replacement or
  insertion ranges needed to reject stale publication, plus whether exactly
  one PET declaration owns the replacement range.

`DeclarationTarget` distinguishes an existing PET declaration from a new
in-memory declaration. A new declaration retains the exact validated header
information supplied by `declare`, because it does not yet exist in the
source. This is input required for writeback and crash replay, not a catalogue.

There is no map of every declaration touched in the project and no engine-side
attempt table. The active `ProofSession` is the entire unpublished proof state
owned by that connection.

Public status remains only `Open` or `Completed`. There is no `SourceClosed`,
`Pending`, or wrapper-guessed completion state. `pet_finished` means that PET
accepted a terminal proof state; `Completed` additionally requires successful
publication, Dune validation, and the configured PET trust audit.

## 8. PET state ID lifecycle

### 8.1 State IDs are ephemeral

`PetStateId` is an opaque `rocq-engine::pet` handle whose integer is private.
It is neither a theorem identity nor durable state. It is valid only while its
owning PET process is alive.

Every PET call that returns a state ID transfers explicit ownership to one of:

- a checkpoint node; or
- the current operation as a temporary state.

The operation must either transfer that temporary ID into a checkpoint or
release it before returning. State IDs are never written to disk and never
returned through MCP.

### 8.2 Exact, non-cascading release API

The pinned PET exposes an internal JSON-RPC operation:

```json
{
  "method": "petanque/release_states",
  "params": {"states":[12,15,18]}
}
```

It removes exactly the supplied IDs from PET's JSON `Obj_map`. It is batch,
idempotent, and non-cascading. Unknown or repeated IDs are ignored and
reported as missing rather than treated as a semantic failure.

PET does not know checkpoint parent/child relationships. Removing a parent ID
must not remove a child ID. A child `Coq.State.t` is a complete immutable
snapshot and retains any persistent structures it shares with its parent.
Removing the integer mapping only makes otherwise unreachable data eligible
for OCaml garbage collection; it does not promise an immediate reduction in
resident memory.

The endpoint is deliberately small:

1. extend `petanque/json/obj_map.ml` with exact batch removal from its existing
   `Hashtbl`;
2. expose `petanque/release_states` in the JSON-shell protocol;
3. register it in the JSON-shell dispatcher;
4. add PET protocol tests.

The pinned PET also exposes an optional `petanque/state_count` diagnostic used
only by lifecycle tests. It is not part of the required handshake and is not
mapped to MCP.

There is currently one `Obj_map.Make` instantiation for PET states, so this
does not require modifying Rocq's `Vernacstate.t` or Fleche document nodes.

### 8.3 Who releases states

The MCP proof-session owner decides reachability and calls
`PetActor::release_states`; PET never guesses it.

| Event | IDs released |
|---|---|
| `try` result returned | Every hypothetical state produced for that call |
| Rejected `check` candidate | Every exported temporary state, normally none when PET rejects atomically |
| Successful open `check` | Winning state is transferred to the new checkpoint; temporaries are released |
| `rewind` | None, because old branches remain selectable |
| `abandon`, proof replacement, disconnect | Every checkpoint state in that proof |
| Named query using a temporary context state | That temporary state after the query result is materialized |
| Successful writeback | No per-ID release; `refresh_workspace` clears the complete exported-state table |
| Project shutdown or PET crash | No release RPC; terminating the process releases the complete table |

Retiring a subtree, if a future interface ever permits it, requires MCP to
enumerate the now-unreachable checkpoint nodes and send their exact IDs in one
batch. PET release never walks descendants.

If a release RPC loses the PET transport, the old IDs are discarded; they
must not be retried against the replacement process.

## 9. Crash recovery and lazy replay

When PET exits, or when workspace refresh deliberately ends its current epoch,
all affected checkpoint nodes are changed to `pet_state = None`. Parent links,
exact accepted fragments, source anchors, and PET-reported completion bits
remain in MCP memory.

To use a checkpoint whose state is absent:

1. verify through Dune that the file still belongs to the same project/build
   context;
2. compare the current source digest with the proof's source anchor;
3. ask the new PET process to recreate the declaration's root state;
4. compute the unique root-to-target path from MCP parent links;
5. run each stored fragment atomically in order;
6. compare the replayed PET completion results with the stored results;
7. populate fresh state IDs for the replayed path;
8. release every newly created ID if replay fails before commit.

Other branches remain with `pet_state = None` and are replayed only if selected.
There is no replay cache, root affinity, state hash repository, or second trace
representation.

A changed source, changed Dune context, missing declaration, or divergent PET
result invalidates recovery and returns the existing typed error. Recovery
must not silently continue against a different theorem.

After another connection publishes:

- an active proof in an unchanged source file is replayed under the fresh
  dependency environment before it can run or publish again;
- an active proof whose own source file changed fails its whole-file digest
  check with `declaration_changed` and must be reopened;
- a newly opened declaration is always obtained from the refreshed PET
  workspace and therefore receives current ranges and a current source digest.

The conservative same-file rule avoids silently rebasing a proof onto a
possibly changed statement. A future transparent rebase would require PET to
re-resolve the declaration, prove that its interface is unchanged, return new
ranges, and successfully replay every accepted fragment; range shifting alone
is not sufficient.

## 10. Dune wrapper

The Dune module is a process/protocol wrapper. It is responsible for:

- resolving the canonical project and build context;
- returning Dune-selected `.v` source files;
- mapping a source to its logical compilation/build target;
- providing the generated project/load-path configuration consumed by PET;
- invoking the exact native validation target after writeback.

It must not:

- search for `_build`, `Library`, `target`, `.git`, or other conventional
  directory names;
- parse `dune` files to infer module membership;
- synthesize `-Q`, `-R`, or `-I` flags when Dune can report them;
- scan the workspace for declarations;
- mirror the project into a temporary tree;
- impose a default correctness deadline on a valid Dune build.

Request cancellation and an explicitly configured operator limit may terminate
a build. A hard-coded short timeout is not part of proof validity.

`start` attaches Dune state only. It does not enumerate declarations or start
PET solely to populate a catalogue. `list_files` is Dune-only.

## 11. PET wrapper

The PET module provides typed calls corresponding directly to PET operations.
It owns no proof topology. Its responsibilities include:

- process startup using the exact Dune-provided environment;
- capability negotiation;
- loading one requested document;
- returning all declarations in that document in one request;
- opening an exact declaration and returning its state/source metadata;
- executing a complete fragment atomically from a supplied state;
- goals, search, type, notation, statement, proof, definition, dependency, and
  assumption queries;
- exact state release;
- transport failure detection and process restart coordination.

The wrapper does not split Rocq source into sentences, rebuild an AST one
sentence at a time, reconstruct nested module paths, or infer whether a proof
has completed. If the existing PET call cannot execute a multi-sentence
fragment atomically or return the required declaration metadata, PET is
extended at the source of truth.

Read-only queries use the same per-project PET process. `DisposablePet` and
one-process-per-query paths are prohibited.

## 12. Writeback

Writeback consumes a fully prepared, linear publication request:

- the Dune-selected source file;
- the anchored whole-file digest;
- PET-provided proof replacement or declaration insertion range;
- the declaration header when the declaration is new;
- the ordered accepted fragments on the selected checkpoint path;
- the exact finalizer validated by PET.

The writeback module does not receive a checkpoint graph and does not discover
source structure. MCP linearizes the selected root-to-current path before
calling it.

Publication is:

1. acquire the project's exclusive publication barrier;
2. if an earlier publication cleared the selected checkpoint's state while
   this proof was waiting, replay its selected path in the current PET and
   require PET to report it finished again;
3. read the actual source, compare its digest with the anchor, and prepare
   exactly one PET-range-based replacement/insertion without changing disk;
4. clear every project checkpoint's state ID and begin the workspace epoch
   transition from section 5.4;
5. atomically replace the actual file;
6. invoke Dune on the exact target without a default build timeout;
7. refresh the existing PET workspace, restarting PET only if native refresh
   cannot complete safely;
8. reopen the written declaration and run the PET assumption/trust audit in
   that refreshed context;
9. return `Completed` only if every preceding step succeeds, retire the
   publishing proof, and release the barrier.

Step 2 closes the race between two proofs that both reached a terminal PET
state before either acquired the publication barrier. The second writer may
not publish from its pre-first-write state merely because its own source file
is unchanged; it must first replay under the post-first-write dependency
environment.

The project is never copied to a staging tree. If validation after the source
replacement fails, writeback attempts a CAS-protected restoration of the
original bytes, rebuilds the restored Dune target, and refreshes PET from that
final project state before releasing the barrier. It must not overwrite an
intervening editor or external-tool change while rolling back. If restoration
loses its CAS race, the actual external contents win; PET is refreshed (or
restarted if refresh cannot be guaranteed) only after the resulting Dune state
is known, and affected sessions fail normal source validation rather than
using the old state.

The source digest is required even when one MCP process owns the proof:
editors, version-control operations, generators, and other processes can still
modify the file.

Trust inspection is also PET-backed. The wrapper may compare structured PET
results against configured policy, but it must not scan source text for
`Axiom`, `Admitted`, module scopes, or declaration names. Missing PET metadata
is a PET capability gap, not permission to restore a local scanner.

## 13. MCP operation mapping

The public tool set and current request/response schemas are specified by
`crates/rocq-mcp/COMMANDS.md`.

### `start`

Resolve and attach a Dune `ProjectId`. Reuse that project's runtime or create
one. Retire any previously selected proof according to the existing session
contract. Do not enumerate files or declarations.

### `list_files`

Return Dune-selected workspace-relative `FileId` values. Do not invoke PET.

### `list_decls(file)`

Validate `file` against Dune and make exactly one PET document-declaration
request. Return PET's canonical identities, kinds, statements, and ranges. Do
not inspect unrelated files.

### `prove(declaration)`

Ask PET to resolve the exact `DeclarationId`. If the source declaration is
unfinished, create a root checkpoint holding PET's root state. If PET reports
it finished, validate the exact Dune target and trust result before returning
`Completed`. A required native build uses the project epoch boundary from
section 5.4, so its post-build audit cannot reuse the pre-build PET state. Do
not create an engine attempt or trace cursor.

### `declare`

Ask PET to validate the new declaration header and produce its root proof
state and insertion anchor. Derive the compilation-unit prefix from the
Dune-selected `file`; never ask the caller to repeat Dune's logical library.
Retain the exact header in the active `ProofSession`; do not touch the source
until publication succeeds.

### `check`

Structurally validate the entire candidate array first. Run every evaluated
candidate from the same current PET state. PET treats a multi-sentence
fragment atomically. The first accepted candidate wins.

- if it remains open, allocate one child checkpoint containing the whole
  fragment and final PET state;
- if PET reports it finished, append the winning fragment to the selected
  path and invoke writeback/validation;
- on successful publication, retire the active session; PET workspace refresh
  has already destroyed all old exported state IDs;
- rejected and unevaluated candidates do not alter checkpoint topology.

Publication-error behavior remains the public behavior documented in
`COMMANDS.md`; no `Pending` or `SourceClosed` backend state is introduced.

### `try`

Run every candidate independently from the same current state, materialize its
goals/result, release every hypothetical state, and leave the checkpoint graph
unchanged.

### `rewind`

Resolve the parent, ancestor count, or exact public checkpoint entirely in the
MCP checkpoint graph. If its PET state is absent, perform lazy replay. Change
`current` only after source validation and replay succeed. Do not delete the
old branch, build, or write source.

### `query`

Run all semantic variants through PET. With an active proof, use the current
PET state. Named queries without an active proof obtain a temporary state in
the target document and release it after returning the materialized result.
`search` is Rocq `Search`, not a wrapper metadata search.

The MCP boundary, not the engine, bounds materialized text to 32 KiB UTF-8
pages. Continuation is one integer byte offset returned by the preceding page;
the server retains no query cursor and never interprets or reimplements PET's
text. Unpaged small results preserve the original `{text}` response.

### `abandon`

Verify that the request names the active unpublished declaration, batch-release
all of its PET state IDs, and clear the proof session. It never edits source.

## 14. Concurrency and atomicity

- PET RPCs are serialized per project by `PetActor`.
- Different projects may use their independent PET actors concurrently.
- Checkpoint mutation is serialized per MCP connection.
- Publication takes an exclusive project barrier, pauses that PET actor, and
  serializes source replacement, Dune validation, rollback, PET workspace
  refresh (or fail-closed restart), and trust audit into one coherent
  transition.
- Source CAS remains the final protection against writers outside the server.
- A failed `rewind`, replay, `check`, or publication must not partially change
  `current` or expose a checkpoint whose PET state was never committed.

No global PET mutex serializes unrelated projects, and no lane-capacity waiter
or eviction policy is required.

## 15. Direct deletion list

The following code is superseded and must be deleted after callers migrate,
not wrapped or retained as a fallback.

### Whole crate

- `crates/trace-forest/**`;
- the workspace member and `trace-forest` dependency entries;
- trace-forest-specific tests, configuration, and documentation.

### Engine state and topology

- `AttemptId`;
- `ProofAttempt`;
- engine-owned `PetProofState` caches;
- `ProjectState::traces` and the old touched-attempt maps;
- all `TraceForest`, `CursorId`, `RootKey`, `ActionKey`, and `ForestConfig`
  uses;
- cursor/root/action hashing and trace-memory watermark configuration;
- engine checkout/replay APIs whose only purpose is translating an attempt ID
  into a trace cursor.

### PET runtime machinery

- `ProjectLane`;
- `ProjectPool` and `ProjectPoolState`;
- `PetLease`;
- `CachedReplay`;
- `DisposablePet`;
- lane acquisition, capacity waiting, root affinity, eviction, and per-query
  PET spawn paths.

They are replaced by one `PetActor` stored directly in each `ProjectRuntime`.

### Duplicate semantic infrastructure

- any `Catalog`, `ProofRepository`, `SourceIndex`, or equivalent declaration
  database;
- workspace-wide declaration refresh/indexing;
- wrapper-side declaration/source scanners;
- scope stacks and qualified-name reconstruction;
- per-sentence AST reconstruction used to implement whole-document listing;
- wrapper-inferred proof completion and source-derived lifecycle states;
- cached semantic query results that can disagree with PET;
- name-only lookup fallbacks and legacy protocol adapters.

### Publication machinery

- workspace mirroring and temporary-project staging;
- hard-coded directory exclusion lists used by that mirroring;
- hard-coded build directories or state directories such as
  `_build/.rocq-engine`;
- the default native-build close deadline and the `Pending` lifecycle it
  created. The typed `build_timeout` error remains only for an explicit
  operator-configured Dune-command deadline.

Tests that cover still-required behavior must be rewritten against the new
owner rather than deleted. Tests whose sole subject is a removed abstraction
are deleted with it.

## 16. Migration order

Migration follows this dependency order:

1. Freeze the existing MCP schemas and request-boundary rewind behavior in
   protocol tests.
2. Add and test the required pinned-PET capabilities, especially canonical
   document declarations, atomic fragment execution, exact state release, and
   authoritative workspace refresh.
3. Introduce `PetActor` and the per-project runtime registry; make all PET
   operations use the single project process.
4. Replace the MCP checkpoint payload from `AttemptId` to the minimal
   checkpoint fields in section 7.
5. Migrate `list_decls`, queries, `prove`, and `declare` to direct PET calls and
   remove disposable/read-only PET paths.
6. Migrate `check`, `try`, and `rewind` to direct PET state IDs, including
   branch retention and exact temporary-state release.
7. Migrate writeback to consume the selected checkpoint path directly.
8. Add PET-crash invalidation and lazy replay from accepted fragments.
9. Migrate close, abandon, replacement, disconnect, and project shutdown to
   the single batch-release path.
10. Delete the superseded engine attempt/trace/pool/catalog/scanner code and
    remove the `trace-forest` crate.
11. Regenerate current-protocol E2E traces rather than adding legacy parsers.
12. Run unit, process-boundary, multi-project, crash-recovery, memory-lifecycle,
    and real-project publication validation.

A migration step is incomplete until all callers have moved and its old path
has been deleted.

## 17. Acceptance criteria

The redesign is complete only when all of the following hold.

### Static architecture

- `trace-forest` is absent from the workspace and dependency graph.
- Production code has no `AttemptId`, `ProjectLane`, `ProjectPool`,
  `CachedReplay`, or `DisposablePet`.
- There is no wrapper source walker, scope stack, or declaration catalogue.
- One project runtime owns exactly one PET child process.
- No MCP response contains a PET state ID.
- No hard-coded build/source directory names determine project behavior.
- Every admitted operation uses a current Dune-reported view; a changed view
  invalidates the old PET epoch before any retained state is consumed.

### PET protocol

- releasing one state makes that exact ID unavailable;
- releasing a parent does not invalidate an unreleased child;
- releasing unknown/duplicate IDs is idempotent;
- batch release removes every supplied live ID;
- workspace refresh invalidates every previously exported state ID without
  normally changing the PET process identity;
- after refresh, document, source, `.glob`, and `.vo` queries observe the
  post-build project and cannot return memoized pre-build results;
- a forced refresh failure terminates PET and produces the same fresh semantic
  view through the restart fallback;
- `try` and rejected candidates leave no exported state IDs retained;
- document declaration listing returns nested declarations with unique,
  canonical qualified paths in one request.

### Proof semantics

- one successful multi-sentence `check` creates one public checkpoint;
- all candidates in `check` and `try` start from the identical base state;
- rewind retains old branches and checkpoint IDs are never reused;
- writeback linearizes only the selected branch;
- PET alone determines proof completion;
- `Completed` requires PET completion, successful Dune build, and trust audit;
- no failed build is reported as completed.

### Lifecycle and recovery

- same-project connections share one PET process;
- two active projects use two independent PET processes;
- killing one PET invalidates only that project's state IDs;
- successful writeback also invalidates every pre-write state ID before the
  refreshed project workspace is admitted;
- an old checkpoint is lazily reconstructed from its root and accepted
  fragments after PET restart;
- source or Dune-context changes prevent replay instead of changing theorem
  identity silently;
- `close`, `abandon`, proof replacement, and disconnect release every retained
  checkpoint state;
- repeated proof lifecycles do not grow PET's exported-state table.

### Publication

- no project tree is copied for validation;
- source changes are detected by CAS;
- replacement/insertion ranges originate in PET;
- a range shared by multiple PET declarations is never replaced as an
  individual declaration;
- Dune selects and builds the target;
- the trust audit uses a PET context refreshed after the source/build change;
- no declaration opened after writeback can observe a pre-write PET document
  or compiled environment;
- ordinary successful writeback refreshes the existing PET process rather than
  paying process/plugin startup cost; forced restart is tested as the fallback;
- an active proof in another file replays against the post-write dependency
  environment before its next writeback;
- two proofs that finish concurrently cannot publish sequentially from the
  same pre-mutation PET environment; the later publisher replays after taking
  the project barrier;
- an active proof in the changed file is rejected as stale rather than writing
  with old ranges;
- rollback cannot overwrite an intervening external edit;
- reopening the written declaration through PET reports the same completed
  theorem and the trust audit passes.

### Real-project validation

At least one large Dune project must complete the complete public lifecycle:

```text
start -> list_files -> list_decls -> prove/declare -> query -> try ->
check -> rewind -> branch -> check -> writeback -> dune build ->
assumptions audit -> reopen -> abandon cleanup
```

The validation must also kill PET during an open branched proof, recover two
different checkpoints lazily, and complete publication afterward.

## 18. Explicit risks and deferred work

`RISK:` A same-directory rename is atomic, but POSIX does not provide an
atomic replace-if-content-digest-matches operation. The project barrier
serializes every server-owned writer, and writeback checks the whole-file
digest immediately before replacement plus ownership afterward. A
non-cooperating external writer can nevertheless race inside that final
compare/rename interval. Rollback remains digest-guarded and never
unconditionally recreates or overwrites the path.

`RISK:` Releasing JSON-exported state IDs does not necessarily evict Fleche's
own document cache or immediately return OCaml heap pages to the operating
system. The pinned PET tests prove exact exported-map accounting, but they do
not promise immediate operating-system RSS reduction. If document retention
is unbounded in a long-running deployment, add a PET-native document lifecycle
operation; do not implement a Rust-side eviction model.

`RISK:` A process-loss notification must clear every affected connection's
state IDs before a replacement PET accepts calls. The project-scoped crash and
branch-replay tests cover the current serialized runtime; any future
concurrency change must preserve the same ordering so no queued old-process
result can commit after invalidation.

`RISK:` `refresh_workspace` is correct only if every project-dependent PET and
Fleche cache participates. The pinned PET suite changes a compiled dependency,
source text, and `.glob` data independently and observes each post-refresh;
any unclassified cache or refresh error forces process restart and never falls
back to the old state.

`DEFERRED:` Persistence of unpublished proof sessions across an MCP server
restart is intentionally out of scope. If it is later required, persist only
the minimal target, parent links, and accepted fragments; never persist PET
state IDs or restore the generic trace-forest abstraction.
