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

One shared llama.cpp Dockerfile builds all accelerator variants with build args:

| Variant | Base/build args | Hardware | Notes |
|---|---|---|---|
| `cpu` | default | CPU | Default, works everywhere |
| `cuda` | `BASE_IMAGE=nvidia/cuda:13.3.0-devel-ubuntu24.04`, `CMAKE_ARGS=-DGGML_CUDA=on` | NVIDIA GPU | Requires NVIDIA container support on host |
| `rocm` | `BASE_IMAGE=rocm/dev-ubuntu-22.04:7.2.4-complete`, `CMAKE_ARGS=-DGGML_HIP=on ...` | AMD GPU | Requires ROCm devices on host |
| `vulkan` | `CMAKE_ARGS=-DGGML_VULKAN=on` plus Vulkan packages | Vulkan GPU | NVIDIA / AMD / Intel Arc |

Tag images by runtime and variant. The daemon does not infer image tags from model profiles; it resolves `runtime_id + detected variant` through runtime manifests.

## Build

```sh
# CPU
podman build \
    -f runtimes/llama-cpp.Dockerfile \
    --build-arg RUNTIME_ID=llm-llama-cpp \
    --build-arg ENTRYPOINT_PATH=runtimes/llm-llama-cpp/entrypoint.py \
    --build-arg EXTRA_PIP_PACKAGES=jsonschema \
    -t docker.io/example/aileron-runtime-llm-llama-cpp:cpu \
    .

# NVIDIA CUDA
podman build \
    -f runtimes/llama-cpp.Dockerfile \
    --build-arg BASE_IMAGE=nvidia/cuda:13.3.0-devel-ubuntu24.04 \
    --build-arg APT_PACKAGES="python3 python3-pip python3-dev build-essential cmake git ninja-build" \
    --build-arg CMAKE_ARGS="-DGGML_CUDA=on" \
    --build-arg CUDA_DOCKER_ARCH=all \
    --build-arg RUNTIME_ID=llm-llama-cpp \
    --build-arg ENTRYPOINT_PATH=runtimes/llm-llama-cpp/entrypoint.py \
    --build-arg EXTRA_PIP_PACKAGES=jsonschema \
    -t docker.io/example/aileron-runtime-llm-llama-cpp:cuda \
    .

# AMD ROCm
podman build \
    -f runtimes/llama-cpp.Dockerfile \
    --build-arg BASE_IMAGE=rocm/dev-ubuntu-22.04:7.2.4-complete \
    --build-arg APT_PACKAGES="python3 python3-pip python3-dev build-essential cmake git ninja-build" \
    --build-arg CMAKE_ARGS="-DGGML_HIP=on -DAMDGPU_TARGETS=gfx1030" \
    --build-arg FORCE_CMAKE=1 \
    --build-arg HSA_OVERRIDE_GFX_VERSION=10.3.0 \
    --build-arg PIP_INSTALL_ARGS="--no-binary llama-cpp-python" \
    --build-arg RUNTIME_ID=llm-llama-cpp \
    --build-arg ENTRYPOINT_PATH=runtimes/llm-llama-cpp/entrypoint.py \
    --build-arg EXTRA_PIP_PACKAGES=jsonschema \
    -t docker.io/example/aileron-runtime-llm-llama-cpp:rocm \
    .

# Vulkan
podman build \
    -f runtimes/llama-cpp.Dockerfile \
    --build-arg APT_PACKAGES="build-essential cmake git ninja-build libvulkan-dev vulkan-tools libvulkan1 mesa-vulkan-drivers" \
    --build-arg CMAKE_ARGS="-DGGML_VULKAN=on" \
    --build-arg RUNTIME_ID=llm-llama-cpp \
    --build-arg ENTRYPOINT_PATH=runtimes/llm-llama-cpp/entrypoint.py \
    --build-arg EXTRA_PIP_PACKAGES=jsonschema \
    -t docker.io/example/aileron-runtime-llm-llama-cpp:vulkan \
    .
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
  "use_cases": ["llm.summarize", "llm.translate", "llm.extract", "llm.analyze"],
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
    '{"runtime_id":"llm-llama-cpp","url":"https://huggingface.co/.../resolve/main/model.gguf","sha256":"...","use_cases":["llm.summarize","llm.analyze"]}'
```

Aileron derives the filename, model ID, and profile ID from the URL and checksum.

## Environment

| Variable | Default | Description |
|---|---|---|
| `MODEL_PATH` | `/model/model.gguf` | Path to the mounted GGUF model file |
| `N_CTX` | `4096` | Context window size |
| `N_GPU_LAYERS` | auto | Layers to offload; auto-detected if unset (`-1` = all) |
| `N_THREADS` | all cores | CPU threads used when GPU is not available |
