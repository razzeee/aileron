# Private llama-server validation

The runtime now uses llama-server for every GGUF profile. Native llama Rust
bindings, text/vision executables, and the backend selector are removed.
Whisper and independent vision/TTS runtimes retain their existing dispatch.
The engine pin is `5f55650a78f92aff4d48d671423e888fac0469ff`.

## Verified locally

- Complete CPU, CUDA, ROCm, and Vulkan runtime images build. Package checks confirm that
  the adapter and Whisper are present and native llama executables are absent.
- The full workspace run passes 383 tests with five opt-in tests ignored.
  Both real-model OCI tests were then run explicitly and passed. Workspace
  Clippy with warnings denied, formatting, shell syntax, workflow YAML parsing,
  diff checks, and the seven Python runtime/benchmark tests pass.
- Both ignored real-model OCI tests pass with the final CPU image. They cover
  private Unix-only listening, mount isolation, warm reuse, structured output,
  context-limit recovery, recoverable truncated JSON, fatal server termination,
  cancellation, bundle cleanup, and retiring failed pooled handles.
- The real daemon/Gemma check passes cold and active cancellation with session
  reuse, tool calls, schema-constrained continuation, session isolation, replay
  rejection, thinking on/off, opt-in reasoning, and exhaustion metadata.
- The private-bus frontend → Rust portal backend → daemon → final CPU image
  check passes capability discovery, answers, reasoning, and completion metadata.
- Model portal integration tests pass from both the existing build and a clean
  build using the new PR CI commands.
- Reasoning checks pass on GPT-OSS 20B, DeepSeek R1, Gemma 4 E4B, and the original
  Qwen3 0.6B hybrid checkpoint. They cover supported controls, unsupported
  controls, trace privacy, and bounded output-token accounting.
- CPU and Vulkan embedding checks match the pre-optimization server with cosine
  `1.0` for short inputs and inputs exceeding the initial 512-token graph
  reservation. Alternating generation and embeddings still works after graph
  buffers grow. Mixed-workload embeddings match the native reference at
  `1.000000000`.
- The dedicated mixedbread encoder also passes all four fixtures, including
  Unicode, with cosine `1.000000000`. Its full five-cold/twenty-warm comparison
  passes latency and memory limits; see `final-encoder-full.{json,md}`.
- The reasoning demo was opened in an isolated X11 container. Controls are
  disabled before capability discovery, the reasoning/answer panes remain
  separate, and a missing portal is reported without enabling generation.
  This visual check does not substitute for interactive generation testing.

Qwen model provenance: `Qwen/Qwen3-0.6B-GGUF`, revision
`23749fefcc72300e3a2ad315e1317431b06b590a`, file `Qwen3-0.6B-Q8_0.gguf`,
SHA256 `9465e63a22add5354d9bb4b99e90117043c7124007664907259bd16d043bb031`.

## Performance and remaining coverage

See [the comparison report](llama-server-comparison.md) for full measurements,
image digests, regressions, and the accepted CPU startup tradeoff. Vulkan was
tested on an AMD Radeon RX 5600 XT, RADV NAVI10, with 6 GiB VRAM. CPU and Vulkan
results do not establish CUDA or ROCm inference correctness.

PR CI now builds CPU, CUDA, ROCm, and Vulkan images without publishing them,
checks packaged binaries, and runs model portal integration. All four local
image builds passed. The CUDA build needed upstream's portable architecture
defaults instead of literal `-arch=all`, which fails on generic `sm_120` FP4
instructions. ROCm needed a non-PIE executable link for its HIP static objects,
matching the existing Whisper wrapper's link policy.

CUDA inference is untested because this host has no NVIDIA device/driver.
ROCm mixed generation/embedding and Qwen3 reasoning checks pass on the RX 5600
XT. ROCm OCR fails on this card because the vendor rocBLAS package does not
include a `gfx1010` Tensile library. The same OCR workload passes with Vulkan.
ROCm vision still needs validation on a device supported by the vendor library;
the text checks do not establish general ROCm support for this card.

The portal frontend dependency is published in
[razzeee/xdg-desktop-portal#12](https://github.com/razzeee/xdg-desktop-portal/pull/12).
The submodule points to its `b9a5140b4f07e83af1a7dcb47d39b1823f088392` commit.

## Publication integration

The PR is integrated onto Aileron `main` at `6b66b7d`. It retains main's
asynchronous cleanup workers, startup reservations, and media-memory budget.
The V2 portal uses the same bounded blocking transport workers as V1. Bundle
ownership follows the cleanup worker so bundle removal occurs after payload
termination. Cancellation epochs cover startup and queued requests.

On this integrated revision, the workspace suite passes 432 tests with seven
opt-in tests ignored; Clippy with warnings denied and formatting also pass.
Both real-model OCI tests and the daemon/public-portal conformance checks were
rerun with the integrated daemon and portal and passed. The CPU image was also
rebuilt successfully with the integrated lockfile.
The lockfile retains current main's dependency versions and adds only the
adapter dependencies/removes the native llama dependency graph. Unrelated
working-tree edits were excluded using a separate PR worktree.

The performance measurements below and in the comparison report were collected
before integration, against the explicitly preserved native reference image.
They are not fresh performance comparisons against current main's newer native
llama dependency.

## Reproduction and evidence

Commands for unit tests and exporting an OCI rootfs are in the
[runtime README](../runtimes/llm-vision-whisper/README.md#tests). Local builds use
`TMPDIR="$PWD/target/agent-tmp"` to avoid the host `/tmp` quota.

Final reports under `target/agent-tmp/` include `final-public-portal.json`,
`final-daemon.json`, `final-qwen3.json`, `final-thinking-only.json`,
`final-gpt-oss.json`, `final-cpu-embedding-batches.json`,
`bounded-graph-embedding-batches.json`, `bounded-graph-vulkan-full.{json,md}`,
`final-cpu-ocr.{json,md}`, and `final-vulkan-ocr.{json,md}`.
Accelerator evidence includes `final-cuda-fixed-build.log`,
`final-rocm-fixed-build.log`, `final-rocm-mixed.json`, `final-rocm-qwen3.json`,
and `final-rocm-ocr-diagnostic.json`. The conformance runner now retains bounded
server diagnostics so a failed stream can be traced to the underlying error.

The cancellation fix targets the OCI container with `crun kill --all`, reaps
the monitor, and force-deletes its state. Killing only the monitor had left
inference alive. Fatal `inference_failed` responses retire the container before
releasing its pooled handle; request-local validation errors allow reuse.
