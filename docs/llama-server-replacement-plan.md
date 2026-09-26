# Full llama-server replacement implementation plan

Status: approved for sequential execution by the user; implementation in progress.

Design: [llama-server-replacement-design.md](llama-server-replacement-design.md),
approved by the user. This supersedes the earlier adapter-only implementation
plan.

## Execution and completion

Execute sequentially in this conversation, with the checkpoints below. The
deliverable is the full replacement, reasoning API, comparison runner, and
results report. The existing opt-in adapter is an intermediate implementation.

Preserve the native reference before changing inference behavior. Use the same
fixtures throughout implementation. Keep native source only until its reference
build and measurements are captured, then remove the production native llama
paths at cutover. Do not commit, push, or publish images unless requested.

Preserve the existing working-tree changes, including the lockfile, management
UI, vision-foundation work, and modified portal submodule. Inspect that
submodule's own diff before editing it. Load GLib C conventions before changing
its C implementation. Use its actual build layout and configured source path;
do not assume a `src/` directory or reuse an unrelated build tree.

## 1. Preserve the reference and add the comparison runner

Files:

- New `runtimes/llm-vision-whisper/bench/compare.py`
- New fixtures and workload manifest under `runtimes/llm-vision-whisper/bench/`
- `runtimes/llm-vision-whisper/README.md`
- New `docs/llama-server-comparison.md`

Build or identify the native reference image. Record its immutable image ID,
source provenance, lockfile hash, binding version, bundled llama.cpp revision,
model/projector hashes, and hardware configuration. Export a reusable local
reference artifact before native binaries are removed.

Implement a runner that starts isolated runtimes, waits for actual readiness,
sends JSON-lines requests, checks terminal responses, and records raw results.
Support separate correctness and timing runs. Record startup, first answer
token, total completion time, output counts, cgroup memory, PSS, available GPU
memory telemetry, and teardown latency. Record unavailable metrics as missing,
not zero. Do not treat streamed chunks as token counts.

Add fixtures for text/history, schemas, embeddings/retrieval, PNG/JPEG vision,
tool rounds, reasoning, invalid input, context limits, and cancellation. Run
the baseline first and record which features it actually implements. Keep
reference failures visible when evaluating new capabilities.

Checkpoint: a single documented command reproduces baseline results and emits
JSON plus Markdown. Use the approved five cold and twenty warm repetitions for
final performance runs, alternating implementations. Test the runner itself
against deterministic fake runtimes before trusting its measurements.

## 2. Resolve mixed generation/embedding and resource controls

Files:

- `crates/aileron-runtime/src/llama_server/process.rs`
- `runtimes/llm-vision-whisper/build-server.sh`
- If needed, narrowly scoped patches under `runtimes/llm-vision-whisper/patches/`

Exercise the pinned server with the same generation and embedding models as the
reference. Verify whether one loaded model can serve generation and correctly
pooled embedding requests without duplicate weight residency or reloads.

If separate contexts are required, add lazy embedding-context management
inside the server, sharing the loaded model. Avoid a second server containing
another full model copy. Measure the context allocation rather than assuming
sharing from filenames or mappings.

Verify background decode pacing and prompt-plus-output context reservation. If
the pinned server has no suitable control, implement the small server-side
extensions specified in the design:

- Per-request background pacing of 20 ms between generated tokens, including
  reasoning. Keep interactive requests unpaced and cancellation responsive.
- Context reservation using the actual rendered/tokenized prompt, including
  image tokens, with count-only context errors.

Apply patches with verification against the pinned revision and include their
hashes in runtime provenance. Test them with the real server. Sleeping in the
adapter's output loop is not a compute-throttling implementation.

Checkpoint: mixed-operation memory behavior, background compute control, and
context reservation have explicit passing probes before building on them.

## 3. Implement embedding parity and pipeline identity

Files:

- New `crates/aileron-runtime/src/llama_server/embeddings.rs`
- `crates/aileron-runtime/src/llama_server/{mod,transport,process}.rs`
- `crates/aileron-daemon/src/handlers/inference.rs`
- Runtime provenance plumbing in `crates/aileron-daemon/src/container.rs`

Translate `embed` requests using explicit mean pooling and no normalization.
Preserve the reference tokenization recipe and compare empty/short/long inputs.
Validate flat vector shape, dimensions, finite values, and terminal response.
Preserve recoverable context errors.

Include the actual engine revision, embedding recipe, and immutable runtime
image identity in `embedding_pipeline_id`. Verify that updating a mutable image
tag cannot preserve the old identity accidentally. Keep the app-facing vector
and identity fields compatible and document reindexing requirements.

Checkpoint: matched-recipe cosine similarity meets `0.9999`, retrieval fixture
changes are explained, and an embedding request followed by generation does
not reload or duplicate the model. Report any reference-model limitation.

## 4. Implement every llama vision path

Files:

- New `crates/aileron-runtime/src/llama_server/vision.rs`
- `crates/aileron-runtime/src/llama_server/{request,response,process}.rs`
- Vision conformance fixtures in the comparison suite

Load the mounted projector, translate canonical image parts and the legacy
`image` field, and implement `describe`, `ocr`, and `detect`. Preserve existing
prompt defaults and `VISION_PROMPT`, `VISION_OCR_PROMPT`, and
`VISION_DETECT_PROMPT` overrides. Inline local image bytes; never fetch remote
media URLs.

Exercise image-bearing text and structured requests as well as the dedicated
vision operations. Preserve empty OCR success and detection schema/bounds.
Validate missing, corrupt, incorrectly typed, unsupported, and oversized media.
Keep unsupported modalities explicit until tested. Reasoning text must not
enter OCR, detection JSON, or final-answer content.

Checkpoint: every previously working native vision operation passes the
corresponding real-model fixture and retains its documented output shape.

## 5. Complete native tools and session-owned continuations

Files:

- New `crates/aileron-runtime/src/llama_server/tools.rs`
- Runtime request and response types
- `crates/aileron-daemon/src/state.rs`
- `crates/aileron-daemon/src/handlers/{inference,sessions}.rs`
- `crates/aileron-daemon/src/container.rs`
- `docs/tool-calling.md`

Translate tool schemas and assemble streamed calls into complete objects.
Validate names and arguments JSON, preserve call IDs, and return the existing
tool-call response form. The app remains responsible for executing tools.

Keep pending assistant calls and their conversation history in daemon session
state. Forward complete assistant/tool messages on continuation. Reject unknown,
duplicate, stale, and cross-session result IDs. Preserve pending state across
runtime idle teardown when the session is still valid; clear it on session
close, cancellation, or completed conversation. Enforce bounded state and
validate new continuations before mutating it.

Test upstream tools-plus-schema behavior. If separate selection/final-answer
phases are necessary, share a single output budget and aggregate their usage.
Keep both phases cancellable and include them in benchmarks. Do not silently
reissue a tool or replay a partially delivered result after failure.

Checkpoint: real-model tool selection, app result submission, final schemas,
multiple calls, malformed arguments, and interleaved sessions work end to end.

## 6. Add reasoning capabilities and runtime events

Files:

- Model/profile capability declarations in the daemon
- `crates/aileron-runtime/src/lib.rs`
- `crates/aileron-runtime/src/llama_server/{request,response,mod}.rs`
- `docs/runtime-protocol.md`

Represent supported thinking modes and effort values for the exact model,
template, and runtime. An unverified combination must report unknown support,
not an optimistic universal list. Revalidate capabilities after profile or
runtime changes.

Add `thinking`, optional `reasoning_effort`, and `include_reasoning` to runtime
requests. Validate conflicting/unsupported settings. Emit separate reasoning
events only when requested; preserve answer events for existing consumers.
Carry finish reason and total token usage through completion and tool rounds.

Make default resolution explicit: existing entry points retain their current
option defaults; explicit thinking or effort controls use validated model/mode
sampling defaults when no temperature override is supplied. Requesting only
reasoning output does not change sampling defaults. Preserve whether an override
was absent instead of replacing absence with a number prematurely. Existing
explicit temperatures must reach the runtime, including paths that currently
validate but fail to forward them. Record that correction in comparison results.

Checkpoint: hybrid on/off, thinking-only rejection of off, documented GPT-OSS
efforts, default behavior, split reasoning markers, and exhaustion before an
answer pass with their respective real models. No arbitrary effort-to-budget
mapping.

## 7. Expose reasoning without changing existing public signatures

Files:

- `crates/aileron-varlink/varlink/aileron.Inference.varlink`
- `crates/aileron-portal/src/portal.rs`
- Public and implementation Language XML in `xdg-desktop-portal/data/`
- The corresponding portal frontend implementation and tests
- `docs/app-developer-guide.md`
- Demo controls for exercising capability-gated reasoning

Use the following contract. This refines the design using the actual public
XML, whose generation methods already have an options vardict.

### Public D-Bus Language interface, version 2

Keep `StreamResponse`, `StreamRespondGuided`, and
`StreamSubmitToolResultsGuided` argument/return signatures intact. Add these
optional keys to their existing `a{sv}` options:

- `thinking`: string, `auto`, `on`, or `off`.
- `reasoning_effort`: string, one of the session's advertised values.
- `include_reasoning`: boolean, default false.

Add asynchronous capability discovery:

```text
GetReasoningCapabilities(session_handle: o, options: a{sv}) -> handle: o
```

Use the standard request lifecycle so discovery can prepare the actual runtime
when necessary. Successful Request.Response results contain `thinking_modes`
as `as`, `reasoning_efforts` as `as`, and `reasoning_output` as `b`. Unsupported
or unverified controls have empty capability lists. Preserve request/session
ownership checks and cancellation during preparation.

Add the caller-scoped signal:

```text
ReasoningReceived(request_handle: o, session_handle: o, text: s)
```

Add `finish_reason` and a `usage` vardict to successful generation
Request.Response results when known. Usage contains signed 64-bit
`prompt_tokens`, `completion_tokens`, and `total_tokens`; unknown counts are
omitted. Existing token, snapshot, tool, and request-completion signals retain
their signatures and ordering.

### Backend D-Bus interface

Add `StreamResponse2`, `StreamRespondGuided2`, and
`StreamSubmitToolResultsGuided2`. Each has the same arguments as its existing
backend counterpart except that its final generation-options struct becomes
`a{sv}`. Retain the old backend methods as wrappers. Add
`GetReasoningCapabilities(request_handle: o, session_handle: o) -> ()` using
the backend request-completion mechanism. Forward the new reasoning signal and
completion metadata through the frontend.

If the selected backend is version 1, ordinary legacy requests continue to use
its original methods. An explicit new reasoning setting must fail as unsupported
rather than being silently dropped.

### Varlink interface

Retain current methods. Add the corresponding `StreamResponse2`,
`StreamRespondGuided2`, and `StreamSubmitToolResultsGuided2`, using typed options
with optional temperature/thinking/effort and a boolean reasoning-output flag.
Each streams a `GenerationEvent` with `kind` and optional `text`,
`snapshot_json`, `tool_calls`, `finish_reason`, and `usage` fields. Defined kinds
are `answer`, `reasoning`, `snapshot`, `tool_calls`, and `completed`. Exactly one
`completed` event terminates a successful request with `continues=false`;
tool-call completion uses `finish_reason=tool_calls`. Retain existing error
types for failed requests. Add capability discovery by session ID.

Checkpoint: existing app calls and a version-1 backend compatibility fixture
pass; version-2 reasoning, capability discovery, metadata, and cancellation
work through the public frontend. Add demo controls so the new behavior can be
judged without constructing raw D-Bus calls.

## 8. Run the approved comparison and fix regressions

Run the corpus against native reference and replacement on matched hardware.
Use CPU plus available accelerator hardware, and build all four advertised
image variants. Measure whole-container resources, including helper processes.
Exercise repeated start/idle/cancel cycles and background/interactive contention.

Apply the approved functional, cosine-similarity, latency, throughput, memory,
and teardown gates without adjusting thresholds to fit results. Record every
regression, even within a gate. Separate structural improvements, observed
quality improvements, and performance changes. Do not describe a different
underlying llama.cpp revision as pure transport overhead.

Checkpoint: populate `docs/llama-server-comparison.md` from raw results, with
provenance and reproducible commands. Fix failed gates or present the measured
tradeoff for a decision. Missing models/devices are named blockers for the
corresponding claims, never fabricated passes.

## 9. Switch the default and remove native production paths

Files:

- `runtimes/llm-vision-whisper/{Dockerfile,entrypoint.sh,README.md}`
- `crates/aileron-runtime/Cargo.toml` and `Cargo.lock`
- Native llama text/vision binaries and `llama_runtime.rs`
- Runtime manifests, build workflows, and documentation

Dispatch all GGUF text/vision work to the server adapter. Remove
`AILERON_LLM_BACKEND` and the native llama binaries/features/dependencies from
the production build. Preserve Whisper and the non-llama runtime dispatch.
Record the reference image digest as the rollback target. Remove obsolete
native-only settings or document their replacement explicitly; no ignored
configuration that appears to work.

Checkpoint: run the complete conformance suite against the final production
image, not just the transitional build. Confirm the image contains one llama
engine path and all intended operations remain callable without opt-in flags.

## 10. Final verification and report

Required Rust checks include:

```sh
cargo fmt --all -- --check
cargo test -p aileron-runtime --features llama-server
cargo test -p aileron-daemon -p aileron-portal -p aileron-varlink -p aileron-ipc
cargo clippy -p aileron-runtime --features llama-server --all-targets -- -D warnings
cargo clippy -p aileron-daemon -p aileron-portal --all-targets -- -D warnings
git diff --check
```

Run the portal frontend's configured build/tests, runtime Python tests, real
OCI conformance tests, and final comparison commands. Add model-free regressions
to normal CI and explicit jobs for packaged-runtime checks. Report device-backed
tests separately from compilation and fake-server tests.

Finish with the measured verdict against the approved gates, any remaining
downsides, image/protocol changes, embedding reindexing impact, and the commands
needed to repeat the comparison. Completion requires the default replacement
and reasoning controls, not another opt-in foundation milestone.
