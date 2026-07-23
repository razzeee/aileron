# Streaming TTS Integration Design

## Context

Aileron routes model capabilities through task-and-modality use-case tokens.
Each public operation follows the same path:

1. An app checks availability and creates a use-case-specific session through
   an xdg-desktop-portal interface.
2. The portal backend forwards the operation to `aileron.Inference` over
   Varlink.
3. The daemon resolves permissions, assignments, profiles, and runtime images.
4. A long-lived OCI runtime receives newline-delimited JSON and streams
   newline-delimited JSON responses.

The existing `org.freedesktop.portal.Speech` interface supports
`speech.transcribe` and `speech.translate`, both through `StreamTranscribe`.
It has no speech-output use case. Language generation already streams tokens,
so text-to-speech must begin before the complete language response exists and
must optionally produce a complete audio file without synthesizing twice.

## Objective

Add low-latency streaming text-to-speech as `speech.synthesize`, following the
same availability, session, permission, assignment, portal, daemon, and
runtime path as existing use cases. The first playable audio must be available
while language generation is still in progress. The same PCM stream may be
played immediately and written to a valid WAV file at completion.

## Acceptance Criteria

- `speech.synthesize` is independently assignable and permissioned.
- A client can submit stable phrases before the complete source text exists.
- The portal emits the first audio chunk without waiting for phrase or
  utterance completion.
- Phrase audio is emitted and played in submission order.
- Live playback and optional recording consume identical PCM bytes from one
  synthesis pass.
- A completed recording is a valid WAV file with duration matching the PCM
  stream.
- Cancellation stops pending synthesis and playback promptly.
- Incomplete recordings are removed by default unless partial output was
  explicitly requested.
- Runtime, daemon, implementation portal, public portal, and stub roundtrip
  tests cover multi-chunk output and terminal events.

## Scope

In scope:

- The `speech.synthesize` use-case token.
- A streaming synthesis operation in the runtime protocol, Varlink API,
  implementation portal, and public Speech portal.
- Incremental phrase submission and ordered audio playback in the demo.
- Optional app-owned WAV recording from the live PCM stream.
- A deterministic stub implementation for end-to-end testing.
- One real local TTS runtime and model profile after the contract works through
  the stub.

Out of scope for the first pass:

- Sending arbitrary per-token text directly to the TTS model.
- Portal-owned filesystem destinations or media libraries.
- MP3, AAC, or Opus encoding.
- Voice cloning, user-provided speaker samples, and custom voice training.
- SSML and word-level timing metadata.
- Mixing, effects, or simultaneous speakers.
- A combined daemon method that internally chains language generation and TTS.

## Approach Options

### Option A: Phrase Requests With Streamed PCM

The client buffers generated tokens into stable phrases, submits each phrase
through `StreamSynthesize`, and consumes streamed PCM chunks. It tees each
chunk to playback and an optional WAV writer.

Pros:

- Starts speaking before the complete language response exists.
- Keeps language generation and speech synthesis as independently assignable
  use cases.
- Reuses Aileron's existing session and streaming request model.
- Saves exactly the audio that was played without a second synthesis pass.
- Bounds retries and failures to individual phrases.

Cons:

- The client must coordinate phrase boundaries and ordering.
- Poor phrase boundaries can affect prosody.
- A runtime may need continuation metadata to preserve prosody across phrases.

### Option B: Complete-Text Synthesis

Wait for language generation to finish, then synthesize the complete response.

Pros:

- Simplest API and best opportunity for whole-utterance prosody.
- No phrase queue or continuation state.

Cons:

- Violates the first-audio latency requirement.
- Users hear nothing while the language model is generating.

### Option C: Combined Language-And-Speech Operation

Add a daemon operation that runs language generation, chunks its tokens, and
invokes TTS internally.

Pros:

- Minimal orchestration work for applications.
- The daemon could enforce one chunking policy.

Cons:

- Couples two profiles, permissions, runtimes, and cancellation domains.
- Does not follow the existing one-use-case-per-session path.
- Makes either service harder to use independently and test.

## Decision

Use Option A. Add a narrow `speech.synthesize` primitive and demonstrate the
language-to-speech composition in the client. Do not couple language and TTS
inside the daemon.

Use raw interleaved PCM for the first version. The runtime protocol carries
base64 because it is JSON over stdio. The portal backend decodes it and the
public portal emits small D-Bus byte arrays. Apps can play those bytes directly
and optionally append them to a WAV writer. This avoids one memfd per small
chunk and avoids waiting for a complete output file.

## End-To-End Data Flow

```text
Language StreamResponse
  -> generated text tokens
  -> client phrase buffer
  -> ordered phrase queue
  -> Speech StreamSynthesize
  -> runtime base64 PCM chunks
  -> daemon Varlink streamed replies
  -> portal AudioReceived byte-array signals
  +-> playback queue
  `-> optional WAV writer
```

Language generation and speech synthesis use separate sessions and may use
different profiles and runtime containers. The language stream continues while
a worker drains the phrase queue through the speech session. Only one synthesis
request is active for a speech session at a time, preserving phrase order.

## Phrase Buffering

The client, not the daemon or TTS runtime, owns conversion from unstable token
fragments to speakable phrases. It appends non-empty language tokens and flushes
the buffer on the first applicable rule:

1. A sentence boundary such as `.`, `?`, or `!` is followed by whitespace or
   the language stream ends.
2. A softer punctuation boundary such as `,`, `;`, or `:` is available and the
   buffer is large enough to sound natural.
3. The oldest buffered text has waited 200 ms and contains a safe word
   boundary.
4. The buffer reaches a configured maximum size.
5. The language stream ends, flushing any remaining text as the final phrase.

The exact punctuation classifier should remain replaceable for languages whose
sentence boundaries differ. The first implementation may use a conservative
Unicode-aware boundary function rather than language-specific segmentation.
It must never split inside a UTF-8 code point or submit an empty phrase.

The 200 ms timeout is an initial default, not a public API guarantee. Tests use
a controllable clock rather than sleeping.

## Use Case And Session Semantics

Add `speech.synthesize` to the supported manifest token catalog and the Speech
portal allow-list. It receives its own assignment and app permission; assignment
fallback behavior used between Whisper transcription and translation does not
apply.

A speech synthesis session fixes the selected profile and stable session
instructions. Requests on that session may select a voice supported by the
profile. A first version may expose only the profile's default voice, represented
by an empty `voice_id`.

Each `StreamSynthesize` call handles one independent phrase and ends when that
phrase's audio is complete. The orchestration client uses one worker and permits
only one active synthesis call per speech session, which preserves queue order
without adding utterance state to the daemon or runtime.

The client considers the complete utterance finished only when the language
stream has ended, its text buffer is empty, its phrase queue is empty, and the
last synthesis request has completed. Playback and WAV finalization use this
condition; the TTS primitive does not need an explicit finalization request.

## Runtime Protocol

Request:

```json
{
  "id": "request-id",
  "type": "synthesize",
  "text": "Hello there,",
  "voice_id": "",
  "language_hint": "en",
  "execution_mode": "interactive"
}
```

Streaming response:

```json
{"id":"request-id","audio":"<base64 PCM>","sample_rate":24000,"channels":1,"sample_format":"s16le"}
{"id":"request-id","audio":"<base64 PCM>","done":true}
```

Rules:

- `audio` contains complete interleaved PCM sample frames. A chunk must not end
  partway through a sample frame.
- The first non-empty chunk declares `sample_rate`, `channels`, and
  `sample_format`.
- Later chunks may omit unchanged metadata. If present, it must match the first
  chunk.
- The first version supports `s16le`; the protocol names the format so future
  profiles can add formats deliberately.
- A successful phrase emits at least one non-empty audio chunk followed by a
  terminal response with `done=true`.
- Runtime errors use the existing terminal error shape.
- The runtime should produce chunks sized for latency, initially targeting
  roughly 20-100 ms of audio rather than multi-second blocks.

## Varlink API

Add:

```varlink
type SynthesisOptions (
  voice_id: string,
  language_hint: string,
  execution_mode: string
)

type AudioChunk (
  audio_base64: string,
  sample_rate: int,
  channels: int,
  sample_format: string
)

method StreamSynthesize(
  session_id: string,
  text: string,
  options: SynthesisOptions
) -> (chunk: AudioChunk)
```

Varlink replies use `continues=true` until the final chunk, matching existing
streaming methods. The daemon validates the use case, non-empty text, execution
mode, decoded chunk size, base64, sample framing, and stable metadata.
It forwards cancellation through the existing active-request mechanism.

## Portal API

Extend both `org.freedesktop.impl.portal.Speech` and
`org.freedesktop.portal.Speech`.

Public method:

```text
StreamSynthesize(
  session_handle: o,
  text: s,
  options: a{sv}
) -> handle: o
```

Public options:

- Standard `handle_token`.
- `voice_id: s`.
- `language_hint: s`.
- `execution_mode: s`.

Public signal:

```text
AudioReceived(
  request_handle: o,
  session_handle: o,
  audio: ay,
  sample_rate: u,
  channels: u,
  sample_format: s,
  done: b
)
```

The signal is sent only to the request owner and is correlated by request and
session handles, like existing stream signals. The final signal may contain an
empty byte array and `done=true`. Metadata is present on every public signal to
keep individual signals self-describing; the portal backend verifies it remains
stable before forwarding.

D-Bus byte arrays are intentionally limited to small audio chunks. The backend
rejects chunks above a fixed safety limit. If profiling later shows material
D-Bus overhead, a versioned pipe-fd transport can be designed without changing
the runtime protocol or synthesis use case.

Increment the Speech interface version when adding the method and signal.

## Playback And Recording

The demo adds an audio sink abstraction with two consumers:

- A bounded playback queue starts the platform audio stream after receiving the
  first chunk and keeps feeding it as chunks arrive.
- An optional WAV writer writes a placeholder header, appends the same PCM
  chunks, and patches RIFF/data lengths after the final phrase queue drains.

Both consumers receive the same immutable chunk in stream order. Recording must
not invoke synthesis again. A slow sink applies bounded backpressure; it must
not permit unbounded memory growth. The initial demo may pause phrase submission
when the audio queue reaches its high-water mark while language token collection
continues within a separately bounded text buffer.

File selection remains app-owned. A sandboxed app uses the existing file chooser
or document portal when the user chooses an export destination. The Speech
portal does not write arbitrary host paths.

On successful completion, the app atomically publishes the finalized WAV from a
temporary file where practical. On cancellation or failure, it closes playback
and removes the temporary file unless the caller explicitly selected a
keep-partial policy.

## Cancellation And Errors

- Closing a synthesis request cancels the active phrase request.
- Closing the speech session cancels active and queued phrases for that session.
- Cancelling the language request causes the orchestration client to stop
  accepting tokens, cancel active TTS, clear queued phrases, and stop playback.
- `InvalidInput` covers empty text, bad PCM metadata, and unsupported public
  option values.
- `UnsupportedLanguage` covers a valid but unsupported language hint.
- A stable unsupported-voice error should be added if clients need to branch on
  it; until then, unsupported voices return `InvalidInput` with a clear reason.
- Runtime synthesis failures map through `GenerationFailed` initially unless a
  dedicated `SynthesisFailed` error is introduced consistently across all
  layers.
- Metadata changes, malformed base64, oversized chunks, and partial sample
  frames are runtime failures and are never forwarded to playback.
- If playback fails while recording is active, the app may continue recording
  only after making that state visible; otherwise it cancels the utterance.
- If recording fails, playback may continue after reporting that no final file
  will be produced.

## Backpressure And Limits

Define conservative limits before accepting untrusted runtime output:

- Maximum UTF-8 bytes per phrase.
- Maximum queued phrase count and total queued text bytes.
- Maximum decoded bytes per audio chunk.
- Maximum buffered playback duration.
- Maximum channels and sample rate accepted by the first implementation.

Exact constants should be selected during implementation from the existing media
limits and measured chunk sizes. They must be centralized, documented, and
covered by boundary tests. Interactive synthesis should receive the same
priority and preemption treatment as other interactive requests.

## Runtime And Model Packaging

The contract should first be exercised by the existing stub runtime. The stub
returns deterministic `s16le` PCM in multiple chunks, allowing roundtrip tests
to verify ordering, framing, cancellation, and WAV output without model weights.

After the stub path passes, add a dedicated TTS runtime rather than adding a
large synthesis stack to `llm-vision-whisper`. The runtime owns model-specific
tokenization and waveform generation but implements only the generic
`synthesize` contract. Its model manifest declares `speech.synthesize`, artifact
layout, supported languages, and voice identifiers where metadata supports
them.

The first real backend should be chosen through a short implementation spike
based on:

- Time to first audio on CPU.
- Ability to emit waveform chunks incrementally.
- Model and code licensing suitable for distribution.
- Offline artifact loading.
- Voice and language metadata discoverability.
- Build size and accelerator requirements.

Backend selection is intentionally separate from the public contract.

## Delivery Plan

### Phase 1: Contract And Stub Runtime

- Add `speech.synthesize` to supported tokens and profile validation.
- Add runtime request fields and deterministic multi-chunk stub synthesis.
- Add Varlink synthesis types and `StreamSynthesize`.
- Implement daemon validation, streaming, cancellation, and tests.

Exit criterion: a Varlink client receives valid ordered PCM chunks from the
stub through a `speech.synthesize` session.

### Phase 2: Portal End To End

- Add implementation and public Speech XML API surface.
- Regenerate xdg-desktop-portal bindings.
- Implement backend forwarding and public signal forwarding.
- Add public frontend validation, request lifecycle, ownership, and cancellation
  tests.

Exit criterion: a sandbox-style portal client receives caller-scoped
`AudioReceived` signals before the request completes.

### Phase 3: Demo Orchestration

- Add the phrase buffer and ordered synthesis worker.
- Add bounded PCM playback.
- Add optional WAV writing and atomic finalization.
- Wire language cancellation to TTS, playback, and recording cancellation.
- Display model-loading, speaking, recording, cancellation, and failure states.

Exit criterion: the demo begins speaking a streaming language response and can
save the exact spoken audio as a valid WAV.

### Phase 4: Real TTS Runtime

- Spike candidate backends against first-audio latency and licensing criteria.
- Add the selected dedicated runtime image and entrypoint.
- Add one distributable model profile and runtime manifest.
- Measure first-audio latency, real-time factor, chunk cadence, and memory use.
- Tune phrase and audio buffering defaults from measurements.

Exit criterion: an assigned local profile synthesizes through the same API with
no demo or portal contract changes.

### Phase 5: Documentation And Hardening

- Update the README use-case table and API summaries.
- Document the runtime protocol, app integration, output format, limits, and
  cancellation semantics.
- Add CI stub roundtrips and WAV validation.
- Run daemon, portal backend, xdg-desktop-portal, and demo test suites.

Exit criterion: all layers document the same contract and automated tests cover
the success, cancellation, malformed-output, and slow-consumer paths.

## Test Strategy

Runtime tests:

- Parse synthesis requests and reject invalid fields.
- Emit deterministic non-empty PCM across multiple chunks.
- Preserve request IDs and terminate with `done=true`.
- Verify every chunk ends on a complete sample frame.

Daemon tests:

- Accept `speech.synthesize` and reject it on unrelated methods.
- Enforce assignment and permission isolation.
- Validate text, metadata, base64, limits, and execution mode.
- Preserve chunk order and Varlink continuation state.
- Stop forwarding after cancellation.

Portal backend tests:

- Validate Speech session use case and options.
- Decode runtime chunks and emit self-describing audio signals.
- Correlate signals by request and session handles.
- Reject metadata changes and oversized chunks.

xdg-desktop-portal tests:

- Forward `StreamSynthesize` to the implementation backend.
- Emit audio only to the request owner.
- Complete the Request only after the terminal audio signal.
- Propagate request and session closure.

Client tests:

- Flush phrases on punctuation, timeout, maximum size, and stream completion.
- Preserve Unicode and never split a code point.
- Serialize phrase requests and playback in sequence order.
- Start playback after the first audio chunk, before language completion.
- Write byte-identical PCM to playback and recording sinks.
- Produce a valid WAV header, byte length, and duration.
- Remove partial output on cancellation and retain it only when requested.
- Bound queues under slow playback and slow disk simulation.

## Observability

Use existing session and request logging with `speech.synthesize`. Add metrics or
structured timing fields for:

- Language request start to first stable phrase.
- Phrase submission to first audio chunk.
- Language request start to first played audio.
- Audio buffered duration and underrun count.
- Synthesized audio duration and real-time factor.
- Queue high-water marks and cancellation stage.

Logs must not include synthesized text or audio payloads by default.

## Compatibility

This is additive API surface. Existing Speech clients continue using version 1
transcription methods. New clients check the Speech interface version and
`GetUseCaseAvailability("speech.synthesize")` before creating a session.

The runtime protocol is extensible by request `type`; older runtimes return an
unsupported-request error for `synthesize`. Profiles only advertise
`speech.synthesize` when their runtime and artifacts support it, preventing the
daemon from routing synthesis to Whisper-only profiles.

## Open Implementation Choice

The public contract does not select a concrete TTS engine. Phase 4 must compare
candidate local engines and record the decision, model license, supported
languages, artifact layout, measured first-audio latency, and whether true
incremental waveform generation is available. A backend that only returns a
complete phrase waveform remains compatible, but it may require shorter phrases
to meet latency goals and should not be selected if its measured latency is
unacceptable.
