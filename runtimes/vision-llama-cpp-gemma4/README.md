# Vision Runtime

This runtime runs llama.cpp through the Rust `llama-cpp-2` bindings for a Gemma GGUF model mounted at `/model/model.gguf` plus a matching projection file mounted at `/model/mmproj.gguf`.

The Rust entrypoint supports text generation, structured generation, structured streaming, embeddings, and image-specific `describe`, `ocr`, and `segment` requests through llama.cpp's MTMD multimodal path.

Model weights are not baked into the image. A model manifest downloads and verifies both artifacts, then references this runtime by `runtime_id`.

Run all commands below from the repository root.

## Runtime ID

```json
{
  "runtime_id": "vision-llama-cpp-gemma4"
}
```

## Platforms

One shared llama.cpp Dockerfile builds all accelerator variants with build args:

| Variant | Base/build args | Hardware | Notes |
|---|---|---|---|
| `cpu` | default | CPU | Default, works everywhere |
| `cuda` | `BUILDER_IMAGE=nvidia/cuda:13.3.0-devel-ubuntu24.04`, `CMAKE_ARGS=-DGGML_CUDA=on` | NVIDIA GPU | Requires NVIDIA driver devices and `libcuda.so.1` on host |
| `rocm` | `BUILDER_IMAGE=rocm/dev-ubuntu-22.04:7.2.4`, `CMAKE_ARGS=-DGGML_HIP=on` | AMD GPU | Requires ROCm devices on host |
| `vulkan` | `CMAKE_ARGS=-DGGML_VULKAN=on` plus Vulkan packages | Vulkan GPU | NVIDIA / AMD / Intel Arc, Xe, and integrated graphics |

Tag images by runtime and variant. The daemon does not infer image tags from model profiles; it resolves `runtime_id + detected variant` through runtime manifests.

## Build

```sh
# CPU
podman build \
    -f runtimes/llama-cpp.Dockerfile \
    --build-arg RUNTIME_ID=vision-llama-cpp-gemma4 \
    --build-arg RUNTIME_BIN=aileron-runtime-vision-llama-cpp \
    -t docker.io/example/aileron-runtime-vision-llama-cpp-gemma4:cpu \
    .

# NVIDIA CUDA
podman build \
    -f runtimes/llama-cpp.Dockerfile \
    --build-arg BUILDER_IMAGE=nvidia/cuda:13.3.0-devel-ubuntu24.04 \
    --build-arg FINAL_IMAGE=nvidia/cuda:13.3.0-runtime-ubuntu24.04 \
    --build-arg CMAKE_ARGS="-DGGML_CUDA=on" \
    --build-arg CUDA_DOCKER_ARCH=all \
    --build-arg LDFLAGS="-L/usr/local/cuda/lib64/stubs -Wl,-rpath-link,/usr/local/cuda/lib64/stubs" \
    --build-arg RUNTIME_ID=vision-llama-cpp-gemma4 \
    --build-arg RUNTIME_VARIANT=cuda \
    --build-arg RUNTIME_BIN=aileron-runtime-vision-llama-cpp \
    -t docker.io/example/aileron-runtime-vision-llama-cpp-gemma4:cuda \
    .

# AMD ROCm
podman build \
    -f runtimes/llama-cpp.Dockerfile \
    --build-arg BUILDER_IMAGE=rocm/dev-ubuntu-22.04:7.2.4 \
    --build-arg FINAL_IMAGE=rocm/dev-ubuntu-22.04:7.2.4 \
    --build-arg APT_PACKAGES="hipblas-dev rocblas-dev" \
    --build-arg CMAKE_ARGS="-DGGML_HIP=on -DAMDGPU_TARGETS=gfx1030;gfx1031;gfx1032;gfx1100;gfx1101;gfx1102;gfx1103;gfx1150;gfx1151;gfx1152;gfx1153" \
    --build-arg RUNTIME_ID=vision-llama-cpp-gemma4 \
    --build-arg RUNTIME_VARIANT=rocm \
    --build-arg RUNTIME_BIN=aileron-runtime-vision-llama-cpp \
    -t docker.io/example/aileron-runtime-vision-llama-cpp-gemma4:rocm \
    .

# Vulkan
podman build \
    -f runtimes/llama-cpp.Dockerfile \
    --build-arg APT_PACKAGES="libvulkan-dev glslc glslang-tools spirv-headers" \
    --build-arg RUNTIME_APT_PACKAGES="libgomp1 libstdc++6 libgcc-s1 ca-certificates libvulkan1 mesa-vulkan-drivers" \
    --build-arg CMAKE_ARGS="-DGGML_VULKAN=on" \
    --build-arg RUNTIME_ID=vision-llama-cpp-gemma4 \
    --build-arg RUNTIME_VARIANT=vulkan \
    --build-arg RUNTIME_BIN=aileron-runtime-vision-llama-cpp \
    -t docker.io/example/aileron-runtime-vision-llama-cpp-gemma4:vulkan \
    .
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
  "use_cases": ["language.summarize", "vision.describe", "vision.segment"],
  "artifacts": [
    {
      "role": "model",
      "url": "https://huggingface.co/.../resolve/main/model.gguf",
      "filename": "model.gguf",
      "sha256": "...",
      "size_bytes": 4977169568
    },
    {
      "role": "mmproj",
      "url": "https://huggingface.co/.../resolve/main/mmproj.gguf",
      "filename": "mmproj.gguf",
      "sha256": "...",
      "size_bytes": 990372672
    }
  ]
}
```

When the profile is installed, the daemon downloads the artifacts to `$XDG_DATA_HOME/aileron/models/<model-id>/`, verifies the checksums, detects the host variant, resolves this runtime through runtime manifests, and pulls the selected OCI image.

## Manual Install

This runtime needs both `model.gguf` and `mmproj.gguf`, so install it through a manifest with two artifacts. Use artifact roles `model` and `mmproj` to make the layout explicit. The one-URL manual installer is intended for single-artifact runtimes such as LLM and ASR.

## Environment

| Variable | Default | Description |
|---|---|---|
| `MODEL_PATH` | `/model/model.gguf` | Path to the mounted multimodal GGUF model file |
| `MMPROJ_PATH` | `/model/mmproj.gguf` | Path to the mounted projection GGUF file |
| `VISION_PROMPT` | built-in concise description prompt | Prompt used for `describe` requests when none is supplied |
| `VISION_OCR_PROMPT` | built-in OCR prompt | Prompt used for `ocr` requests when none is supplied |
| `VISION_SEGMENT_PROMPT` | built-in object box prompt | Prompt used for `segment` requests when none is supplied |
| `N_CTX` | `4096` | Context window size |
| `N_GPU_LAYERS` | `0` | Layers to offload; daemon starts GPU variants at `-1` (`-1` = all), then retries lower values on cold-start failure unless explicitly set by the profile |
| `N_THREADS` | all cores | CPU threads used when GPU is not available |
