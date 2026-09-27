# MCP tools

Each call is `{"tool":"<name>","args":{...}}`. The server exposes ten
tools. There are no public cursors, session IDs, or publication commands. Files
are workspace-relative `FileId` values; declarations are PET-backed
`DeclarationId` objects. A declaration listing has an id, name, kind, and statement;
proof status is supplied
by PET-backed proof operations. A proof state has `theorem`, `statement`,
`status`, and `goals`. An open, selected state also has a session-local integer
`checkpoint`. Status is `Open` or `Completed`.
`Completed` is returned only after PET reports a proved terminal AST, Dune/Rocq
successfully builds the source, and PET's `Print Assumptions` output passes the
wrapper's trust policy. The wrapper never infers completion from source text.

Errors are JSON objects of the form
`{"kind":"invalid_request","message":"call start first"}`. The `message` gives
the specific field, declaration, or Rocq diagnostic. Error kinds are listed
under each tool below. `check` and `try` can also return errors *inside* their
result, preserving ordered-alternative diagnostics without turning a rejected
proof fragment into a protocol failure.

## `start`

```json
{"tool":"start","args":{"project_path":"./project"}}
```

Returns `{"attached":true}`. It only attaches the Dune workspace and does not
start a workspace-wide PET declaration index. Call `list_files`, then
`list_decls(file)` to discover a target.

| Error kind | When |
|---|---|
| `invalid_request` | Missing, empty, mistyped, or extra argument. |
| `invalid_configuration` | Unavailable path, invalid layout, or unusable project environment. |
| `ambiguous` | More than one project layout applies. |
| `project_timeout` | Dune project discovery or description timed out. |

## `list_files`

```json
{"tool":"list_files","args":{}}
```

Returns Dune's selected source files as workspace-relative `FileId` values.
This operation does not invoke PET or parse source files.

## `list_decls`

```json
{"tool":"list_decls","args":{"file":"Library/Foo.v"}}
```

Asks PET once for the document declarations of exactly one Dune-selected
source. Each declaration has an `id` containing the relative `file` and PET's
complete `qualified_path`. Duplicate leaves in different nested modules remain
distinct. The wrapper does not reconstruct module scopes or issue per-sentence
AST requests, and it does not index unrelated workspace files.

## `query`

The query variant is `args.kind`; there is no `request` wrapper.
`goals` takes no other fields. `statement`, `proof`, `definition`,
`assumptions`, and `dependencies` require `target` as a `DeclarationId`. `type` and `notations`
require `expression`. `search` requires a Rocq Search pattern. `search`,
`type`, and `notations` accept an optional `at` `DeclarationId` selecting the
original PET source state when no proof is selected.
Do not mix fields from different variants.

```json
{"tool":"query","args":{"kind":"goals"}}
{"tool":"query","args":{"kind":"search","pattern":"plus","at":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"statement","target":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"proof","target":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"definition","target":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"assumptions","target":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"dependencies","target":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"type","expression":"Nat.add 1 2","at":{"file":"Main.v","qualified_path":["Demo","t"]}}}
{"tool":"query","args":{"kind":"notations","expression":"x + y","at":{"file":"Main.v","qualified_path":["Demo","t"]}}}
```

`search` executes Rocq `Search` through PET; it is not a substring or status
filter over wrapper metadata. `statement` executes `About`, while `proof` and
`definition` execute `Print`; `proof` returns Rocq's proof term, not the
original tactic script. `goals` returns a proof state. Other variants return
`{"text":"<Rocq output>"}`. With a
selected open proof, `type` and `notations` run in that proof's current
replayed context. Without one, `search`, `type`, and `notations` require `at`
and run after that declaration in its original source file. The wrapper never
chooses a first file or library implicitly. Named queries likewise run after
their target declaration, not in a synthetic theorem. Before returning cached
goals, the server revalidates the declaration's source digest and Dune source
selection; a changed or malformed environment is reported instead of exposing
stale PET state.

| Error kind | When |
|---|---|
| `invalid_request` | Missing project, unknown kind, invalid field, target, or expression. |
| `not_found` | Target declaration does not exist. |
| `ambiguous` | PET/Dune produced a duplicate exact declaration identity. |
| `declaration_changed` | Selected proof no longer matches its declaration. |
| `proof_timeout` | Timed out waiting for PET process recovery or runtime capacity. |
| `project_timeout` | Dune project discovery or description timed out. |
| `invalid_configuration` | Project or query environment is unusable. |

## `declare`

```json
{"tool":"declare","args":{"name":"Demo.Main.new_t","statement":"True","kind":"Theorem","library":"Demo.Main","file":"Main.v"}}
```

`kind` defaults to `Theorem` and accepts `Theorem`, `Lemma`, or `Definition`.
`library` is required and is a Dune-selected logical compilation unit, not a
file path. `file` is the workspace-relative Dune-selected source file where
the declaration is inserted. The result is the new open proof state. `name` is
either the local constant or its full library/module-qualified name. The target
source file must already be selected by Dune; the wrapper does not create files
or edit `(modules ...)`. Source insertion occurs only when the proof closes.

| Error kind | When |
|---|---|
| `invalid_request` | Missing, empty, mistyped, or extra argument. |
| `invalid_declaration` | Invalid name, kind, statement, or lexical context. |
| `ambiguous` | Declaration placement is not unique. |
| `declaration_changed` | Target declaration changed while being created. |
| `invalid_configuration` | Project layout, logical library, or state directory is unusable. |

## `prove`

```json
{"tool":"prove","args":{"declaration":{"file":"Main.v","qualified_path":["Demo","t"]}}}
```

Returns the selected proof state.
The declaration ID must be returned unchanged by `list_decls`. When `prove`
loads an existing declaration it asks PET whether the terminal AST
is proved. If so, it builds the exact Dune target and queries PET for
assumptions before returning `Completed`; otherwise PET opens the proof and
supplies its goals. A failed build or unauthorized assumption is never reported
as completed.

| Error kind | When |
|---|---|
| `invalid_request` | No project, or an invalid or extra argument. |
| `not_found` | Declaration is absent or its proof record is closed. |
| `ambiguous` | PET/Dune produced a duplicate exact declaration identity. |
| `declaration_changed` | The declaration interface changed. |
| `proof_timeout` | PET open or replay timed out. |
| `project_timeout` | Dune project discovery or description timed out. |
| `invalid_configuration` | Project, PET state, or proof environment is unusable. |

## `abandon`

```json
{"tool":"abandon","args":{"declaration":{"file":"Main.v","qualified_path":["Demo","new_t"]}}}
```

Discards one uniquely identified unpublished proof. Its open in-memory attempt is
retired. It never deletes source code. Returns
`{"abandoned":"Demo.new_t"}` and clears the connection's selected attempt.

| Error kind | When |
|---|---|
| `invalid_request` | No project, invalid name, or extra argument. |
| `not_found` | No active unpublished proof has the name. |
| `ambiguous` | More than one unpublished proof has the exact identity. |
| `invalid_configuration` | The project or in-memory trace state is unavailable. |

## `check`

```json
{"tool":"check","args":{"attempts":["intro n. reflexivity.","intros; auto."]}}
```

Accepts 1–20 ordered proof fragments; each fragment may contain one or more
Rocq sentences. Every fragment starts from the same selected state. The first
fragment whose every sentence PET accepts is committed, and later fragments
are not evaluated. A rejected multi-sentence fragment is atomic: none of its
accepted prefix is appended.

Returns
`{"selected":1,"state":<proof state>,"rejected":[<error>],"error":null}`.
`selected` is the zero-based winning input index. `rejected` contains the
ordered errors before it. If every fragment is rejected, `selected` is `null`,
`state` and its checkpoint are unchanged, `rejected` contains every error, and
`error` is `null`. A selected solved proof closes automatically. `error` is
reserved for a close, writeback, or trust failure after selection. A
writeback/trust failure that preserves the anchored source leaves the proof
`Open` on the selected trace. If the source CAS fails (`declaration_changed`)
or a concurrent close already retired the trace (`not_found`), the diagnostic
`Open` state has no checkpoint; the caller must reopen the declaration to
observe its current source state.

The complete array is structurally validated before PET evaluates any
fragment. Before consuming the selected PET state, the engine validates its
source digest and current Dune source selection through the shared attempt
gateway. Native close has no default build deadline; an explicit deadline can
be set with `ROCQ_CLOSE_TIMEOUT_SECS`. PET/Rocq execution has a per-request
process-safety watchdog. A non-responsive PET lane is killed and reported as
`proof_timeout`; this watchdog is not a deadline on a valid native Dune build
or on source publication. Runtime lane capacity and Dune metadata waits remain
bounded separately.

| Top-level error kind | When |
|---|---|
| `invalid_request` | No selected proof, or invalid, empty, oversized, or extra input. |
| `declaration_changed` | The declaration interface changed. |
| `proof_timeout` | Timed out waiting for PET process recovery or runtime capacity. |
| `project_timeout` | Dune project discovery or description timed out. |
| `invalid_configuration` | Project, saved state, or proof environment is unusable. |

## `try`

```json
{"tool":"try","args":{"attempts":["intro n. reflexivity.","auto."]}}
```

Accepts the same 1–20 multi-sentence proof fragments as `check` and evaluates
all of them independently from the same selected PET state. It never appends a
trace, changes selection or checkpoint, writes source, or closes a proof. The
current Dune source selection is checked before PET evaluates any fragment,
so a cached PET lane cannot hide a changed project configuration.
Returns
`{"attempts":[{"solved":false,"state":<hypothetical proof state or null>,"error":<error or null>}]}`
in input order. A rejected fragment exposes no partial-prefix state and has its
own `proof_step_failed` or other typed error. Hypothetical states never contain
a checkpoint.

| Top-level error kind | When |
|---|---|
| `invalid_request` | No selected proof, invalid array, invalid fragment, or extra argument. |
| `declaration_changed` | The declaration interface changed. |
| `proof_timeout` | Timed out waiting for PET process recovery or runtime capacity. |
| `project_timeout` | Dune project discovery or description timed out. |
| `invalid_configuration` | Project or proof environment is unusable. |

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
replays the target trace in PET when its cached state is not available, and
changes the selected checkpoint only after validation/replay succeeds.

Returns:

```json
{"state":<proof state>}
```

It never edits source, runs a native build, or invokes writeback; Dune is used
only to validate the current source-selection metadata. Going back beyond the
root, an unknown or stale checkpoint, a missing selected proof, a retired
proof, or a changed source/environment is rejected. Rewinding and then
checking creates a new branch; checkpoints on the old branch remain selectable
until the active proof is completed, abandoned, or replaced. New checkpoint
integers are allocated monotonically for the MCP connection and are never
reused.

| Error kind | When |
|---|---|
| `invalid_request` | No selected proof, invalid or conflicting arguments, unavailable history, or an unknown/stale checkpoint. |
| `not_found` | The proof root was retired or the attempt is unavailable. |
| `declaration_changed` | The source snapshot no longer matches the open proof. |
| `proof_timeout` | PET replay or runtime recovery timed out. |
| `invalid_configuration` | Project, PET, or trace state is unusable. |
