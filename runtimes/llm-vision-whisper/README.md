# LLM, Vision, And Whisper Runtime

This runtime image contains the Rust llama.cpp text runtime, the Rust llama.cpp vision runtime, and the Rust whisper.cpp runtime. Its entrypoint dispatches by mounted artifacts:

| Artifacts under `/model` | Runtime binary |
|---|---|
| `model.bin` | Whisper speech transcription and translation |
| `model.gguf` plus `mmproj.gguf` | llama.cpp vision, OCR, segmentation, and text operations |
| `model.gguf` | llama.cpp text generation, structured output, completion, and embeddings |

Model weights are not baked into the image. Model manifests download artifacts separately and reference this runtime by `runtime_id`.

## Runtime ID

```json
{
  "runtime_id": "llm-vision-whisper"
}
```

## Platforms

One Dockerfile builds all accelerator variants with build args:

| Variant | Base/build args | Hardware | Notes |
|---|---|---|---|
| `cpu` | default | CPU | Default, works everywhere |
| `cuda` | `BUILDER_IMAGE=nvidia/cuda:13.3.0-devel-ubuntu24.04`, `CMAKE_ARGS=-DGGML_CUDA=on` | NVIDIA GPU | Requires NVIDIA driver devices and `libcuda.so.1` on host |
| `rocm` | `BUILDER_IMAGE=rocm/dev-ubuntu-22.04:7.2.4`, `CMAKE_ARGS=-DGGML_HIP=on ...` | AMD GPU | Requires ROCm devices on host and a ROCm-supported GPU architecture |
| `vulkan` | `CMAKE_ARGS=-DGGML_VULKAN=on` plus Vulkan packages | Vulkan GPU | NVIDIA, AMD, Intel Arc, Xe, and integrated graphics |

## Build

Run all commands from the repository root.

```sh
# CPU
podman build \
    -f runtimes/llm-vision-whisper/Dockerfile \
    --build-arg RUNTIME_VARIANT=cpu \
    -t docker.io/example/aileron-runtime-llm-vision-whisper:cpu \
    .

# NVIDIA CUDA
podman build \
    -f runtimes/llm-vision-whisper/Dockerfile \
    --build-arg BUILDER_IMAGE=nvidia/cuda:13.3.0-devel-ubuntu24.04 \
    --build-arg FINAL_IMAGE=nvidia/cuda:13.3.0-runtime-ubuntu24.04 \
    --build-arg CMAKE_ARGS="-DGGML_CUDA=on" \
    --build-arg CUDA_DOCKER_ARCH=all \
    --build-arg LDFLAGS="-L/usr/local/cuda/lib64/stubs -Wl,-rpath-link,/usr/local/cuda/lib64/stubs" \
    --build-arg RUNTIME_VARIANT=cuda \
    -t docker.io/example/aileron-runtime-llm-vision-whisper:cuda \
    .

# AMD ROCm
podman build \
    -f runtimes/llm-vision-whisper/Dockerfile \
    --build-arg BUILDER_IMAGE=rocm/dev-ubuntu-22.04:7.2.4 \
    --build-arg FINAL_IMAGE=rocm/dev-ubuntu-22.04:7.2.4 \
    --build-arg APT_PACKAGES="hipblas-dev rocblas-dev" \
    --build-arg RUNTIME_APT_PACKAGES="libgomp1 libstdc++6 libgcc-s1 ca-certificates hipblas-dev rocblas-dev" \
    --build-arg CMAKE_ARGS="-DGGML_HIP=on -DAMDGPU_TARGETS=gfx900;gfx906;gfx908;gfx90a;gfx942;gfx950;gfx1010;gfx1011;gfx1012;gfx1030;gfx1031;gfx1032;gfx1035;gfx1036;gfx1100;gfx1101;gfx1102;gfx1103;gfx1150;gfx1151;gfx1152;gfx1153;gfx1200;gfx1201" \
    --build-arg FORCE_CMAKE=1 \
    --build-arg RUNTIME_VARIANT=rocm \
    -t docker.io/example/aileron-runtime-llm-vision-whisper:rocm \
    .

# Vulkan
podman build \
    -f runtimes/llm-vision-whisper/Dockerfile \
    --build-arg APT_PACKAGES="libvulkan-dev glslc glslang-tools spirv-headers" \
    --build-arg RUNTIME_APT_PACKAGES="libgomp1 libstdc++6 libgcc-s1 ca-certificates libvulkan1 mesa-vulkan-drivers" \
    --build-arg CMAKE_ARGS="-DGGML_VULKAN=on" \
    --build-arg RUNTIME_VARIANT=vulkan \
    -t docker.io/example/aileron-runtime-llm-vision-whisper:vulkan \
    .
```

## Runtime Manifest

Publish image refs through a runtime manifest such as `/usr/share/aileron/manifests/runtimes/llm-vision-whisper.json`:

```json
{
  "runtime_id": "llm-vision-whisper",
  "images": {
    "cpu": "docker.io/example/aileron-runtime-llm-vision-whisper:cpu",
    "cuda": "docker.io/example/aileron-runtime-llm-vision-whisper:cuda",
    "rocm": "docker.io/example/aileron-runtime-llm-vision-whisper:rocm",
    "vulkan": "docker.io/example/aileron-runtime-llm-vision-whisper:vulkan"
  }
}
```

Use digest-pinned refs, such as `image@sha256:...`, for distribution manifests.

## Environment

| Variable | Default | Description |
|---|---|---|
| `MODEL_PATH` | derived from `/model/model.bin` or `/model/model.gguf` | Mounted model path |
| `MMPROJ_PATH` | `/model/mmproj.gguf` | Projection file that selects the vision runtime when present |
| `N_CTX` | `4096` | llama.cpp context window size |
| `N_GPU_LAYERS` | `0` | llama.cpp layers to offload; daemon starts GPU variants at `-1` and retries lower values unless explicitly set |
| `N_THREADS` | all cores | CPU threads used by llama.cpp and whisper.cpp |
| `AILERON_DEVICE` | `cpu` | Device selected by the daemon (`cpu`, `cuda`, `rocm`, or `vulkan`) |
