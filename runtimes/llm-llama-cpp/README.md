# LLM Runtime

This runtime runs llama.cpp through `llama-cpp-python` and implements text generation plus structured output for GGUF models mounted at `/model/model.gguf`.

Model weights are not baked into the image. A model manifest downloads and verifies the GGUF artifact, then references this runtime by `runtime_id`.

Run all commands below from the repository root.

## Runtime ID

```json
{
  "runtime_id": "llm-llama-cpp"
}
```

## Platforms

Four Dockerfiles are provided:

| Dockerfile | Variant | Hardware | Notes |
|---|---|---|---|
| `Dockerfile` | `cpu` | CPU | Default, works everywhere |
| `Dockerfile.cuda` | `cuda` | NVIDIA GPU | Requires NVIDIA container support on host |
| `Dockerfile.rocm` | `rocm` | AMD GPU | Requires ROCm devices on host |
| `Dockerfile.vulkan` | `vulkan` | Vulkan GPU | NVIDIA / AMD / Intel Arc |

Tag images by runtime and variant. The daemon does not infer image tags from model profiles; it resolves `runtime_id + detected variant` through runtime manifests.

## Build

```sh
# CPU
podman build \
    -t docker.io/example/aileron-runtime-llm-llama-cpp:cpu \
    runtimes/llm-llama-cpp

# NVIDIA CUDA
podman build \
    -f runtimes/llm-llama-cpp/Dockerfile.cuda \
    -t docker.io/example/aileron-runtime-llm-llama-cpp:cuda \
    runtimes/llm-llama-cpp

# AMD ROCm
podman build \
    -f runtimes/llm-llama-cpp/Dockerfile.rocm \
    -t docker.io/example/aileron-runtime-llm-llama-cpp:rocm \
    runtimes/llm-llama-cpp

# Vulkan
podman build \
    -f runtimes/llm-llama-cpp/Dockerfile.vulkan \
    -t docker.io/example/aileron-runtime-llm-llama-cpp:vulkan \
    runtimes/llm-llama-cpp
```

## Runtime Manifest

Publish the image refs through a runtime manifest file such as `/usr/share/aileron/manifests/runtimes/llm-llama-cpp.json`:

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

Use digest-pinned refs, such as `image@sha256:...`, for distribution manifests.

## Model Manifest

A model manifest points at this runtime and provides the model file URL/checksum:

```json
{
  "profile_id": "llama3.2-3b-instruct-q4",
  "model_id": "llama3.2-3b-instruct-q4",
  "runtime_id": "llm-llama-cpp",
  "use_cases": ["llm.summarize", "llm.translate", "llm.extract"],
  "artifacts": [
    {
      "url": "https://huggingface.co/.../resolve/main/model.gguf",
      "filename": "model.gguf",
      "sha256": "..."
    }
  ]
}
```

When the profile is installed, the daemon downloads the artifact to `$XDG_DATA_HOME/aileron/models/<model-id>/`, verifies the checksum, detects the host variant, resolves this runtime through runtime manifests, and pulls the selected OCI image.

## Manual URL Install

For ad-hoc GGUF models, use the management UI's **Add Profile...** action, or call Varlink directly:

```sh
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Models.InstallUrlProfile" \
    '{"runtime_id":"llm-llama-cpp","url":"https://huggingface.co/.../resolve/main/model.gguf","sha256":"...","use_cases":["llm.summarize"]}'
```

Aileron derives the filename, model ID, and profile ID from the URL and checksum.

## Environment

| Variable | Default | Description |
|---|---|---|
| `MODEL_PATH` | `/model/model.gguf` | Path to the mounted GGUF model file |
| `N_CTX` | `4096` | Context window size |
| `N_GPU_LAYERS` | auto | Layers to offload; auto-detected if unset (`-1` = all) |
| `N_THREADS` | all cores | CPU threads used when GPU is not available |
