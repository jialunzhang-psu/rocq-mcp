# E2E trace format

A trace is plain JSONL (`.jsonl`), executed in file order. It uses five events:

```json
{"event":"server_start"}
{"event":"user_connect","user":"alice"}
{"event":"command","user":"alice","command":{"tool":"query","args":{"kind":"goals"}},"expected":{"kind":"invalid_request","message":"call start first"}}
{"event":"user_disconnect","user":"alice"}
{"event":"server_kill"}
```

`command` holds a user ID, a complete tool call, and the exact expected JSON
result. Output comparison ignores object key order, not missing or extra fields.
The sole leaf marker `{"$checkpoint":true}` accepts any positive integer so a
trace need not predict a connection-local allocation ID; every surrounding
object and array remains exact.
`user_connect` opens a fresh MCP session; it does not restore the previous
selection. `user_disconnect` closes that session. `server_kill` terminates the
server and all sessions; a later `server_start` starts a new process.

Two adjacent commands with the same non-empty `parallel_group` execute
concurrently on different user connections. Their complete outputs are compared
as an unordered pair. A parallel command may use
`"expected":{"$one_of":[{...},{...}]}` for a finite set of exact results.
These fields belong to the test runner, not the MCP request.

Each trace is an isolation boundary. The maintained
`traces/current_protocol.jsonl` file covers all ten current tool names without
retaining the deleted lane/eviction/timeout/fault abstractions.
