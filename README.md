# Aileron

A system-level local AI API for Linux — the missing counterpart to Apple's FoundationModels framework.

Aileron provides sandboxed applications with access to on-device inference through [xdg-desktop-portal](https://github.com/flatpak/xdg-desktop-portal), with no network exposure, no shared REST server, and no inference engine code running in the host session.

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

The management UI (`aileron`) also speaks directly to the daemon over the same Varlink socket. It runs outside any sandbox.

## Workspace

| Crate | Type | Description |
|---|---|---|
| `aileron-daemon` | binary | systemd user service; Varlink socket; container and inference management |
| `aileron-portal` | binary | xdg-desktop-portal backend; bridges D-Bus ↔ Varlink |
| `aileron` | binary | GTK4/libadwaita management UI |
| `aileron-demo` | binary | Sandboxed GTK4 article summarizer; end-to-end demo app |
| `aileron-varlink` | library | Varlink IDL files and generated bindings |
| `aileron-ipc` | library | Varlink client/server connection helpers |

## Building

### Prerequisites

```sh
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# System libraries (Debian/Ubuntu)
sudo apt install \
    libgtk-4-dev \
    libadwaita-1-dev \
    libdbus-1-dev \
    pkg-config

# Runtime dependency
sudo apt install podman
```

### Build all crates

```sh
cargo build --workspace
```

### Build only the daemon and portal (no GTK required)

```sh
cargo build -p aileron-daemon -p aileron-portal
```

## Installation

### Daemon

Copy the binary and install the systemd user service:

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
```

The portal must be registered with xdg-desktop-portal. See the [xdg-desktop-portal documentation](https://flatpak.github.io/xdg-desktop-portal/docs/) for how to install a portal backend.

### Management UI

```sh
install -Dm755 target/release/aileron ~/.local/bin/aileron
aileron
```

## Getting started

### 1. Pull a model

Pull an OCI image that implements the container stdio protocol:

```sh
# Using the management UI:
aileron
# → Models page → enter image ref → Pull

# Or directly via the future aileron-cli (not yet implemented):
# aileron pull ghcr.io/aileron/llama3.2-3b-instruct:latest
```

### 2. Assign a use-case

Open the management UI, right-click the model, and select "Assign use-case". Or use the Varlink interface directly:

```
aileron.Models.AssignUseCase(
    image_ref: "ghcr.io/aileron/llama3.2-3b-instruct:latest",
    use_case:  "llm.summarize"
)
```

### 3. Grant permission to an app

The first time an app requests inference, Aileron will deny it (no entry exists). Grant access through the Permissions page in the management UI, or set it directly:

```
aileron.Permissions.SetAppPermission(
    app_id:   "org.example.MyApp",
    use_case: "llm.summarize",
    allowed:  true
)
```

### 4. Run the demo app

```sh
cargo run -p aileron-demo
```

Paste or fetch an article URL, then click **Summarize**. The app calls the portal with `llm.summarize`; the portal forwards to the daemon; the daemon runs the assigned container and streams tokens back.

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

Create sessions, generate text, transcribe audio, describe images.

```varlink
method CreateSession(app_id: string, use_case: string) -> (session_id: string)
method Generate(session_id: string, prompt: string) -> (token: string)
method Transcribe(session_id: string, audio: string) -> (text: string)
method Describe(session_id: string, image: string) -> (text: string)
method EndSession(session_id: string) -> ()
```

`audio` and `image` are base64-encoded bytes.

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

Each OCI image must implement a simple newline-delimited JSON protocol over stdin/stdout. No ports, no sockets — the container has no network access.

**Request (daemon → container):**
```json
{"id": "uuid", "type": "generate", "prompt": "...", "max_tokens": 512}
{"id": "uuid", "type": "transcribe", "audio": "<base64>"}
{"id": "uuid", "type": "describe", "image": "<base64>"}
```

**Response (container → daemon, streamed):**
```json
{"id": "uuid", "token": "Hello"}
{"id": "uuid", "token": " world"}
{"id": "uuid", "done": true}
```

## Container lifecycle

- Containers are started on demand, the first time a session uses a given use-case.
- One container runs per use-case, shared across all sessions for that use-case.
- Idle containers are terminated after 5 minutes (configurable).
- A crash in the container kills only that container, not the daemon.

## Data files

| File | Contents |
|---|---|
| `$XDG_DATA_HOME/aileron/assignments.json` | Use-case → OCI image ref mapping |
| `$XDG_DATA_HOME/aileron/permissions.json` | Per-app, per-use-case permission grants |

## Security properties

- No inference engine code runs in the daemon process.
- No REST API, no TCP, no UDP. The attack surface is the Varlink Unix socket and D-Bus.
- OCI images are content-addressed; image refs can be pinned to a digest.
- Per-app permissions are enforced in the daemon, not the portal.
- `aileron-demo` is a real Flatpak sandbox — it cannot reach the Varlink socket directly.
- The daemon runs with `PrivateNetwork=yes` in the systemd unit.

## Out of scope for v1

- GPU/accelerator selection (the container runtime handles this)
- Multi-user (the daemon is a user service, one instance per login session)
- Model signature verification (podman trust policy handles this)
- Structured output / typed responses (plain text streaming only)
- Cloud model fallback
- A system-wide shared model store

## License

GPL-3.0-or-later
