# App Developer Guide

Aileron gives Linux apps access to local AI through the desktop portal model. Apps do not connect to a localhost REST server, do not discover model files, and do not run inference engines themselves. They request a task-oriented capability from the portal, and the portal/daemon enforce the local policy boundary.

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

- `llm.summarize`
- `llm.translate`
- `llm.rephrase`
- `llm.classify`
- `llm.extract`
- `llm.analyze`
- `llm.chat`
- `asr.transcribe`
- `vision.describe`
- `vision.segment`

Avoid treating model names as application requirements. A user may satisfy `llm.summarize` with a small CPU model, a GPU model, or a future system model without changing the app.

## Recommended Flow

1. Check availability for the use case.
2. Create a session with stable instructions.
3. Optionally prewarm before the first visible response.
4. Send task input through the appropriate method.
5. End the session when the user-visible task is complete.

For chat features, keep stable instructions in the session and send the explicit message list with `Chat` or `StreamChat`. The app owns conversation history and can trim it to fit its UI or context policy. For one-shot features such as "summarize this article", a short-lived session with `Respond` or `StreamResponse` is usually enough.

## Text Generation

Use `llm.*` use cases with `Respond` for full responses or `StreamResponse` for token streaming.

Good prompt shape:

```text
Summarize the article below in three bullet points. Preserve important names and numbers.

<article text>
```

Prefer explicit output constraints over relying on a specific model's behavior.

## Chat

Use `llm.chat` with `Chat` for a full assistant turn or `StreamChat` for token streaming. The message list is stateless: include the prior turns the model should consider on every call.

Messages accept `user` and `assistant` roles. Keep system or developer instructions in `CreateSession.instructions` instead of adding them to the message list.

For `llm.translate`, `GenerationOptions` includes optional `source_language_hint` and `target_language_hint` strings. Pass empty strings when the app does not know one side. These are hints, not strict locale settings; apps should still make the requested translation clear in the prompt.

## Guided Output

Use guided generation when the app needs structured data. The portal API accepts field guides and the daemon converts them to a JSON Schema used by the runtime protocol.

This is appropriate for extraction, classification, and form-filling workflows. It is not a replacement for validating untrusted data in the app; keep normal app-side validation.

## Audio And Vision

Use `asr.transcribe` for speech-to-text. Audio is passed as base64-encoded raw PCM bytes through the portal-facing API. `Transcribe` also accepts an optional `language_hint` string; pass an empty string to let the runtime auto-detect or use its default behavior.

Use `vision.describe` for image description. Images are passed as base64-encoded PNG or JPEG bytes.

Large media inputs can be expensive. Prefer user-initiated actions, visible progress, and cancellation-friendly UI.

## Privacy And Permissions

Design UI as if AI access is a user-controlled capability, not a hidden implementation detail.

- Explain why the feature needs local AI.
- Request the narrowest use case.
- Do not send data to network services as a fallback without explicit user consent.
- Do not ask users to start or configure a localhost server.
- Handle unavailable models gracefully.

## Handling Unavailable Models

An unavailable use case is normal. The user may not have installed a matching profile, the runtime image may be unavailable for the hardware, or policy may deny the app.

Recommended behavior:

- Disable or soften the AI feature entry point.
- Explain that a local model profile is required.
- Offer a non-AI fallback when possible.
- Avoid model-specific instructions in the app UI.

## Development And Testing

Use the stub runtime for end-to-end development when model quality is irrelevant. It returns deterministic fake responses for text generation, structured generation, transcription, and image description.

The stub path exercises the same daemon, manifest, container, and IPC layers without requiring a model download.

See the root `README.md` for the current stub runtime test flow and `docs/runtime-protocol.md` for runtime implementer details.
