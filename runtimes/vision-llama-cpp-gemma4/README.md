# Vision Runtime

This runtime runs llama.cpp through `llama-cpp-python` and implements `vision.describe` for a multimodal GGUF model mounted at `/model/model.gguf` plus a matching projection file mounted at `/model/mmproj.gguf`.

Model weights are not baked into the image. A model manifest downloads and verifies both artifacts, then references this runtime by `runtime_id`.

Run all commands below from the repository root.

## Runtime ID

```json
{
  "runtime_id": "vision-llama-cpp-gemma4"
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
    -t docker.io/example/aileron-runtime-vision-llama-cpp-gemma4:cpu \
    runtimes/vision-llama-cpp-gemma4

# NVIDIA CUDA
podman build \
    -f runtimes/vision-llama-cpp-gemma4/Dockerfile.cuda \
    -t docker.io/example/aileron-runtime-vision-llama-cpp-gemma4:cuda \
    runtimes/vision-llama-cpp-gemma4

# AMD ROCm
podman build \
    -f runtimes/vision-llama-cpp-gemma4/Dockerfile.rocm \
    -t docker.io/example/aileron-runtime-vision-llama-cpp-gemma4:rocm \
    runtimes/vision-llama-cpp-gemma4

# Vulkan
podman build \
    -f runtimes/vision-llama-cpp-gemma4/Dockerfile.vulkan \
    -t docker.io/example/aileron-runtime-vision-llama-cpp-gemma4:vulkan \
    runtimes/vision-llama-cpp-gemma4
```

## Runtime Manifest

Publish the image refs through a runtime manifest file such as `/usr/share/aileron/manifests/runtimes/vision-llama-cpp-gemma4.json`:

```json
{
  "runtime_id": "vision-llama-cpp-gemma4",
  "images": {
    "cpu": "docker.io/example/aileron-runtime-vision-llama-cpp-gemma4:cpu",
    "cuda": "docker.io/example/aileron-runtime-vision-llama-cpp-gemma4:cuda",
    "rocm": "docker.io/example/aileron-runtime-vision-llama-cpp-gemma4:rocm",
    "vulkan": "docker.io/example/aileron-runtime-vision-llama-cpp-gemma4:vulkan"
  }
}
```

Use digest-pinned refs, such as `image@sha256:...`, for distribution manifests.

## Model Manifest

A model profile points at this runtime and provides both artifact URLs/checksums:

```json
{
  "profile_id": "gemma-4-e4b-it-qat",
  "model_id": "gemma-4-e4b-it-qat",
  "runtime_id": "vision-llama-cpp-gemma4",
  "use_cases": ["vision.describe"],
  "artifacts": [
    {
      "url": "https://huggingface.co/.../resolve/main/model.gguf",
      "filename": "model.gguf",
      "sha256": "..."
    },
    {
      "url": "https://huggingface.co/.../resolve/main/mmproj.gguf",
      "filename": "mmproj.gguf",
      "sha256": "..."
    }
  ]
}
```

When the profile is installed, the daemon downloads the artifacts to `$XDG_DATA_HOME/aileron/models/<model-id>/`, verifies the checksums, detects the host variant, resolves this runtime through runtime manifests, and pulls the selected OCI image.

## Manual Install

This runtime needs both `model.gguf` and `mmproj.gguf`, so install it through a manifest with two artifacts. The one-URL manual installer is intended for single-artifact runtimes such as LLM and ASR.

## Environment

| Variable | Default | Description |
|---|---|---|
| `MODEL_PATH` | `/model/model.gguf` | Path to the mounted multimodal GGUF model file |
| `MMPROJ_PATH` | `/model/mmproj.gguf` | Path to the mounted projection GGUF file |
| `VISION_PROMPT` | built-in concise description prompt | Prompt used for `describe` requests |
| `N_CTX` | `4096` | Context window size |
| `N_GPU_LAYERS` | auto | Layers to offload; auto-detected if unset (`-1` = all) |
| `N_THREADS` | all cores | CPU threads used when GPU is not available |
