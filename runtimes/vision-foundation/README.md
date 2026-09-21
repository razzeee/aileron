# Vision Foundation Runtime

`vision-foundation` is a Torch-based Aileron runtime for single-image computer vision tasks that do not belong in the combined `llm-vision-whisper` llama.cpp/Whisper image.

It implements the existing container stdio protocol for:

- `detect` with Ultralytics YOLO26 at `/model/model.pt`.
- `segment` with Ultralytics SAM2.1 via a writable `sam2.1_t.pt` alias that targets `/model/model.pt`.
- `depth` with Ultralytics YOLO26 depth at `/model/model.pt`.

The runtime never downloads checkpoints during inference. Missing artifacts or optional Python loaders return structured `model_unavailable` responses.

## Model reuse

The runtime loads its model lazily on the first valid request and reuses the model
and predictor for subsequent requests. Each process retains at most one model,
keyed by loader type and resolved checkpoint path. The mounted checkpoint is
assumed to be immutable for the process lifetime; restart the runtime to replace
a checkpoint at the same path.

SAM's writable checkpoint alias lives as long as its cached model. Image features,
prompts, and segmentation mode are reset between requests, including failed
predictions. Repeated prompts on the same image still recompute image features.
Model loading or prediction failures discard the cached model so the next request
can retry with a fresh instance.

This reduces repeated loading and predictor setup, not the first request's startup
cost. Model memory remains allocated until the runtime exits or replaces the cached
model. The daemon normally terminates idle runtime containers after 300 seconds,
configurable through `--idle-timeout-secs`.

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

Every task-specific profile mounts one checkpoint:

```text
/model/model.pt
```

The assigned use case and profile specialization select the loader; filenames are never used to infer a task. Detection and depth use `ultralytics.YOLO`, while promptable segmentation creates a writable `sam2.1_t.pt` alias that targets `/model/model.pt` and passes that alias to `ultralytics.SAM`. The image pins `ultralytics==8.4.115` because YOLO26 depth result support is version-sensitive.

## Limitations

- CPU inference can be slow, especially for SAM2 and depth models.
- Depth responses are downsampled to at most 65,536 values before JSON serialization. Set `MAX_DEPTH_PIXELS` in the runtime environment to tune this cap.
- SAM2 video segmentation, memory state, and masklet tracking are intentionally out of scope.
- Empty SAM2 prompts return `invalid_input` instead of running automatic mask generation.
- Depth values are nonnegative monocular distance estimates in meters from the checkpoint's baked-in calibration. They are not sensor-grade measurements and absolute accuracy depends on the input domain.
- The runtime does not add new portal or Varlink methods.

## Manifests

The runtime image manifest is `manifests/runtimes/vision-foundation.json`.

Curated model manifests are available under `manifests/models/`:

- `yolo26n.json` for `vision.detect` using an AGPL-3.0 Ultralytics YOLO26 nano PyTorch artifact.
- `sam2.1-tiny-ultralytics.json` for `vision.segment` using the Ultralytics-packaged SAM2.1 tiny checkpoint.
- `yolo26n-depth.json` for `vision.depth` using calibrated YOLO26 nano depth.

Ultralytics 8.4.115 is AGPL-3.0. The distributed runtime therefore relies on AGPL-3.0 unless the distributor obtains an Ultralytics enterprise license. SAM2.1's checkpoint also retains its Apache-2.0 model license. Release remains gated on confirming that the chosen Ultralytics license path is acceptable.
