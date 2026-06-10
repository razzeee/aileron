# Vision Image

This image runs llama.cpp through `llama-cpp-python` and implements `vision.describe` with a multimodal GGUF model and matching mmproj file.

Run all commands below from the repository root.

## Platforms

Four Dockerfiles are provided. Pick the one that matches your hardware:

| Dockerfile | Hardware | Notes |
|---|---|---|
| `Dockerfile` | CPU (any) | Default, works everywhere |
| `Dockerfile.cuda` | NVIDIA GPU | Requires `nvidia-container-toolkit` on host |
| `Dockerfile.rocm` | AMD GPU | Requires ROCm drivers on host |
| `Dockerfile.vulkan` | Any Vulkan GPU | NVIDIA / AMD / Intel Arc, no proprietary drivers needed |

Tag images as `<name>:cpu`, `<name>:cuda`, `<name>:rocm`, or `<name>:vulkan`.
When you assign a base ref with no tag, such as `aileron/gemma-4-e4b-it-qat`, the daemon appends the detected hardware variant automatically at runtime. Assigning an explicit tag, such as `:cpu`, pins it.

## Build

The example below uses a Gemma 4 E4B QAT GGUF model and its matching projection file:

```sh
MODEL_URL="https://huggingface.co/unsloth/gemma-4-E4B-it-qat-GGUF/resolve/main/gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf"
MMPROJ_URL="https://huggingface.co/unsloth/gemma-4-E4B-it-qat-GGUF/resolve/main/mmproj-F16.gguf"

podman build \
    --build-arg MODEL_URL="$MODEL_URL" \
    --build-arg MMPROJ_URL="$MMPROJ_URL" \
    -t aileron/gemma-4-e4b-it-qat:cpu \
    images/vision

# NVIDIA GPU (requires nvidia-container-toolkit on host):
podman build \
    --build-arg MODEL_URL="$MODEL_URL" \
    --build-arg MMPROJ_URL="$MMPROJ_URL" \
    -f images/vision/Dockerfile.cuda \
    -t aileron/gemma-4-e4b-it-qat:cuda \
    images/vision

# AMD GPU (requires ROCm drivers on host):
podman build \
    --build-arg MODEL_URL="$MODEL_URL" \
    --build-arg MMPROJ_URL="$MMPROJ_URL" \
    -f images/vision/Dockerfile.rocm \
    -t aileron/gemma-4-e4b-it-qat:rocm \
    images/vision

# Any Vulkan-capable GPU (NVIDIA / AMD / Intel Arc):
podman build \
    --build-arg MODEL_URL="$MODEL_URL" \
    --build-arg MMPROJ_URL="$MMPROJ_URL" \
    -f images/vision/Dockerfile.vulkan \
    -t aileron/gemma-4-e4b-it-qat:vulkan \
    images/vision
```

Use a different multimodal GGUF pair by changing both URLs together. The model and mmproj file must be compatible.

## Local Model Mount

You can build an image without baking in model weights and mount local GGUF files at runtime:

```sh
podman build -t aileron/gemma-4-e4b-it-qat:cpu images/vision
podman run --rm -i \
    -v /path/to/model.gguf:/model/model.gguf:ro \
    -v /path/to/mmproj.gguf:/model/mmproj.gguf:ro \
    aileron/gemma-4-e4b-it-qat:cpu
```

## Assignment

After building all variants you want to support, assign the base ref so the daemon picks the right variant automatically:

```sh
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Models.AssignUseCase" \
    '{"image_ref":"aileron/gemma-4-e4b-it-qat","use_case":"vision.describe"}'
```

Or pin a specific platform tag:

```sh
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Models.AssignUseCase" \
    '{"image_ref":"aileron/gemma-4-e4b-it-qat:cpu","use_case":"vision.describe"}'
```

## Environment

| Variable | Default | Description |
|---|---|---|
| `MODEL_PATH` | `/model/model.gguf` | Path to the multimodal GGUF model file |
| `MMPROJ_PATH` | `/model/mmproj.gguf` | Path to the matching mmproj GGUF file |
| `VISION_PROMPT` | built-in concise description prompt | Prompt used for `describe` requests |
| `N_CTX` | `4096` | Context window size |
| `N_GPU_LAYERS` | auto | Layers to offload; auto-detected if unset (`-1` = all) |
| `N_THREADS` | all cores | CPU threads used when GPU is not available |
