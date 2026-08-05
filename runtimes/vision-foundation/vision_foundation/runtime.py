from __future__ import annotations

import base64
import contextlib
import io
import json
import os
import sys
import traceback
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np
from PIL import Image


MODEL_DIR = Path(os.environ.get("MODEL_DIR", "/model"))
MAX_DEPTH_PIXELS = int(os.environ.get("MAX_DEPTH_PIXELS", "65536"))


class RuntimeErrorCode(Exception):
    def __init__(self, code: str, reason: str):
        super().__init__(reason)
        self.code = code
        self.reason = reason


@dataclass(frozen=True)
class DecodedImage:
    image: Image.Image
    width: int
    height: int


def error_response(request_id: str, code: str, reason: str) -> dict[str, Any]:
    return {"id": request_id, "error": code, "reason": reason, "done": True}


def result_response(request_id: str, result: dict[str, Any]) -> dict[str, Any]:
    return {"id": request_id, "result": json.dumps(result, separators=(",", ":")), "done": True}


def decode_image(value: Any) -> DecodedImage:
    if not isinstance(value, str) or not value:
        raise RuntimeErrorCode("invalid_input", "image must be a non-empty base64 string")
    try:
        raw = base64.b64decode(value, validate=True)
        image = Image.open(io.BytesIO(raw)).convert("RGB")
    except Exception as exc:  # noqa: BLE001 - stable protocol error hides decoder details.
        raise RuntimeErrorCode("invalid_input", "image must be valid base64 PNG or JPEG bytes") from exc
    return DecodedImage(image=image, width=image.width, height=image.height)


def clamp01(value: float) -> float:
    return min(1.0, max(0.0, float(value)))


def normalize_box(x1: float, y1: float, x2: float, y2: float, width: int, height: int) -> dict[str, float]:
    x1 = clamp01(x1 / width)
    y1 = clamp01(y1 / height)
    x2 = clamp01(x2 / width)
    y2 = clamp01(y2 / height)
    return {"x": x1, "y": y1, "width": max(0.0, x2 - x1), "height": max(0.0, y2 - y1)}


def point_prompts(points: Any, width: int, height: int) -> tuple[np.ndarray, np.ndarray]:
    if points is None:
        return np.empty((0, 2), dtype=np.float32), np.empty((0,), dtype=np.int32)
    if not isinstance(points, list):
        raise RuntimeErrorCode("invalid_input", "points must be an array")
    coords: list[list[float]] = []
    labels: list[int] = []
    for point in points:
        if not isinstance(point, dict):
            raise RuntimeErrorCode("invalid_input", "points must contain objects")
        x = point.get("x")
        y = point.get("y")
        if not isinstance(x, (int, float)) or not isinstance(y, (int, float)):
            raise RuntimeErrorCode("invalid_input", "point x and y must be numbers")
        if not 0.0 <= float(x) <= 1.0 or not 0.0 <= float(y) <= 1.0:
            raise RuntimeErrorCode("invalid_input", "point coordinates must be normalized")
        coords.append([float(x) * width, float(y) * height])
        labels.append(1 if point.get("positive", True) else 0)
    return np.asarray(coords, dtype=np.float32), np.asarray(labels, dtype=np.int32)


def box_prompts(boxes: Any, width: int, height: int) -> np.ndarray:
    if boxes is None:
        return np.empty((0, 4), dtype=np.float32)
    if not isinstance(boxes, list):
        raise RuntimeErrorCode("invalid_input", "boxes must be an array")
    converted: list[list[float]] = []
    for box in boxes:
        if not isinstance(box, dict):
            raise RuntimeErrorCode("invalid_input", "boxes must contain objects")
        x = box.get("x")
        y = box.get("y")
        w = box.get("width")
        h = box.get("height")
        if not all(isinstance(v, (int, float)) for v in (x, y, w, h)):
            raise RuntimeErrorCode("invalid_input", "box x, y, width and height must be numbers")
        x = float(x)
        y = float(y)
        w = float(w)
        h = float(h)
        if x < 0.0 or y < 0.0 or w < 0.0 or h < 0.0 or x + w > 1.0 or y + h > 1.0:
            raise RuntimeErrorCode("invalid_input", "box coordinates must be normalized and in range")
        converted.append([x * width, y * height, (x + w) * width, (y + h) * height])
    return np.asarray(converted, dtype=np.float32)


def encode_mask_png(mask: np.ndarray) -> str:
    mask_u8 = (np.asarray(mask).astype(bool).astype(np.uint8)) * 255
    image = Image.fromarray(mask_u8)
    out = io.BytesIO()
    image.save(out, format="PNG")
    return base64.b64encode(out.getvalue()).decode("ascii")


def downsample_depth(values: np.ndarray, max_pixels: int = MAX_DEPTH_PIXELS) -> np.ndarray:
    arr = np.asarray(values, dtype=np.float32)
    if arr.ndim != 2:
        raise RuntimeErrorCode("inference_failed", "depth output must be a 2D array")
    height, width = arr.shape
    if height * width <= max_pixels:
        return arr
    scale = (max_pixels / float(height * width)) ** 0.5
    target_height = max(1, int(height * scale))
    target_width = max(1, int(width * scale))
    y_indices = np.linspace(0, height - 1, target_height).astype(np.int64)
    x_indices = np.linspace(0, width - 1, target_width).astype(np.int64)
    return arr[np.ix_(y_indices, x_indices)]


def prepare_depth_response(values: np.ndarray, max_pixels: int = MAX_DEPTH_PIXELS) -> dict[str, Any]:
    arr = np.asarray(values, dtype=np.float32)
    if arr.ndim != 2:
        raise RuntimeErrorCode("inference_failed", "depth output must be a 2D array")
    if arr.size == 0 or not np.isfinite(arr).all() or np.any(arr < 0.0):
        raise RuntimeErrorCode("inference_failed", "depth output must be finite, nonnegative and non-empty")
    downsampled = downsample_depth(arr, max_pixels=max_pixels)
    minimum = float(downsampled.min())
    maximum = float(downsampled.max())
    height, width = downsampled.shape
    return {
        "width": int(width),
        "height": int(height),
        "values": [float(v) for v in downsampled.reshape(-1)],
        "unit": "meter",
        "minimum": minimum,
        "maximum": maximum,
    }


def yolo_model_path() -> Path | None:
    path = MODEL_DIR / "model.pt"
    return path if path.is_file() else None


def handle_detect(request: dict[str, Any]) -> dict[str, Any]:
    decoded = decode_image(request.get("image"))
    model_path = yolo_model_path()
    if model_path is None:
        raise RuntimeErrorCode("model_unavailable", "YOLO artifact /model/model.pt is required")
    try:
        with contextlib.redirect_stdout(sys.stderr):
            from ultralytics import YOLO
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("model_unavailable", "Ultralytics YOLO is not installed in this runtime image") from exc
    try:
        with contextlib.redirect_stdout(sys.stderr):
            model = YOLO(str(model_path))
            results = model.predict(decoded.image, verbose=False)
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("inference_failed", f"YOLO inference failed: {exc}") from exc

    detections: list[dict[str, Any]] = []
    names = getattr(model, "names", {}) or {}
    for result in results:
        boxes = getattr(result, "boxes", None)
        if boxes is None:
            continue
        for box in boxes:
            xyxy = box.xyxy[0].tolist()
            cls = int(box.cls[0].item()) if getattr(box, "cls", None) is not None else -1
            label = str(names.get(cls, cls if cls >= 0 else "object"))
            confidence = float(box.conf[0].item()) if getattr(box, "conf", None) is not None else 0.0
            detections.append({"label": label, "confidence": clamp01(confidence), **normalize_box(*xyxy, decoded.width, decoded.height)})
    return result_response(str(request.get("id", "unknown")), {"detections": detections})


def handle_segment(request: dict[str, Any]) -> dict[str, Any]:
    decoded = decode_image(request.get("image"))
    point_coords, point_labels = point_prompts(request.get("points"), decoded.width, decoded.height)
    boxes = box_prompts(request.get("boxes"), decoded.width, decoded.height)
    if len(point_coords) == 0 and len(boxes) == 0:
        raise RuntimeErrorCode("invalid_input", "SAM segmentation requires at least one point or box prompt")
    if len(boxes) > 1:
        raise RuntimeErrorCode("invalid_input", "SAM segmentation accepts at most one box prompt")
    checkpoint = MODEL_DIR / "model.pt"
    if not checkpoint.is_file():
        raise RuntimeErrorCode("model_unavailable", "SAM artifact /model/model.pt is required")
    try:
        with contextlib.redirect_stdout(sys.stderr):
            from ultralytics import SAM
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("model_unavailable", "Ultralytics SAM is not installed in this runtime image") from exc
    try:
        with contextlib.redirect_stdout(sys.stderr):
            model = SAM(str(checkpoint))
            # Ultralytics selects SAM2Predictor from the checkpoint path stem, but
            # Aileron intentionally mounts every task artifact as model.pt.
            model.is_sam2 = True
            results = model.predict(
                decoded.image,
                points=[point_coords.tolist()] if len(point_coords) else None,
                labels=[point_labels.tolist()] if len(point_labels) else None,
                bboxes=boxes[0].tolist() if len(boxes) == 1 else None,
                conf=0.0,
                verbose=False,
            )
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("inference_failed", f"SAM inference failed: {exc}") from exc

    if len(results) != 1:
        raise RuntimeErrorCode("inference_failed", "SAM returned an unexpected result count")
    result = results[0]
    masks = getattr(getattr(result, "masks", None), "data", None)
    scores = getattr(getattr(result, "boxes", None), "conf", None)
    if masks is None or scores is None:
        raise RuntimeErrorCode("inference_failed", "SAM result is missing masks or mask scores")
    mask_values = tensor_to_numpy(masks)
    score_values = tensor_to_numpy(scores).reshape(-1)
    if mask_values.ndim != 3 or mask_values.shape[0] == 0 or len(mask_values) != len(score_values):
        raise RuntimeErrorCode("inference_failed", "SAM returned inconsistent masks and mask scores")
    if len(mask_values) != 1 or not np.isfinite(score_values).all():
        raise RuntimeErrorCode("inference_failed", "SAM must return one mask with a finite score")

    response_masks: list[dict[str, Any]] = []
    for mask, score in zip(mask_values, score_values, strict=True):
        mask_arr = np.asarray(mask) > 0.5
        if mask_arr.shape != (decoded.height, decoded.width):
            resized = Image.fromarray(mask_arr.astype(np.uint8)).resize(
                (decoded.width, decoded.height),
                resample=Image.Resampling.NEAREST,
            )
            mask_arr = np.asarray(resized).astype(bool)
        ys, xs = np.where(mask_arr)
        if xs.size == 0 or ys.size == 0:
            box = {"x": 0.0, "y": 0.0, "width": 0.0, "height": 0.0}
            cropped_mask = mask_arr[:1, :1]
        else:
            x1 = int(xs.min())
            y1 = int(ys.min())
            x2 = int(xs.max() + 1)
            y2 = int(ys.max() + 1)
            box = normalize_box(float(x1), float(y1), float(x2), float(y2), decoded.width, decoded.height)
            cropped_mask = mask_arr[y1:y2, x1:x2]
        response_masks.append({
            "label": "mask",
            "confidence": clamp01(float(score)),
            **box,
            "mask_base64": encode_mask_png(cropped_mask),
            "mask_width": int(cropped_mask.shape[1]),
            "mask_height": int(cropped_mask.shape[0]),
        })
    return result_response(str(request.get("id", "unknown")), {"masks": response_masks})


def handle_depth(request: dict[str, Any]) -> dict[str, Any]:
    decoded = decode_image(request.get("image"))
    model_path = yolo_model_path()
    if model_path is None:
        raise RuntimeErrorCode("model_unavailable", "YOLO depth artifact /model/model.pt is required")
    try:
        with contextlib.redirect_stdout(sys.stderr):
            from ultralytics import YOLO
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("model_unavailable", "Ultralytics YOLO is not installed in this runtime image") from exc
    try:
        with contextlib.redirect_stdout(sys.stderr):
            model = YOLO(str(model_path))
            results = model.predict(decoded.image, verbose=False)
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("inference_failed", f"YOLO depth inference failed: {exc}") from exc
    if len(results) != 1:
        raise RuntimeErrorCode("inference_failed", "YOLO depth returned an unexpected result count")
    depth = getattr(getattr(results[0], "depth", None), "data", None)
    if depth is None:
        raise RuntimeErrorCode("inference_failed", "YOLO depth result is missing depth.data")
    return result_response(
        str(request.get("id", "unknown")),
        {"depth": prepare_depth_response(tensor_to_numpy(depth))},
    )


def tensor_to_numpy(value: Any) -> np.ndarray:
    if hasattr(value, "detach"):
        value = value.detach()
    if hasattr(value, "cpu"):
        value = value.cpu()
    if hasattr(value, "numpy"):
        value = value.numpy()
    return np.asarray(value)


def handle_request(request: dict[str, Any]) -> dict[str, Any]:
    request_type = request.get("type")
    if request_type == "detect":
        return handle_detect(request)
    if request_type == "segment":
        return handle_segment(request)
    if request_type == "depth":
        return handle_depth(request)
    raise RuntimeErrorCode("unsupported_request", f"request type {request_type} is not supported by this runtime")


def main() -> int:
    print("[aileron-vision-foundation] ready", file=sys.stderr, flush=True)
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        request_id = "unknown"
        try:
            request = json.loads(line)
            if isinstance(request, dict):
                request_id = str(request.get("id", "unknown"))
            else:
                raise RuntimeErrorCode("invalid_input", "request must be a JSON object")
            response = handle_request(request)
        except RuntimeErrorCode as exc:
            response = error_response(request_id, exc.code, exc.reason)
        except Exception as exc:  # noqa: BLE001
            traceback.print_exc(file=sys.stderr)
            response = error_response(request_id, "inference_failed", str(exc))
        print(json.dumps(response, separators=(",", ":")), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
