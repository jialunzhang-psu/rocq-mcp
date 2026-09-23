# trace-forest

`trace-forest` stores immutable, branching prefixes in one process. It is a
generic data structure: roots, actions, validation callbacks, and close effects
belong to the caller. It has no Rocq or MCP concepts. Spill files control memory
use; they are not a write-ahead log and are not reopened after a crash.

## Identity and operations

A root is identified by bounded canonical `RootKey` bytes. An edge is identified
by `(parent CursorId, ActionKey)`. `CursorId` wraps a UUID; cursor order does not
define trace order. Repeating an open or edge insertion with the same key returns
the existing cursor without rerunning its callback. Different actions below one
parent branch; old prefixes never change.

- `open(key, prepare)` creates a root if absent.
- `step(parent, key, prepare)` prepares one action and publishes one child.
- `inspect(cursor)` returns the root and ordered actions, loading spilled
  payloads when needed. It does not replay application state.
- `close(cursor, effect)` passes a stable trace view to the caller. One close
  effect runs per root at a time. Success retires the entire family; failure or
  panic leaves its branches available for retry.

Callbacks run outside global metadata locks. Identical concurrent opens or steps
single-flight; independent roots can progress concurrently. Publication of a
root, edge, closing state, or retirement is linearizable. Returned views own
their data, so concurrent close or spill cannot invalidate them.

## Memory and spill

The configured watermark accounts for encoded payload bytes, not caller-owned
callback results. New payloads exceeding the resident target are written to
checksummed, versioned segment frames in a unique directory owned by the
forest. Admission fails if spill capacity cannot be established; a cursor is
never returned for an uncommitted edge. `open` and `step` do not `fsync` because
they promise process-lifetime state only. `Drop` removes the owned directory,
never its caller-supplied parent.

Cursor, root, and edge indexes remain in memory for expected constant-time
lookup. Appending does not copy ancestor actions. Inspecting a trace costs
linear time in its depth plus any cold payload reads. Successful close reclaims
its public indexes; no cursor tombstone history is retained.
