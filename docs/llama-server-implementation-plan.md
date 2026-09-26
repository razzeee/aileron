# Private llama-server implementation plan

Status: approved for sequential execution by the user, 2026-09-24.

This records the initial opt-in milestone. The
[full replacement plan](llama-server-replacement-plan.md) supersedes its scope;
see [current validation](llama-server-validation.md) for cutover status.

Design: [llama-server-design.md](llama-server-design.md), approved by the user.

## Deliverable and execution

Implement the opt-in private llama-server text adapter, build it into the combined runtime image, and verify its transport and lifecycle before enabling it for profiles. Execute the steps sequentially in this conversation. No additional agents are needed.

The public reasoning API is the next deliverable. It requires a versioned D-Bus contract and capability advertisement after this adapter proves the runtime behavior. The work below must leave reasoning separate from answer content so that follow-on does not require another generation implementation.

Preserve existing working-tree edits, including the modified Cargo.lock and portal submodule. Dependency changes must extend the current lockfile rather than restore the committed version. Commit only when explicitly requested.

## 1. Add the adapter target and request conversion

Files:

- `crates/aileron-runtime/Cargo.toml`
- `crates/aileron-runtime/src/lib.rs`
- New `crates/aileron-runtime/src/bin/llm_llama_server.rs`
- New `crates/aileron-runtime/src/llama_server/mod.rs`
- New `crates/aileron-runtime/src/llama_server/request.rs`

Add a `llama-server` feature and `aileron-runtime-llm-llama-server` binary. Reuse the workspace's reqwest dependency with its Unix-socket support, confirming the actual locked API before implementation. Keep this feature independent of the native llama bindings.

Convert supported runtime requests to chat-completions JSON. Preserve canonical message roles and text parts; use legacy system/prompt only when canonical input is absent. Inspect daemon message construction to prevent duplicated system instructions. Preserve explicit temperature and maximum-token values. Translate the existing schema wrapper into the upstream request format.

Initially accept `generate`, `generate_structured`, and `generate_structured_stream`. Reject unsupported request kinds, image/audio content, tools, and tool results with a terminal protocol error. Do not flatten unsupported inputs into text or silently ignore them.

Add focused conversion tests covering message order, system deduplication, defaults, schema translation, and explicit rejection. Run `cargo test -p aileron-runtime --features llama-server` as the feature becomes buildable.

## 2. Implement bounded Unix HTTP and SSE transport

New files:

- `crates/aileron-runtime/src/llama_server/transport.rs`
- `crates/aileron-runtime/src/llama_server/stream.rs`

Use only a Unix-socket HTTP connector, disable proxy discovery and redirects, and make connect/startup timeouts explicit. Stream response bodies instead of buffering the whole generation. Keep SSE decoding independent of the HTTP client so chunk-boundary tests do not require inference.

The decoder must handle LF/CRLF, comments, event framing, split UTF-8, multiple events per read, and `[DONE]`. Enforce an explicit maximum buffered event size and fail on malformed JSON, oversized frames, or incomplete terminal streams. HTTP errors must preserve useful error detail without copying arbitrary full request bodies to diagnostics.

Test every byte boundary for representative multibyte and multi-event streams. Include a fake Unix HTTP server test that exercises real chunked transfer decoding and verifies the connector cannot redirect to TCP.

## 3. Own server startup and teardown

New file: `crates/aileron-runtime/src/llama_server/process.rs`.

Create a private temporary socket directory. Spawn the fixed server executable with the local model path, the Unix host path, one inference slot, bounded HTTP workers, context size, accelerator settings, and UI disabled. Confirm all command-line options against the pinned source. Do not accept arbitrary server arguments or external model URLs from requests.

Drain child output without exposing it on protocol stdout. Filter child diagnostics that would trigger Aileron's readiness detector. Poll `/health` until ready while checking child exit and a startup deadline; emit the adapter's readiness marker only after successful health response.

Handle stdin EOF, adapter termination, child failure, and write failure. Stop and reap the child; bound graceful shutdown before force-killing. Ensure the adapter's PID-1 behavior does not leave zombies. Keep cancellation based on daemon container termination rather than adding a new cancellation protocol.

Test with a fake child process for readiness, startup timeout, early exit, misleading readiness logs, normal EOF, and shutdown. Inspect the daemon's exact kill behavior before choosing signal handling dependencies.

## 4. Translate generation and terminal responses

New file: `crates/aileron-runtime/src/llama_server/response.rs`.

Wire the request loop to the server. Translate only answer content to `token` responses; retain separate internal reasoning deltas and discard them in the initial adapter. Do not concatenate reasoning into answers or structured results. Request upstream parsed reasoning explicitly rather than depending on ambiguous format defaults.

Collect finish reasons and usage, including usage-only final chunks. Emit one terminal response per request while stdout remains available. EOF before a valid upstream terminal sequence must produce an error, not successful completion. Include additive runtime terminal metadata and document whether current daemon consumers forward or ignore it.

For structured requests, parse only accumulated answer content as JSON. Preserve daemon-side schema validation. Emit complete structured snapshots using the existing protocol allowance; never emit incomplete JSON as a valid snapshot. A length-truncated invalid object is an error.

Use the fake server to test answer/reasoning separation, metadata ordering, empty output, truncation, structured completion, invalid JSON, upstream errors, and a child crash after partial output. Do not retry partially emitted requests automatically.

## 5. Package an opt-in runtime

Files:

- `runtimes/llm-vision-whisper/Dockerfile`
- `runtimes/llm-vision-whisper/entrypoint.sh`
- `runtimes/llm-vision-whisper/README.md`
- Relevant runtime build workflow under `.github/workflows/`, if its existing argument plumbing needs extension

Build server revision `5f55650a78f92aff4d48d671423e888fac0469ff` from verified source, with the same backend selection as the image. This replaces the initial `e6ab7c1a41054a888ada952eab4c886444c2f5ad` pin following OCR comparison testing; see [the comparison report](llama-server-comparison.md). Build the Rust adapter without linking native llama. Copy the server and required shared libraries. Record the server revision in image metadata. Keep native binaries available during rollout.

Introduce `AILERON_LLM_BACKEND=llama-server` as an explicit runtime launch option. The entrypoint must continue routing Whisper and existing vision workloads correctly. Validate unknown backend values instead of silently choosing a different backend. Profiles using embeddings or unsupported tools must stay on the native route until their migration work is complete.

Test entrypoint dispatch for default text, opted-in text, Whisper, and projector-bearing vision profiles. Build the CPU image first. Exercise hardware image builds through their existing workflow; report hardware execution separately from build success.

## 6. Validate the actual container boundary

Add integration coverage following the existing daemon container-test conventions and a documented opt-in real-model smoke procedure.

With a built CPU image and an available small local model:

1. Observe daemon readiness only after the server health endpoint succeeds.
2. Confirm no TCP listener and no host-visible socket mount.
3. Generate normal text and structured JSON through the daemon.
4. Exercise multiple sequential requests and idle teardown.
5. Close a session during startup and during streaming; confirm both adapter and server processes terminate.
6. Verify context overflow, output exhaustion, and child failure produce errors or accurate terminal metadata.

Run `cargo fmt --all -- --check`, `cargo test -p aileron-runtime --features llama-server`, and the affected daemon tests. Run `git diff --check`. Existing unrelated failures must be reported with enough detail to distinguish them from this change. Do not mark real inference, isolation, or hardware checks passed when the required environment is unavailable.

## 7. Document results and prepare reasoning API work

Update `docs/runtime-protocol.md` for adapter behavior and additive metadata. Update runtime documentation with the launch option, pinned server revision, supported operations, error behavior, and reproducible smoke commands. Record the tests actually run and any remaining rollout gates.

Use the validated server to settle the follow-on reasoning contract: exact model/template capability declarations, D-Bus versioning, optional thinking/effort values, reasoning opt-in events, and omitted-versus-explicit sampling defaults. Require the model conformance matrix from the approved design before advertising supported reasoning levels. Keep all profiles opt-in until their relevant checks pass.

## Completion criteria

The adapter works through the existing stdio boundary, uses a container-private Unix socket, preserves supported text/structured semantics, and has automated protocol/lifecycle coverage. The CPU image and real OCI smoke checks pass when the environment supports them; outstanding gates are explicitly recorded. No reasoning support is advertised merely because upstream accepts a request field.
