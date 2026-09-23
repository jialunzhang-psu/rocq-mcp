# MCP tools

Each call is `{"tool":"<name>","args":{...}}`. The server exposes exactly six
tools. There are no public cursors, session IDs, source paths, or publication
commands. A declaration has `name`, `statement`, and `status`; a proof state has
`theorem`, `statement`, `status`, and `goals`. Status is `Open`, `Completed`,
`Pending`, or `Rejected`.

Errors are JSON objects of the form
`{"kind":"invalid_request","message":"call start first"}`. The `message` gives
the specific field, declaration, or Rocq diagnostic. Error kinds are listed
under each tool below. `check` and `check_multi` can also return errors *inside*
their result, preserving accepted proof prefixes or candidate order.

## `start`

```json
{"tool":"start","args":{"project_path":"./project"}}
```

Returns `{"declarations":[{"name":"Demo.t","statement":"Theorem t : True","status":"Open"}]}`.

| Error kind | When |
|---|---|
| `invalid_request` | Missing, empty, mistyped, or extra argument. |
| `invalid_configuration` | Unavailable path, invalid layout, or unusable project state. |
| `ambiguous` | More than one project layout applies. |
| `project_timeout` | Timed out waiting for project ownership. |

## `query`

The query variant is `args.kind`; there is no `request` wrapper.
`goals` takes no other fields. `statement`, `proof`, `definition`,
`assumptions`, and `dependencies` require `target`. `type` and `notations`
require `expression`. `search` accepts only its optional filters shown below.
Do not mix fields from different variants.

```json
{"tool":"query","args":{"kind":"goals"}}
{"tool":"query","args":{"kind":"search","name_contains":"plus","statement_pattern":"nat","status":"Open","offset":0,"limit":20}}
{"tool":"query","args":{"kind":"statement","target":"Demo.t"}}
{"tool":"query","args":{"kind":"proof","target":"Demo.t"}}
{"tool":"query","args":{"kind":"definition","target":"Demo.t"}}
{"tool":"query","args":{"kind":"assumptions","target":"Demo.t"}}
{"tool":"query","args":{"kind":"dependencies","target":"Demo.t"}}
{"tool":"query","args":{"kind":"type","expression":"Nat.add 1 2"}}
{"tool":"query","args":{"kind":"notations","expression":"x + y"}}
```

`search` filters are optional; `offset` defaults to 0, and `limit` defaults to
20 (range 1–100). `goals` returns a proof state. Other variants return
`{"text":"<Rocq output>"}`; an empty search returns `{"text":""}`.

| Error kind | When |
|---|---|
| `invalid_request` | Missing project, unknown kind, invalid field, target, or expression. |
| `not_found` | Target declaration does not exist. |
| `ambiguous` | Target suffix resolves to multiple declarations. |
| `declaration_changed` | Selected proof no longer matches its declaration. |
| `proof_timeout` | PET query timed out. |
| `project_timeout` | Timed out waiting for project ownership. |
| `invalid_configuration` | Project or query environment is unusable. |

## `declare`

```json
{"tool":"declare","args":{"name":"Demo.new_t","statement":"True","kind":"Theorem","library":"Demo.Main"}}
```

`kind` defaults to `Theorem` and accepts `Theorem`, `Lemma`, or `Definition`.
`library` is an optional logical compilation unit, not a file path. The result
is the new open proof state. Source insertion occurs only when the proof closes.

| Error kind | When |
|---|---|
| `invalid_request` | Missing, empty, mistyped, or extra argument. |
| `invalid_declaration` | Invalid name, kind, statement, or lexical context. |
| `ambiguous` | Declaration placement is not unique. |
| `declaration_changed` | Target declaration changed while being created. |
| `invalid_configuration` | Project layout, logical library, or state directory is unusable. |

## `prove`

```json
{"tool":"prove","args":{"theorem":"Demo.t"}}
```

Returns the selected or recovered proof state.

| Error kind | When |
|---|---|
| `invalid_request` | No project, or an invalid or extra argument. |
| `not_found` | Declaration is absent or its proof record is closed. |
| `ambiguous` | The theorem suffix is not unique. |
| `declaration_changed` | The declaration interface changed. |
| `proof_timeout` | PET open or replay timed out. |
| `project_timeout` | Timed out waiting for project ownership. |
| `invalid_configuration` | Project or saved proof state is unusable. |

## `check`

```json
{"tool":"check","args":{"commands":"intro n. reflexivity."}}
```

Returns `{"state":<proof state>,"error":null}`. If PET rejects a sentence,
`error` is `{"kind":"proof_step_failed","message":"<Rocq diagnostic>"}` and
`state` retains the accepted prefix. A solved proof closes automatically.
A build timeout returns a `Pending` state and nested `build_timeout` error.
Trust rejection returns a `Rejected` state with either
`axiom_dependency_out_of_scope` or `unfinished_dependency` as the nested error.

| Top-level error kind | When |
|---|---|
| `invalid_request` | No selected proof, or invalid, empty, oversized, or extra input. |
| `declaration_changed` | The declaration interface changed. |
| `proof_timeout` | PET execution or replay timed out. |
| `project_timeout` | Timed out waiting for project ownership. |
| `invalid_configuration` | Project, saved state, or proof environment is unusable. |

## `check_multi`

```json
{"tool":"check_multi","args":{"candidates":["intro n.","auto."]}}
```

Accepts 1–20 single-sentence candidates. It never commits a candidate. Returns
`{"candidates":[{"solved":false,"state":<proof state or null>,"error":<error or null>}]}`
in input order. A rejected candidate has its own `proof_step_failed` or other
typed error.

| Top-level error kind | When |
|---|---|
| `invalid_request` | No selected proof, invalid array, invalid sentence, or extra argument. |
| `declaration_changed` | The declaration interface changed. |
| `proof_timeout` | Candidate evaluation timed out. |
| `project_timeout` | Timed out waiting for project ownership. |
| `invalid_configuration` | Project or proof environment is unusable. |
