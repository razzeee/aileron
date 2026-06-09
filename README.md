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
│  aileron-demo  ──── D-Bus ────────────► │──► org.freedesktop.portal.AI
└─────────────────────────────────────────┘             │
                                                        │ D-Bus
                                                 aileron-portal
                                                        │ Varlink (Unix socket)
                                                 aileron-daemon
                                                        │ stdin/stdout
                                                 podman container
                                                        │
                                               llama.cpp / whisper.cpp
                                               (inside OCI image)
```

The management UI (`aileron`) also speaks directly to the daemon over the same Varlink socket and runs outside any sandbox.

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

Images are built with `podman build` (or `docker build`). Model weights must either be baked in at build time or mounted at runtime.

### LLM image

```sh
cd images/llm

# CPU-only, bake in a model from HuggingFace:
podman build \
    --build-arg MODEL_URL="https://huggingface.co/bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf" \
    -t ghcr.io/aileron/llama3.2-3b-instruct:latest .

# NVIDIA GPU (requires nvidia-container-toolkit):
podman build \
    --build-arg CMAKE_ARGS="-DLLAMA_CUDA=on" \
    --build-arg MODEL_URL="..." \
    -t ghcr.io/aileron/llama3.2-3b-instruct:cuda .

# Mount a local model file at runtime instead of baking it in:
podman run --rm -i \
    -v /path/to/model.gguf:/model/model.gguf:ro \
    ghcr.io/aileron/llama3.2-3b-instruct:latest
```

Supported environment variables:

| Variable | Default | Description |
|---|---|---|
| `MODEL_PATH` | `/model/model.gguf` | Path to the GGUF model file |
| `N_CTX` | `4096` | Context window size |
| `N_GPU_LAYERS` | `0` | Number of layers to offload to GPU |

### ASR image

```sh
cd images/asr

podman build \
    --build-arg MODEL_SIZE=small \
    -t ghcr.io/aileron/whisper-small:latest .
```

Supported environment variables:

| Variable | Default | Description |
|---|---|---|
| `MODEL_SIZE` | `small` | Whisper model size (tiny/base/small/medium/large-v3) |
| `MODEL_PATH` | `/model` | Directory for cached model weights |
| `DEVICE` | `cpu` | Inference device (`cpu` or `cuda`) |
| `COMPUTE_TYPE` | `int8` | Quantisation type (`int8`, `float16`, `float32`) |

## Getting started

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
method CreateSession(app_id: string, use_case: string) -> (session_id: string)
method Generate(session_id: string, prompt: string) -> (token: string)
method GenerateStructured(session_id: string, prompt: string, schema: string) -> (result: string)
method Transcribe(session_id: string, audio: string) -> (text: string)
method Describe(session_id: string, image: string) -> (text: string)
method EndSession(session_id: string) -> ()
```

`audio` and `image` are base64-encoded bytes. `schema` is a JSON Schema object serialised as a string.

### `aileron.Models`

Pull, list, delete, and assign OCI images.

```varlink
method List() -> (models: []ModelInfo)
method Pull(image_ref: string) -> (progress: PullProgress)
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

Full IDL: [`crates/aileron-varlink/varlink/`](crates/aileron-varlink/varlink/)

## Container stdio protocol

Each OCI image implements a simple newline-delimited JSON protocol over stdin/stdout. No ports, no sockets — the container has no network access.

### Streaming text generation

```json
// request
{"id": "uuid", "type": "generate", "prompt": "Summarise this: ...", "max_tokens": 512}

// response (one line per token)
{"id": "uuid", "token": "Here"}
{"id": "uuid", "token": " is", "done": true}
```

### Structured output

The daemon sends a `response_format` object containing the caller's JSON Schema. The container must constrain sampling to valid JSON (e.g. via llama.cpp grammar) and reply with a single `result` line. The daemon validates the result against the schema before returning it; mismatches produce a `SchemaValidationFailed` error.

```json
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

```json
// request
{"id": "uuid", "type": "transcribe", "audio": "<base64 PCM 16kHz mono f32le>"}

// response (streamed segment text)
{"id": "uuid", "token": "Hello world.", "done": true}
```

### Image description

```json
// request
{"id": "uuid", "type": "describe", "image": "<base64 PNG or JPEG>"}

// response (streamed)
{"id": "uuid", "token": "A cat", "done": true}
```

## Container lifecycle

- Containers start on demand, the first time a session uses a given use-case.
- One container runs per use-case, shared across all sessions for that use-case.
- Idle containers are terminated after 5 minutes (configurable via the daemon).
- A crash in the container kills only that container, not the daemon.

## Data files

| File | Contents |
|---|---|
| `$XDG_DATA_HOME/aileron/assignments.json` | Use-case → OCI image ref mapping |
| `$XDG_DATA_HOME/aileron/permissions.json` | Per-app, per-use-case permission grants |

## Security properties

- No inference engine code runs in the daemon process. A crash in llama.cpp kills the container, not the daemon.
- No REST API, no TCP, no UDP. Attack surface is the Varlink Unix socket and D-Bus.
- OCI images are content-addressed; image refs can be pinned to a digest (`@sha256:...`).
- Per-app permissions are enforced in the daemon, not the portal.
- `aileron-demo` is a real Flatpak sandbox — it cannot reach the Varlink socket directly.
- The daemon runs with `PrivateNetwork=yes` in the systemd unit.

## Out of scope for v1

- GPU/accelerator selection (the container runtime handles this)
- Multi-user (the daemon is a user service, one instance per login session)
- Model signature verification (podman trust policy handles this)
- A system-wide shared model store

## License

GPL-3.0-or-later
