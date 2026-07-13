# Runtime Stdio Protocol

Aileron runtimes are OCI containers that speak newline-delimited JSON over stdin and stdout. The daemon owns container lifecycle, model artifact mounting, permissions, and app-facing IPC. Runtime containers only implement inference for the mounted model.

The daemon starts containers with stdin, stdout, and stderr attached. A runtime must write a line containing `ready` to stderr after it has loaded enough state to accept requests. Other stderr lines are treated as human-readable loading status.

## Container Contract

- Read exactly one JSON request per stdin line.
- Write exactly one JSON response object per stdout line.
- Preserve the request `id` in every response.
- Send `done: true` on the final response for every request.
- Send errors as `{"id":"...","error":"code","reason":"human-readable detail","done":true}`.
- Do not write protocol messages to stderr.
- Do not require network access. The daemon runs runtimes in an isolated OCI network namespace.
- Read model artifacts from `/model`, mounted read-only by the daemon.

## Common Fields

Every request contains:

```json
{"id":"request-id","type":"request_type"}
```

Every response contains:

```json
{"id":"request-id"}
```

The daemon may pipeline unrelated requests in future versions. Runtimes should not assume request IDs are sequential UUIDs or meaningful beyond correlation.

## Text Generation

Request:

```json
{
  "id": "request-id",
  "type": "generate",
  "system": "Stable system instructions",
  "prompt": "User prompt",
  "input": [
    {
      "role": "user",
      "content": [
        {"type": "input_text", "text": "Describe this image."},
        {"type": "input_image", "image": "base64-encoded-png", "mime_type": "image/png"}
      ]
    }
  ],
  "max_tokens": 512,
  "execution_mode": "interactive"
}
```

The `system` field is optional. `input` is the canonical multimodal message array for `generate` requests. The daemon also sends `prompt` as the text-only rendering of `input` for runtimes that implement text generation. `max_tokens` is a positive integer selected by the caller or daemon defaults. `execution_mode` is `interactive` by default or `background` for work that may be delayed, deprioritized, preempted, or cancelled to protect interactive requests and reduce system pressure.

Streaming response:

```json
{"id":"request-id","token":"Hello"}
{"id":"request-id","token":" world","done":true}
```

The final response may include a token and `done: true`, or may be an empty final marker:

```json
{"id":"request-id","done":true}
```

## Structured Generation

Request:

```json
{
  "id": "request-id",
  "type": "generate_structured",
  "system": "Stable system instructions",
  "prompt": "Extract a contact record",
  "max_tokens": 1024,
  "execution_mode": "interactive",
  "response_format": {
    "type": "json_schema",
    "schema": {
      "type": "object",
      "required": ["name"],
      "properties": {
        "name": {"type": "string"}
      }
    }
  }
}
```

Response:

```json
{"id":"request-id","result":"{\"name\":\"Ada\"}","done":true}
```

`result` is a string containing JSON. The daemon validates that JSON against the requested schema before returning it to the app.

The daemon-side validator supports the schema subset used by guided generation: `type`, `required`, `properties`, `items`, `minItems`, `maxItems`, `minLength`, `maxLength`, `minimum`, `maximum`, `enum`, and `additionalProperties: false`. `$ref`, `allOf`, `anyOf`, and `oneOf` are intentionally unsupported.

## Structured Snapshot Streaming

Request:

```json
{
  "id": "request-id",
  "type": "generate_structured_stream",
  "system": "Stable system instructions",
  "prompt": "Extract a contact record",
  "max_tokens": 1024,
  "execution_mode": "interactive",
  "response_format": {
    "type": "json_schema",
    "schema": {
      "type": "object",
      "required": ["name"],
      "properties": {
        "name": {"type": "string"}
      }
    }
  }
}
```

Response:

```json
{"id":"request-id","snapshot":"{\"name\":\"Ada\"}"}
{"id":"request-id","snapshot":"{\"name\":\"Ada Lovelace\"}","done":true}
```

`snapshot` is a string containing JSON. Every snapshot must satisfy the requested schema because the daemon validates each one before forwarding it. Runtimes that cannot produce true partial structured output may emit the same valid object once as an initial snapshot and once as the final snapshot.

## Tool Calling

Tool execution is app-mediated. Runtimes may request tool calls, but the daemon never executes them; it forwards calls to the app-facing API and later forwards app-supplied results back to the runtime.

Initial guided request with tools:

```json
{
  "id": "request-id",
  "type": "generate_structured",
  "system": "Stable system instructions",
  "prompt": "What is on my calendar?",
  "max_tokens": 512,
  "response_format": {
    "type": "json_schema",
    "schema": {"type":"object","properties":{"answer":{"type":"string"}}}
  },
  "tools": [
    {
      "name": "calendar_lookup",
      "description": "Look up calendar events",
      "schema_json": "{\"type\":\"object\",\"properties\":{}}"
    }
  ]
}
```

Response requesting app tool execution:

```json
{"id":"request-id","tool_calls":[{"id":"call-1","name":"calendar_lookup","arguments_json":"{}"}],"done":true}
```

Continuation request with app-provided results:

```json
{
  "id": "request-id",
  "type": "generate_structured",
  "prompt": "What is on my calendar? Use the tool result to answer.",
  "max_tokens": 512,
  "response_format": {
    "type": "json_schema",
    "schema": {"type":"object","properties":{"answer":{"type":"string"}}}
  },
  "tool_results": [
    {"id":"call-1","content":"Team sync at 10:00","content_json":""}
  ]
}
```

Final response:

```json
{"id":"request-id","result":"{\"answer\":\"You have team sync at 10:00.\"}","done":true}
```

`arguments_json` and `content_json` are JSON strings. Tool calling is attached to guided structured generation so the final answer can still be constrained by the requested schema. Runtimes should preserve tool call IDs so apps can correlate results.

## Embeddings

Request:

```json
{
  "id": "request-id",
  "type": "embed",
  "prompt": "text to embed",
  "execution_mode": "interactive"
}
```

Response (single event):

```json
{"id":"request-id","embedding":[0.012,-0.044,0.031],"embedding_pipeline_id":"profile-id","done":true}
```

`embedding` is a flat array of floats. `embedding_pipeline_id` is the compatibility boundary for vector search; apps that persist vectors should store it and only compare vectors whose pipeline ids match. The pipeline id is derived from the assigned profile's model, runtime, artifact, runtime image, and runtime option metadata.

## Audio Transcription And Translation

Request:

```json
{
  "id": "request-id",
  "type": "transcribe",
  "audio": "base64-encoded-audio",
  "task": "transcribe",
  "language_hint": "en",
  "execution_mode": "interactive"
}
```

Audio is raw PCM bytes encoded as base64. The current portal-facing API documents 16 kHz mono `f32le` input. `language_hint` is an optional source-language hint and is omitted when the caller leaves the hint unspecified. `task` is `transcribe` for a verbatim transcript in the source language (the default when omitted) or `translate` to translate the speech to English.

Response uses the same token stream shape as text generation. ASR runtimes may emit one `token` per recognized segment; the daemon forwards those tokens progressively through `StreamTranscribe` without changing this container protocol.

```json
{"id":"request-id","token":"Hello "}
{"id":"request-id","token":"world","done":true}
```

## Image Description

Request:

```json
{
  "id": "request-id",
  "type": "describe",
  "image": "base64-encoded-png-or-jpeg",
  "prompt": "optional per-image instructions"
}
```

`prompt` is omitted when the caller leaves per-image instructions empty.

Response uses the same token stream shape as text generation:

```json
{"id":"request-id","token":"A cat sitting on a windowsill.","done":true}
```

## Image OCR

Request:

```json
{
  "id": "request-id",
  "type": "ocr",
  "image": "base64-encoded-png-or-jpeg",
  "prompt": "optional per-image instructions"
}
```

`prompt` is omitted when the caller leaves per-image instructions empty.

Response uses the same token stream shape as text generation:

```json
{"id":"request-id","token":"Invoice #4815 - Total: $42.00","done":true}
```

## Image Segmentation

Request:

```json
{
  "id": "request-id",
  "type": "segment",
  "image": "base64-encoded-png-or-jpeg",
  "prompt": "optional per-image instructions"
}
```

`prompt` is omitted when the caller leaves per-image instructions empty.

Response is a single line containing a JSON string of normalized object boxes:

```json
{"id":"request-id","result":"{\"segments\":[{\"label\":\"cat\",\"confidence\":0.9,\"x\":0.1,\"y\":0.1,\"width\":0.5,\"height\":0.6}]}","done":true}
```

`result` is a string containing JSON. Coordinates are normalized to `0.0..1.0` relative to the image dimensions, where `x` and `y` are the top-left corner.

## Error Responses

Runtimes should use stable, machine-readable `error` codes and put details in `reason`:

```json
{
  "id": "request-id",
  "error": "unsupported_request",
  "reason": "request type classify is not supported by this runtime",
  "done": true
}
```

The daemon maps these runtime error codes to specific app-facing inference errors when possible: `context_window_exceeded`, `unsupported_language`, `safety_refusal`, `request_cancelled`, and `invalid_input`. Unknown runtime codes are surfaced as generic inference failures. A runtime should prefer a clear error over malformed JSON, closed stdout, or hanging forever.

For `context_window_exceeded`, runtimes should include count-only context telemetry when available:

```json
{
  "id": "request-id",
  "error": "context_window_exceeded",
  "reason": "prompt plus requested output exceeds context: 4200 + 512 > 4096",
  "prompt_tokens": 4200,
  "max_tokens": 512,
  "context_tokens": 4096,
  "operation": "generate",
  "done": true
}
```

`prompt_tokens` is the token count for the evaluated prompt or embedding input. `max_tokens` is the requested output budget and may be omitted for non-generating requests such as `embed`. `context_tokens` is the active model context size. `operation` is a short runtime-defined label such as `generate`, `generate_continuation`, or `embed`. Do not include prompt text or user content in these fields.

## Security Assumptions

The daemon runs runtimes with a hardened generated OCI runtime configuration:

- no network
- read-only root filesystem
- writable per-container tmpfs at `/tmp` (`1777`, `noexec`, `nodev`)
- all Linux capabilities dropped
- no new privileges
- PID and memory limits
- model artifacts mounted read-only at `/model`

`/tmp` is not a host bind mount and is not shared between separate containers. Warm runtime containers are reused for multiple requests to the same profile, so runtimes must create per-request temporary paths and remove request-specific data before replying.

Runtime images should not require mutable state outside `/tmp`, package downloads at startup, background daemons, or external services.
