# LLM Image

This image runs llama.cpp through `llama-cpp-python` and implements text generation and structured output backed by a GGUF model.

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
When you assign a base ref with no tag, such as `aileron/llama3.2-3b-instruct`, the daemon appends the detected hardware variant automatically at runtime. Assigning an explicit tag, such as `:cpu`, pins it.

## Build

```sh
MODEL_URL="https://huggingface.co/bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf"

# CPU - works on any machine, no GPU required:
podman build \
    --build-arg MODEL_URL="$MODEL_URL" \
    -t aileron/llama3.2-3b-instruct:cpu \
    images/llm

# NVIDIA GPU (requires nvidia-container-toolkit on host):
podman build \
    --build-arg MODEL_URL="$MODEL_URL" \
    -f images/llm/Dockerfile.cuda \
    -t aileron/llama3.2-3b-instruct:cuda \
    images/llm

# AMD GPU (requires ROCm drivers on host):
podman build \
    --build-arg MODEL_URL="$MODEL_URL" \
    -f images/llm/Dockerfile.rocm \
    -t aileron/llama3.2-3b-instruct:rocm \
    images/llm

# Any Vulkan-capable GPU (NVIDIA / AMD / Intel Arc):
podman build \
    --build-arg MODEL_URL="$MODEL_URL" \
    -f images/llm/Dockerfile.vulkan \
    -t aileron/llama3.2-3b-instruct:vulkan \
    images/llm
```

## Local Model Mount

You can build an image without baking in model weights and mount a local GGUF file at runtime:

```sh
podman build -t aileron/llama3.2-3b-instruct:cpu images/llm
podman run --rm -i \
    -v /path/to/model.gguf:/model/model.gguf:ro \
    aileron/llama3.2-3b-instruct:cpu
```

## Assignment

After building all variants you want to support, assign the base ref so the daemon picks the right variant automatically:

```sh
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Models.AssignUseCase" \
    '{"image_ref":"aileron/llama3.2-3b-instruct","use_case":"llm.summarize"}'
```

The daemon detects your hardware once at startup, logs the selected variant, and resolves `aileron/llama3.2-3b-instruct` to a platform tag such as `aileron/llama3.2-3b-instruct:cuda` at container spawn time.

Override with `AILERON_VARIANT=cpu|cuda|rocm|vulkan` if needed.

## Environment

| Variable | Default | Description |
|---|---|---|
| `MODEL_PATH` | `/model/model.gguf` | Path to the GGUF model file |
| `N_CTX` | `4096` | Context window size |
| `N_GPU_LAYERS` | auto | Layers to offload; auto-detected if unset (`-1` = all) |
| `N_THREADS` | all cores | CPU threads used when GPU is not available |
