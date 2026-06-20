# Llamafile LLM Runtime

This runtime wraps `llamafile` as a localhost-only HTTP server and translates the existing Aileron newline-delimited JSON runtime protocol over stdin/stdout.

The runtime is separate from `llm-llama-cpp`; existing profiles stay on their current runtime unless they explicitly set `runtime_id` to `llm-llamafile`.

## Runtime ID

```json
{
  "runtime_id": "llm-llamafile"
}
```

## Build

Run from the repository root:

```sh
podman build \
    -f runtimes/llamafile.Dockerfile \
    --build-arg RUNTIME_ID=llm-llamafile \
    --build-arg RUNTIME_VARIANT=cpu \
    -t docker.io/example/aileron-runtime-llm-llamafile:cpu \
    .
```

Build the Vulkan image. This path compiles the `llamafile`-pinned native `llama-server` with Vulkan enabled because the portable APE binary cannot `dlopen()` GPU backends in containers:

```sh
podman build \
    -f runtimes/llamafile.Dockerfile \
    --build-arg RUNTIME_ID=llm-llamafile \
    --build-arg RUNTIME_VARIANT=vulkan \
    --build-arg RUNTIME_STAGE=gpu \
    --build-arg RUNTIME_DESCRIPTION="Aileron llamafile Vulkan runtime for local text generation." \
    -t docker.io/example/aileron-runtime-llm-llamafile:vulkan \
    .
```

The runtime manifest publishes `cpu` and `vulkan` variants. The `vulkan` image was smoke-tested with `/dev/dri` mounted and verified to enumerate a Vulkan device before serving a protocol request. CUDA and ROCm variants are not advertised until they pass equivalent built-image smoke tests.

## Environment

| Variable | Default | Description |
|---|---|---|
| `MODEL_PATH` | `/model/model.gguf` | Mounted GGUF model file |
| `LLAMAFILE_PATH` | `/usr/local/bin/llamafile` | Llamafile executable path |
| `LLAMAFILE_RUNNER` | `/bin/sh` | Launcher used for the llamafile APE binary; set to an empty string for native executables |
| `LLAMAFILE_SERVER_KIND` | `llamafile` | Command-line dialect: `llamafile` for the portable APE binary, `llama-server` for the native GPU image |
| `LLAMAFILE_HOST` | `127.0.0.1` | Local bind host |
| `LLAMAFILE_PORT` | `0` | Local port; `0` chooses a free port before startup |
| `N_CTX` | `4096` | Context window size |
| `N_THREADS` | all cores | CPU thread count |
| `N_GPU_LAYERS` | `0` on CPU, `-1` on accelerators | Layers to offload |
| `LLAMAFILE_GPU_MODE` | derived from `AILERON_DEVICE` | Explicit llamafile GPU backend; defaults to `nvidia` for CUDA and `amd` for ROCm/Vulkan/GPU |
| `AILERON_DEVICE` | `cpu` | Selected daemon device hint |
| `LLAMAFILE_STARTUP_TIMEOUT` | `120` | Server readiness timeout in seconds |

## Protocol

The wrapper implements the full protocol path in code, but `runtime.json` advertises only `generate` and `predict_next` until the built image passes CPU parity checks for structured output and embeddings.

Implemented paths:

- `generate` through the llamafile OpenAI-compatible chat completions endpoint.
- `predict_next` through raw `/completion`, preserving prefix-style inline prediction.
- `generate_structured` through `/completion` with a JSON schema constraint, then local schema validation.
- `generate_structured_stream` as one validated snapshot followed by the same final snapshot.
- `embed` through `/embedding`, normalizing flat or token-level vectors into one flat vector.

If the local server exits, HTTP requests fail with `internal_error` and the wrapper exits after the active request so the daemon can discard the container.

## Model Manifests

Llamafile consumes the same GGUF artifact layout as `llm-llama-cpp`: the daemon mounts the selected profile artifact directory at `/model`, and this runtime reads `/model/model.gguf`.

Profiles cannot list multiple runtimes. To expose the same model through llamafile, create a second model manifest with a distinct `profile_id`, the same `model_id`, and the same artifact URL/checksum, then set `runtime_id` to `llm-llamafile`:

```json
{
  "profile_id": "tinyllama-1.1b-f16-llamafile",
  "model_id": "tinyllama-1.1b-f16",
  "runtime_id": "llm-llamafile",
  "use_cases": ["language.summarize", "language.rephrase"],
  "artifacts": [
    {
      "role": "model",
      "url": "https://huggingface.co/ggml-org/models-moved/resolve/main/tinyllama-1.1b/ggml-model-f16.gguf",
      "filename": "model.gguf",
      "sha256": "92982a0b96adfe5a8cea15ed6272bd11282f9a257eca74e40225becc6ae61c71"
    }
  ]
}
```
