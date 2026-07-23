# llmfit VITS TTS Installation Design

## Context

Aileron exposes `speech.synthesize` through its runtime protocol, Varlink API,
implementation portal, public desktop portal, and demo. The deployed contract is
currently exercised by a deterministic stub runtime. It does not yet provide an
installable real text-to-speech model.

llmfit-core 1.1.6 contains 212 models with `Capability::Tts`. All currently use
Safetensors, but they span incompatible architectures including VITS, SpeechT5,
Qwen3-TTS, Parler-TTS, Dia, CSM, and VibeVoice. llmfit correctly marks TTS models
as requiring a specialized runtime, but it does not select an Aileron runtime or
provide complete per-file artifact manifests.

## Objective

Make llmfit VITS models installable and runnable as `speech.synthesize` profiles
through a dedicated offline Aileron runtime. Only models supported by an
implemented runtime appear in Aileron's installable catalog.

## Non-Goals

- Supporting every llmfit TTS architecture in the first implementation.
- Voice cloning or user-provided speaker embeddings.
- True incremental waveform generation inside VITS.
- SSML, word timing, effects, or encoded output formats.
- Routing TTS through `llm-vision-whisper`.

## Dependency Update

Upgrade `llmfit-core` to 1.1.6. Aileron continues to treat llmfit metadata as
catalog and fit information, while owning runtime compatibility and artifact
installation policy.

## Compatibility Boundary

Add one shared predicate for llmfit models supported by the first TTS runtime. A
model is installable when all of the following are true:

- `Capability::infer(model)` contains `Capability::Tts`.
- `model.format` is `ModelFormat::Safetensors`.
- `model.architecture`, compared case-insensitively, is `vits`.

Compatible models become generated catalog profiles with:

- `use_cases: ["speech.synthesize"]`.
- `runtime_id: "tts-vits"`.
- llmfit provider, language, license, size, memory, and fit metadata preserved.

They do not inherit language-generation, transcription, translation, or vision
use cases. Unsupported TTS architectures remain absent from Aileron's installable
catalog until a compatible runtime is implemented.

## Generated Profile Identity

Generated profile IDs continue using normalized llmfit model identity and
quantization so installation, assignment, and catalog deduplication remain
stable. TTS profile generation does not require GGUF sources. The original
Hugging Face repository is `model.name`.

## Artifact Resolution

Installing a generated VITS profile queries the Hugging Face model API for the
repository named by llmfit. Aileron resolves a conservative offline snapshot
rather than downloading arbitrary repository contents.

Required files:

- `config.json`.
- `vocab.json`.
- One complete Safetensors weight layout:
  - `model.safetensors`, or
  - `model.safetensors.index.json` and every referenced shard.

Optional recognized files:

- `tokenizer_config.json`.
- `special_tokens_map.json`.
- `preprocessor_config.json`.
- `generation_config.json`.

Only root-level regular files with recognized names are accepted in the first
version. Filenames must pass the existing traversal and duplicate-role checks.
Unknown repository scripts, pickle weights, nested files, and unrelated assets
are not installed.

Hugging Face LFS entries use their declared SHA-256 and size. Small Git-backed
files without LFS metadata are fetched during resolution, bounded by a
centralized maximum size, and hashed locally. The verified artifact installer
then downloads all selected files through the existing progress, cancellation,
temporary-file, and atomic-publication path. Resolution fails before profile
installation when required files, referenced shards, or trustworthy hashes are
missing.

Artifact roles identify their purpose while preserving the filenames expected
by Transformers. Shards receive deterministic roles derived from their ordered
filenames.

## Runtime Packaging

Add a dedicated `tts-vits` runtime directory and packaged runtime manifest. The
initial image is CPU-capable and contains pinned offline dependencies for:

- PyTorch.
- Transformers VITS model and tokenizer support.
- Safetensors.
- NumPy.
- Required VITS text normalization support, including uroman where applicable.

The image has no model weights and performs no network access. Aileron mounts the
verified profile directory read-only at `/model`. The runtime loads with local
files only and writes readiness/status only to stderr.

## Runtime Behavior

The runtime accepts the existing synthesis request:

```json
{
  "id": "request-id",
  "type": "synthesize",
  "text": "Hello.",
  "voice_id": "",
  "language_hint": "en",
  "execution_mode": "interactive"
}
```

At startup it loads the VITS tokenizer and model from `/model`, determines the
model sample rate, and validates the mounted layout. For each request it:

1. Validates non-empty text and supported options.
2. Rejects non-empty `voice_id` unless a future profile explicitly adds voice
   metadata.
3. Checks `language_hint` against the llmfit-derived supported-language runtime
   option when language metadata exists.
4. Runs one inference pass for the phrase.
5. Rejects non-finite output, clamps samples to `[-1.0, 1.0]`, and converts them
   to mono signed 16-bit little-endian PCM.
6. Emits frame-aligned chunks targeting 20-100 ms, with metadata on the first
   non-empty event.
7. Emits an empty terminal event with `done=true`.

The first VITS backend may produce the complete phrase waveform before emitting
its first chunk. This remains compatible with the portal contract. Applications
continue submitting stable phrases before language generation finishes, so
playback remains incremental at phrase granularity.

## Languages And Voices

Generated profiles preserve llmfit language metadata and expose it in catalog
responses. The generated manifest also provides a normalized, comma-separated
supported-language runtime option. An empty `language_hint` selects the model's
native language. A non-empty unsupported hint returns `UnsupportedLanguage`.

The first version supports only the model's default voice and represents it with
an empty `voice_id`. Non-empty voice IDs return `InvalidInput`. Multispeaker
metadata and stable voice identifiers require a later contract extension.

## Fit And Recommendation Behavior

The management UI treats `speech.synthesize` as its own task. Candidate ranking
uses llmfit fit scores, memory requirements, language metadata, and license
metadata without applying Whisper-specific quality heuristics. Only compatible
VITS profiles participate in recommendations.

llmfit's generic token-per-second estimate is not presented as TTS throughput.
Until measured synthesis metrics exist, the UI shows fit and memory information
but does not claim a real-time factor or first-audio estimate.

## Errors

- Missing or unsupported repository layouts fail installation with
  `InstallFailed` and a specific reason.
- Empty text, unsupported voices, invalid PCM, and malformed options map to
  `InvalidInput`.
- Unsupported language hints map to `UnsupportedLanguage`.
- Model loading and inference failures map to `GenerationFailed`.
- Cancellation uses the existing active-request termination path and removes
  partial install/runtime state under existing policies.

Logs include model/runtime identity, startup duration, inference duration,
sample rate, and output duration. They do not include submitted text or PCM.

## Testing

### Catalog And Manifest Tests

- VITS plus TTS plus Safetensors maps only to `speech.synthesize` and `tts-vits`.
- SpeechT5, Qwen3-TTS, unknown architectures, and non-Safetensors entries are
  excluded.
- TTS profiles never receive language or Whisper use cases.
- Languages, license, fit metadata, and stable profile identity are preserved.

### Artifact Tests

- Resolve valid single-file Safetensors snapshots.
- Resolve valid sharded snapshots and every referenced shard.
- Hash bounded non-LFS configuration and vocabulary files.
- Reject missing config, vocabulary, weights, index shards, LFS hashes, unsafe
  names, duplicate output names, oversized metadata files, and nested layouts.

### Runtime Tests

- Parse synthesis requests and reject invalid options.
- Load only from the mounted offline model directory.
- Convert finite waveform data to correctly clipped `s16le` PCM.
- Emit multiple complete sample-frame chunks and a terminal event.
- Preserve request IDs, sample rate, channel count, and sample format.
- Stop output promptly on process termination.

Tests use a small deterministic VITS fixture or a mocked model adapter for normal
CI. A separate container smoke test uses a distributable real VITS profile.

### End-To-End Test

The integration test installs a compatible generated llmfit profile, assigns
`speech.synthesize`, creates a public Speech portal session, receives multiple
`AudioReceived` events, writes their exact PCM to WAV, validates duration and
header lengths, tests cancellation, and reuses the session afterward. The stub
runtime remains the fast contract test.

## Delivery Sequence

1. Upgrade llmfit and add the compatibility/use-case mapping.
2. Add VITS Hugging Face artifact resolution and generated manifests.
3. Add the offline `tts-vits` runtime and deterministic tests.
4. Add runtime/model manifests and container smoke tests.
5. Run the public-portal end-to-end installation and synthesis test.

Each phase must leave unsupported llmfit architectures excluded rather than
advertised as installable.
