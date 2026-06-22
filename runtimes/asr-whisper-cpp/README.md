# ASR Runtime

This runtime runs whisper.cpp through the Rust `whisper-rs` bindings and implements `speech.transcribe` for Whisper artifacts mounted under `/model`.

Model weights are not baked into the image. A model manifest downloads and verifies the Whisper artifact, then references this runtime by `runtime_id`.

Run all commands below from the repository root.

## Runtime ID

```json
{
  "runtime_id": "asr-whisper-cpp"
}
```

## Platforms

Four Dockerfiles are provided:

| Dockerfile | Variant | Hardware | Notes |
|---|---|---|---|
| `Dockerfile` | `cpu` | CPU | Default, works everywhere |
| `Dockerfile.cuda` | `cuda` | NVIDIA GPU | Requires NVIDIA driver devices and `libcuda.so.1` on host |
| `Dockerfile.rocm` | `rocm` | AMD GPU | Requires ROCm devices on host |
| `Dockerfile.vulkan` | `vulkan` | Vulkan GPU | NVIDIA / AMD / Intel Arc, Xe, and integrated graphics |

If the host detects an accelerator and the runtime manifest has no matching image, the daemon falls back through the compatible variants declared in the manifest, then to `cpu` as the final fallback.

## Build

```sh
# CPU
podman build \
    -f runtimes/asr-whisper-cpp/Dockerfile \
    -t docker.io/example/aileron-runtime-asr-whisper-cpp:cpu \
    .

# NVIDIA CUDA
podman build \
    -f runtimes/asr-whisper-cpp/Dockerfile.cuda \
    -t docker.io/example/aileron-runtime-asr-whisper-cpp:cuda \
    .

# AMD ROCm
podman build \
    -f runtimes/asr-whisper-cpp/Dockerfile.rocm \
    -t docker.io/example/aileron-runtime-asr-whisper-cpp:rocm \
    .

# Vulkan
podman build \
    -f runtimes/asr-whisper-cpp/Dockerfile.vulkan \
    -t docker.io/example/aileron-runtime-asr-whisper-cpp:vulkan \
    .
```

## Runtime Manifest

Publish the image refs through a runtime manifest file such as `/usr/share/aileron/manifests/runtimes/asr-whisper-cpp.json`:

```json
{
  "runtime_id": "asr-whisper-cpp",
  "images": {
    "cpu": "docker.io/example/aileron-runtime-asr-whisper-cpp:cpu",
    "cuda": "docker.io/example/aileron-runtime-asr-whisper-cpp:cuda",
    "rocm": "docker.io/example/aileron-runtime-asr-whisper-cpp:rocm",
    "vulkan": "docker.io/example/aileron-runtime-asr-whisper-cpp:vulkan"
  }
}
```

Use digest-pinned refs, such as `image@sha256:...`, for distribution manifests.

## Model Manifest

A model profile points at this runtime and provides the artifact URL/checksum:

```json
{
  "profile_id": "whisper-small",
  "model_id": "whisper-small",
  "runtime_id": "asr-whisper-cpp",
  "use_cases": ["speech.transcribe"],
  "artifacts": [
    {
      "url": "https://huggingface.co/.../resolve/main/model.bin",
      "filename": "model.bin",
      "sha256": "..."
    }
  ]
}
```

When the profile is installed, the daemon downloads the artifact to `$XDG_DATA_HOME/aileron/models/<model-id>/`, verifies the checksum, detects the host variant, resolves this runtime through runtime manifests, and pulls the selected OCI image.

## Manual URL Install

For ad-hoc Whisper models, use the management UI's **Add Profile...** action, or call Varlink directly:

```sh
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Models.InstallUrlProfile" \
    '{"runtime_id":"asr-whisper-cpp","url":"https://huggingface.co/.../resolve/main/model.bin","sha256":"...","use_cases":["speech.transcribe"]}'
```

Aileron derives the filename, model ID, and profile ID from the URL and checksum.

## Environment

| Variable | Default | Description |
|---|---|---|
| `MODEL_PATH` | `/model/model.bin` | Path to the mounted Whisper artifact |
| `AILERON_DEVICE` | `cpu` | Device selected by the daemon (`cpu`, `cuda`, or `vulkan`) |
| `N_THREADS` | host CPU count | Number of CPU threads passed to whisper.cpp |
