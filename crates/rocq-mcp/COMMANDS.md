# MCP tools

Examples below use the compact command notation
`{"tool":"<name>","args":{...}}`; on the MCP wire this is a
`tools/call` request whose `params` are `{"name":"<name>","arguments":{...}}`.
The server exposes ten tools. There are no public cursors, session IDs, or publication commands. Files
are workspace-relative `FileId` values; declarations are PET-backed
`DeclarationId` objects. A declaration listing has an id, kind, and statement;
the qualified name is already encoded by the id. Proof status is supplied by
PET-backed proof operations. A proof state has the exact reusable `target`
`DeclarationId` and `status`; a nonterminal open state additionally has
`goals`, and an open selected state also has a session-local integer
`checkpoint`. Status is `Open` or `Completed`; proof states do not duplicate
the target as a string or repeat its source statement.
`Completed` is returned only after PET reports a proved terminal AST, Dune/Rocq
successfully builds the source, and PET's structured global-context report
passes the wrapper's trust policy. The wrapper never infers completion from
source text or parses human-readable Rocq output to classify dependencies.

Errors are JSON objects of the form
`{"kind":"invalid_request","message":"call start first"}`. The `message` gives
the specific field, declaration, or Rocq diagnostic. Every tool after `start`
first refreshes the typed Dune view and can therefore return
`project_timeout` or `invalid_configuration`; tables below describe the
operation-specific cases and repeat those common errors where useful. `check`
and `try` can also return errors *inside* their result, preserving
ordered-alternative diagnostics without turning a rejected proof fragment into
a protocol failure.

## `start`

```json
{"tool":"start","args":{"project_path":"./project"}}
```

Returns `{}`. It only attaches the Dune workspace and does not
start a workspace-wide PET declaration index. Call `list_files`, then
`list_decls(file)` to discover a target. Every call obtains a fresh typed Dune
view under the shared project barrier. Reattaching the same workspace retires
this connection's selected proof; if Dune's view changed, all connections keep
their checkpoint text but discard the old PET state handles for lazy replay.

| Error kind | When |
|---|---|
| `invalid_request` | Missing, empty, mistyped, or extra argument. |
| `invalid_configuration` | Unavailable path, invalid layout, or unusable project environment. |
| `ambiguous` | More than one project layout applies. |
| `project_timeout` | Dune project discovery or description timed out. |
| `pet_lost` | Retiring a selected proof lost the project PET child or transport. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error while retiring a proof. |

## `list_files`

```json
{"tool":"list_files","args":{}}
```

Returns Dune's selected source files as workspace-relative `FileId` values.
This operation re-queries Dune, but does not invoke PET or parse source files.

## `list_decls`

```json
{"tool":"list_decls","args":{"file":"Library/Foo.v"}}
```

Asks PET once for the document declarations of exactly one Dune-selected
source. Each declaration has an `id` containing the relative `file` and PET's
complete `qualified_path`, plus its statement and kind. Duplicate leaves in
different nested modules remain distinct. The wrapper does not reconstruct
module scopes or issue per-sentence AST requests, and it does not index
unrelated workspace files. Every declaration ID uses normalized `/`
separators; the request's `file` is not echoed because it is already present in
each returned ID, and the qualified name is not duplicated as a second string.

| Error kind | When |
|---|---|
| `not_found` | The requested file is not selected by Dune. |
| `invalid_declaration` | PET could not check the selected document as a declaration source. |
| `pet_lost` | The PET child or its protocol transport was lost. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error. |
| `invalid_configuration` | The project or PET capability surface is unusable. |

## `query`

The query variant is `args.kind`; there is no `request` wrapper.
`goals` takes no other fields. `about`, `print`, `assumptions`, and
`dependencies` require `target` as a `DeclarationId`. `type` and `notations`
require `expression`. `search` requires a Rocq Search pattern. `search`,
`type`, and `notations` accept an optional `at` `DeclarationId` selecting an
explicit original PET source context.
Every text-returning variant accepts an optional non-negative `offset` for
resuming a bounded result; `goals` does not.
Do not mix fields from different variants.

```json
{"tool":"query","args":{"kind":"goals"}}
{"tool":"query","args":{"kind":"search","pattern":"plus","at":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"about","target":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"print","target":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"assumptions","target":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"dependencies","target":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"type","expression":"Nat.add 1 2","at":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"notations","expression":"x + y","at":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"print","target":{"file":"Main.v","qualified_path":["Demo","t"]},"offset":32768}}
```

`search`, `about`, and `print` execute Rocq `Search`, `About`, and `Print`
directly through PET; they are not wrapper metadata projections. `print`
returns Rocq's printed term, not the original tactic script. `goals` returns a
proof state. Other variants return
`{"text":"<Rocq output>"}` when the complete result fits in 32 KiB. Larger
results are split at UTF-8 boundaries and return
`{"text":"...","next_offset":K}` when another page exists. Pass the
returned `next_offset` unchanged as the next request's `offset`; the last page
omits `next_offset`. Offsets count raw UTF-8 bytes, and callers must not invent
or adjust them. Paging is stateless: PET recomputes the same semantic query,
while the wrapper only slices its materialized text and stores no cursor. With a
selected open proof and no `at`, `search`, `type`, and `notations` run in that
proof's current replayed context. An explicit `at` always wins, even while a
proof is active, and runs after that declaration in its original source file.
Without either an active proof or `at`, those variants are rejected. The
wrapper never chooses a first file or library implicitly. With an active
proof, named queries also run in that proof's current PET state; without one
they run after their target declaration, not in a synthetic theorem. Before returning cached
goals, the server revalidates the declaration's source digest and Dune source
selection; a changed or malformed environment is reported instead of exposing
stale PET state.

| Error kind | When |
|---|---|
| `invalid_request` | Missing project, unknown kind, invalid field, target, expression, or paging offset. |
| `not_found` | Target declaration does not exist. |
| `ambiguous` | PET/Dune produced a duplicate exact declaration identity. |
| `declaration_changed` | Selected proof no longer matches its declaration. |
| `pet_lost` | The project PET child or its protocol transport was lost. |
| `query_failed` | Rocq rejected the query or its expression after request validation. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error. |
| `project_timeout` | Dune project discovery or description timed out. |
| `invalid_configuration` | Project or query environment/capability surface is unusable. |

## `declare`

```json
{"tool":"declare","args":{"name":"new_t","statement":"True","kind":"Theorem","file":"Main.v"}}
```

`kind` defaults to `Theorem` and accepts `Theorem`, `Lemma`, or `Definition`.
`file` is the workspace-relative Dune-selected source file where the
declaration is inserted. Dune is the sole owner of that file's logical
compilation-unit prefix; callers do not repeat it in a separate `library`
field or at the start of `name`. The result is the new open proof state. `name`
is exactly a compilation-unit-relative local constant such as `new_t` or a
nested-module path such as `Nested.new_t`. A name beginning with the derived
Dune prefix is rejected rather than silently normalized. The target source file must already be
selected by Dune; the wrapper does not create files or edit `(modules ...)`.
Source insertion occurs only when the proof closes. If a proof is already
active, `declare` rejects without changing it; call `abandon` explicitly.

| Error kind | When |
|---|---|
| `invalid_request` | Missing, empty, mistyped, or extra argument. |
| `invalid_declaration` | Invalid name, kind, statement, or lexical context. |
| `ambiguous` | Declaration placement is not unique. |
| `declaration_changed` | Target declaration changed while being created. |
| `invalid_configuration` | Project layout or PET environment is unusable. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error. |

## `prove`

```json
{"tool":"prove","args":{"target":{"file":"Main.v","qualified_path":["Demo","t"]}}}
```

Returns the selected proof state.
The declaration ID must be returned unchanged by `list_decls`. When `prove`
loads an existing declaration it asks PET whether the terminal AST
is proved. If so, it builds the exact Dune target and requests PET's typed
assumption identities and theory flags before returning `Completed`; otherwise
PET opens the proof and supplies its goals. A failed build or unauthorized
assumption is never reported as completed. If a proof is already active,
`prove` rejects without changing it; call `abandon` explicitly.

| Error kind | When |
|---|---|
| `invalid_request` | No project, or an invalid or extra argument. |
| `invalid_declaration` | PET reports an unsupported proof declaration or a compound source command that cannot be published independently. |
| `not_found` | The exact declaration is absent from the requested Dune-selected file. |
| `ambiguous` | PET/Dune produced a duplicate exact declaration identity. |
| `declaration_changed` | The declaration interface changed. |
| `pet_lost` | The project PET child or its protocol transport was lost during open or replay. |
| `project_timeout` | Dune project discovery or description timed out. |
| `build_timeout` | An explicitly configured Dune-command deadline expired while validating a PET-finished declaration. |
| `axiom_dependency_out_of_scope` | PET reports an axiom outside the selected Dune project, a non-constant kernel assumption, or an unsafe theory flag. |
| `unfinished_dependency` | PET reports dependence on an admitted or otherwise unfinished project declaration. |
| `invalid_configuration` | Project, PET state, or proof environment is unusable. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error. |

## `abandon`

```json
{"tool":"abandon","args":{"target":{"file":"Main.v","qualified_path":["Demo","new_t"]}}}
```

Discards one uniquely identified unpublished proof. Its in-memory proof session
and PET state handles are retired; it never deletes source code. Returns `{}`
and clears the connection's selected proof.

| Error kind | When |
|---|---|
| `invalid_request` | No project, invalid name, or extra argument. |
| `not_found` | No active unpublished proof has the name. |
| `ambiguous` | More than one unpublished proof has the exact identity. |
| `pet_lost` | Retiring the proof lost the project PET child or transport. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error while retiring the proof. |
| `invalid_configuration` | The project or in-memory proof state is unavailable. |

## `check`

```json
{"tool":"check","args":{"attempts":["intro n. reflexivity.","intros; auto."]}}
```

Accepts 1–20 ordered proof fragments; each fragment may contain one or more
Rocq sentences. Every fragment starts from the same selected state. The first
fragment whose every sentence PET accepts is committed, and later fragments
are not evaluated. A rejected multi-sentence fragment is atomic: none of its
accepted prefix is appended.

`selected` is the zero-based winning input index. `rejected` contains the
ordered errors before it and is omitted when empty. Results have these sparse
forms:

```json
{"selected":0,"state":<proof state>}
{"selected":1,"state":<proof state>,"rejected":[<error>]}
{"state":<unchanged proof state>,"rejected":[<error>,<error>]}
{"selected":0,"state":<proof state>,"error":<publication error>}
```

The third form means every fragment was rejected; it has no `selected` field.
A selected solved proof closes automatically. `error` appears only for a
close, writeback, or trust failure after selection. A
writeback/trust failure that preserves the anchored source leaves the proof
`Open` on the selected checkpoint graph. If the source CAS fails
(`declaration_changed`) or a concurrent close already retired the proof
(`not_found`), the diagnostic
`Open` state has no checkpoint; the caller must reopen the declaration to
observe its current source state. A selected proof's nested `error` can also
report native-build rejection or an explicit `build_timeout`, PET transport or
configuration failure, `axiom_dependency_out_of_scope`, or
`unfinished_dependency`; none is converted into `Completed`.

The complete array is structurally validated before PET evaluates any
fragment. Before consuming the selected PET state, the wrapper validates its
source digest and current Dune source selection. Native close has no default
build deadline. An operator may set the single explicit Dune-command limit
with `ROCQ_COMMAND_TIMEOUT_SECS`; PET proof execution itself has no
wrapper-invented correctness deadline. `pet_lost` means loss of the PET
child/protocol transport, after which
checkpoint states are invalidated and lazily replayed.

| Top-level error kind | When |
|---|---|
| `invalid_request` | No selected proof, or invalid, empty, oversized, or extra input. |
| `declaration_changed` | The declaration interface changed. |
| `pet_lost` | The project PET child or its protocol transport was lost. |
| `project_timeout` | Dune project discovery or description timed out. |
| `invalid_configuration` | Project, checkpoint state, or proof environment is unusable. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error. |

## `try`

```json
{"tool":"try","args":{"attempts":["intro n. reflexivity.","auto."]}}
```

Accepts the same 1–20 multi-sentence proof fragments as `check` and evaluates
all of them independently from the same selected PET state. It never appends a
checkpoint, changes selection, writes source, or closes a proof. The
current Dune source selection is checked before PET evaluates any fragment,
so a retained PET state cannot hide a changed project configuration.
Returns accepted entries as
`{"solved":<boolean>,"state":<hypothetical proof state>}` and rejected entries as
`{"solved":false,"error":<error>}` in input order. A rejected fragment
exposes no partial-prefix state and has its own `proof_step_failed` or other
typed error. Hypothetical states never contain a checkpoint; a solved
hypothetical state omits its empty `goals` field, while `solved:true` remains
the explicit indication that the fragment would close the proof.

| Top-level error kind | When |
|---|---|
| `invalid_request` | No selected proof, invalid array, invalid fragment, or extra argument. |
| `declaration_changed` | The declaration interface changed. |
| `pet_lost` | The project PET child or its protocol transport was lost. |
| `project_timeout` | Dune project discovery or description timed out. |
| `invalid_configuration` | Project or proof environment is unusable. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error. |

## `rewind`

```json
{"tool":"rewind","args":{}}
{"tool":"rewind","args":{"steps":3}}
{"tool":"rewind","args":{"checkpoint":17}}
```

Moves the selected proof to an existing `check` request boundary without
deleting any branch. With no argument it goes back one request; `steps` goes
back that many requests; `checkpoint` selects an exact checkpoint returned by
an earlier open-state response for the active proof. `steps` and `checkpoint`
are mutually exclusive. A successful `check` creates at most one boundary for
its whole selected fragment: a fragment that remains open creates one, while a
solved fragment closes the active proof. An all-rejected `check` and every
`try` create no boundary.
The operation validates the source digest and current Dune source selection,
replays the selected checkpoint path in PET when its cached state is not available, and
changes the selected checkpoint only after validation/replay succeeds.

Returns the proof state directly:

```json
<proof state>
```

It never edits source, runs a native build, or invokes writeback; Dune is used
only to validate the current source-selection metadata. Going back beyond the
root, an unknown or stale checkpoint, a missing selected proof, a retired
proof, or a changed source/environment is rejected. Rewinding and then
checking creates a new branch; checkpoints on the old branch remain selectable
until the active proof is completed, abandoned, or retired by reattachment or
disconnect. New checkpoint
integers are allocated monotonically for the MCP connection and are never
reused.

| Error kind | When |
|---|---|
| `invalid_request` | No selected proof, invalid or conflicting arguments, unavailable history, or an unknown/stale checkpoint. |
| `not_found` | The proof root was retired or the selected proof is unavailable. |
| `declaration_changed` | The source snapshot no longer matches the open proof. |
| `pet_lost` | The project PET child or its protocol transport was lost during replay. |
| `proof_step_failed` | PET rejected a replayed proof fragment while reconstructing the requested checkpoint. |
| `invalid_configuration` | Project, PET, or checkpoint state is unusable. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error. |
