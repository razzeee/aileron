# Private llama-server runtime design

Status: approved by the user, 2026-09-24.

The user has since requested a full replacement and comparative evaluation.
The approved [replacement design](llama-server-replacement-design.md) supersedes
this document's opt-in migration boundary.

## Goal and agreed direction

Apps should be able to request supported reasoning levels through Aileron. Use a private llama-server inside the existing inference container so Aileron can reuse upstream chat templates, reasoning parsing, and generation controls. The user approved investigating this route after comparing it with a native C++ bridge.

Success means model-supported thinking and effort settings reach inference, final answers do not contain parsed reasoning, and container isolation and cancellation remain effective. Existing text and structured requests must continue to work. Capability claims must describe the installed runtime and model, rather than everything upstream can theoretically do.

This document specifies the runtime migration first. Public reasoning options follow on that foundation. The other opportunities in [the capability assessment](llama-capabilities-assessment.md) are follow-up work unless explicitly included below.

## Source feasibility checks

The initial candidate upstream revision was `e6ab7c1a41054a888ada952eab4c886444c2f5ad`, matching the capability assessment. Comparison testing changed the build pin to `5f55650a78f92aff4d48d671423e888fac0469ff` to address OCR latency. See [the comparison report](llama-server-comparison.md) for measurements and compatibility changes. Pin the build to an immutable revision and record it in image provenance.

- [Upstream HTTP implementation](https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/tools/server/server-http.cpp) treats a host ending in `.sock` as an AF_UNIX listener. `--host /tmp/aileron-llama/server.sock` therefore avoids TCP and does not require bringing container loopback up.
- The same implementation returns HTTP 503 until ready, exposes connection-closed checks to request handlers, and streams chunks through a content provider with completion cleanup. This supports incremental translation. It is not proof of bounded inference cancellation latency.
- `crates/aileron-daemon/src/container.rs` creates private mount, PID, and network namespaces, mounts a private writable `/tmp`, mounts `/model` read-only, and drops capabilities. A socket under that private `/tmp` requires no new host mount or network permission.
- `crates/aileron-daemon/src/request_execution.rs` cancels active requests through their container handle when a session closes. Real-model testing exposed that killing only the `crun run` monitor left the container running. The implementation now sends `crun kill --all <id> KILL`, reaps the monitor, and removes OCI state with `crun delete --force`. The real process regression test passes with this fix.
- The runtime protocol requires newline-delimited JSON on stdout, a readiness marker on stderr, and a final `done` response. Server logs and HTTP/SSE bytes must never reach protocol stdout directly.

The initial checks were source-only. Implementation validation subsequently built the CPU image and exercised it through the daemon's actual OCI wrapper. See [validation results](llama-server-validation.md).

## Architecture

```text
app -> portal -> daemon -> JSON-lines runtime adapter
                                  |
                       HTTP over private Unix socket
                                  |
                             llama-server
```

Add a Rust adapter in `crates/aileron-runtime`. Separate child-process lifecycle, HTTP/SSE transport, and Aileron request/response conversion. The adapter is the container entry process and owns one server child for the mounted model. Initially execute one inference request at a time, matching the current runtime. Use one server inference slot and explicitly bounded HTTP worker settings compatible with the container PID limit.

The adapter creates a private directory under `/tmp`, starts the server with only its Unix listener, and polls `/health` while also checking child exit. It emits Aileron's readiness marker only after successful health response. Forward child diagnostics to stderr with readiness-like lines filtered so upstream logging cannot accidentally satisfy the daemon's readiness detector. Route child stdout away from protocol stdout.

Use fixed executable and socket paths. Construct arguments from validated runtime configuration. Load local model/projector files from `/model`; do not use remote model identifiers. Disable the web UI. The HTTP client must use the Unix connector and must not honor proxy settings or follow redirects to other transports.

On stdin EOF, transport failure requiring shutdown, or adapter termination, stop and reap the child. Give graceful shutdown a bounded interval before force-killing. A child crash during inference produces a terminal runtime error if stdout is still writable, then exits the adapter so the daemon can discard the handle. Do not replay partially emitted requests automatically.

## Migration boundary

Introduce the adapter alongside the existing native runtime. Opt selected test profiles into it through a runtime launch option before changing the default entrypoint path. Keep Whisper, native embeddings, and native vision available during this first migration. This avoids changing embedding identity or multimodal behavior as an incidental part of reasoning support.

The first production switch covers text generation, structured generation, and structured snapshot streaming after their parity checks pass. Native tool calls and vision migration need their own conformance checks before selecting this adapter for profiles advertising those features. Unsupported operation combinations must fail explicitly rather than run a degraded prompt fallback.

Build the pinned server in `runtimes/llm-vision-whisper/Dockerfile` with the image's CPU/CUDA/ROCm/Vulkan backend settings. Copy required runtime libraries and record the server revision. During transition, the Rust bindings and server can contain different llama.cpp revisions; report both and do not mistake the binding version for server provenance. Remove redundant native generation builds only after all their remaining consumers migrate.

## Request and streaming behavior

- Use canonical `input` messages when present, preserving roles. Fall back to `system` and `prompt` for legacy requests without canonical input. Avoid duplicating system content.
- Send chat requests to `/v1/chat/completions` with streaming enabled. Preserve `max_tokens` as a total generated-token cap, including reasoning. Resolve existing temperature settings explicitly during parity migration.
- Parse SSE incrementally across arbitrary HTTP read boundaries, including split UTF-8, CRLF, multiple events per read, and usage-only events. Bound buffered event sizes. Convert answer deltas into existing `token` responses with the original Aileron request ID.
- Collect finish reason and token usage before emitting the one terminal `done` response. An unexpected EOF or malformed stream is a failure, not successful completion. Do not equate an early finish-reason chunk with the end of a stream that still has usage metadata.
- Structured generation accumulates only final-answer content, parses JSON, and returns the existing `result` form. Structured snapshot streaming emits only complete schema-valid snapshots; the current protocol permits emitting the completed object as initial and final snapshots. Preserve daemon validation. Never extract JSON from reasoning text.
- Request rejection, model context exhaustion, child death, and malformed upstream output map to stable Aileron errors. Preserve cancellation classification when the daemon terminates the container.

## Reasoning contract after migration

The follow-on public API design must cover portal D-Bus options, Varlink types, daemon validation, model/profile capabilities, and runtime events together. Existing D-Bus options use fixed structs, so adding members requires an explicit versioned compatibility decision rather than assuming optional JSON fields preserve that ABI.

Required semantics:

- `thinking` is `auto`, `on`, or `off`; omission means `auto`.
- `reasoning_effort` is optional and validated against the selected artifact/template's advertised values. Initially advertise `low`, `medium`, and `high` only for verified GPT-OSS profiles.
- `auto` leaves template thinking defaults intact. Explicit supported on/off maps to `chat_template_kwargs.enable_thinking`; effort maps to the pinned server's effort handling. Reject effort combined with `thinking=off`.
- Unknown support is not affirmative support. Explicit unsupported settings fail before inference; omission continues using model defaults. Template variable detection alone does not establish a supported effort enumeration.
- Parsed reasoning travels separately from answer tokens. Apps opt into receiving reasoning; otherwise the adapter discards parsed reasoning while preserving total usage. Do not persist traces in history by default.
- Exhausting the total token cap reports a length finish reason, even if the model never reached an answer. Schema-incomplete output remains an error for structured calls.
- Resolve sampling defaults by verified profile and thinking mode when the caller has not supplied overrides. In particular, do not silently retain greedy defaults for a Qwen thinking profile while claiming to follow its recommended sampling recipe.

Reasoning budgets, a manual finish-thinking action, general sampler overrides, and cache reuse are deferred. Effort levels must not be approximated by arbitrary token budgets.

## Validation and rollout gates

Before making the adapter a default, require:

1. Adapter tests against a fake Unix-socket HTTP server for fragmented streams, Unicode, errors, metadata ordering, child startup failure, unexpected EOF, and shutdown. Verify exactly one terminal response when the transport remains writable.
2. OCI tests with the pinned real server demonstrating health readiness, no TCP listener, the socket confined to the container mount namespace, and session cancellation removing the server process. Repeat during startup and active generation.
3. Real-model parity checks for ordinary text, role-aware history, structured results, schema failure, snapshots, context overflow, and output-token exhaustion. Check warm reuse and daemon idle teardown.
4. Reasoning conformance with a hybrid Qwen model, a thinking-only checkpoint, GPT-OSS, and a non-reasoning instruct model. Check default/on/off, supported and unsupported efforts, prompt-prefilled reasoning, answer separation, and truncation before an answer.
5. Build and smoke-check each advertised hardware image variant before switching its profiles. Record any unavailable hardware tests as outstanding, not passed.

Enable the adapter for tested text profiles first. Existing runtime selection provides rollback to the native path during rollout; a failed request must never silently retry through that path because semantics and partial output can differ.

## Next design checkpoint

Approve this migration boundary and lifecycle/transport contract before writing the implementation plan. The first deliverable is the private-server adapter with parity tests. The next deliverable specifies and implements the versioned public reasoning API on the validated runtime.
