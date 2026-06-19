# App Developer Guide

Aileron gives Linux apps access to local model capabilities through the desktop portal model. Apps do not connect to a localhost REST server, do not discover model files, and do not run inference engines themselves. They request a task-oriented capability from the portal, and the portal/daemon enforce the local policy boundary.

## Mental Model

Traditional local AI stacks often look like this:

```text
app -> http://127.0.0.1:PORT -> shared model server
```

Aileron intentionally does not. Localhost services are visible inside many Flatpak sandboxes when network access is granted, which makes them a poor permission boundary.

Aileron uses this shape instead:

```text
app sandbox -> xdg-desktop-portal D-Bus API -> aileron-portal -> aileron-daemon -> isolated runtime container
```

The app asks for a use case such as summarization or transcription. The user or system policy decides which installed profile satisfies that use case.

## Use Cases

Use cases are stable task tokens. Apps should request the narrowest token that matches the user-visible feature.

Current tokens:

- `language.summarize`
- `language.translate`
- `language.rephrase`
- `language.classify`
- `language.extract`
- `language.analyze`
- `language.embed`
- `speech.transcribe`
- `speech.translate`
- `vision.describe`
- `vision.ocr`
- `vision.segment`

Avoid treating model names as application requirements. A user may satisfy `language.summarize` with a small CPU model, a GPU model, or a future system model without changing the app.

## Recommended Flow

1. Check availability for all use cases.
2. Create a session with stable instructions.
3. Optionally prewarm on the same portal interface before the first visible operation.
4. Send task input through the appropriate method.
5. End the session when the user-visible task is complete.

For conversational features, keep stable instructions in the session and send the relevant local history as part of the prompt with `Respond`, `StreamResponse`, or `RespondGuided`. The app owns conversation history and can trim it to fit its UI or context policy. For one-shot features such as "summarize this article", a short-lived session with `Respond` or `StreamResponse` is usually enough.

## Text Generation

Use `language.*` use cases with `Respond` for full responses or `StreamResponse` for token streaming.

Good prompt shape:

```text
Summarize the article below in three bullet points. Preserve important names and numbers.

<article text>
```

Prefer explicit output constraints over relying on a specific model's behavior.

For `language.translate`, `GenerationOptions` includes optional `source_language_hint` and `target_language_hint` strings. Pass empty strings when the app does not know one side. These are hints, not strict locale settings; apps should still make the requested translation clear in the prompt.

## Guided Output

Use guided generation when the app needs structured data. The portal API accepts field guides and the daemon converts them to a JSON Schema used by the runtime protocol.

This is appropriate for extraction, classification, and form-filling workflows. It is not a replacement for validating untrusted data in the app; keep normal app-side validation.

Use `StreamGuidedResponse` when the UI benefits from progressive structured updates. It emits JSON snapshots instead of token deltas; each snapshot is validated against the same guided schema before the daemon returns it.

## Tool Calling

Use guided tool calls when the model should ask the app for app-local data or actions. Pass tool definitions to `RespondGuided`, execute or reject any returned `ToolCall` objects in the app, then send results back with `SubmitToolResultsGuided` using the same guided fields.

The daemon and runtime never execute tools. Tool execution stays app-mediated so sandbox policy, user confirmation, and app-specific authorization remain under the app's control.

## Conversation History

Aileron sessions do not retain conversation transcripts. Apps own chat history, trim it according to their UI and privacy policy, and include relevant context explicitly in prompts or tool results.

## Embeddings

Use `language.embed` with `Embed` to turn text into a fixed-length vector for semantic search, clustering, deduplication, or retrieval-augmented generation. `Embed` returns the embedding as a list of floats. Embed documents and queries with the same assigned profile so the vectors share a space

## Speech And Vision

Use `speech.transcribe` for verbatim speech-to-text and `speech.translate` to translate spoken audio into English text. Both use the `Transcribe` method; the daemon selects the whisper transcribe or translate task from the session use case. Audio is passed as base64-encoded raw PCM bytes through the portal-facing API. `Transcribe` also accepts an optional `language_hint` string; pass an empty string to let the runtime auto-detect or use its default behavior.

Use `vision.describe` for image description and `vision.ocr` to extract text from an image with the `Ocr` method. Use `vision.segment` to identify visible objects with normalized rectangular boxes. Images are passed as base64-encoded PNG or JPEG bytes.

Large media inputs can be expensive. Prefer user-initiated actions, visible progress, and cancellation-friendly UI.

## Privacy And Permissions

Design UI as if local model access is a user-controlled capability, not a hidden implementation detail.

- Explain why the feature needs a local model capability.
- Request the narrowest use case.
- Do not send data to network services as a fallback without explicit user consent.
- Do not ask users to start or configure a localhost server.
- Handle unavailable models gracefully.

## Handling Unavailable Models

An unavailable use case is normal. The user may not have installed a matching profile, the runtime image may be unavailable for the hardware, or policy may deny the app.

Availability responses include a stable `code` and a human-readable `reason`. Apps should branch on `code`, not parse `reason`. Common codes are `available`, `permission_denied`, `no_profile_assigned`, `profile_not_installed`, `artifact_missing`, `runtime_unsupported`, `runtime_missing`, `hardware_unsupported`, and `busy`.

Recommended behavior:

- Disable or soften the model-backed feature entry point.
- Explain that a local model profile is required.
- Offer a non-AI fallback when possible.
- Avoid model-specific instructions in the app UI.

Handle specific inference errors when useful. `ContextWindowExceeded` can prompt the user to shorten input, `UnsupportedLanguage` can ask for another language, `SafetyRefusal` should be shown as a refusal rather than a crash, `RequestCancelled` should leave UI state clean, and `InvalidInput` means the app should fix or reject the submitted payload.

## Development And Testing

Use the stub runtime for end-to-end development when model quality is irrelevant. It returns deterministic fake responses for text generation, structured generation, transcription, and image description.

The stub path exercises the same daemon, manifest, container, and IPC layers without requiring a model download.

See the root `README.md` for the current stub runtime test flow and `docs/runtime-protocol.md` for runtime implementer details.
