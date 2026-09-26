# Full llama-server replacement and comparison

Status: approved by the user, 2026-09-24.

This replaces the staged, opt-in migration boundary in
[llama-server-design.md](llama-server-design.md). The user wants a complete
replacement that can be evaluated against the existing system, including its
costs. The earlier adapter and cancellation fixes are the starting point.

## Outcome

All existing llama-backed operations run through the private server adapter by
default. Users and apps do not select an inference backend. The production
image contains the server adapter and Whisper, with no native llama fallback.
Whisper, VITS, and the separate vision-foundation runtime keep their existing
engines because they are not llama-backed operations.

Deliver an executable comparison suite and a results report alongside the
replacement. A passing build or a successful chat request is insufficient.
Report regressions and added dependencies as well as improvements. Unsupported
hardware tests remain explicitly unvalidated, rather than receiving a pass.

Include the original reasoning-level goal in the completed system, using
capabilities of the actual model/template/runtime combination.

## Exact compatibility scope

The current native request dispatchers expose these operations:

| Operation | Replacement requirement |
| --- | --- |
| `generate` | Role-aware text and supported image input, incremental answer events, total token cap |
| `generate_structured` | Schema-constrained final JSON, existing result and error forms |
| `generate_structured_stream` | Schema-valid snapshots and terminal marker |
| `embed` | One flat vector, explicit pooling/normalization, correct pipeline identity |
| `describe` | Existing image and prompt inputs, description output |
| `ocr` | Existing image and prompt inputs, including successful empty text |
| `detect` | Existing normalized bounding-box schema and validation |
| Guided tools and tool results | Actual tool-call generation, IDs, continuation, and a schema-constrained final answer |

There is no standalone runtime `complete` request in the current native
dispatcher. `generate_completion` is an internal helper used by chat generation.
The earlier claim that a separate public completion operation needed migrating
was incorrect.

Native llama currently accepts tool definitions without generating native tool
calls, and renders tool results as ordinary prompt text. Preserve functioning
guided requests and implement the existing app-facing tool contract fully;
measure that as added capability rather than falsely claiming baseline parity.

Preserve model/projector paths, CPU thread and GPU-offload settings, context
limits, per-operation token defaults, explicit temperatures, prompt overrides,
background execution, cancellation, and model-loading behavior. Preserve useful
error classifications, including invalid images, unsupported modalities,
schema failures, and context-window errors. Request-validation failures must
not unnecessarily unload a usable model.

## Integration choice

Use one server-backed adapter for all llama operations. Retain the current
private Unix-socket boundary, container isolation, and single active request
per loaded runtime.

The alternatives are retaining separate native engines for embeddings/vision,
or maintaining the whole chat/template/parser stack through a C++ bridge.
The first prevents a full replacement; the second duplicates more upstream
integration work. The selected server approach must account for its extra
process, HTTP framing, worker threads, build size, and any downstream patches
in the comparison report.

Do not load duplicate copies of model weights merely to switch between
generation and embeddings. First verify mixed-operation support in the pinned
server. If its fixed pooling/context setup cannot provide the required
behavior, implement a narrow server-side context-management extension that
shares the loaded model and creates the embedding context lazily. A second
full model instance or repeated model reloads do not count as parity.

## Embeddings

The native implementation uses mean pooling, special-token-aware tokenization,
and no explicit vector normalization. Upstream exposes embedding requests and
`embd_normalize`, but its defaults are not assumed equivalent.

Use explicit mean pooling and disabled normalization for compatibility tests.
Check tokenization, vector dimensions, finite values, norms, cosine similarity,
and retrieval results. Do not silently switch to model-default pooling or
normalized vectors during migration. Such improvements require a distinct
embedding-pipeline identity and separate measurements.

Strengthen `embedding_pipeline_id` to include the actual engine revision,
embedding recipe, and immutable runtime image identity. The current hash
includes the image reference string; a mutable tag can remain unchanged after
an engine update. Previously persisted vectors must not be represented as
compatible merely because that tag is unchanged. Document when reindexing is
required.

## Images and guided output

Translate base64/byte-array image inputs into local, inline server media inputs.
Load the configured projector from `/model`; do not introduce remote URL
fetches. Preserve existing description, OCR, detection prompts and override
precedence. Test both dedicated vision requests and images inside canonical
generation messages.

Support every currently accepted PNG/JPEG path and explicitly reject invalid
or unsupported media. Additional formats, multiple images, or audio are only
advertised after their tests pass. Preserve empty OCR as a valid result.

Enforce schemas on final answers, not reasoning text. Keep daemon-side
validation for all returned objects and snapshots. Test the exact schema subset
already supported by the daemon, including bounds and `additionalProperties`.

## Tools and conversation ownership

Translate Aileron's tool definitions to native server tool schemas. Assemble
streamed call arguments before emitting complete `ToolCall` objects. Preserve
call IDs, validate the response shape and declared tool names, and keep tool
execution app-owned.

The daemon must retain outstanding assistant tool calls and their conversation
context per session. Continuation currently supplies IDs and results without
the assistant tool-call messages required by native chat templates. Forward
that complete history to the adapter; do not infer it from a shared runtime's
last request. Bound pending state and clear it on session close, cancellation,
or completion. Reject cross-session and unknown call IDs explicitly.

Test tool selection and final-answer schemas together. If upstream's combined
constraint path cannot honor both, use separate tool-selection and constrained
final-answer inference phases. All phases share one caller-visible output
budget and usage accounting, remain cancellable, and appear in latency
measurements. A phase split must not silently double the requested budget.

## Reasoning and API compatibility

Expose `thinking=auto|on|off` and optional, model-supported `reasoning_effort`.
GPT-OSS's documented effort levels are `low|medium|high`; hybrid thinking
controls and thinking-only models are separate capabilities. Reject unsupported
explicit settings. Defaults leave model/template choices intact, with sampling
defaults selected deliberately for the validated profile and mode.

Return parsed reasoning through an opt-in event channel; existing consumers
receive final-answer content. Expose finish reasons and total usage so callers
can distinguish a completed answer from exhaustion during reasoning. Do not
map effort levels to arbitrary token budgets.

The public generation methods already accept extensible option dictionaries.
Expose the additional options under interface version 2 without changing their
signatures. Add version-2 backend entry points to replace the fixed backend
option structs, plus session reasoning-capability discovery. Keep the existing
backend methods as compatibility wrappers. Apply the same semantics through
the portal frontend, backend, Varlink, and runtime. The implementation plan
specifies the additional method and signal signatures before coding.

## Background execution and context budgets

The old decode loop inserts a 20 ms delay per generated token for background
requests. The current adapter forwards no equivalent compute throttle.
Sleeping while forwarding output would only delay display, not inference, and
is not an acceptable replacement.

Preserve daemon scheduling/preemption and implement server-side decode pacing
for background requests. Prefer an existing upstream control if verified;
otherwise carry a minimal pinned patch for per-request decode delay. Keep the
interactive path unthrottled. Measure inference CPU/GPU activity and foreground
responsiveness to verify the policy, including reasoning tokens.

Preserve prompt-plus-requested-output context checks, including image tokens.
The native runtime rejects a request whose reserved output budget cannot fit;
server behavior that instead fills the context and returns an incomplete answer
must not be mistaken for identical behavior.

## Comparison suite

Build and preserve a native reference image before removing native code. Pin
its model hashes, image ID/digest, binding version, and bundled llama.cpp SHA.
Record the replacement's corresponding provenance and all downstream patches.
Keep the reference image as a test artifact, not a selectable production engine.

Run the same corpus through both stdio interfaces in equivalent OCI isolation.
Use identical model/projector artifacts, thread count, GPU placement, context
size, output limits, and sampling settings for parity runs. Run improved
model-specific defaults as a separate experiment. Separate engine-version
changes from adapter overhead wherever a version-matched control is possible;
otherwise report that attribution limit explicitly.

Include a non-reasoning instruct model, a hybrid reasoning model, a thinking-only
model, an embedding model, a vision model, and a tool-capable model. Existing
local models can cover several roles. GPT-OSS is required before claiming its
effort levels work. Use fixed, inspectable prompt/image/retrieval fixtures.

Record machine-readable per-request results and a Markdown comparison:

- Functional outcomes, schema validity, tool IDs/arguments/continuations, error
  codes, token caps, reasoning separation, and cross-session isolation.
- Cold readiness time, warm time to first answer token, total latency,
  generated tokens/second, and embedding throughput.
- Peak/idle cgroup memory, process PSS, GPU memory, process/thread counts,
  post-cancellation resource release, and image size.
- Background compute utilization and foreground preemption latency.

Use at least five cold runs and twenty warm runs per representative workload,
alternating backend order. Report median and p95, individual results, and
run-to-run spread. Keep prompt-cache reuse disabled for baseline runs and
measure it separately if enabled later. Compare semantic task outcomes rather
than requiring byte-identical prose from different template implementations.

## Approved acceptance gates

These numeric budgets were approved with the design. Every measured regression is
reported even if it falls inside a budget.

- All currently functioning operations and app-facing contracts pass. No
  silent native fallback, skipped modality, or falsely successful request.
- No unexplained functional or retrieval-quality regression. With matched
  tokenization and embedding recipe, require cosine similarity at least
  `0.9999` and investigate ranking changes in the retrieval fixture corpus.
- Warm median latency/throughput regression at most 5%; p95 latency regression
  at most 10%, measured on matched workloads and justified against noise.
- Cold readiness regression at most the larger of 10% or 250 ms.
- Peak host/GPU memory regression at most the larger of 5% or 64 MiB. Count all
  adapter/server resources, not just the model process.
- Cancellation releases processes and resources within two seconds, and no
  worse than the reference beyond 100 ms measurement tolerance. Repeated
  idle/start/cancel cycles leave no accumulating resources or OCI state.
- CPU/CUDA/ROCm/Vulkan builds pass. Hardware inference results are reported
  separately for each tested device; missing device coverage is visible.

If a gate fails, fix the cause or present the measured tradeoff for a decision.
Do not revise thresholds after seeing results simply to produce a passing
report. Benefits such as reliable tools and structured output are demonstrated
by tests, not inferred from upstream feature lists.

## Cutover and completion

After conformance and comparison, make the server adapter the only production
llama path, remove `AILERON_LLM_BACKEND`, remove native text/vision binaries and
their binding dependencies, and update image manifests, documentation, and CI.
Use the previous image digest for rollback rather than carrying two inference
implementations indefinitely.

Completion includes the reasoning API, complete operation coverage, a default
server-based image, the reproducible comparison suite, and its results report.
The opt-in adapter alone no longer satisfies the requested scope.
