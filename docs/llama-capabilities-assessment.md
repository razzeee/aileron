# llama.cpp capabilities assessment

Research date: 2026-09-23. This is a source assessment, not an inference benchmark. "Verified" below means the inspected source implements the behavior. It does not mean every model follows the requested behavior or that Aileron's runtime exposes it.

## Summary

Current upstream llama.cpp has distinct controls for enabling thinking, requesting a model-specific effort level, limiting a reasoning block, and separating reasoning from the answer. Aileron currently exposes none of these controls through its llama runtime. It uses `llama-cpp-2` and its own decode loop, not `llama-server`. Adding server-style JSON fields alone would not implement the features.

The recommended first scope is template-aware thinking on/off, separate reasoning and answer events, total-output accounting, and model-appropriate sampling defaults. Add effort only for models with documented levels. Treat reasoning budgets as a subsequent sampler integration, with explicit semantics for forced endings and repeated reasoning blocks. Native tool calling and reliable schema-constrained final answers should share the same template/parser work.

## Revisions and dependency provenance

| Baseline | Inspected version or revision | Meaning |
| --- | --- | --- |
| Aileron checkout | `7b03b0877e7c656e40a5b364e3243d2e62611280` plus existing working-tree changes | Local source used for this assessment. |
| Runtime manifest | `llama-cpp-2 = { version = "0.1.150", optional = true }` | Cargo-compatible version requirement, not an exact `=0.1.150` pin. |
| Committed `Cargo.lock` | `llama-cpp-2` and `llama-cpp-sys-2` **0.1.151** | Committed reproducible dependency baseline. |
| Existing local `Cargo.lock` | Both crates **0.1.154** | Already modified before this research. The local runtime build would use this version. |
| Bundled llama.cpp in 0.1.151 | `9e3b928fd8c9d14dbf15a8768b9fdd7e5c721d66`, 2026-06-07 | Resolved through the published crate's VCS metadata and the binding repository's submodule entry. |
| Bundled llama.cpp in 0.1.154 | `5f55650a78f92aff4d48d671423e888fac0469ff`, 2026-07-30 | Same provenance method. |
| Current upstream HEAD observed | `e6ab7c1a41054a888ada952eab4c886444c2f5ad`, 2026-09-22 | All "current upstream" claims refer to this exact commit, not a moving `master`. |

Sources: [runtime Cargo manifest](../crates/aileron-runtime/Cargo.toml), [local lockfile](../Cargo.lock), [published 0.1.151 source archive][crate151], [published 0.1.154 source archive][crate154], binding trees [0.1.151][binding151] and [0.1.154][binding154], and upstream commits [June][up151], [July][up154], [current][head]. The published sys-crate archives' SHA-256 values matched their respective lockfile checksums: `23c676b02f8f2bdd9d7df7bcca0bd176b27964440998e7aa31c6fe32552964d0` and `13a9ea2ce0cdc20bcb1870534022e340b391663f8fe09133951e2fe37fbc29cf`.

The [runtime Dockerfile](../runtimes/llm-vision-whisper/Dockerfile) copies `Cargo.lock` and builds with `--locked`. Its [entrypoint](../runtimes/llm-vision-whisper/entrypoint.sh) selects Aileron's Rust text, vision, or Whisper executable. The sources do not establish which revision an already-installed OCI image contains. Deployment compatibility must use the actual image identity/build provenance.

## What Aileron implements today

Verified in [shared llama runtime](../crates/aileron-runtime/src/llama_runtime.rs), [text executable](../crates/aileron-runtime/src/bin/llm_llama_cpp.rs), [vision executable](../crates/aileron-runtime/src/bin/vision_llama_cpp.rs), and [request types](../crates/aileron-runtime/src/lib.rs):

- `Request` has `max_tokens`, `temperature`, structured-output fields, tools/results, and typed input, but no reasoning, template-kwargs, seed, stop-sequence, or sampling-tuning fields. Unknown JSON fields are not captured for forwarding.
- `render_chat_prompt` creates one system message and one user message, retrieves the model template, and calls `LlamaModel::apply_chat_template`. It falls back to plain `System: ... User: ... Assistant:` text if template application fails. Text generation uses the rendered `prompt`, rather than passing the canonical message array to a role-aware chat renderer.
- Both generation loops sample tokens and send their decoded pieces directly to the callback. There is no reasoning parser or separate reasoning channel. The total `max_tokens` loop includes any generated reasoning and delimiters. It stops at end-of-generation or the token limit, without reporting which occurred.
- The usual text-generation defaults are 512 tokens and temperature zero. Structured generation defaults to 1,024 tokens and uses temperature zero. Nonzero temperature uses fixed `top_k=40`, `top_p=0.95`, and seed `1234`.
- The runtime clears the KV cache for each new text completion. Context defaults to 4,096 and rejects a prompt plus requested maximum output that will not fit.
- Structured generation adds schema instructions, then extracts the first JSON value from generated text. Grammar sampling is opt-in via `AILERON_LLAMA_GRAMMAR=1`, because the code records aborts with some valid schemas. Grammar setup failure logs an error and continues without it. The [protocol](runtime-protocol.md) separately requires daemon-side schema validation. Do not describe this runtime as always grammar-constrained.
- The text executable accepts tool definitions in its request structure but does not use them to render native tool schemas or produce tool-call events. It appends tool results as plain text for structured requests. The protocol's tool-call contract is broader than this implementation.
- The vision generate path rejects audio input and multiple images. It shares the text-generation/template helpers; upstream server multimodal support does not establish equivalent Aileron support.

## Reasoning controls in current upstream

### Thinking on/off and template kwargs

Verified current chat-completions request syntax:

```json
{
  "messages": [{"role": "user", "content": "Explain the result briefly."}],
  "chat_template_kwargs": {"enable_thinking": false}
}
```

`enable_thinking` is a boolean inside `chat_template_kwargs`, not a universal top-level libllama argument. The server merges startup template kwargs with request kwargs, with request values overriding matching startup keys. It reads the boolean to update chat-template inputs and rejects a string such as `"false"`. Startup `--reasoning on|off|auto` controls the default; auto relies on template support. The current server uses Jinja by default. [Server docs][server], [request conversion][request], [server initialization][context]

Current upstream also handles top-level `"reasoning_effort":"none"` by disabling its thinking input and removing the effort kwarg. This is a server compatibility convention, not a model-independent promise of a supported non-thinking mode. Avoid sending contradictory controls: template kwargs are also merged into the template's execution context, and conflicting aliases need dedicated validation. [Request conversion][request], [template execution][chat]

### Effort levels

Verified current behavior:

- Top-level nonempty `reasoning_effort` values other than `none` become the Jinja `reasoning_effort` kwarg. The server does not translate `low`, `medium`, or `high` into fixed token counts.
- `--reasoning-effort default` removes the startup effort kwarg, allowing the template default. Other strings are passed through. The help lists `minimal`, `low`, `medium`, `high`, `xhigh`, and `max` as examples, not a universally validated enum.
- CLI `default` has explicit unset semantics. The request conversion does not give top-level `"reasoning_effort":"default"` the same special handling; it forwards that literal string. Omit the request field to inherit defaults.
- The common template layer also maps the effort input to the `reasoning_strength` alias. Its `supports_reasoning_effort` capability probe checks whether a template uses an effort variable when supplied `low`; it does not enumerate meaningful levels or establish output quality.

Sources: [request conversion][request], [CLI option handling][args], [template capability detection][caps]. See the model table below before exposing levels in an application API.

### Token budgets

Current upstream chat-completions conversion recognizes:

```json
{
  "reasoning_budget_tokens": 256,
  "reasoning_budget_message": "Conclude the analysis and answer now."
}
```

These are llama.cpp extensions. `thinking_budget_tokens` is a fallback alias. `reasoning_budget_tokens` takes precedence when supplied. A resolved value of `-1` inherits the server's startup budget, so a request containing `-1` does not necessarily override a finite server default with an unlimited budget. Startup `--reasoning-budget` accepts `-1` for unrestricted, `0` for immediate ending, and positive token budgets. [Request conversion][request], [CLI options][args]

The common reasoning-budget sampler tracks template-derived start/end token sequences and forces an optional message plus an end-of-thinking sequence when the budget expires. It needs usable delimiters; the chat conversion only forwards budget settings when the template handler supplies end tags, and sampler initialization checks both start and end sequences. It also consumes prefilled tokens so a thinking block opened by the prompt can be recognized. [Common sampler][sampling], [budget implementation][budget]

Important limits verified in the current implementation:

- It resets the budget when another reasoning block begins. This is a per-block budget, not a strict aggregate reasoning-token cap for an entire response.
- It may wait for a UTF-8 sequence to complete and then emit forced tokens. The named budget is not an exact maximum on every token attributable to reasoning and its closing syntax.
- The overall output/context limit still applies. Forcing an ending cannot guarantee that enough output budget remains for a useful final answer.
- Budget zero is a forced transition mechanism. It is distinct from rendering the model's supported non-thinking prompt.
- A template kwarg named `thinking_budget`, as used by Seed-OSS, changes the prompt. It is not the same as sampler-enforced `reasoning_budget_tokens`.

The server also supports `reasoning_control:true` and a concurrent `/v1/chat/completions/control` request with `action:"reasoning_end"` and the completion ID. This can request an early transition to the answer. Aileron's sequential stdio request handling has no corresponding control path. [Server control API][server], [budget state machine][budget]

### Separating reasoning from the answer

Verified upstream formats:

| Format | Behavior |
| --- | --- |
| `none` | Leaves reasoning unparsed in raw content. It does **not** disable reasoning. |
| `deepseek` | Extracts reasoning into `message.reasoning_content` and leaves the answer in `message.content`. The format name does not restrict use to DeepSeek models. |
| `deepseek-legacy` | Also keeps thinking tags/content in the content representation for compatibility. |
| `auto` | The server default. Rely on the selected template/parser, rather than treating it as an explicit cross-model output contract. |

The common chat representation carries separate reasoning/content fields and deltas; the server serializes the parsed message and streaming deltas. The Responses adapter uses reasoning output items, while the Anthropic adapter uses thinking blocks. Aileron would need an explicit protocol/IPC representation rather than mixing these into ordinary token events. [Server docs][server], [chat structures][chat-h], [response serialization][task]

Parsing must account for the generation prefix already present in the prompt. For example, a model can start its returned text inside a thinking block and emit only the closing marker. A regex that waits for a generated `<think>` opening tag will miss that case. Different models also use different delimiters or channels. History preservation is another independent choice: current upstream offers `--reasoning-preserve` and template capability detection, but not every template retains reasoning in the same way. [Chat inputs/parser parameters][chat-h], [Qwen thinking-only model card][qwen-thinking], [capability aliases][caps]

## Model and template support

These are examples grounded in the inspected templates and first-party model cards, not an exhaustive supported-model list. Template behavior alone is not a quality guarantee. GGUF exporters can embed different template revisions.

| Model/template | Verified control | What not to assume |
| --- | --- | --- |
| Original Qwen3 hybrid models, represented by Qwen3-0.6B | `enable_thinking=false` inserts an empty thinking prefix; the model card documents both modes. `/think` and `/no_think` are model-specific soft prompt switches when thinking is enabled. [Template][t-qwen], [model card][qwen] | No low/medium/high effort implementation in this template. Soft prompt text is not a universal runtime control. |
| Qwen3-4B-Thinking-2507 | First-party card explicitly says thinking-only; the prompt opens `<think>`. [Model card][qwen-thinking] | Do not infer a non-thinking mode from the Qwen3 family name or assume `enable_thinking=false` works. |
| Qwen3.5-4B | Inspected template branches on `enable_thinking` and either opens thinking or inserts an empty thinking block. [Template][t-qwen35] | Other Qwen3.5 exports and quality under forced budgets still need testing. |
| GPT-OSS | Template inserts `Reasoning: <effort>`, defaulting to `medium`. OpenAI documents `low`, `medium`, and `high`. It uses analysis/final channels. [Template][t-gpt], [model card][gpt] | Accepting arbitrary strings does not establish support for `minimal`, `xhigh`, `max`, or a true off mode. The inspected template has no `enable_thinking` branch. |
| DeepSeek R1 distills | The inspected upstream R1-Distill-Qwen template opens `<think>` and can immediately close it when thinking is disabled. The alternative `llama-cpp-deepseek-r1.jinja` opens thinking without that branch. [Distill template][t-r1], [alternative][t-r1-alt] | A template prefill workaround is not evidence of a trained hybrid mode. Even upstream's two templates differ. |
| DeepSeek V3.1 | `thinking` falls back to `enable_thinking`; it chooses open or empty thinking prefixes. [Template][t-v31] | Its alias and defaults need deliberate mapping; it has no generic effort scale in the inspected template. |
| DeepSeek V4 | Thinking toggle plus a special `reasoning_effort == 'max'` prompt addition when thinking is enabled. [Template][t-v4] | This does not imply GPT-OSS-style low/medium/high distinctions. |
| GLM-4.7-Flash | `enable_thinking` controls the generation prefix; `clear_thinking` affects retained history. [Template][t-glm] | Output parsing, current-turn mode, and historical trace preservation are separate capabilities. |
| Seed-OSS | `thinking_budget` produces budget instructions/reflection intervals; zero adds a skip-thinking prefix. [Template][t-seed] | Prompt-level budget instructions are not a hard token counter. |
| Gemma 4 31B IT | `enable_thinking` adds a system thinking token; reasoning uses a thought channel with different markers. [Template][t-gemma] | A parser that only understands `<think>...</think>` is insufficient. |

Qwen's first-party guidance also makes sampling defaults high priority. Original Qwen3 recommends temperature `0.6`, top-p `0.95`, top-k `20`, min-p `0` for thinking and explicitly advises against greedy decoding. Aileron's current zero-temperature default and fixed sampling values do not implement those recommendations. This is evidence for profile-specific defaults, not a claim that one setting is best for every model. [Qwen model card][qwen]

## Native library versus server, including the pinned versions

The public `llama_chat_apply_template` C API accepts a template, role/content messages, and an assistant-prefix flag. Its header explicitly says it does not use a Jinja parser and only supports predefined templates. The inspected Rust `apply_chat_template` method calls that function directly. It has no kwargs, effort, reasoning-output, or budget parameter. [Public C API][c-api], [Rust binding][rust-model]

The richer functionality is reusable native C++ code in `common/`: Jinja rendering, template capabilities, chat parsers, grammar handling, and the reasoning-budget sampler. Therefore it is not inherently HTTP-only. However, `llama-cpp-sys-2`'s inspected C wrapper does not expose the common chat renderer/parser or reasoning-budget controls. Building/linking `common` for schema conversion does not expose all of its C++ APIs to Rust. The sys crate disables server builds. Aileron must add bindings/its own implementation or adopt a server adapter. [Common chat API][chat-h], [binding wrapper][wrapper], [binding build][build]

The bundled revisions differ materially from current HEAD:

| Behavior in bundled upstream source | 0.1.151 / June revision | 0.1.154 / July revision | Current HEAD |
| --- | --- | --- | --- |
| Request `chat_template_kwargs` and boolean thinking input in common/server code | Present | Present | Present |
| Top-level `reasoning_effort` in chat request conversion | No special handling in inspected conversion | Handles `none`; explicitly says other values are not yet handled | `none` disables; other nonempty values forwarded to template |
| Effort via `chat_template_kwargs.reasoning_effort` | Generic kwargs path exists; requires a consuming template | Same | Same, plus current effort alias/capability support |
| CLI `--reasoning-effort` | Absent in inspected option source | Absent in inspected option source | Present |
| Request reasoning budget | `thinking_budget_tokens` used only if startup budget is unrestricted | `reasoning_budget_tokens`, fallback alias, and request message override | Same request mapping, with current sampler behavior described above |
| Common chat and budget controls wired into Aileron | No | No | Upgrading the underlying source alone would still not wire them in |

Sources: bundled request conversion [June][request151] and [July][request154], bundled option handling [June][args151] and [July][args154], and [current conversion][request]. The per-block reset and UTF-8 details above were verified at current HEAD; do not assume identical behavior at both older revisions without comparing their sampler implementations.

## Other high-value request features

Priority reflects Aileron's current gaps and existing use cases. These are recommendations, not implemented Aileron capabilities.

| Priority | Feature | Upstream support and Aileron work |
| --- | --- | --- |
| High | Correct message history, assistant continuation, and template capability reporting | Common chat supports rich messages, tool results, generation prefixes, and continuation. Preserve roles instead of flattening into a user prompt. Report support for the actual artifact/template/runtime combination. [Chat API][chat-h] |
| High | Reliable structured final answers | Upstream offers JSON Schema-to-grammar and lazy grammar activation. Integrate reasoning-aware constraints instead of forcing JSON on the reasoning block. Resolve Aileron's opt-in grammar limitation and keep daemon validation. Upstream supports only a schema subset and documents silently skipped unsupported keywords. [Grammar guide][grammar], [sampler/grammar coordination][sampling] |
| High | Native tool calling and `tool_choice` | Upstream renders tool schemas, parses tool calls, and handles auto/required/none; parallel calls depend on template support. Aileron's protocol already has app-mediated calls, but its llama implementation needs the generation/parsing part. Upstream rejects custom grammar with active tools in request conversion, so test Aileron's combined tools-plus-final-schema contract explicitly. [Tool documentation][tools], [request conversion][request] |
| High | Sampling profiles and caller overrides | Expose seed, top-k/top-p/min-p, and selected repetition/presence/frequency controls with profile defaults. The current Rust binding already has sampler constructors including penalties, min-p, seed, and logit bias. Respect sampler ordering and avoid claiming seed reproducibility across backends/batch layouts. [Rust samplers][rust-sampling], [server sampling/cache docs][server] |
| High | Stop sequences, finish reasons, usage, and timings | Server requests support stop strings; responses distinguish length from normal stop/tool calls and include token usage/timings. Aileron can implement accounting in its own loop without a server migration. Buffer partial stop matches so stop text is not prematurely streamed. Keep a total-output cap even when introducing reasoning budgets. [Server docs][server], [response serialization][task] |
| Medium | Prompt/KV reuse | `cache_prompt` reuses a matching prefix upstream. Aileron currently clears KV for each completion. Evaluate reuse for repeated system prompts and multi-turn requests, with explicit ownership/lifetime and exact token-prefix matching. Upstream notes caching can change reproducibility because batching changes logits. [Server docs][server] |
| Medium | Better embedding controls and reranking | Upstream supports selectable/model-default pooling, normalization, and a reranking endpoint. Aileron currently chooses mean pooling and returns the sequence embedding without explicit normalization. Match the embedding model's required recipe and include changes in its existing embedding-pipeline identity. Reranking requires an appropriate model and a new task contract. [Server docs][server], [local embedding protocol](runtime-protocol.md#embeddings) |
| Later | Log probabilities, per-request LoRA selection, richer multimodal input, and infill | Useful for diagnostics, specialization, vision applications, and code editing, but each needs a concrete caller and compatibility tests. Current source notes chat logprobs compatibility limitations and rejects logprobs with tools plus streaming. LoRA selection also affects batching. [Request conversion][request], [server docs][server] |

Speculative decoding, flash attention, KV quantization, batch sizing, and accelerator placement are also valuable, but mostly belong to runtime/profile configuration and benchmarking rather than general application request fields. Their appearance in server help does not imply they can be toggled safely inside Aileron's existing loaded context. [Server configuration][server]

## Recommended delivery scope and caveats

1. Establish an explicit dependency/image baseline. Record the bundled llama.cpp SHA in build provenance. Choose whether to add a small native C++ bridge for `common` or use a pinned server adapter behind Aileron's stdio protocol. Compare these integration approaches before defining a broad public request API.
2. Add a capability-gated thinking setting with `auto`, `on`, and `off` semantics. Resolve it against the exact template and artifact. Reject unsupported explicit settings rather than silently presenting them as successful. Keep model-specific kwargs internal or restricted to validated profile configuration initially.
3. Add separate reasoning and answer events, parser state initialized from the generation prefix, finish reasons, and total token usage. Existing answer consumers should receive answer text through the established path. Make trace retention/output an explicit independent setting. Test plain text, structured generation, tool calls, and image generation.
4. Add model-specific effort choices. Start with verified GPT-OSS `low|medium|high` and any other explicitly validated model profiles. Preserve template defaults when unspecified. Do not map effort to arbitrary token counts or present every upstream help example for every model.
5. Add budgets after the template/parser integration is reliable. Define whether Aileron offers upstream-compatible per-block budgets or a separate aggregate limit. Document forced closing-token overhead, overall output limits, unsupported delimiters, and behavior when truncation happens before an answer. Keep manual "finish thinking" separate from request cancellation.
6. Add sampling overrides, native tools, and dependable schema constraints alongside that work. Reuse the same chat representation and parser rather than building incompatible paths for normal answers, tool calls, and reasoning.

Before claiming support, run a small conformance matrix against pinned model/template pairs: a Qwen3 hybrid, a thinking-only Qwen checkpoint, GPT-OSS, a non-reasoning instruct model, and any advertised vision model. Cover default/off/on, documented effort levels, zero/small/unlimited budgets, split delimiters across streaming chunks, prompt-prefilled thinking, multiple reasoning blocks, total-token exhaustion, JSON inside reasoning, tools plus schemas, and multi-turn history. This assessment did not download weights, compile a runtime, or run those inference tests. Actual model quality, installed image versions, and unsupported-control behavior remain unverified.

## Primary-source references

All llama.cpp code links below are pinned to the inspected revision. Model-card links are first-party moving documents read on the research date.

[head]: https://github.com/ggml-org/llama.cpp/commit/e6ab7c1a41054a888ada952eab4c886444c2f5ad
[up151]: https://github.com/ggml-org/llama.cpp/commit/9e3b928fd8c9d14dbf15a8768b9fdd7e5c721d66
[up154]: https://github.com/ggml-org/llama.cpp/commit/5f55650a78f92aff4d48d671423e888fac0469ff
[crate151]: https://static.crates.io/crates/llama-cpp-sys-2/llama-cpp-sys-2-0.1.151.crate
[crate154]: https://static.crates.io/crates/llama-cpp-sys-2/llama-cpp-sys-2-0.1.154.crate
[binding151]: https://github.com/utilityai/llama-cpp-rs/tree/7f0a0d95514aebe86efab16527745852ee72931c/llama-cpp-sys-2
[binding154]: https://github.com/utilityai/llama-cpp-rs/tree/bed81ad4ab1a6c904b11d425608e50f976d8ea62/llama-cpp-sys-2
[rust-model]: https://github.com/utilityai/llama-cpp-rs/blob/bed81ad4ab1a6c904b11d425608e50f976d8ea62/llama-cpp-2/src/model.rs#L917-L993
[rust-sampling]: https://github.com/utilityai/llama-cpp-rs/blob/bed81ad4ab1a6c904b11d425608e50f976d8ea62/llama-cpp-2/src/sampling.rs
[wrapper]: https://github.com/utilityai/llama-cpp-rs/blob/bed81ad4ab1a6c904b11d425608e50f976d8ea62/llama-cpp-sys-2/wrapper_common.h
[build]: https://github.com/utilityai/llama-cpp-rs/blob/bed81ad4ab1a6c904b11d425608e50f976d8ea62/llama-cpp-sys-2/build.rs#L648-L663
[server]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/tools/server/README.md
[request]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/tools/server/server-common.cpp#L1317-L1409
[context]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/tools/server/server-context.cpp#L1450-L1505
[args]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/common/arg.cpp#L3703-L3735
[chat]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/common/chat.cpp
[chat-h]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/common/chat.h
[caps]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/common/jinja/caps.cpp
[sampling]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/common/sampling.cpp
[budget]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/common/reasoning-budget.cpp
[task]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/tools/server/server-task.cpp
[c-api]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/include/llama.h#L1221-L1237
[request151]: https://github.com/ggml-org/llama.cpp/blob/9e3b928fd8c9d14dbf15a8768b9fdd7e5c721d66/tools/server/server-common.cpp#L1061-L1137
[request154]: https://github.com/ggml-org/llama.cpp/blob/5f55650a78f92aff4d48d671423e888fac0469ff/tools/server/server-common.cpp#L1069-L1141
[args151]: https://github.com/ggml-org/llama.cpp/blob/9e3b928fd8c9d14dbf15a8768b9fdd7e5c721d66/common/arg.cpp
[args154]: https://github.com/ggml-org/llama.cpp/blob/5f55650a78f92aff4d48d671423e888fac0469ff/common/arg.cpp
[tools]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/docs/function-calling.md
[grammar]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/grammars/README.md
[qwen]: https://huggingface.co/Qwen/Qwen3-0.6B/blob/main/README.md
[qwen-thinking]: https://huggingface.co/Qwen/Qwen3-4B-Thinking-2507/blob/main/README.md
[gpt]: https://huggingface.co/openai/gpt-oss-20b/blob/main/README.md
[t-qwen]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/models/templates/Qwen-Qwen3-0.6B.jinja
[t-qwen35]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/models/templates/Qwen3.5-4B.jinja
[t-gpt]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/models/templates/openai-gpt-oss-120b.jinja
[t-r1]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/models/templates/deepseek-ai-DeepSeek-R1-Distill-Qwen-32B.jinja
[t-r1-alt]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/models/templates/llama-cpp-deepseek-r1.jinja
[t-v31]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/models/templates/deepseek-ai-DeepSeek-V3.1.jinja
[t-v4]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/models/templates/deepseek-ai-DeepSeek-V4.jinja
[t-glm]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/models/templates/GLM-4.7-Flash.jinja
[t-seed]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/models/templates/ByteDance-Seed-OSS.jinja
[t-gemma]: https://github.com/ggml-org/llama.cpp/blob/e6ab7c1a41054a888ada952eab4c886444c2f5ad/models/templates/google-gemma-4-31B-it.jinja
