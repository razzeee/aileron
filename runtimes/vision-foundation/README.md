# Vision Foundation Runtime

`vision-foundation` is a Torch-based Aileron runtime for single-image computer vision tasks that do not belong in the combined `llm-vision-whisper` llama.cpp/Whisper image.

It implements the existing container stdio protocol for:

- `detect` with YOLO artifacts at `/model/model.pt`.
- `segment` with SAM2 artifacts at `/model/model.pt` and `/model/config.yaml`.
- `depth` with a local Hugging Face-style depth model directory at `/model/model/`.

The runtime never downloads checkpoints during inference. Missing artifacts or optional Python loaders return structured `model_unavailable` responses.

## Build

Run from the repository root:

```sh
podman build -f runtimes/vision-foundation/Dockerfile -t docker.io/example/aileron-runtime-vision-foundation:cpu .
```

The first image is CPU-only. The Dockerfile keeps the runtime isolated so future CUDA/ROCm/Vulkan variants can use a different base image without changing the portal API.

## Local Smoke Test

Start the runtime directly:

```sh
PYTHONPATH=runtimes/vision-foundation python3 -m vision_foundation.runtime
```

It prints a stderr line containing `ready`, then accepts one newline-delimited JSON request per stdin line.

With no mounted artifacts, a valid image request fails clearly:

```json
{"id":"req-1","type":"detect","image":"<base64-png-or-jpeg>"}
```

Response:

```json
{"id":"req-1","error":"model_unavailable","reason":"YOLO artifact /model/model.pt is required","done":true}
```

## Artifact Layout

Mount model artifacts read-only at `/model`.

YOLO detection profile:

```text
/model/model.pt
```

SAM2 promptable segmentation profile:

```text
/model/model.pt
/model/config.yaml
```

The base CPU image bundles the `sam2` Python package for the curated SAM2.1 tiny profile. If a different checkpoint requires a different package revision, build a derived image with that exact SAM2 package.
`sam2.build_sam2` resolves configs through Hydra from the installed `sam2` package, so the runtime uses `SAM2_CONFIG_NAME` for the package config name and defaults to `configs/sam2.1/sam2.1_hiera_t.yaml`. `/model/config.yaml` is still required so installed artifacts remain self-describing and verifiable.

Depth estimation profile:

```text
/model/model/
  config.json
  model.safetensors
```

The runtime also accepts these files flat under `/model`. Curated manifests use the flat layout because Aileron's artifact installer stores each manifest artifact by filename within the profile artifact directory.

The depth loader uses `depth-anything-3` for DA3 checkpoints. Generic Hugging Face Transformers depth directories are intentionally not supported by this image.

## Limitations

- CPU inference can be slow, especially for SAM2 and depth models.
- Depth responses are downsampled to at most 65,536 values before JSON serialization. Set `MAX_DEPTH_PIXELS` in the runtime environment to tune this cap.
- SAM2 video segmentation, memory state, and masklet tracking are intentionally out of scope.
- Empty SAM2 prompts return `invalid_input` instead of running automatic mask generation.
- Depth output is normalized relative monocular depth. Metric depth is not guaranteed.
- The runtime does not add new portal or Varlink methods.

## Manifests

The runtime image manifest is `manifests/runtimes/vision-foundation.json`.

Curated model manifests are available under `manifests/models/`:

- `yolo26n.json` for `vision.detect` using an AGPL-3.0 Ultralytics YOLO26 nano PyTorch artifact.
- `sam2.1-hiera-tiny.json` for `vision.segment` using Apache-2.0 SAM2.1 tiny artifacts.
- `depth-anything-3-base.json` for `vision.depth` using Apache-2.0 Depth Anything 3 base artifacts.

The YOLO26 curated profile is AGPL-3.0. `mudler/depth-anything.cpp-gguf` is not listed as an installable profile yet because this runtime does not load Depth Anything GGUF artifacts.
