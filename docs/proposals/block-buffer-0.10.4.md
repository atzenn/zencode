# Proposal: Block buffer for stream-json output (0.10.4)

Status: PROPOSAL - awaiting leader approval. No code changes in this PR.

## Context

Today StreamJsonSink (crates/zen-cli/src/output.rs) serializes every
AgentEvent to one NDJSON line and appends it to Buffered, an unbounded
in-memory Vec of bytes. Callers in agent_run.rs flush by printing
buf.text(), which copies the whole accumulated buffer on every flush.
For long runs this means unbounded memory growth and quadratic copying,
and flush granularity is whatever the caller happens to do - there is no
single, testable buffering policy between the sink and stdout. Desktop
consumes this stream record by record (docs/stream-json-v1.md), so flush
behavior is part of the contract surface even though record shapes do
not change.

## Proposal

Introduce a block-buffer layer between StreamJsonSink and the writer:

- Fixed-size blocks of 8 KiB. Serialized lines are appended to the
  current block; a block is flushed to the writer only when the next
  line would overflow it. A single line larger than one block is
  written directly (never split, never truncated).
- Flush points are explicit and minimal: block full, the final result
  record, process exit paths (including signal-driven shutdown), and an
  explicit flush() the run loop can call at turn boundaries.
- The init record is flushed immediately, so consumers see session
  metadata without waiting for a full block.
- Record shapes, ordering, and exit codes are unchanged: output bytes
  for any given event sequence are identical to today's implementation.
- The whole-buffer text() copies go away on the stream-json path;
  memory stays bounded at roughly two blocks plus the largest single
  line.

## Risks

- Latency: block filling can delay small records (tool results,
  errors) by up to one block of subsequent output. Mitigated by the
  turn-boundary flush; the result record and errors always flush
  immediately.
- Partial consumers: a reader that expects a line the moment an event
  happens (live Desktop UI) could feel the delay. Same mitigation;
  needs a Desktop-side smoke check before rollout.
- Signal paths: SIGINT/SIGTERM handlers must flush before exit or the
  last partial block is lost. The contract tests cover the normal
  result path only, so signal-path flushing needs its own test.

## Verification plan (for the implementation PR, not this proposal)

- The stream_json_contract test suite stays green (record shapes
  unchanged, byte level).
- Golden test: a fixed scripted event sequence produces byte-identical
  stdout before and after the change.
- New unit tests: block boundary with an oversized line, flush on
  result, flush on turn boundary, memory bound under a synthetic
  10k-event run.
- Manual smoke: a headless stream-json run against the mock server
  used by the existing headless tests, plus one Desktop read of the
  stream to confirm no visible latency regression.
