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
  "max_tokens": 512
}
```

The `system` field is optional. `max_tokens` is a positive integer selected by the caller or daemon defaults.

Streaming response:

```json
{"id":"request-id","token":"Hello"}
{"id":"request-id","token":" world","done":true}
```

The final response may include a token and `done: true`, or may be an empty final marker:

```json
{"id":"request-id","done":true}
```

## Chat Generation

Request:

```json
{
  "id": "request-id",
  "type": "chat",
  "system": "Stable system instructions",
  "messages": [
    {"role": "user", "content": "Hello"},
    {"role": "assistant", "content": "Hi. How can I help?"},
    {"role": "user", "content": "Explain desktop portals briefly."}
  ],
  "max_tokens": 512
}
```

The daemon forwards chat messages as structured roles instead of flattening them into a prompt. Apps own conversation history and send the relevant prior turns on each request.

Streaming responses use the same token stream shape as text generation.

## Structured Generation

Request:

```json
{
  "id": "request-id",
  "type": "generate_structured",
  "system": "Stable system instructions",
  "prompt": "Extract a contact record",
  "max_tokens": 1024,
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

## Embeddings

Request:

```json
{
  "id": "request-id",
  "type": "embed",
  "prompt": "text to embed"
}
```

Response (single line, not streamed):

```json
{"id":"request-id","embedding":[0.012,-0.044,0.031],"done":true}
```

`embedding` is a flat array of floats. Documents and queries embedded by the same profile share a vector space.

## Audio Transcription And Translation

Request:

```json
{
  "id": "request-id",
  "type": "transcribe",
  "audio": "base64-encoded-audio",
  "task": "transcribe",
  "language_hint": "en"
}
```

Audio is raw PCM bytes encoded as base64. The current portal-facing API documents 16 kHz mono `f32le` input. `language_hint` is optional and omitted when the caller leaves the hint unspecified. `task` is `transcribe` for a verbatim transcript in the source language (the default when omitted) or `translate` to translate the speech to English.

Response uses the same token stream shape as text generation:

```json
{"id":"request-id","token":"Hello world","done":true}
```

## Image Description

Request:

```json
{
  "id": "request-id",
  "type": "describe",
  "image": "base64-encoded-png-or-jpeg"
}
```

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
  "image": "base64-encoded-png-or-jpeg"
}
```

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
  "image": "base64-encoded-png-or-jpeg"
}
```

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

The daemon surfaces runtime errors as inference failures. A runtime should prefer a clear error over malformed JSON, closed stdout, or hanging forever.

## Security Assumptions

The daemon runs runtimes with a hardened generated OCI runtime configuration:

- no network
- read-only root filesystem
- writable tmpfs at `/tmp`
- all Linux capabilities dropped
- no new privileges
- PID and memory limits
- model artifacts mounted read-only at `/model`

Runtime images should not require mutable state outside `/tmp`, package downloads at startup, background daemons, or external services.
