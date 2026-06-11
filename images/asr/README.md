# ASR Image

This image runs whisper.cpp through `pywhispercpp` and implements `asr.transcribe`.

Run all commands below from the repository root.

## Platforms

Three Dockerfiles are provided:

| Dockerfile | Hardware | Notes |
|---|---|---|
| `Dockerfile` | CPU (any) | Default, works everywhere |
| `Dockerfile.cuda` | NVIDIA GPU | Requires `nvidia-container-toolkit` on host |
| `Dockerfile.vulkan` | Any Vulkan GPU | NVIDIA / AMD / Intel Arc with Vulkan support |

Tag images as `<name>:cpu`, `<name>:cuda`, or `<name>:vulkan`.

The daemon can resolve an untagged base ref to the detected hardware variant, but this image does not currently include a ROCm-specific Dockerfile. On hosts where `rocm-smi` is detected, assign an explicit ASR tag such as `:vulkan` or `:cpu` unless you have built a compatible `:rocm` image yourself.

## Build

```sh
# CPU - works on any machine, no GPU required:
podman build \
    --build-arg MODEL_SIZE=small \
    -t aileron/whisper-small:cpu \
    images/asr

# NVIDIA GPU (requires nvidia-container-toolkit on host):
podman build \
    --build-arg MODEL_SIZE=small \
    -f images/asr/Dockerfile.cuda \
    -t aileron/whisper-small:cuda \
    images/asr

# Any Vulkan-capable GPU (NVIDIA / AMD / Intel Arc):
podman build \
    --build-arg MODEL_SIZE=small \
    -f images/asr/Dockerfile.vulkan \
    -t aileron/whisper-small:vulkan \
    images/asr
```

`MODEL_SIZE` is baked into the image at build time. Use one of the model names accepted by whisper.cpp, such as `tiny`, `base`, `small`, `medium`, or `large-v3`.

## Assignment

Assign the base ref only when every auto-detected variant you need exists:

```sh
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Models.AssignUseCase" \
    '{"image_ref":"aileron/whisper-small","use_case":"asr.transcribe"}'
```

Or pin a specific platform tag:

```sh
varlink call "unix:$XDG_RUNTIME_DIR/aileron.socket/aileron.Models.AssignUseCase" \
    '{"image_ref":"aileron/whisper-small:vulkan","use_case":"asr.transcribe"}'
```

## Environment

| Variable | Default | Description |
|---|---|---|
| `MODEL_SIZE` | `base` | Whisper model size baked into the image (`tiny`, `base`, `small`, `medium`, `large-v3`, etc.) |
| `MODEL_DIR` | `/model` | Directory containing cached model weights |
| `AILERON_DEVICE` | auto | Override device detection (`cpu`, `cuda`, or `vulkan`) |
