# Prompt API primary-source assessment

Research date: 2026-10-20. Scope: MDN's Prompt API overview, the linked specification and Chrome guides, and the current Aileron working tree (HEAD `7b03b0877e7c656e40a5b364e3243d2e62611280`, including existing uncommitted changes). No browser/model benchmarks were run. Recommendations below are design proposals, not measured performance claims.

## Recommendation for Aileron

Implementation update (2026-10-22): The unreleased Language version 1 exposes initial-request
measurement and the documented object-root `response_schema` subset. The
llama-server measurement path renders/templates and tokenizes text without
generation; media counting and continuation measurement remain unsupported.
Broader configuration-aware availability is deferred: it must use existing
trustworthy metadata, without generation probes or a new curated capability
catalog. Current catalog labels alone do not establish end-to-end runtime
support. See the [app guide](app-developer-guide.md#measuring-request-size).

**Borrow configuration-aware availability, context-budget preflight, and richer structured-output constraints first.** These close concrete gaps without changing Aileron's task-oriented portal boundary. Prompt API's stateful sessions and prefill/clone operations are interesting later optimizations, but need a separate resource and privacy design. [1][2][3]

| Priority | Idea to borrow | Existing Aileron support | Useful addition and tradeoff |
| --- | --- | --- | --- |
| High | Check the actual requested configuration | Availability checks permission, assigned profile, artifacts and runtime; separate reasoning-capability discovery exists. | Include required input/output modalities, languages and tools in capability checks and session creation. Prevent “available” features failing only after submission; requires trustworthy per-profile capability metadata/probes. |
| High | Measure context before inference | Context overflow errors and runtime token counts; V2 completion usage and finish reasons. | Expose session-bound request measurement and context capacity, including instructions, tool schemas and output constraints. Apps can trim documents/history before expensive inference; measurement can itself require loading/tokenization. |
| High | Accept richer output constraints | Guided fields become validated JSON Schema; runtime supports a larger schema subset. | Expose enums, bounds and nested shapes through an explicitly supported schema contract. Makes classification/extraction materially more reliable structurally; schema validation does not establish factual correctness. |
| Medium | Distinguish setup/download readiness | Stable availability codes, optional `Prewarm`, installation progress in management API. | Connect app-facing readiness to ongoing setup and progress. Better first-use UI; keep profile installation a user/system-policy decision. |
| Later | Prefill stable input, then branch | Instructions and pooled warm runtimes; app-owned ordinary history. | Investigate request-scoped prefix preparation/reuse for repeated document questions. Potential latency savings, but introduces cache ownership, memory accounting and invalidation. |

Local evidence for each row is detailed below.

## Highest-value changes

### 1. Make availability answer “can this feature run?”

The Prompt API uses the same core configuration for `availability()` and `create()`: `expectedInputs`, `expectedOutputs`, and language lists. The specification computes availability across requested modalities and languages. Chrome explicitly recommends matching the options between availability checks and actual use. [2][3]

Aileron's [`GetUseCaseAvailability`](../crates/aileron-varlink/varlink/aileron.Inference.varlink) takes only app ID and use case. Its [handler](../crates/aileron-daemon/src/handlers/inference.rs) verifies policy, assignment, artifacts and runtime availability, rather than the full request shape. This matters today: the portal accepts `input_audio` parts, but the [llama-server request adapter](../crates/aileron-runtime/src/llama_server/request.rs) explicitly rejects audio input. An installed language profile therefore does not establish that an audio-bearing language request will work. Dedicated speech transcription is a separate capability.

Proposed increment: let apps declare required modalities and structured/tool output requirements alongside their narrow use-case token; return unsupported or unverified requirements before input submission. Keep language *requirements* distinct from today's translation language *hints*. Reuse the conservative approach of [`GetReasoningCapabilities`](../crates/aileron-varlink/varlink/aileron.Inference.varlink) and [verified template capabilities](../crates/aileron-runtime/src/llama_server/reasoning.rs), rather than interpreting every accepted wire type as model support.

**Benefit:** reliable feature gating across user-selected profiles. **Cost:** metadata maintenance, possible runtime probes, and revalidation when profiles change. Language availability is a support claim, not an accuracy guarantee.

### 2. Give apps a context budget before they spend it

The current draft exposes `measureContextUsage()`, `contextUsage`, and `contextWindow`; measurement accepts the same response-constraint options as prompting. Older names `measureInputUsage()` and `inputQuota` are deprecated in the inspected draft. [2]

Aileron already reports `ContextWindowExceeded`; the [runtime protocol](runtime-protocol.md#private-server-terminal-metadata) also has post-generation `usage` and `finish_reason`, and [count-only overflow telemetry](runtime-protocol.md#error-responses). These are not an app-facing preflight API: the [inference interface](../crates/aileron-varlink/varlink/aileron.Inference.varlink) has no measurement operation, and its overflow error carries a reason string rather than typed requested/capacity fields.

Proposed increment: measure the *complete* request against the session's actual profile/template, including stable instructions, role formatting, tools, media and guided schema. Return capacity and measured usage, with explicit precision/unsupported semantics where a runtime cannot count exactly. Expose typed counts on overflow too. Apps can then shorten a selection, trim history or choose a chunk size without maintaining model tokenizers.

**Tradeoff:** preflight can cost model startup or media processing and is not a reservation. Do not promise that a fitting prompt guarantees the requested answer length: the current llama-server adapter allows generation until the output limit or remaining context is exhausted. A useful UI budget should account for requested output separately. [Runtime protocol](runtime-protocol.md#private-server-terminal-metadata).

### 3. Expose more of the structured-output machinery already present

Prompt API's `responseConstraint` accepts a JSON Schema; Chrome's guide demonstrates even a root boolean classification. Constraint text may itself consume context, which measurement can include. [2][3]

Aileron already does constrained generation and validates every emitted snapshot. The gap is expressiveness at the app boundary: [`GuidedField`](../crates/aileron-varlink/varlink/aileron.Inference.varlink) has name, kind, description and required status; [`guided_fields_schema`](../crates/aileron-daemon/src/handlers/inference.rs) generates only strings, numbers, integers, booleans and string arrays inside an object. Meanwhile the [runtime schema contract](runtime-protocol.md#structured-generation) already supports `enum`, numeric/string/array bounds, nested properties and items.

Proposed increment: support explicit enum choices and bounds first, or accept a versioned, documented JSON Schema subset alongside the convenient field API. Examples: a classifier constrained to known category IDs, an urgency score bounded to 0–5, or nested extracted contacts. This also aligns with the existing [Laya assessment's contract gap](laya-assessment.md#integration-is-feasible-but-not-just-a-model-manifest).

**Tradeoff:** reject unsupported keywords before inference and preserve daemon-side validation. Do not promise arbitrary JSON Schema compatibility or replace schema-valid snapshots with partially parseable JSON fragments. More complex schemas can consume context and constrain which runtimes are usable.

## Useful secondary ideas

### Setup states and progress

Prompt API distinguishes `available`, `downloadable`, `downloading`, and `unavailable`, and creation can report `downloadprogress`. [2][3] Aileron already has more diagnostic failure codes in the [app guide](app-developer-guide.md#handling-unavailable-models), plus [`InstallProgress`, `ListInstalls` and streamed installation methods](../crates/aileron-varlink/varlink/aileron.Models.varlink). This is an opportunity to connect existing management state to app-facing UX, not build a new downloader.

Add readiness/progress information orthogonally to existing reason codes: downloading assets, starting a runtime and ready to infer are different states. Keep progress unavailable/indeterminate when it cannot be measured. Automatically downloading a browser-selected model during `create()` does not map directly to Aileron's user-selected profile policy.

### Prefill and branching: evaluate before adopting stateful sessions

`append()` processes context without generating an answer; `clone()` forks a session's initial prompts and interaction history. The guides propose appending media as it arrives and branching independent conversations from shared context. [3][4]

Aileron's [`Prewarm`](../crates/aileron-varlink/varlink/aileron.Inference.varlink) starts the assigned runtime, not a particular prompt. Its [session state](../crates/aileron-daemon/src/state.rs) stores instructions and tool-conversation state, while the [app guide](app-developer-guide.md#conversation-history) deliberately assigns ordinary conversation history to apps. The [llama-server request adapter](../crates/aileron-runtime/src/llama_server/request.rs) currently sets `cache_prompt: false`. Thus prompt preparation/reuse is distinct from existing prewarming.

For repeated questions over one document, first benchmark isolated prefix reuse against full resubmission. Consider a bounded, session-owned preparation handle rather than silently introducing daemon-owned chat history. Define cleanup on cancellation/session close, eviction behavior, profile/template invalidation, and cross-app isolation. Cloning is not evidence of zero-copy caches or guaranteed parallel throughput; gains depend on the runtime. App-owned history already makes logical branching possible without a new daemon API.

### Typed tool failures

The draft distinguishes `LanguageModelToolSuccess` and `LanguageModelToolError`, and allows typed result content. These tool features are explicitly experimental. [2] Aileron already has app-mediated calls, correlation IDs and continuation; [`ToolResult`](../crates/aileron-varlink/varlink/aileron.Inference.varlink) supplies `content` and `content_json`, without a standard success/error discriminator. A small future improvement would standardize success, execution failure and rejection outcomes so apps do not invent incompatible JSON envelopes. Preserve the [existing app-owned execution boundary](tool-calling.md); the useful borrowing is result semantics, not another tool executor.

## What not to copy wholesale

- **Automatic history retention/eviction.** Chrome's guide describes dropping old turns and emitting `contextoverflow`, whereas the inspected draft's prefill algorithm rejects input exceeding current capacity. These sources do not present one settled overflow policy. Aileron's explicit app-owned trimming is a sound default; borrow budget visibility first. [2][3]
- **Browser-specific permissions or a universal model session.** Aileron already has use-case permissions, cancellation, streaming, isolated local execution and model-independent apps. The browser's origin/iframe permissions model is not a replacement for the desktop portal model. [App guide](app-developer-guide.md), [runtime protocol](runtime-protocol.md#security-assumptions), [1][2].
- **Raw sampling knobs as portable behavior guarantees.** Chrome is experimenting with semantic `samplingMode` presets because `temperature`/`topK` do not transfer consistently between models. Worth tracking if Aileron revisits its existing temperature option, but lower priority than capability and context contracts; presets still need per-backend calibration. [3]

## Source status and primary sources

The inspected specification identifies itself as a **16 October 2026 Draft Community Group Report**, not a W3C Standard or Standards Track document. MDN records limited availability and negative Mozilla/WebKit standards positions. Chrome documentation describes one implementation; experimental tools/sampling and draft behavior should not be treated as universally shipped features. These constraints favor borrowing specific interface ideas rather than targeting API compatibility. [1][2][3]

[1]: https://developer.mozilla.org/en-US/docs/Web/API/Prompt_API
[2]: https://webmachinelearning.github.io/prompt-api/
[3]: https://developer.chrome.com/docs/ai/prompt-api
[4]: https://developer.chrome.com/docs/ai/session-management
