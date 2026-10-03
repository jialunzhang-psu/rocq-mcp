# MCP tools

Examples below use the compact command notation
`{"tool":"<name>","args":{...}}`; on the MCP wire this is a
`tools/call` request whose `params` are `{"name":"<name>","arguments":{...}}`.
The server exposes the ten tools listed in the proof-editing workflow below.
The tool API exposes no query cursors or
connection identifiers, and no separate publication command. Streamable HTTP
may still carry its transport-level `Mcp-Session-Id`; open proof responses may
contain the documented session-local checkpoints and epoch-scoped goal IDs.
Files are workspace-relative `FileId` values; declarations are PET-backed
`DeclarationId` objects. A declaration listing has an id, kind, and statement;
the qualified name is already encoded by the id. Proof status is supplied by
PET-backed proof operations. A proof state has the exact reusable `target`
`DeclarationId` and `status`; every `Open` state additionally has `goals`,
`goal_counts`, and `focus` (the rendering may be empty when Rocq has parked
all goals). An open selected state also has a session-local integer
`checkpoint`. Status is `Open` or `Completed`; proof states do not duplicate
the target as a string or repeat its source statement.
`Completed` is returned only after PET reports a proved terminal AST, Dune/Rocq
successfully builds the source, and PET's structured global-context report
passes the wrapper's trust policy. The wrapper never infers completion from
source text or parses human-readable Rocq output to classify dependencies.
`Completed` does **not** mean axiom-free: the trust policy permits explicit
`Axiom` declarations inside the selected Dune project. Use
`query(kind:"assumptions")`; an `Axioms:` list means the target still depends
on every listed declaration, while Rocq's closed-under-the-global-context
message is the axiom-free result.

Structured goal contexts, sentence traces, and publication/replay progress are
opt-in observations. They are returned only when the corresponding request
flag is set (or when `query(kind:"progress")` is polled), so clients that use
the historical rendered fields remain wire-compatible. Progress is never sent
as an MCP notification.

Errors are JSON objects of the form
`{"kind":"invalid_request","message":"call start first"}`. The `message` gives
the specific field, declaration, or Rocq diagnostic. `query(kind:"progress")` is
the exception: it is connection-local, does not require `start`, and bypasses
the project/PET operation locks. After `start`, each other project operation
checks a cheap filesystem layout probe under the project barrier; only a probe
change causes a fresh typed Dune discovery. Operations that do invoke Dune can
therefore return `project_timeout` or `invalid_configuration`; tables below
describe the operation-specific cases and repeat those common errors where
useful. `check` and `try` can also return errors *inside* their result, preserving
ordered-alternative diagnostics without turning a rejected proof fragment into
a protocol failure. When Rocq attaches a precise location to a rejected proof
fragment, the error also carries
`"diagnostic":{"byte_range":{"start":N,"end":M}}`. The half-open offsets are
UTF-8 bytes in that exact fragment, are computed by PET from Rocq's exception
range, and are never guessed by splitting on periods in the wrapper.

For a semantic rejection, `message` is Rocq's diagnostic verbatim (and may
include the diagnostic range); the server does not replace it with a generic
explanation. For a request, Dune, transport, lifecycle, or other
infrastructure failure, `message` contains a concrete `Next step:` telling the
caller what to retry, inspect, or reconfigure. Public messages do not expose
the prover implementation name or JSON-RPC numeric error codes; those details
remain internal diagnostics only.

`pet_lost` means the in-flight PET request lost its child or JSON-RPC
transport. The server does not blindly repeat that request because a pipe can
fail after PET has consumed a state-changing proof command. It does retain the
MCP attachment and checkpoint text, invalidates every state ID for that
project, and lazily starts/replays PET on the next safe request. A restarted
MCP server/connection is a different session and still requires `start`.
Transport diagnostics preserve the OS error, child exit status or signal, and
a bounded stderr tail when available.

MCP `notifications/cancelled` is observed while waiting in the connection queue,
at wrapper transaction boundaries, and continuously while waiting for PET.
PET's stdio protocol executes one Rocq request synchronously and cannot read an
interrupt concurrently, so cancellation that wins before the operation's
commit point terminates that PET process epoch. The server then reaps it,
releases the serialized operation, retains the proof/checkpoint text, and
lazily replays on the next safe request. Such a cancellation never advances a
checkpoint or writes source. A notification racing after the explicit commit
point is too late: publication/rollback runs to a consistent boundary. Before
that point, an in-flight Dune subprocess is terminated with its process group
when request cancellation or an active command/query deadline wins. An explicit
native command deadline can still drive normal rollback/error handling after
publication has committed. If a cancelled response is still observed, its kind
is `request_cancelled`; normally the cancelling client discards it.

## Proof-editing workflow

The server's uncursored `tools/list` returns exactly `start`, `list_files`,
`list_decls`, `query`, `declare`, `prove`, `abandon`, `check`, `try`, and
`rewind`.

The complete existing-declaration workflow is `start` → `list_files` →
`list_decls` → `prove` → (`query(kind:"goals")` / `try` / `check` /
`rewind`). Call `prove` with the exact declaration id from `list_decls` before
calling `try` or `check`. For a new declaration, call `declare` instead of
`prove` to create the selected in-memory proof.

`try` is read-only speculation: it never enters a proof, commits a command,
writes a file, or closes a theorem. `check` submits and commits the first
accepted fragment. When that fragment closes the selected proof, `check`
atomically writes the source, runs the Dune build, refreshes PET, and performs
the trust audit; there is no separate save or publish tool. A successful
`query(kind:"type")` or `query(kind:"assumptions")` is inspection only and
does not select a proof. Desktop or shell/file-writing tools are not required
for Rocq-owned publication.

`invalid_request: call prove first` from `try`, `check`, `rewind`, or a goals
query means this MCP connection has no active proof selected by `prove` or
`declare`; it does not mean that the `prove` tool is absent. The uncursored
`tools/list` result is authoritative. If a client-side tool picker or tool
search exposes only a subset, reconnect or refresh that MCP connector and
inspect `tools/list` rather than reporting the filtered catalog as a server
limitation.

## `start`

Attach one Dune project. This operation does not select or enter a proof; use
`prove` for an existing declaration or `declare` for a new one afterward.

```json
{"tool":"start","args":{"project_path":"/absolute/path/to/project"}}
```

Returns `{}`. It attaches the Dune workspace and does not start a
workspace-wide PET declaration index. Call `list_files`, then
`list_decls(file)` to discover a target. Each `start` resolves its requested
path through Dune; after attachment, ordinary project calls use the shared
project barrier and a cheap layout probe, rerunning Dune discovery only when
that probe changes. Reattaching the same workspace retires this connection's
selected proof; if Dune's view changed, all connections keep their checkpoint
text but discard the old PET state handles for lazy replay.
If PET was already lost while retiring this connection's proof, its old IDs no
longer exist: retirement succeeds, sibling sessions discard their stale IDs,
and the attachment is installed normally.

Pass an absolute path to a directory on the MCP server, especially through
HTTP/Funnel or another remote client. A relative `project_path` is accepted
only when the client explicitly supplies a per-request working directory
(including Codex's negotiated `codex/sandbox-state-meta`) or exactly one local
`file:` URI through MCP `roots/list`. Most remote clients supply neither. The
server never guesses from the caller's shell and never interprets the path
relative to the MCP process's own working directory; use an absolute path when
the client context is unknown.

| Error kind | When |
|---|---|
| `invalid_request` | Missing, empty, mistyped, or extra argument. |
| `invalid_configuration` | Unavailable path, missing/non-local client base for a relative path, invalid layout, or unusable project environment. |
| `ambiguous` | More than one client root or project layout applies. |
| `project_timeout` | Dune project discovery or description timed out. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error while retiring a proof. |

## `list_files`

```json
{"tool":"list_files","args":{}}
```

Returns the current Dune-selected source files as workspace-relative `FileId`
values. It checks the cheap layout probe and refreshes Dune's typed view only if
the project layout inputs changed; it does not invoke PET or parse source
files.

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

Inspect goals, declarations, expressions, symbols, assumptions, or operation
progress. A query never enters, commits, saves, or publishes a proof.

The query variant is `args.kind`; there is no `request` wrapper.
`goals` accepts optional `scope`, `goal_id`, and `offset` fields. `scope` is
`focused` (the default), `unfocused`, `shelved`, `given_up`, or `all`;
`goal_id` is one of PET's evar arrays returned in `focus` and selects that
goal across all collections; `scope` and `goal_id` are mutually exclusive.
`offset` continues the bounded UTF-8 rendering. `about`, `print`, `assumptions`, and
`dependencies` require `target` as a `DeclarationId`. `type` and `notations`
require `expression`. `search` requires a Rocq Search pattern. `search`,
`type`, and `notations` accept an optional `at` `DeclarationId` selecting an
explicit original PET source context.
`locate_symbol` requires a non-empty `symbol`. It uses current compiler `.glob`
files (or a conservative source-presence fallback for a missing/stale index)
to select candidate documents, then asks PET for canonical declaration
metadata only in those documents. `limit` (1–100, default 20) bounds the
result. It returns the complete declaration id, qualified name, declaration
kind/statement, Dune-derived module, a suggested `Require Import`, and the
match class (`exact`, `suffix`, or `leaf`). `progress` is a read-only polling
query; it accepts only an optional previously observed `generation`, takes only
a short progress-record lock, and never waits for project/admission/PET locks
or pushes a state transition.
For `goals`, `structured:true` adds local hypotheses, goal types, stack-frame
sides, the bullet suggestion, and epoch-scoped ids where the selected state is
retained. `diff:true` adds a semantic before/after goal diff when a parent
checkpoint is available. Both flags are additive and omitted by default.
Every text-returning variant accepts an optional non-negative `offset` for
resuming a bounded result, including `goals`.
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
{"tool":"query","args":{"kind":"locate_symbol","symbol":"map","limit":10}}
{"tool":"query","args":{"kind":"progress","generation":7}}
{"tool":"query","args":{"kind":"goals","structured":true,"diff":true}}
```

`search`, `about`, and `print` execute Rocq `Search`, `About`, and `Print`
directly through PET; they are not wrapper metadata projections. `print`
returns Rocq's printed term, not the original tactic script. PET hint-level
loader progress (for example, fetching opaque proofs from a `.vo` file) is
not query output; Rocq's notice, warning, and error messages remain intact.
`assumptions` has the semantics of Rocq `Print Assumptions`. If its text
contains `Axioms:` followed by names, the target's logical dependency closure
contains those assumptions. Type checking, a successful Dune build, or a
`Completed` proof state does not erase or contradict that list. The publication
trust policy accepts only explicit in-project `Axiom` dependencies; it rejects
unfinished (`Admitted`) project dependencies, out-of-project axioms, other
kernel assumption kinds, and unsafe theory flags. Consequently, `Completed`
means “built and accepted by this policy,” not “closed under the global
context.”
`goals` returns a
proof state with PET-owned observability: `goals` is always present for an
open state (possibly `""`), `goal_counts` reports focused/unfocused/shelved/
given-up/total counts, and `focus` reports the stack depth, PET's raw
`next_bullet` suggestion, and—for a selected checkpoint—epoch-scoped goal IDs
for each stack side. Hypothetical `try` states have already been released and
therefore omit unusable goal IDs while retaining counts, depth, and any bullet
suggestion. A
`goal_id` query renders exactly one goal; `scope:"all"` renders every PET
collection through the same pager. State responses cap the focused rendering
at 32 KiB. A selected state exposes `goals_next_offset` when it can be resumed
with a `query(kind:"goals", offset:...)` call. A hypothetical `try` state is
released immediately, so an oversized one instead exposes
`goals_truncated:true`; commit the candidate with `check` before paging it.
`next_bullet` is present only when PET supplies a suggestion. These fields are
snapshots, not guesses from rendered text. Other variants return
`{"text":"<Rocq output>"}` when the complete result fits in 32 KiB. Larger
results are split at UTF-8 boundaries and return
`{"text":"...","next_offset":K}` when another page exists. Pass the
returned `next_offset` unchanged as the next request's `offset`; the last page
omits `next_offset`. Repeat the same `scope` or `goal_id` selector on every
page. Offsets count raw UTF-8 bytes, and callers must not invent or adjust
them. Paging is stateless: PET recomputes the same semantic query,
while the wrapper only slices its materialized text and stores no cursor. With a
selected open proof and no `at`, `search`, `type`, and `notations` run in that
proof's current replayed context. An explicit `at` always wins, even while a
proof is active, and runs after that declaration in its original source file.
Without either an active proof or `at`, those variants are rejected. The
wrapper never chooses a first file or library implicitly. An explicit `target`
determines the document context for `about`, `print`, `assumptions`, and
`dependencies` whenever it differs from the selected proof; the target's
temporary post-declaration PET state is released after the result is
materialized. This releases only the exported snapshot handle—not PET's
process, workspace, or checked-document cache—and allows declarations in files
that are not imported by the active proof to be queried in the same PET
instance without growing wrapper-owned state. When `target` exactly
matches the selected proof, the retained proof state is used so unpublished
`declare` targets remain queryable. `search`, `type`, and `notations` use the
active proof only when no explicit `at` is supplied. Before returning cached
goals, the server revalidates the declaration's source digest and Dune source
selection; a changed or malformed environment is reported instead of exposing
stale PET state.

Before PET consumes a target document, MCP asks Dune to build that exact
consumer `.vo`. If the source, `.vo`, or `.glob` fingerprint changes (including
a rebuild caused only by a transitive dependency), the server changes the PET
epoch and leaves every retained checkpoint replayable. This is automatic; do
not run `dune clean` or replace PET manually after an inconsistent-assumptions
diagnostic. The first creation of a previously absent `.vo`/`.glob` is the one
exception: no live PET state could have loaded that artifact, so it is recorded
without invalidating the current epoch.

With `structured:true`, an open state additionally contains
`structured_goals:{focused,stack,unfocused,shelved,given_up,next_bullet}`. Each
goal has `hypotheses` (names, optional definition, and type), `type`, and
an optional `id`; stack frames retain distinct `left` and `right` sides. Goal
ids are explicitly scoped to the current PET epoch and are omitted from
hypothetical `try` states. With `diff:true`, `goal_diff` reports semantic
context/type matches, additions, removals, counts, and whether duplicate goals
made matching ambiguous; PET evar numbers are never used as durable identity.

`kind:"progress"` returns the most recent operation for this MCP connection.
Before any operation it returns the idle `generation`/`status` snapshot; when a
record exists it includes `status` (`running`, `completed`, `failed`, or
`cancelled`), `phase`, `completed`, `elapsed_ms`, `changed`, and optional
`total`, `target`, and bounded `log_summary`. A terminal record is retained
until the next operation, so polling cannot miss a fast build. Supplying the
last generation sets `changed:false` when no newer record exists and
`changed:true` otherwise.
The query is intentionally independent of the connection admission, project,
selection, and PET locks, so it can be issued while a build or PET replay is
running. No progress notification is emitted.

| Error kind | When |
|---|---|
| `invalid_request` | Missing project, unknown kind, invalid field, target, expression, or paging offset. |
| `not_found` | Target declaration does not exist. |
| `ambiguous` | PET/Dune produced a duplicate exact declaration identity. |
| `declaration_changed` | Selected proof no longer matches its declaration. |
| `pet_lost` | The project PET child or its protocol transport was lost. |
| `query_failed` | Rocq rejected the query or its expression after request validation. |
| `query_timeout` | The query exceeded the 240-second watchdog (or `ROCQ_QUERY_TIMEOUT_SECS`). |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error. |
| `project_timeout` | Dune project discovery or description timed out. |
| `invalid_configuration` | Project or query environment/capability surface is unusable. |

## `declare`

Begin and select a new in-memory theorem, lemma, or definition proof. The
declaration becomes durable only when a later `check` closes it and completes
atomic writeback; there is no separate save operation.

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
If the statement references an identifier that is not in the declaration
environment, the semantic error keeps PET's message and adds
`resolution:{kind:"missing_identifier",identifier,candidates,suggested_imports}`.
Its diagnostic byte range is translated from the synthetic header to the exact
statement-relative range, and each candidate includes its full id and a
Dune-derived `Require Import` suggestion. An empty candidate list means that
the bounded project index found no canonical declaration candidate; it does not
by itself prove that the identifier is absent from source text.

| Error kind | When |
|---|---|
| `invalid_request` | Missing, empty, mistyped, or extra argument. |
| `invalid_declaration` | Invalid name, kind, statement, or lexical context. |
| `ambiguous` | Declaration placement is not unique. |
| `declaration_changed` | Target declaration changed while being created. |
| `invalid_configuration` | Project layout or PET environment is unusable. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error. |

## `prove`

Open, enter, and select an existing proof declaration for interactive editing.
This is the required entry operation before `try`, `check`, `rewind`, or a
selected `query(kind:"goals")`; pass the exact target returned by
`list_decls`.

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
and clears the connection's selected proof. If the PET epoch was already lost,
the integer state IDs are already gone, so abandonment still succeeds and
invalidates stale IDs held by sibling sessions.

| Error kind | When |
|---|---|
| `invalid_request` | No project, invalid name, or extra argument. |
| `not_found` | No active unpublished proof has the name. |
| `ambiguous` | More than one unpublished proof has the exact identity. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error while retiring the proof. |
| `invalid_configuration` | The project or in-memory proof state is unavailable. |

## `check`

Submit and commit proof commands to the active proof selected by `prove` or
`declare`. If the committed fragment closes the proof, this same call saves it
by atomic source writeback, Dune build, PET refresh, and trust audit; there is
no separate save or publish call.

```json
{"tool":"check","args":{"attempts":["intro n. reflexivity.","intros; auto."]}}
{"tool":"check","args":{"attempts":["eauto.","firstorder."],"timeout_ms":5000}}
{"tool":"check","args":{"attempts":["intros; auto."],"trace":true,"structured":true}}
```

Accepts 1–20 ordered proof fragments; each fragment may contain one or more
Rocq sentences. Every fragment starts from the same selected state. The first
fragment whose every sentence PET accepts is committed, and later fragments
are not evaluated. A rejected multi-sentence fragment is atomic: none of its
accepted prefix is appended. If Rocq identifies the failing command, that
entry's error contains its PET-provided fragment-relative byte range.

With `trace:true`, a rejected fragment additionally reports a bounded parser
trace (`trace` entries contain sentence index, byte range, and command), the
zero-based `sentence_index`/one-based `sentence_number` of the failing command,
and a read-only `before_state` captured immediately before it. Rocq's parser,
not a period-splitting heuristic, determines sentence boundaries. With
`structured:true`, that `before_state` and the selected state include
`structured_goals`; the response also includes a semantic `goal_diff` against
the state from which the alternative started. Both options default to false.

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
fragment. Optional `timeout_ms` is a positive integer applied independently to
each complete fragment, not to the whole alternatives list. A timed-out
fragment is a `proof_step_timeout` rejection; its partial execution is never
committed, PET is replaced, and the next alternative is replayed from the same
selected checkpoint. With no `timeout_ms`, no tactic deadline is invented, but
MCP request cancellation remains effective. Before consuming the selected PET state, the wrapper validates its
source digest and current Dune source selection. Native close has no default
build deadline. An operator may set the single explicit Dune-command limit
with `ROCQ_COMMAND_TIMEOUT_SECS`. `pet_lost` means unexpected loss of the PET
child/protocol transport, after which
checkpoint states are invalidated and lazily replayed.
While `check` is running, poll `query(kind:"progress")` on the same MCP
connection. Publication phases expose writeback, Dune build, PET refresh,
trust audit, and rollback summaries; the final generation remains available
after completion. The server does not push progress notifications.

| Top-level error kind | When |
|---|---|
| `invalid_request` | No selected proof, or invalid, empty, oversized, or extra input. |
| `request_cancelled` | The MCP peer cancelled the admitted operation. |
| `declaration_changed` | The declaration interface changed. |
| `pet_lost` | The project PET child or its protocol transport was lost. |
| `project_timeout` | Dune project discovery or description timed out. |
| `invalid_configuration` | Project, checkpoint state, or proof environment is unusable. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error. |

## `try`

Speculatively test proof commands against the active proof selected by `prove`
or `declare`. This read-only tool requires that prior selection and never
commits or saves its result; use `check` to submit a successful fragment.

```json
{"tool":"try","args":{"attempts":["intro n. reflexivity.","auto."]}}
{"tool":"try","args":{"attempts":["eauto.","firstorder."],"timeout_ms":5000}}
{"tool":"try","args":{"attempts":["idtac. nonsense."],"trace":true,"structured":true}}
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
hypothetical state is still an `Open` view and therefore contains
`goals:""`, while `solved:true` remains the explicit indication that the
fragment would close the proof if committed.
`timeout_ms` has the same per-fragment semantics as `check`. A timed-out entry
is returned with `solved:false` and a `proof_step_timeout` error; later entries
still run independently after the original checkpoint is replayed.
`trace` and `structured` have the same meanings as in `check`: a rejected
multi-sentence alternative identifies its failing sentence and immutable
pre-failure goals without exposing a partial PET state. Poll
`query(kind:"progress")` for per-alternative execution and PET replay timing;
no notifications are sent.

| Top-level error kind | When |
|---|---|
| `invalid_request` | No selected proof, invalid array, invalid fragment, or extra argument. |
| `request_cancelled` | The MCP peer cancelled the admitted operation. |
| `declaration_changed` | The declaration interface changed. |
| `pet_lost` | The project PET child or its protocol transport was lost. |
| `project_timeout` | Dune project discovery or description timed out. |
| `invalid_configuration` | Project or proof environment is unusable. |
| `pet_failure` | PET reported an anomaly, system failure, or unknown remote error. |

## `rewind`

Move an active proof selected by `prove` or `declare` to an earlier committed
`check` boundary. It does not open a proof or write source.

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
If replay is needed, poll `query(kind:"progress")` to observe the
`pet_replay` phase, completed/total checkpoint count, target, elapsed time, and
the latest checkpoint summary. The poll remains available while replay owns
the project and selection locks.

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
