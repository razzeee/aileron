# Vision Foundation Runtime Design

## Context

Aileron now has separate single-image vision use cases for detection,
promptable segmentation, and depth estimation:

- `vision.detect` returns normalized labeled boxes.
- `vision.segment` returns promptable masks from point and box prompts.
- `vision.depth` returns a normalized dense depth map.

The existing `llm-vision-whisper` runtime is a combined llama.cpp and
Whisper runtime. It can provide image description, OCR, and structured box
detection through a vision-language model, but it is not the right place to
add heavy Torch-based computer-vision stacks.

The target model families are:

- YOLOv9 for object detection.
- SAM2 for promptable image segmentation.
- Depth Anything 3 for monocular depth estimation.

## Objective

Add a new `vision-foundation` runtime family that can smoke-test locally on
CPU while preserving clean boundaries for future GPU variants. The first pass
must use the existing runtime stdio protocol and avoid new public portal API
surface unless a concrete model output cannot fit the current shapes.

## Scope

In scope:

- A new runtime image under `runtimes/vision-foundation/`.
- A Python stdio runtime process that implements `detect`, `segment`, and
  `depth` request types.
- CPU-compatible build and local smoke-test path.
- Model manifests for practical initial profiles where licensing permits.
- Documentation for local build, artifact layout, and runtime limitations.

Out of scope for the first pass:

- Video segmentation, masklet tracking, and SAM2 memory state.
- DA3 multiview depth, camera pose, 3D Gaussian output, GLB/PLY export, or
  reconstruction workflows.
- Metric-depth guarantees in the public API.
- New portal methods or new Varlink methods.
- Runtime network access or checkpoint downloads at inference time.
- GPU-specific images, except for leaving the Dockerfile layout ready for
  later variants.

## Approach Options

### Option A: New Torch Runtime, One Image, Three Adapters

Create a separate `vision-foundation` runtime with a small dispatcher and one
adapter each for YOLO, SAM2, and DA3.

Pros:

- Keeps Torch dependencies out of `llm-vision-whisper`.
- Matches the existing use-case split.
- Lets each adapter remain small and replaceable.
- Clean path to future CUDA/Vulkan variants.

Cons:

- Adds another runtime image and manifest family.
- CPU inference may be slow for SAM2 and DA3.

### Option B: Extend `llm-vision-whisper`

Add YOLO, SAM2, and DA3 to the existing combined runtime.

Pros:

- Fewer runtime manifests and assignments.
- One image can cover description, OCR, detect, segment, depth, speech, and
  text.

Cons:

- Mixes llama.cpp, Whisper, and Torch dependency stacks.
- Makes the image larger and harder to debug.
- Couples unrelated acceleration and packaging concerns.

### Option C: One Runtime Per Model Family

Create separate runtimes for YOLO, SAM2, and DA3.

Pros:

- Best dependency isolation.
- Each image can be optimized independently.

Cons:

- More manifests, runtime assignments, image builds, and UX fragmentation.
- More work before a useful end-to-end demo exists.

## Decision

Use Option A: a new `vision-foundation` Torch runtime with three narrow
adapters. Build the first version for CPU smoke testing and document that GPU
variants are future work.

## Runtime Structure

Add:

- `runtimes/vision-foundation/Dockerfile`
- `runtimes/vision-foundation/entrypoint.sh`
- `runtimes/vision-foundation/README.md`
- A Python runtime package or script copied into the image.

The entrypoint starts a single long-lived process that:

- Writes `ready` to stderr after imports and lightweight setup succeed.
- Reads one newline-delimited JSON request per stdin line.
- Dispatches by `request.type`.
- Writes one final JSON response per request.
- Uses stable runtime errors for unsupported requests, invalid images, missing
  artifacts, and model inference failures.

## Artifact Layout

The daemon mounts model artifacts read-only under `/model`. The runtime must
not download weights during inference.

Initial artifact conventions:

- YOLO profile: `/model/model.pt` or `/model/model.onnx`.
- SAM2 profile: `/model/model.pt` plus optional `/model/config.yaml` if the
  chosen loader requires it.
- DA3 profile: `/model/model/` for Hugging Face-style local directory, or a
  model-specific directory documented in the manifest.

The runtime should infer the active adapter from the request type and the
mounted profile artifacts. If a request type is incompatible with the mounted
artifacts, return `unsupported_request` or `model_unavailable` rather than
trying to auto-download another model.

## Adapter Behavior

### YOLO Detection

Request:

- `type: "detect"`
- `image`: base64 PNG/JPEG
- optional `prompt`, ignored initially except for future open-vocabulary
  detectors
- `execution_mode`

Response:

- `result` JSON string with `detections`.
- Each detection has `label`, `confidence`, `x`, `y`, `width`, `height`.
- Coordinates are normalized to the original decoded image dimensions.

The first implementation can use Ultralytics YOLO. If ONNX Runtime is simpler
for CPU packaging and reproducibility, use ONNX exports for YOLO first and keep
the adapter boundary unchanged.

### SAM2 Segmentation

Request:

- `type: "segment"`
- `image`: base64 PNG/JPEG
- optional `points`: normalized `{ x, y, positive }`
- optional `boxes`: normalized `{ x, y, width, height }`
- optional `prompt`, ignored initially because SAM2 image prediction is visual
  prompt driven
- `execution_mode`

Response:

- `result` JSON string with `masks`.
- Each mask includes normalized bounding box metadata and mask bytes.
- For the first pass, encode masks as PNG bytes in `mask_base64` unless a
  lower-level raw encoding is explicitly documented before implementation.
- `mask_width` and `mask_height` describe the decoded mask dimensions.

Prompt coordinates must be converted from normalized portal coordinates to
pixel coordinates before calling SAM2.

Empty prompts may either run automatic mask generation if the selected SAM2
loader supports it or return `invalid_input` with a clear reason. The first
implementation should prefer clear `invalid_input` over surprising expensive
automatic segmentation.

### Depth Anything 3 Depth

Request:

- `type: "depth"`
- `image`: base64 PNG/JPEG
- optional `prompt`, ignored initially
- `execution_mode`

Response:

- `result` JSON string with `depth`.
- `depth.values` is row-major normalized `0.0..=1.0`.
- `minimum` and `maximum` record the raw model output range before
  normalization when available; otherwise use `0.0` and `1.0`.

The first pass targets monocular relative depth. Metric depth and camera
intrinsics are intentionally out of scope.

## Model Manifests

Add manifests only for models that are suitable to distribute or reference
under the project policy.

Preferred initial choices:

- A small YOLO detection model with a permissive license.
- A small SAM2 checkpoint for promptable segmentation.
- An Apache-compatible DA3 model such as the small/base/metric variants if the
  artifact format works with offline loading.

Avoid making non-commercial DA3 nested/giant models defaults. They may be
documented as user-provided profiles if needed, but should not be the default
curated profile.

## Error Handling

The runtime should use stable error codes:

- `unsupported_request` for unknown request types.
- `invalid_input` for malformed image bytes, missing prompts when prompts are
  required, or out-of-range prompt data.
- `model_unavailable` for missing or incompatible mounted artifacts.
- `inference_failed` for unexpected model execution failures.

Errors must include `done: true` and preserve the request `id`.

## Testing

Add tests at three levels:

- Runtime unit tests for request parsing, coordinate conversion, mask encoding,
  and depth normalization.
- A local container smoke test using a tiny fixture image and CPU runtime.
- Daemon integration coverage through the existing stub-shaped protocol where
  practical.

The first pass does not need quality assertions for model accuracy. Success is
defined by valid protocol output, sane dimensions, normalized coordinates, and
clear failures when artifacts are missing.

## Acceptance Criteria

- A `vision-foundation` runtime image can be built locally on CPU.
- The runtime starts without network access and emits `ready`.
- With installed artifacts, `detect`, `segment`, and `depth` return outputs
  accepted by the existing daemon validators.
- With missing or incompatible artifacts, requests fail with clear structured
  runtime errors.
- No changes are required to the public portal API for the first pass.
- Documentation explains CPU limitations and the future GPU-variant path.

## Open Decisions For Implementation

- Exact first model checkpoints and manifests, after confirming artifact sizes,
  licenses, and offline loader behavior.
- Whether YOLO should run from `.pt` through Ultralytics or from exported ONNX
  through ONNX Runtime for the CPU smoke path.
- Whether SAM2 empty-prompt segmentation should be rejected initially or mapped
  to automatic mask generation.
