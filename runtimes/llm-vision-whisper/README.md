# LLM, Vision, And Whisper Runtime

This image contains a private llama-server, its Rust protocol adapter, and the
Rust whisper.cpp runtime. Its entrypoint dispatches by mounted artifacts:

| Artifacts under `/model` | Runtime binary |
|---|---|
| `model.bin` | Whisper speech transcription and translation |
| `model.gguf` plus `mmproj.gguf` | Private server for vision, OCR, detection, and language operations |
| `model.gguf` | Private server for generation, structured output, tools, and embeddings |

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
| `cuda` | `BUILDER_IMAGE=docker.io/nvidia/cuda:13.3.0-devel-ubuntu24.04`, `CMAKE_ARGS=-DGGML_CUDA=on` | NVIDIA GPU | Requires NVIDIA driver devices and `libcuda.so.1` on host |
| `rocm` | `BUILDER_IMAGE=docker.io/rocm/dev-ubuntu-22.04:7.2.4`, `CMAKE_ARGS=-DGGML_HIP=on ...` | AMD GPU | Requires ROCm devices on host and a ROCm-supported GPU architecture |
| `vulkan` | `BUILDER_IMAGE=fedora:latest`, `FINAL_IMAGE=fedora:latest`, `CMAKE_ARGS=-DGGML_VULKAN=on` plus Vulkan packages | Vulkan GPU | NVIDIA, AMD, Intel Arc, Xe, and integrated graphics; needs a recent shader compiler for cooperative-matrix shaders |

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
    --build-arg BUILDER_IMAGE=docker.io/nvidia/cuda:13.3.0-devel-ubuntu24.04 \
    --build-arg FINAL_IMAGE=docker.io/nvidia/cuda:13.3.0-runtime-ubuntu24.04 \
    --build-arg CMAKE_ARGS="-DGGML_CUDA=on" \
    --build-arg CUDA_DOCKER_ARCH=all \
    --build-arg LDFLAGS="-L/usr/local/cuda/lib64/stubs -Wl,-rpath-link,/usr/local/cuda/lib64/stubs" \
    --build-arg RUNTIME_VARIANT=cuda \
    -t docker.io/example/aileron-runtime-llm-vision-whisper:cuda \
    .

# AMD ROCm
podman build \
    -f runtimes/llm-vision-whisper/Dockerfile \
    --build-arg BUILDER_IMAGE=docker.io/rocm/dev-ubuntu-22.04:7.2.4 \
    --build-arg FINAL_IMAGE=docker.io/rocm/dev-ubuntu-22.04:7.2.4 \
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
    --build-arg BUILDER_IMAGE=fedora:latest \
    --build-arg FINAL_IMAGE=fedora:latest \
    --build-arg APT_PACKAGES="vulkan-loader-devel glslc glslang spirv-headers-devel" \
    --build-arg RUNTIME_APT_PACKAGES="libgomp libstdc++ libgcc ca-certificates vulkan-loader mesa-vulkan-drivers" \
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
| `N_CTX` | `4096`, or llmfit's effective context for llmfit-backed GGUF profiles | llama.cpp context window size |
| `N_GPU_LAYERS` | `0` on CPU; `-1` on GPU variants unless llmfit selects CPU-only or the manifest sets an explicit value | llama.cpp layers to offload; daemon starts GPU variants at `-1` and retries lower values unless explicitly set |
| `N_THREADS` | all cores on CPU, up to 4 on accelerators | CPU helper threads used by llama.cpp and whisper.cpp |
| `AILERON_DEVICE` | `cpu` | Device selected by the daemon (`cpu`, `cuda`, `rocm`, or `vulkan`) |

## Private llama-server adapter

All GGUF profiles use the pinned `llama-server` through
`aileron-runtime-llm-llama-server`. There is no native llama backend selector
or fallback. Whisper remains a separate executable for `.bin` models.

The adapter supports `generate`, `generate_structured`,
`generate_structured_stream`, `embed`, `describe`, `ocr`, `detect`, and
`capabilities`. It preserves canonical message roles and uses the legacy
`system`/`prompt` fields when canonical input is absent. Tools and tool-result
continuations use session-owned history in the daemon. Image inputs must be
inline PNG or JPEG data; external image URLs are not fetched.

Generation and embeddings share one loaded model. Embeddings use mean pooling
without normalization, and generation disables pooling. Runtime provenance
and the immutable image digest contribute to the embedding pipeline identity.

The server listens exclusively on a Unix socket under a private
`/tmp/aileron-llama-*/` directory in the container. No port is published and
container network access is unnecessary. The adapter waits for a successful
health response before announcing readiness. It reaps the server on normal
EOF, SIGTERM/SIGINT, and inference transport failures. The startup deadline is
300 seconds. Generation has no adapter wall-clock deadline; the daemon owns
request cancellation and container lifetime.

Only final-answer text enters answer events and structured JSON. Reasoning
events require explicit opt-in. The V2 daemon and portal APIs expose supported
thinking modes and effort levels, separate reasoning events, token usage, and
finish reasons. Unsupported settings are rejected. Internal reasoning needed
for a tool continuation stays in daemon-owned history, outside public tool calls.

Capabilities depend on model identity and template support. GPT-OSS exposes
low/medium/high effort; hybrid Gemma and Qwen3 templates expose thinking
on/off; DeepSeek R1 is thinking-only. Explicit reasoning requests use the
recognized model family's sampling defaults unless temperature is supplied.
Legacy calls preserve native sampling and single-turn Llama-3 prompt rendering.

Structured streaming emits the completed JSON object as an initial and a final
snapshot. The daemon continues to validate schemas. A malformed stream or a
truncated, invalid JSON answer produces a terminal error. Partial output is
never retried automatically.

Oversized prompts retain the existing `context_window_exceeded` error and
token counts. They do not shut down the loaded server, so a shorter subsequent
request can reuse it. Sampling explicitly disables the server's default min-p
filter to match the native runtime's top-k/top-p sampling behavior.

### Build provenance

`LLAMA_SERVER_REVISION` defaults to
`5f55650a78f92aff4d48d671423e888fac0469ff`. The build verifies the fetched commit
and records it as the image label `org.aileron.llama-server.revision`.
`LLAMA_BUILD_JOBS`, default `2`, bounds server and Rust build parallelism.
For CUDA, an empty `CUDA_DOCKER_ARCH` or `all` selects the engine's portable
PTX/cubin defaults, including architecture-specific Blackwell targets.
An explicit CMake architecture list overrides those defaults. Literal NVCC
`-arch=all` is not used because generic `sm_120` cannot assemble FP4 instructions.
The server uses static llama/ggml libraries and the image's accelerator
libraries. Its UI assets, HTTPS support, and subprocess-backed server tools
are disabled. The image includes the engine revision and patch hash in
`/usr/share/aileron/runtime-provenance.json`. Rust does not link llama bindings.
Automatic server memory fitting is disabled because the daemon owns resource
sizing and accelerator fallback. See [the comparison report](../../docs/llama-server-comparison.md)
for measured performance and the accepted startup tradeoff.

With Podman, add `--format docker` to the build commands above so its builder
honors the Dockerfile's Bash `SHELL` directive.

### Tests

```sh
cargo test -p aileron-runtime --features llama-server
cargo clippy -p aileron-runtime --features llama-server --all-targets -- -D warnings
python3 -m unittest discover -s runtimes/llm-vision-whisper/tests
```

PR CI builds CPU, CUDA, ROCm, and Vulkan images and runs the adapter tests.
Build checks do not establish GPU inference correctness; CUDA/ROCm execution
requires compatible drivers and hardware. The existing
stub-container CI test also checks cold-start cancellation, closing blocked
stdout readers, repeated termination, and OCI-state cleanup without model
weights.

For an actual daemon-wrapper test, build the CPU image, export its rootfs to a
temporary Aileron OCI store, and provide a local small instruct model directory
containing `model.gguf`. This loads real weights and requires working `crun`
permissions. Run from the repository root:

```sh
podman build --format docker -f runtimes/llm-vision-whisper/Dockerfile \
    --build-arg RUNTIME_VARIANT=cpu -t localhost/aileron-llama-server:validation .
export AILERON_LLAMA_SERVER_IMAGE=localhost/aileron-llama-server:validation
export AILERON_LLAMA_SERVER_MODEL_DIR=/absolute/path/to/model-directory
export AILERON_OCI_STORE="$PWD/target/llama-server-oci"
mkdir -p "$AILERON_OCI_STORE/rootfs/localhost_aileron-llama-server_validation"
container=$(podman create "$AILERON_LLAMA_SERVER_IMAGE")
podman export "$container" | tar -C \
    "$AILERON_OCI_STORE/rootfs/localhost_aileron-llama-server_validation" -xf -
podman rm "$container"
cargo test -p aileron-daemon \
    container::tests::llama_server_ \
    -- --ignored --nocapture --test-threads=1
```

The ignored integration tests check actual Unix-only listening, private mount
namespaces, text and structured generation, warm reuse, and server teardown
through the daemon's cancellation handle. They also check context-limit
recovery and retirement of a handle after a fatal error. CPU success does not establish GPU
image compatibility. Hardware builds, startup cancellation, and the reasoning
model conformance matrix remain separate rollout checks.
