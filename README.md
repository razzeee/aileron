# Aileron

> **Status:** Aileron is in very early development. Nobody should rely on it or use it for real work yet.

We all have reasons to be tired of AI. But NPUs are already shipping in most new PCs, and Linux should have a safe, boring way for apps to use that local compute for things that matter: accessibility, translation, transcription, summarization, and assistive workflows that should not require sending private data to a cloud service.

Aileron is a system-level local AI API for Linux — the missing counterpart to Apple's FoundationModels framework. It provides sandboxed applications with access to on-device inference through [xdg-desktop-portal](https://github.com/flatpak/xdg-desktop-portal), with no network exposure, no shared REST server, and no inference engine code running in the host session. Everything runs locally; no cloud dependency, ever.

## Problem

Linux has no equivalent to Apple's `FoundationModels` API. Two concrete blockers exist for the [xdg-desktop-portal AI proposal](https://github.com/flatpak/xdg-desktop-portal/issues/1743):

1. Existing solutions (e.g. ramalama) expose inference over localhost REST, which leaks into the Flatpak network namespace — every sandboxed app on the system can reach it.
2. There is no programmatic API for model and runtime management; tooling requires `exec + parse stdout`.

Aileron solves both. All IPC is over a Varlink Unix socket. Flatpak sandboxes cannot reach the socket directly; they go through the portal.

## Architecture

```
┌─────────────────────────────────────────┐
│  Flatpak sandbox                        │
│  aileron-demo  ──── D-Bus ────────────► │──► org.freedesktop.impl.portal.{Language,Speech,Vision}
└─────────────────────────────────────────┘             │
                                                        │ D-Bus
                                                 aileron-portal
                                                        │ Varlink (Unix socket)
                                                 aileron-daemon
                                                        │ stdin/stdout (hardened crun OCI bundle)
                                                 OCI runtime bundle
                                                        │
                                               llama.cpp / whisper.cpp
                                               (inside image, no network)
```

The management UI (`aileron`) speaks directly to the daemon over the same Varlink socket and runs outside any sandbox.

Further reading:

| Document | Audience | Description |
|---|---|---|
| [Runtime stdio protocol](docs/runtime-protocol.md) | Runtime authors | Container request/response contract used by the daemon |
| [App developer guide](docs/app-developer-guide.md) | App authors | How to use Aileron through portal-style task APIs instead of localhost REST |
| [Distribution packaging guide](docs/distro-packaging.md) | Packagers | Runtime dependencies, install locations, services, and hardware access notes |


## Workspace

| Crate | Type | Description |
|---|---|---|
| `aileron-daemon` | binary | systemd user service; Varlink socket; container and inference management |
| `aileron-portal` | binary | xdg-desktop-portal backend; bridges D-Bus ↔ Varlink |
| `aileron` | binary | GTK4/libadwaita management UI |
| `aileron-demo` | binary | Sandboxed GTK4 article summarizer; end-to-end demo app |
| `aileron-varlink` | library | Varlink IDL files and generated bindings |
| `aileron-ipc` | library | Varlink client/server connection helpers |

Reusable runtime images live in `runtimes/`. Model artifacts are installed separately under `$XDG_DATA_HOME/aileron/models/<model-id>/` and mounted read-only into runtimes at `/model`. Model manifests reference a `runtime_id`; runtime manifests map that ID plus the detected hardware variant to an OCI image.

| Directory | Description |
|---|---|
| `runtimes/llm-llama-cpp/` | llama-cpp-python runtime for text generation and structured output |
| `runtimes/asr-whisper-cpp/` | pywhispercpp runtime for audio transcription |
| `runtimes/vision-llama-cpp-gemma4/` | llama-cpp-python Gemma 4 multimodal runtime for image description |
| `runtimes/stub/` | no-ML test runtime implementing the stdio protocol |

## Building

### Prerequisites

```sh
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# System libraries — Fedora
sudo dnf install \
    gtk4-devel \
    libadwaita-devel \
    dbus-devel \
    pkg-config

# System libraries — Debian/Ubuntu
sudo apt install \
    libgtk-4-dev \
    libadwaita-1-dev \
    libdbus-1-dev \
    pkg-config

# Runtime dependencies
sudo dnf install skopeo crun   # Fedora
sudo apt install skopeo crun   # Debian/Ubuntu
```

### Build all crates

```sh
cargo build --workspace --release
```

### Build only the daemon and portal (no GTK required)

```sh
cargo build -p aileron-daemon -p aileron-portal --release
```

## Installation

### Daemon

```sh
install -Dm755 target/release/aileron-daemon ~/.local/bin/aileron-daemon
install -Dm644 systemd/aileron-daemon.service \
    ~/.config/systemd/user/aileron-daemon.service
systemctl --user daemon-reload
systemctl --user enable --now aileron-daemon
```

The daemon listens on `$XDG_RUNTIME_DIR/aileron.socket`.

**Daemon flags:**

| Flag | Env var | Default | Description |
|---|---|---|---|
| `--allow-all` | `AILERON_ALLOW_ALL` | false | Bypass all permission checks (dev/test only) |
| `--auto-grant` | `AILERON_AUTO_GRANT` | false | Grant permission automatically on first use |
| `--idle-timeout-secs` | `AILERON_IDLE_TIMEOUT_SECS` | 300 | Container idle timeout in seconds |
| `--container-memory` | `AILERON_CONTAINER_MEMORY` | `8g` | OCI memory limit for each runtime |
| `--oci-store` | `AILERON_OCI_STORE` | `$XDG_DATA_HOME/aileron/oci` | Local OCI runtime store |

### Portal backend

```sh
install -Dm755 target/release/aileron-portal ~/.local/bin/aileron-portal

# systemd user service
install -Dm644 systemd/aileron-portal.service \
    ~/.config/systemd/user/aileron-portal.service

# xdg-desktop-portal descriptor (tells the portal frontend which interfaces
# this backend implements)
install -Dm644 portal/aileron.portal \
    /usr/share/xdg-desktop-portal/portals/aileron.portal

# D-Bus session service activation file
install -Dm644 \
    portal/org.freedesktop.impl.portal.desktop.aileron.service \
    /usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.aileron.service

systemctl --user daemon-reload
systemctl --user enable --now aileron-portal
```

### Management UI

```sh
install -Dm755 target/release/aileron ~/.local/bin/aileron
aileron
```

## Building container images

Runtime images can be built with any OCI/Docker-compatible builder. The daemon pulls runtime images with `skopeo`, renders OCI layouts into root filesystems with `ocirender`, and executes the rendered rootfs with `crun`; model artifacts are not baked into runtime images. Installed profiles point at artifact directories and runtime IDs, and runtime manifests map those IDs to variant-specific OCI images.

| Runtime | Details |
|---|---|
| `runtimes/llm-llama-cpp/` | [LLM runtime README](runtimes/llm-llama-cpp/README.md) |
| `runtimes/asr-whisper-cpp/` | [ASR runtime README](runtimes/asr-whisper-cpp/README.md) |
| `runtimes/vision-llama-cpp-gemma4/` | [Vision runtime README](runtimes/vision-llama-cpp-gemma4/README.md) |
| `runtimes/stub/` | [Stub runtime README](runtimes/stub/README.md) |

Runtime manifests provide explicit runtime image refs per variant such as `cpu`, `cuda`, `rocm`, or `vulkan`; see [Hardware variant selection](#hardware-variant-selection). Model manifests only need to reference the `runtime_id`.

## Manifests

Manifests are discovered from `$XDG_DATA_HOME/aileron/manifests`, `/etc/aileron/manifests`, `/usr/share/aileron/manifests`, and `manifests` in the current working directory. Override the search path with `AILERON_MANIFEST_DIRS`.

Model manifests live under `models/` and reference a reusable runtime by ID:

```json
{
  "profile_id": "llama3.2-3b-instruct-q4",
  "model_id": "llama3.2-3b-instruct-q4",
  "llmfit_model_id": "meta-llama/Llama-3.2-3B-Instruct",
  "runtime_id": "llm-llama-cpp",
  "tier": "balanced",
  "use_cases": ["language.summarize", "language.translate", "language.analyze"],
  "artifacts": [
    {
      "role": "model",
      "url": "https://huggingface.co/.../resolve/main/model.gguf",
      "filename": "model.gguf",
      "sha256": "...",
      "size_bytes": 2019377600
    }
  ]
}
```

The optional `artifacts[].role` field identifies what a file is for runtimes that need more than one artifact. Single-file profiles usually use `model`; llama.cpp vision profiles commonly use both `model` and `mmproj`. Artifact roles must be unique within a manifest when present.

The `artifacts[].size_bytes` field is the preferred source for the user-facing install/download size, because it lives next to the exact URL and SHA-256 it describes. `disk_size_gb` is still accepted as fallback catalog metadata when exact artifact byte sizes are unavailable. The optional `llmfit_model_id` field should be the Hugging Face model name used by `llmfit-core`; when present, Aileron uses llmfit metadata to show a simple fit label and RAM/VRAM requirements for the current PC. If it is absent or unknown, Aileron falls back to the manifest's `min_ram_gb` metadata. These fields do not trigger automatic downloads or automatic reassignment.

Distributions should package Aileron, runtime manifests, and model catalog manifests. They do not need to ship model weights in the distro package. The user explicitly chooses a catalog profile to install, sees its size and recommendation metadata, then Aileron downloads and verifies the declared artifacts.

Runtime manifests live under `runtimes/` and map the runtime ID to OCI images for each hardware variant:

```json
{
  "runtime_id": "llm-llama-cpp",
  "images": {
    "cpu": "docker.io/example/aileron-runtime-llm-llama-cpp:cpu",
    "cuda": "docker.io/example/aileron-runtime-llm-llama-cpp:cuda",
    "rocm": "docker.io/example/aileron-runtime-llm-llama-cpp:rocm",
    "vulkan": "docker.io/example/aileron-runtime-llm-llama-cpp:vulkan"
  }
}
```

Installed model artifacts are stored under `$XDG_DATA_HOME/aileron/models/<model-id>/`, or `$AILERON_DATA_HOME/aileron/models/<model-id>/` when `AILERON_DATA_HOME` is set. Aileron owns this directory and mounts the selected profile's artifact directory read-only into runtimes at `/model`.

## End-to-end test with the stub container

The stub container requires no model download and responds instantly. It implements the full protocol, so it exercises every layer — daemon, Varlink, container stdio, and back.

```sh
# 1. Build the daemon
cargo build -p aileron-daemon

# 2. Build the stub runtime image (no model download needed) and export its
#    merged rootfs into an Aileron OCI store. This keeps Podman as an optional
#    local image builder/exporter; the daemon itself runs the rootfs with crun.
podman build -t aileron/stub:cpu runtimes/stub/
export AILERON_OCI_STORE="$PWD/.aileron-oci"
stub_container=$(podman create localhost/aileron/stub:cpu)
mkdir -p "$AILERON_OCI_STORE/rootfs/localhost_aileron_stub_cpu"
podman export "$stub_container" |
    tar -C "$AILERON_OCI_STORE/rootfs/localhost_aileron_stub_cpu" -xf -
podman rm "$stub_container"

# 3. Create local manifests for the stub runtime and profile
mkdir -p manifests/runtimes manifests/models
cat > manifests/runtimes/stub.json <<'JSON'
{"runtime_id":"stub","images":{"cpu":"localhost/aileron/stub:cpu"}}
JSON
cat > manifests/models/stub.json <<'JSON'
{"profile_id":"stub","model_id":"stub","runtime_id":"stub","use_cases":["language.summarize","language.analyze","speech.transcribe","vision.describe"],"artifacts":[]}
JSON

# 4. Start the daemon in allow-all mode (skips permission checks)
AILERON_ALLOW_ALL=true AILERON_OCI_STORE="$AILERON_OCI_STORE" ./target/debug/aileron-daemon &

# 5. Install the stub profile from the local manifest
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Models.InstallManifest" \
    '{"profile_id":"stub"}'

# 6. Create a session
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Inference.CreateSession" \
    '{"app_id":"test","use_case":"language.summarize","instructions":"You are a concise test assistant."}'
# → {"session_id": "..."}   copy the value

# 7. Generate (replace SESSION_ID)
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Inference.Respond" \
    '{"session_id":"SESSION_ID","prompt":"Hello world","options":{"maximum_response_tokens":64,"temperature":0.0,"sampling_mode":"greedy"}}'
# → {"content": "Hello world"}   stub echoes the prompt

# 8. End the session
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Inference.EndSession" \
    '{"session_id":"SESSION_ID"}'

# 9. Stop the daemon
kill %1
```

Install the `varlink` CLI with: `cargo install varlink-cli`

## Getting started (with a real model)

### 1. Install and assign a profile

```sh
# Start the management UI
aileron
```

In the **Models** page, click **Add Profile...**, choose one of the available runtime IDs, and provide the model file URL, SHA-256, and use-cases. Aileron derives the filename, model ID, and profile ID. Installed profiles can then be assigned to the use-case tokens they declare. Assign the same language profile to multiple declared language task tokens when one model backs several operations, for example `language.summarize` for free-text summaries and `language.analyze` for guided structured analysis.

### 2. Grant permission to an app

The first time an app requests inference, Aileron denies it (no entry exists). Grant access in the **Permissions** page, or directly via Varlink:

```
aileron.Permissions.SetAppPermission(
    app_id:   "org.example.MyApp",
    use_case: "language.summarize",
    allowed:  true
)
```

### 3. Run the demo app

```sh
cargo run -p aileron-demo
```

Paste or fetch an article URL, then click **Summarize**. Tokens stream into the output view as they arrive from the container.

## Use-case tokens

Use-case tokens are the daemon's routing and policy keys. A token maps to one assigned profile, permissions are granted per app and token, and warm containers are pooled per profile. Assigning the same profile to multiple declared tokens is valid.

Use-cases describe the task intent and modality; methods describe the operation shape. `Respond` and `RespondGuided` are text-generation operations for `language.*` sessions (except `language.embed`). `Embed` is for `language.embed`. `Transcribe` serves both `speech.transcribe` and `speech.translate` (the daemon runs the whisper transcribe or translate-to-English task based on the session use-case). `Describe` is for `vision.describe`, `Ocr` is for `vision.ocr`, and `Segment` is for `vision.segment`. There is intentionally no separate `language.guided` token: guided generation is an output constraint for a real language task use-case, not a task intent of its own. Use `language.analyze` when one guided response combines multiple analytical intents, such as summary, extraction, and classification fields.

| Token | Task | Backend |
|---|---|---|
| `language.summarize` | Summarize text | llama.cpp |
| `language.translate` | Translate text | llama.cpp |
| `language.rephrase` | Rewrite / simplify text | llama.cpp |
| `language.classify` | Classify / tag text | llama.cpp |
| `language.extract` | Extract structured data | llama.cpp |
| `language.analyze` | Derive mixed structured insights from text | llama.cpp |
| `language.embed` | Compute text embedding vectors | llama.cpp |
| `speech.transcribe` | Transcribe audio (16 kHz mono f32le, base64) | whisper.cpp |
| `speech.translate` | Translate spoken audio to English text | whisper.cpp |
| `vision.describe` | Describe image contents (PNG/JPEG, base64) | llava / llama.cpp |
| `vision.ocr` | Extract text from an image (PNG/JPEG, base64) | llama.cpp multimodal |
| `vision.segment` | Identify objects in image | llama.cpp multimodal |

## Varlink interfaces

The daemon exposes four interfaces over `$XDG_RUNTIME_DIR/aileron.socket`.

### `aileron.Inference`

Create sessions, generate text, get structured JSON output, compute embeddings, transcribe and translate audio, describe and OCR images.

```varlink
type ModelAvailability (is_available: bool, code: string, reason: string)
type GenerationOptions (maximum_response_tokens: int, temperature: float, sampling_mode: string, source_language_hint: string, target_language_hint: string)
type GuidedField (name: string, kind: string, description: string, required: bool)
type VisionSegment (label: string, confidence: float, x: float, y: float, width: float, height: float)

method GetUseCaseAvailability(app_id: string, use_case: string) -> (availability: ModelAvailability)
method CreateSession(app_id: string, use_case: string, instructions: string) -> (session_id: string)
method Prewarm(session_id: string, prompt_prefix: string) -> ()
method Respond(session_id: string, prompt: string, options: GenerationOptions) -> (content: string)
method StreamResponse(session_id: string, prompt: string, options: GenerationOptions) -> (token: string)
method RespondGuided(session_id: string, prompt: string, fields: []GuidedField, options: GenerationOptions) -> (content: string)
method Embed(session_id: string, text: string) -> (embedding: []float)
method Transcribe(session_id: string, audio: string, language_hint: string) -> (text: string)
method Describe(session_id: string, image: string) -> (text: string)
method Ocr(session_id: string, image: string) -> (text: string)
method Segment(session_id: string, image: string) -> (segments: []VisionSegment)
method EndSession(session_id: string) -> ()
```

`instructions` are stored on the session and forwarded to text containers as the container `system` prompt. `audio` is raw 16 kHz mono f32le PCM bytes encoded as base64; `Transcribe` returns a verbatim transcript for `speech.transcribe` sessions and an English translation for `speech.translate` sessions. `image` is PNG or JPEG bytes encoded as base64. `Embed` returns the embedding vector for `text`. `VisionSegment` coordinates are normalized `0.0..1.0` rectangles relative to image dimensions.

`GenerationOptions.maximum_response_tokens` must be greater than zero and fit in `u32`. `temperature` must be finite and non-negative. `sampling_mode` must be non-empty. `source_language_hint` and `target_language_hint` are optional strings for `language.translate`; pass empty strings when unspecified. Today the daemon validates sampling fields, forwards `maximum_response_tokens` to containers as `max_tokens`, and folds translation hints into the session instructions for `language.translate`.

`GuidedField.kind` supports `string`, `number`, `integer`, `boolean`, and `string_array`. The daemon converts guided fields into a JSON Schema object with `additionalProperties: false`, sends it to the container as `response_format.schema`, then validates the returned JSON before replying.

`ModelAvailability.code` is stable and machine-readable. Known values are `available`, `permission_denied`, `no_profile_assigned`, `profile_not_installed`, `artifact_missing`, `runtime_unsupported`, `runtime_missing`, `hardware_unsupported`, and `busy`. `reason` is human-readable detail.

Inference errors are represented as Varlink errors: `PermissionDenied`, `SessionNotFound`, `ModelUnavailable`, `InvalidGenerationOptions`, `GuidedGenerationFailed`, `GenerationFailed`, `ContextWindowExceeded`, `UnsupportedLanguage`, `SafetyRefusal`, `RequestCancelled`, and `InvalidInput`.

### `aileron.Models`

List, install, delete, and assign installed profiles. Profiles reference model artifacts and reusable runtime IDs; runtime manifests provide variant-specific OCI images.

```varlink
method List() -> (profiles: []ProfileInfo)
method ListRuntimeManifests() -> (runtimes: []RuntimeManifestInfo)
method ListRuntimeImages() -> (images: []OciRuntimeImage)
method RemoveRuntimeImage(image_id: string) -> ()
method UpdateRuntimeImage(image_ref: string) -> ()
method PruneUnusedRuntimeImages() -> (removed: []string, errors: []RuntimeImageCleanupError)
method ListCatalog() -> (profiles: []CatalogProfileInfo)
method ListInstalls() -> (installs: []InstallStatus)
method CancelInstall(profile_id: string) -> ()
method InstallManifest(profile_id: string) -> (progress: InstallProgress, auto_assigned: []string, conflicts: []UseCaseConflict)
method InstallUrlProfile(runtime_id: string, url: string, sha256: string, mmproj_url: string, mmproj_sha256: string, use_cases: []string) -> (progress: InstallProgress, auto_assigned: []string, conflicts: []UseCaseConflict)
method DeleteProfile(profile_id: string, force: bool) -> ()
method AssignUseCase(profile_id: string, use_case: string) -> ()

error ProfileNotFound(profile_id: string)
error ProfileInUse(profile_id: string)
error InstallFailed(profile_id: string, reason: string)
error UnsupportedUseCase(profile_id: string, use_case: string)
```

`InstallManifest` installs a packaged catalog profile, downloads and verifies declared artifacts, resolves the host variant through runtime manifests, and pulls the selected OCI runtime image with `skopeo`. `InstallUrlProfile` is the ad-hoc import path; pass empty `mmproj_url` and `mmproj_sha256` for single-artifact profiles. `AssignUseCase` only accepts use-case tokens listed by the target profile.

### `aileron.Permissions`

Manage per-app, per-use-case access grants.

```varlink
method ListAppPermissions() -> (permissions: []AppPermission)
method SetAppPermission(app_id: string, use_case: string, allowed: bool) -> ()
```

### `aileron.Sessions`

Inspect and kill active inference sessions.

```varlink
method ListActive() -> (sessions: []SessionInfo)
method KillSession(session_id: string) -> ()
```

## D-Bus portal interfaces

`aileron-portal` is the sandbox-facing API. It registers on the session bus as `org.freedesktop.impl.portal.desktop.aileron` at path `/org/freedesktop/portal/desktop` and serves three task-clustered interfaces.

The portal does not talk to containers directly. It translates D-Bus calls into `aileron.Inference` Varlink calls, and the daemon owns permissions, sessions, model assignments, and container stdio.

| Interface | Use-case prefix | Methods |
|---|---|---|
| `org.freedesktop.impl.portal.Language` | `language.*` | `GetUseCaseAvailability`, `CreateSession`, `Prewarm`, `Respond`, `StreamResponse`, `RespondGuided`, `Embed`, `EndSession` |
| `org.freedesktop.impl.portal.Speech` | `speech.*` | `GetUseCaseAvailability`, `CreateSession`, `Prewarm`, `Transcribe`, `EndSession` |
| `org.freedesktop.impl.portal.Vision` | `vision.*` | `GetUseCaseAvailability`, `CreateSession`, `Prewarm`, `Describe`, `Ocr`, `Segment`, `EndSession` |

`GetUseCaseAvailability`, `CreateSession`, and `EndSession` have the same signatures on each interface. Each interface validates that the requested use-case token matches its prefix.

### Shared Methods

| Method | Parameters | Returns | Notes |
|---|---|---|---|
| `GetUseCaseAvailability` | `app_id: s, use_case: s` | `(is_available: b, code: s, reason: s)` | Checks whether an assigned profile has local artifacts and a runtime image |
| `CreateSession` | `app_id: s, use_case: s, instructions: s` | `session_id: s` | Creates a session bound to the assigned profile; does not start the container by itself |
| `Prewarm` | `session_id: s, prompt_prefix: s` | `()` | Starts the backing container before the first user-visible operation; pass an empty prefix when no text prefix applies |
| `EndSession` | `session_id: s` | `()` | Ends the session; the per-profile container remains pooled until idle timeout |

### Language Methods

| Method | Parameters | Returns | Notes |
|---|---|---|---|
| `Respond` | `session_id: s, prompt: s, options: (xdsss)` | `content: s` | Returns full generated text |
| `StreamResponse` | `session_id: s, prompt: s, options: (xdsss)` | `()` | Emits `TokenReceived` signals; final token has `done=true` |
| `RespondGuided` | `session_id: s, prompt: s, fields: a(sssb), options: (xdsss)` | `content: s` | Language sessions only; returns JSON matching guided output fields |
| `Embed` | `session_id: s, text: s` | `embedding: ad` | `language.embed` sessions only; returns an embedding vector |

### Speech Methods

| Method | Parameters | Returns | Notes |
|---|---|---|---|
| `Transcribe` | `session_id: s, audio_b64: s, language_hint: s` | `text: s` | 16 kHz mono f32le PCM, base64; empty hint means auto-detect/no hint; transcribes or translates per use-case |

### Vision Methods

| Method | Parameters | Returns | Notes |
|---|---|---|---|
| `Describe` | `session_id: s, image_b64: s` | `text: s` | PNG or JPEG, base64 |
| `Ocr` | `session_id: s, image_b64: s` | `text: s` | `vision.ocr` sessions only; PNG or JPEG, base64; extracts text |
| `Segment` | `session_id: s, image_b64: s` | `segments: []VisionSegment` | PNG or JPEG, base64; normalized boxes |

These are D-Bus signatures: parentheses define a struct, and `a(...)` means an array of structs. `options: (xdsss)` is `GenerationOptions`: `maximum_response_tokens` as int64, `temperature` as float64, `sampling_mode` as string, `source_language_hint` as string, and `target_language_hint` as string. Empty language hints mean unspecified. The language hints only affect `language.translate`. `fields: a(sssb)` is an array of `GuidedField` structs: name, kind, description, required.

### Signals

| Signal | Parameters | Fired when |
|---|---|---|
| `ModelLoading` | `message: s` | The portal is about to start a cold backing container; available on each interface |
| `TokenReceived` | `session_id: s, token: s, done: b` | Each token during `StreamResponse` on `Language` |

D-Bus callers see underlying Varlink failures as `org.freedesktop.DBus.Error.Failed` with the Varlink error text.

## Portal-to-container API boundary

There is no direct portal-to-container transport. The complete inference API path is:

1. Sandboxed app calls `org.freedesktop.impl.portal.Language`, `org.freedesktop.impl.portal.Speech`, or `org.freedesktop.impl.portal.Vision` over session D-Bus.
2. `aileron-portal` maps that call to `aileron.Inference` over the daemon's Varlink Unix socket.
3. `aileron-daemon` validates permissions/options, resolves the assigned profile for the session use-case, and serializes one request at a time to the profile container over stdio.
4. The container returns newline-delimited JSON chunks on stdout; the daemon aggregates or streams them back through Varlink, and the portal returns a D-Bus value or emits D-Bus signals.

The stable API surfaces are the D-Bus portal interface, the Varlink `aileron.Inference` interface, and the container stdio protocol below. Containers should not assume anything about D-Bus, and portal clients should not assume anything about container JSON beyond the semantics exposed by the portal methods.

## Container stdio protocol

Each OCI image implements a simple newline-delimited JSON protocol over stdin/stdout. No ports, no sockets; the generated OCI bundle uses an isolated network namespace. The daemon is the only process that speaks this protocol.

### Framing and readiness

- Every stdin/stdout line is one UTF-8 JSON object.
- The container signals readiness by writing a stderr line containing `ready` after model initialization.
- Stderr lines before readiness are human-readable loading/status messages.
- Every request has an `id` string and a `type` string.
- Every response for a request echoes the same `id`.
- Successful streamed responses end with a line where `done` is `true`.
- Containers are used serially per profile; do not rely on multiplexed in-flight requests.

### Request fields

| Field | Type | Used by | Description |
|---|---|---|---|
| `id` | string | all requests | Correlation ID generated by the daemon |
| `type` | string | all requests | One of `generate`, `generate_structured`, `transcribe`, `describe`, `segment`, `embed` |
| `system` | string | `generate`, `generate_structured` | Session instructions from `CreateSession` |
| `prompt` | string | `generate`, `generate_structured`, optionally `describe` | User prompt or image prompt |
| `max_tokens` | number | `generate`, `generate_structured`, optionally `describe` | Derived from `GenerationOptions.maximum_response_tokens` for text requests |
| `audio` | string | `transcribe` | Base64-encoded raw PCM bytes, 16 kHz mono f32le |
| `language_hint` | string | `transcribe` | Optional spoken language hint; omitted when unspecified |
| `image` | string | `describe` | Base64-encoded PNG or JPEG bytes |
| `response_format` | object | `generate_structured` | `{ "type": "json_schema", "schema": ... }` |

All requests may include an optional `system` field (string) to set the system prompt. The entrypoint defaults to: _"You are a helpful assistant. Always respond in the same language as the user's message."_

### Streaming text generation

```jsonc
// request
{"id": "uuid", "type": "generate", "prompt": "Summarise this: ...", "max_tokens": 512}

// response (one line per token, final line has done:true)
{"id": "uuid", "token": "Here"}
{"id": "uuid", "token": " is", "done": true}
```

### Structured output

The daemon sends a `response_format` object containing the caller's JSON Schema. The container constrains sampling to valid JSON via llama.cpp grammar and replies with a single `result` line. The daemon validates the result against the schema before returning it; mismatches produce `GuidedGenerationFailed`.

```jsonc
// request
{
  "id": "uuid",
  "type": "generate_structured",
  "prompt": "Extract the author and year from: ...",
  "max_tokens": 256,
  "response_format": {
    "type": "json_schema",
    "schema": {
      "type": "object",
      "properties": {
        "author": {"type": "string"},
        "year":   {"type": "integer"}
      },
      "required": ["author", "year"]
    }
  }
}

// response (single line)
{"id": "uuid", "result": "{\"author\": \"Turing\", \"year\": 1950}", "done": true}
```

### Audio transcription

```jsonc
// request
{"id": "uuid", "type": "transcribe", "audio": "<base64 PCM 16kHz mono f32le>"}

// response (streamed segment text)
{"id": "uuid", "token": "Hello world.", "done": true}
```

### Image description

```jsonc
// request
{"id": "uuid", "type": "describe", "image": "<base64 PNG or JPEG>"}

// response (streamed)
{"id": "uuid", "token": "A cat", "done": true}
```

### Image segmentation

```jsonc
// request
{"id": "uuid", "type": "segment", "image": "<base64 PNG or JPEG>"}

// response (single line)
{"id": "uuid", "result": "{\"segments\":[{\"label\":\"cat\",\"confidence\":0.82,\"x\":0.1,\"y\":0.2,\"width\":0.5,\"height\":0.4}]}", "done": true}
```

## Container lifecycle

- Containers start on demand when `Prewarm` or the first inference call needs an assigned profile runtime.
- The daemon waits for the container to signal `ready` on stderr before using it.
- The portal emits `ModelLoading("starting model")` before cold text-generation calls that may start a container.
- One container runs per profile, shared across all sessions bound to that profile.
- `EndSession` removes the session, but the per-profile container remains pooled until idle timeout.
- `KillSession` removes the session and stops the profile container if no other active session uses it.
- Idle containers are terminated after 5 minutes by default (configurable via `--idle-timeout-secs`).
- A crash in the container kills only that container, not the daemon.

## Hardware variant selection

The daemon probes the host once at startup and selects the best available runtime image variant:

| Detection | Variant tag | How |
|---|---|---|
| `nvidia-smi` reports a GPU | `:cuda` | NVIDIA CUDA |
| `rocm-smi` reports a GPU | `:rocm` | AMD ROCm |
| `vulkaninfo` reports a device | `:vulkan` | Any Vulkan GPU |
| Nothing found | `:cpu` | CPU fallback |

Runtime manifests declare explicit runtime images per variant. If the detected variant is unsupported, the daemon may fall back to the runtime's CPU image. If the selected image is missing locally, availability reports unavailable. Override detection with `AILERON_VARIANT=cpu|cuda|rocm|vulkan`.

## Container security

Every runtime starts from an OCI `config.json` generated by the daemon. The bundle uses:

```
new network namespace   # no host network access
read-only rootfs        # immutable unpacked runtime filesystem
tmpfs /tmp              # per-container scratch, mode 1777, noexec, nodev
tmpfs /dev/shm          # shared memory for ML runtimes, noexec, nodev
empty capabilities      # no Linux capabilities
noNewPrivileges         # privilege escalation disabled
pids limit 256          # fork bomb protection
memory limit 8g         # OOM protection, configurable
read-only /model        # selected profile artifact directory
```

Accelerator runtimes receive only the host device mounts they need: CUDA gets existing `/dev/nvidia*` devices and optional `/proc/driver/nvidia`, ROCm gets `/dev/kfd` and `/dev/dri`, and Vulkan gets `/dev/dri`. Accelerator variants also receive read-only `/sys` for driver and topology discovery.

Runtime `/tmp` is a container-local tmpfs, not a bind mount to host `/tmp`, so it is not shared between containers. A warm runtime container can handle multiple requests for the same profile, so runtime implementations must use per-request temporary paths and remove request-specific files before replying.

Running the daemon as a user service means `crun` executes runtime bundles rootlessly under the user's session.

## Data files

| File | Contents |
|---|---|
| `$XDG_DATA_HOME/aileron/assignments.json` | Use-case → installed profile mapping |
| `$XDG_DATA_HOME/aileron/permissions.json` | Per-app, per-use-case permission grants |
| `$XDG_DATA_HOME/aileron/oci/rootfs/*` | Rendered runtime root filesystems |
| `$XDG_DATA_HOME/aileron/oci/metadata/*.json` | Runtime image metadata used for listing and cleanup |
| `$XDG_DATA_HOME/aileron/manifests/models/*.json` | User-installed model manifests |
| `$XDG_DATA_HOME/aileron/manifests/runtimes/*.json` | User-installed runtime manifests |
| `/etc/aileron/manifests/models/*.json` | Admin-provided model manifests |
| `/etc/aileron/manifests/runtimes/*.json` | Admin-provided runtime manifests |
| `/usr/share/aileron/manifests/models/*.json` | System model manifests |
| `/usr/share/aileron/manifests/runtimes/*.json` | System runtime manifests |

## Security properties

- No inference engine code runs in the daemon process. A crash in llama.cpp kills only the container.
- No REST API, no TCP, no UDP. Attack surface is the Varlink Unix socket and D-Bus.
- Containers run from daemon-generated OCI bundles with isolated networking, no capabilities, read-only root filesystems, and `noNewPrivileges` (see [Container security](#container-security)).
- Rootless `crun` executes runtime bundles under the user's session instead of a privileged system daemon.
- OCI images are content-addressed; image refs can be pinned to a digest (`@sha256:...`).
- Per-app permissions are enforced in the daemon, not the portal.
- The daemon runs with `PrivateNetwork=yes` in the systemd unit.

## Out of scope for v1

- Multi-user (the daemon is a user service, one instance per login session)
- Model/runtime signature verification through containers/image policy
- A system-wide shared model store

## License

GPL-3.0-or-later
