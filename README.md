# Aileron

A system-level local AI API for Linux — the missing counterpart to Apple's FoundationModels framework.

Aileron provides sandboxed applications with access to on-device inference through [xdg-desktop-portal](https://github.com/flatpak/xdg-desktop-portal), with no network exposure, no shared REST server, and no inference engine code running in the host session. Everything runs locally; no cloud dependency, ever.

## Problem

Linux has no equivalent to Apple's `FoundationModels` API. Two concrete blockers exist for the [xdg-desktop-portal AI proposal](https://github.com/flatpak/xdg-desktop-portal/issues/1743):

1. Existing solutions (e.g. ramalama) expose inference over localhost REST, which leaks into the Flatpak network namespace — every sandboxed app on the system can reach it.
2. There is no programmatic API for model and runtime management; tooling requires `exec + parse stdout`.

Aileron solves both. All IPC is over a Varlink Unix socket. Flatpak sandboxes cannot reach the socket directly; they go through the portal.

## Architecture

```
┌─────────────────────────────────────────┐
│  Flatpak sandbox                        │
│  aileron-demo  ──── D-Bus ────────────► │──► org.freedesktop.impl.portal.AI
└─────────────────────────────────────────┘             │
                                                        │ D-Bus
                                                 aileron-portal
                                                        │ Varlink (Unix socket)
                                                 aileron-daemon
                                                        │ stdin/stdout (hardened podman)
                                                 OCI container
                                                        │
                                               llama.cpp / whisper.cpp
                                               (inside image, no network)
```

The management UI (`aileron`) speaks directly to the daemon over the same Varlink socket and runs outside any sandbox.


## Workspace

| Crate | Type | Description |
|---|---|---|
| `aileron-daemon` | binary | systemd user service; Varlink socket; container and inference management |
| `aileron-portal` | binary | xdg-desktop-portal backend; bridges D-Bus ↔ Varlink |
| `aileron` | binary | GTK4/libadwaita management UI |
| `aileron-demo` | binary | Sandboxed GTK4 article summarizer; end-to-end demo app |
| `aileron-varlink` | library | Varlink IDL files and generated bindings |
| `aileron-ipc` | library | Varlink client/server connection helpers |

Container images live in `images/`:

| Directory | Description |
|---|---|
| `images/llm/` | llama-cpp-python image for text generation and structured output |
| `images/asr/` | faster-whisper image for audio transcription |
| `images/vision/` | llama-cpp-python multimodal image for image description |

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

# Runtime dependency
sudo dnf install podman   # Fedora
sudo apt install podman   # Debian/Ubuntu
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

Images are built with `podman build` or `docker build`. Image-specific platform matrices, build commands, model arguments, and environment variables live next to each image:

| Image | Details |
|---|---|
| `images/llm/` | [LLM image README](images/llm/README.md) |
| `images/asr/` | [ASR image README](images/asr/README.md) |
| `images/vision/` | [Vision image README](images/vision/README.md) |
| `images/stub/` | [Stub image README](images/stub/README.md) |

The daemon can resolve untagged model refs to platform tags such as `:cpu`, `:cuda`, `:rocm`, or `:vulkan`; see [Hardware variant selection](#hardware-variant-selection).

## End-to-end test with the stub container

The stub container requires no model download and responds instantly. It implements the full protocol, so it exercises every layer — daemon, Varlink, container stdio, and back.

```sh
# 1. Build the daemon
cargo build -p aileron-daemon

# 2. Build the stub image (no model download needed)
podman build -t aileron/stub:latest images/stub/

# 3. Start the daemon in allow-all mode (skips permission checks)
AILERON_ALLOW_ALL=true ./target/debug/aileron-daemon &

# 4. Assign the stub image to a use-case
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Models.AssignUseCase" \
    '{"image_ref":"localhost/aileron/stub:latest","use_case":"llm.summarize"}'

# 5. Create a session
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Inference.CreateLanguageModelSession" \
    '{"app_id":"test","use_case":"llm.summarize","instructions":"You are a concise test assistant."}'
# → {"session_id": "..."}   copy the value

# 6. Generate (replace SESSION_ID)
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Inference.Respond" \
    '{"session_id":"SESSION_ID","prompt":"Hello world","options":{"maximum_response_tokens":64,"temperature":0.0,"sampling_mode":"greedy"}}'
# → {"content": "Hello world"}   stub echoes the prompt

# 7. End the session
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Inference.EndSession" \
    '{"session_id":"SESSION_ID"}'

# 8. Stop the daemon
kill %1
```

Install the `varlink` CLI with: `cargo install varlink-cli`

## Getting started (with a real model)

### 1. Pull and assign a model

```sh
# Start the management UI
aileron
```

In the **Models** page, enter an image reference and click **Pull**. Once pulled, click **Delete** ▸ dropdown ▸ **Assign use-case** to bind the image to a use-case token.

### 2. Grant permission to an app

The first time an app requests inference, Aileron denies it (no entry exists). Grant access in the **Permissions** page, or directly via Varlink:

```
aileron.Permissions.SetAppPermission(
    app_id:   "org.example.MyApp",
    use_case: "llm.summarize",
    allowed:  true
)
```

### 3. Run the demo app

```sh
cargo run -p aileron-demo
```

Paste or fetch an article URL, then click **Summarize**. Tokens stream into the output view as they arrive from the container.

## Use-case tokens

| Token | Task | Backend |
|---|---|---|
| `llm.summarize` | Summarize text | llama.cpp |
| `llm.translate` | Translate text | llama.cpp |
| `llm.rephrase` | Rewrite / simplify text | llama.cpp |
| `llm.classify` | Classify / tag text | llama.cpp |
| `llm.extract` | Extract structured data | llama.cpp |
| `asr.transcribe` | Transcribe audio (16 kHz mono f32le, base64) | whisper.cpp |
| `vision.describe` | Describe image contents (PNG/JPEG, base64) | llava / llama.cpp |
| `vision.segment` | Identify objects in image | llama.cpp multimodal |

## Varlink interfaces

The daemon exposes four interfaces over `$XDG_RUNTIME_DIR/aileron.socket`.

### `aileron.Inference`

Create sessions, generate text, get structured JSON output, transcribe audio, describe images.

```varlink
type ModelAvailability (is_available: bool, reason: string)
type GenerationOptions (maximum_response_tokens: int, temperature: float, sampling_mode: string)
type GuidedField (name: string, kind: string, description: string, required: bool)

method GetLanguageModelAvailability(app_id: string, use_case: string) -> (availability: ModelAvailability)
method CreateLanguageModelSession(app_id: string, use_case: string, instructions: string) -> (session_id: string)
method Prewarm(session_id: string, prompt_prefix: string) -> ()
method Respond(session_id: string, prompt: string, options: GenerationOptions) -> (content: string)
method StreamResponse(session_id: string, prompt: string, options: GenerationOptions) -> (token: string)
method RespondGuided(session_id: string, prompt: string, fields: []GuidedField, options: GenerationOptions) -> (content: string)
method Transcribe(session_id: string, audio: string) -> (text: string)
method Describe(session_id: string, image: string) -> (text: string)
method EndSession(session_id: string) -> ()
```

`instructions` are stored on the session and forwarded to text containers as the container `system` prompt. `audio` is raw 16 kHz mono f32le PCM bytes encoded as base64. `image` is PNG or JPEG bytes encoded as base64.

`GenerationOptions.maximum_response_tokens` must be greater than zero and fit in `u32`. `temperature` must be finite and non-negative. `sampling_mode` must be non-empty; today the daemon validates it but only forwards `maximum_response_tokens` to containers as `max_tokens`.

`GuidedField.kind` supports `string`, `number`, `integer`, `boolean`, and `string_array`. The daemon converts guided fields into a JSON Schema object with `additionalProperties: false`, sends it to the container as `response_format.schema`, then validates the returned JSON before replying.

Inference errors are represented as Varlink errors: `PermissionDenied`, `SessionNotFound`, `ModelUnavailable`, `InvalidGenerationOptions`, `GuidedGenerationFailed`, and `GenerationFailed`.

### `aileron.Models`

Pull, list, delete, and assign OCI images.

```varlink
method List() -> (models: []ModelInfo)
method Pull(image_ref: string) -> (progress: PullProgress, auto_assigned: []string, conflicts: []UseCaseConflict)
method Delete(image_ref: string) -> ()
method AssignUseCase(image_ref: string, use_case: string) -> ()
```

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

## D-Bus portal interface

`aileron-portal` is the sandbox-facing API. It registers on the session bus as `org.freedesktop.impl.portal.desktop.aileron` at path `/org/freedesktop/portal/desktop`, interface `org.freedesktop.impl.portal.AI`.

The portal does not talk to containers directly. It translates D-Bus calls into `aileron.Inference` Varlink calls, and the daemon owns permissions, sessions, model assignments, and container stdio.

### Methods

| Method | Parameters | Returns | Notes |
|---|---|---|---|
| `GetLanguageModelAvailability` | `app_id: s, use_case: s` | `(is_available: b, reason: s)` | Checks whether an assigned image is present locally |
| `CreateLanguageModelSession` | `app_id: s, use_case: s, instructions: s` | `session_id: s` | Creates a session; does not start the container by itself |
| `Prewarm` | `session_id: s, prompt_prefix: s` | `()` | Starts the backing container before the first response |
| `Respond` | `session_id: s, prompt: s, options: (xds)` | `content: s` | Returns full generated text |
| `StreamResponse` | `session_id: s, prompt: s, options: (xds)` | `()` | Emits `TokenReceived` signals; final token has `done=true` |
| `RespondGuided` | `session_id: s, prompt: s, fields: a(sssb), options: (xds)` | `content: s` | Returns JSON matching guided fields |
| `Transcribe` | `session_id: s, audio_b64: s` | `text: s` | 16 kHz mono f32le PCM, base64 |
| `Describe` | `session_id: s, image_b64: s` | `text: s` | PNG or JPEG, base64 |
| `EndSession` | `session_id: s` | `()` | |

`options: (xds)` is `GenerationOptions`: `maximum_response_tokens` as int64, `temperature` as float64, and `sampling_mode` as string. `fields: a(sssb)` is an array of `GuidedField`: name, kind, description, required.

### Signals

| Signal | Parameters | Fired when |
|---|---|---|
| `ModelLoading` | `message: s` | The portal is about to start a cold text-generation container |
| `TokenReceived` | `session_id: s, token: s, done: b` | Each token during `StreamResponse` |

D-Bus callers see underlying Varlink failures as `org.freedesktop.DBus.Error.Failed` with the Varlink error text.

## Portal-to-container API boundary

There is no direct portal-to-container transport. The complete inference API path is:

1. Sandboxed app calls `org.freedesktop.impl.portal.AI` over session D-Bus.
2. `aileron-portal` maps that call to `aileron.Inference` over the daemon's Varlink Unix socket.
3. `aileron-daemon` validates permissions/options, resolves the assigned image for the session use-case, and serializes one request at a time to the use-case container over stdio.
4. The container returns newline-delimited JSON chunks on stdout; the daemon aggregates or streams them back through Varlink, and the portal returns a D-Bus value or emits D-Bus signals.

The stable API surfaces are the D-Bus portal interface, the Varlink `aileron.Inference` interface, and the container stdio protocol below. Containers should not assume anything about D-Bus, and portal clients should not assume anything about container JSON beyond the semantics exposed by the portal methods.

## Container stdio protocol

Each OCI image implements a simple newline-delimited JSON protocol over stdin/stdout. No ports, no sockets; the container has `--network=none`. The daemon is the only process that speaks this protocol.

### Framing and readiness

- Every stdin/stdout line is one UTF-8 JSON object.
- The container signals readiness by writing a stderr line containing `ready` after model initialization.
- Stderr lines before readiness are human-readable loading/status messages.
- Every request has an `id` string and a `type` string.
- Every response for a request echoes the same `id`.
- Successful streamed responses end with a line where `done` is `true`.
- Containers are used serially per use-case; do not rely on multiplexed in-flight requests.

### Request fields

| Field | Type | Used by | Description |
|---|---|---|---|
| `id` | string | all requests | Correlation ID generated by the daemon |
| `type` | string | all requests | One of `generate`, `generate_structured`, `transcribe`, `describe` |
| `system` | string | `generate`, `generate_structured` | Session instructions from `CreateLanguageModelSession` |
| `prompt` | string | `generate`, `generate_structured`, optionally `describe` | User prompt or image prompt |
| `max_tokens` | number | `generate`, `generate_structured`, optionally `describe` | Derived from `GenerationOptions.maximum_response_tokens` for text requests |
| `audio` | string | `transcribe` | Base64-encoded raw PCM bytes, 16 kHz mono f32le |
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

## Container lifecycle

- Containers start on demand when `Prewarm` or the first inference call needs an assigned use-case image.
- The daemon waits for the container to signal `ready` on stderr before using it.
- The portal emits `ModelLoading("starting model")` before cold text-generation calls that may start a container.
- One container runs per use-case, shared across all sessions for that use-case.
- `EndSession` removes the session, but the per-use-case container remains pooled until idle timeout.
- Idle containers are terminated after 5 minutes by default (configurable via `--idle-timeout-secs`).
- A crash in the container kills only that container, not the daemon.

## Hardware variant selection

The daemon probes the host once at startup and selects the best available image variant:

| Detection | Variant tag | How |
|---|---|---|
| `nvidia-smi` reports a GPU | `:cuda` | NVIDIA CUDA |
| `rocm-smi` reports a GPU | `:rocm` | AMD ROCm |
| `vulkaninfo` reports a device | `:vulkan` | Any Vulkan GPU |
| Nothing found | `:cpu` | CPU fallback |

When you assign a base image ref with no tag (e.g. `aileron/llama3.2-3b-instruct`), the daemon appends the detected variant automatically. An explicit tag (e.g. `:cpu`) pins it. Override with `AILERON_VARIANT=cpu|cuda|rocm|vulkan`.

## Container security

Every container is spawned with:

```
--network=none          # no network access
--read-only             # read-only root filesystem
--tmpfs=/tmp            # in-memory scratch, noexec
--cap-drop=all          # no Linux capabilities
--security-opt=no-new-privileges
--pids-limit=256        # fork bomb protection
--memory=4g             # OOM protection
```

Running under rootless podman additionally places the container in a user namespace where container root maps to your unprivileged uid.

## Data files

| File | Contents |
|---|---|
| `$XDG_DATA_HOME/aileron/assignments.json` | Use-case → OCI image ref mapping |
| `$XDG_DATA_HOME/aileron/permissions.json` | Per-app, per-use-case permission grants |

## Security properties

- No inference engine code runs in the daemon process. A crash in llama.cpp kills only the container.
- No REST API, no TCP, no UDP. Attack surface is the Varlink Unix socket and D-Bus.
- Containers run with `--network=none`, `--cap-drop=all`, `--read-only`, and `--security-opt=no-new-privileges` (see [Container security](#container-security)).
- Rootless podman places containers in a user namespace — container root maps to your unprivileged uid.
- OCI images are content-addressed; image refs can be pinned to a digest (`@sha256:...`).
- Per-app permissions are enforced in the daemon, not the portal.
- The daemon runs with `PrivateNetwork=yes` in the systemd unit.

## Out of scope for v1

- Multi-user (the daemon is a user service, one instance per login session)
- Model signature verification (podman trust policy handles this)
- A system-wide shared model store

## License

GPL-3.0-or-later
