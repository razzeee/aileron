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
SAM2_CONFIG_NAME = os.environ.get("SAM2_CONFIG_NAME", "configs/sam2/sam2_hiera_t.yaml")
YOLO_INPUT_SIZE = int(os.environ.get("YOLO_INPUT_SIZE", "640"))
YOLO_CONFIDENCE_THRESHOLD = float(os.environ.get("YOLO_CONFIDENCE_THRESHOLD", "0.25"))
YOLO_IOU_THRESHOLD = float(os.environ.get("YOLO_IOU_THRESHOLD", "0.45"))
COCO_LABELS = (
    "person", "bicycle", "car", "motorcycle", "airplane", "bus", "train", "truck", "boat", "traffic light",
    "fire hydrant", "stop sign", "parking meter", "bench", "bird", "cat", "dog", "horse", "sheep", "cow",
    "elephant", "bear", "zebra", "giraffe", "backpack", "umbrella", "handbag", "tie", "suitcase", "frisbee",
    "skis", "snowboard", "sports ball", "kite", "baseball bat", "baseball glove", "skateboard", "surfboard", "tennis racket", "bottle",
    "wine glass", "cup", "fork", "knife", "spoon", "bowl", "banana", "apple", "sandwich", "orange",
    "broccoli", "carrot", "hot dog", "pizza", "donut", "cake", "chair", "couch", "potted plant", "bed",
    "dining table", "toilet", "tv", "laptop", "mouse", "remote", "keyboard", "cell phone", "microwave", "oven",
    "toaster", "sink", "refrigerator", "book", "clock", "vase", "scissors", "teddy bear", "hair drier", "toothbrush",
)


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


def normalize_depth(values: np.ndarray) -> tuple[list[float], float, float]:
    arr = np.asarray(values, dtype=np.float32)
    if arr.size == 0 or not np.isfinite(arr).all():
        raise RuntimeErrorCode("inference_failed", "depth output must be finite and non-empty")
    minimum = float(arr.min())
    maximum = float(arr.max())
    if maximum > minimum:
        normalized = (arr - minimum) / (maximum - minimum)
    else:
        normalized = np.zeros_like(arr, dtype=np.float32)
    return [float(v) for v in normalized.reshape(-1)], minimum, maximum


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
    if arr.size == 0 or not np.isfinite(arr).all():
        raise RuntimeErrorCode("inference_failed", "depth output must be finite and non-empty")
    minimum = float(arr.min())
    maximum = float(arr.max())
    downsampled = downsample_depth(arr, max_pixels=max_pixels)
    if maximum > minimum:
        normalized_arr = (downsampled - minimum) / (maximum - minimum)
    else:
        normalized_arr = np.zeros_like(downsampled, dtype=np.float32)
    height, width = downsampled.shape
    return {
        "width": int(width),
        "height": int(height),
        "values": [float(v) for v in normalized_arr.reshape(-1)],
        "minimum": minimum,
        "maximum": maximum,
    }


def yolo_model_path() -> Path | None:
    for filename in ("model.pt", "model.onnx"):
        path = MODEL_DIR / filename
        if path.is_file():
            return path
    return None


def handle_detect(request: dict[str, Any]) -> dict[str, Any]:
    decoded = decode_image(request.get("image"))
    model_path = yolo_model_path()
    if model_path is None:
        raise RuntimeErrorCode("model_unavailable", "YOLO artifact /model/model.pt or /model/model.onnx is required")
    if model_path.suffix == ".onnx":
        return handle_detect_onnx(request, decoded, model_path)
    try:
        with contextlib.redirect_stdout(sys.stderr):
            from ultralytics import YOLO
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("model_unavailable", "Ultralytics YOLO is not installed in this runtime image") from exc
    try:
        with contextlib.redirect_stdout(sys.stderr):
            model = YOLO(str(model_path))
            results = model.predict(np.asarray(decoded.image), verbose=False)
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


def handle_detect_onnx(request: dict[str, Any], decoded: DecodedImage, model_path: Path) -> dict[str, Any]:
    try:
        with contextlib.redirect_stdout(sys.stderr):
            import onnxruntime as ort
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("model_unavailable", "ONNX Runtime is not installed in this runtime image") from exc
    try:
        tensor, scale, pad_x, pad_y = prepare_yolo_input(decoded.image, YOLO_INPUT_SIZE)
        with contextlib.redirect_stdout(sys.stderr):
            session = ort.InferenceSession(str(model_path), providers=["CPUExecutionProvider"])
            input_name = session.get_inputs()[0].name
            outputs = session.run(None, {input_name: tensor})
        detections = parse_yolo_output(outputs[0], decoded.width, decoded.height, scale, pad_x, pad_y)
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("inference_failed", f"YOLO ONNX inference failed: {exc}") from exc
    return result_response(str(request.get("id", "unknown")), {"detections": detections})


def prepare_yolo_input(image: Image.Image, size: int) -> tuple[np.ndarray, float, float, float]:
    width, height = image.size
    scale = min(size / float(width), size / float(height))
    resized_width = max(1, int(round(width * scale)))
    resized_height = max(1, int(round(height * scale)))
    pad_x = (size - resized_width) / 2.0
    pad_y = (size - resized_height) / 2.0
    canvas = Image.new("RGB", (size, size), (114, 114, 114))
    resized = image.resize((resized_width, resized_height), Image.Resampling.BILINEAR)
    canvas.paste(resized, (int(round(pad_x)), int(round(pad_y))))
    arr = np.asarray(canvas, dtype=np.float32) / 255.0
    return np.transpose(arr, (2, 0, 1))[None, ...], scale, pad_x, pad_y


def parse_yolo_output(output: Any, image_width: int, image_height: int, scale: float, pad_x: float, pad_y: float) -> list[dict[str, Any]]:
    predictions = np.asarray(output, dtype=np.float32)
    if predictions.ndim == 3:
        predictions = predictions[0]
    if predictions.ndim != 2:
        raise RuntimeErrorCode("inference_failed", f"YOLO output must be 2D or batched 2D, got shape {predictions.shape}")
    if predictions.shape[0] in (84, 85):
        predictions = predictions.T
    if predictions.shape[1] < 6:
        raise RuntimeErrorCode("inference_failed", f"YOLO output has too few columns: {predictions.shape}")

    candidates: list[dict[str, Any]] = []
    for row in predictions:
        x_center, y_center, width, height = [float(v) for v in row[:4]]
        scores = row[4:]
        if scores.size == 2 and 0.0 <= scores[1] <= len(COCO_LABELS) - 1:
            confidence = float(scores[0])
            class_id = int(scores[1])
        else:
            if scores.size == len(COCO_LABELS) + 1:
                objectness = float(scores[0])
                class_scores = scores[1:]
            else:
                objectness = 1.0
                class_scores = scores
            class_id = int(np.argmax(class_scores))
            confidence = objectness * float(class_scores[class_id])
        if confidence < YOLO_CONFIDENCE_THRESHOLD:
            continue
        x1 = (x_center - width / 2.0 - pad_x) / scale
        y1 = (y_center - height / 2.0 - pad_y) / scale
        x2 = (x_center + width / 2.0 - pad_x) / scale
        y2 = (y_center + height / 2.0 - pad_y) / scale
        x1 = max(0.0, min(float(image_width), x1))
        y1 = max(0.0, min(float(image_height), y1))
        x2 = max(0.0, min(float(image_width), x2))
        y2 = max(0.0, min(float(image_height), y2))
        if x2 <= x1 or y2 <= y1:
            continue
        candidates.append({
            "class_id": class_id,
            "label": COCO_LABELS[class_id] if 0 <= class_id < len(COCO_LABELS) else str(class_id),
            "confidence": clamp01(confidence),
            "x1": x1,
            "y1": y1,
            "x2": x2,
            "y2": y2,
        })

    return [
        {"label": candidate["label"], "confidence": candidate["confidence"], **normalize_box(candidate["x1"], candidate["y1"], candidate["x2"], candidate["y2"], image_width, image_height)}
        for candidate in non_max_suppression(candidates, YOLO_IOU_THRESHOLD)
    ]


def non_max_suppression(candidates: list[dict[str, Any]], iou_threshold: float) -> list[dict[str, Any]]:
    kept: list[dict[str, Any]] = []
    for candidate in sorted(candidates, key=lambda item: item["confidence"], reverse=True):
        if all(candidate["class_id"] != kept_candidate["class_id"] or box_iou(candidate, kept_candidate) <= iou_threshold for kept_candidate in kept):
            kept.append(candidate)
    return kept[:100]


def box_iou(a: dict[str, Any], b: dict[str, Any]) -> float:
    x1 = max(float(a["x1"]), float(b["x1"]))
    y1 = max(float(a["y1"]), float(b["y1"]))
    x2 = min(float(a["x2"]), float(b["x2"]))
    y2 = min(float(a["y2"]), float(b["y2"]))
    intersection = max(0.0, x2 - x1) * max(0.0, y2 - y1)
    area_a = max(0.0, float(a["x2"]) - float(a["x1"])) * max(0.0, float(a["y2"]) - float(a["y1"]))
    area_b = max(0.0, float(b["x2"]) - float(b["x1"])) * max(0.0, float(b["y2"]) - float(b["y1"]))
    union = area_a + area_b - intersection
    return intersection / union if union > 0.0 else 0.0


def handle_segment(request: dict[str, Any]) -> dict[str, Any]:
    decoded = decode_image(request.get("image"))
    point_coords, point_labels = point_prompts(request.get("points"), decoded.width, decoded.height)
    boxes = box_prompts(request.get("boxes"), decoded.width, decoded.height)
    if len(point_coords) == 0 and len(boxes) == 0:
        raise RuntimeErrorCode("invalid_input", "SAM2 segmentation requires at least one point or box prompt")
    if len(boxes) > 1:
        raise RuntimeErrorCode("invalid_input", "SAM2 segmentation currently accepts at most one box prompt")
    checkpoint = MODEL_DIR / "model.pt"
    if not checkpoint.is_file():
        raise RuntimeErrorCode("model_unavailable", "SAM2 artifact /model/model.pt is required")
    config = MODEL_DIR / "config.yaml"
    if not config.is_file():
        raise RuntimeErrorCode("model_unavailable", "SAM2 config /model/config.yaml is required")
    try:
        with contextlib.redirect_stdout(sys.stderr):
            from sam2.build_sam import build_sam2
            from sam2.sam2_image_predictor import SAM2ImagePredictor
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("model_unavailable", "SAM2 Python package is not installed in this runtime image") from exc
    try:
        with contextlib.redirect_stdout(sys.stderr):
            model = build_sam2(SAM2_CONFIG_NAME, str(checkpoint), device="cpu")
            predictor = SAM2ImagePredictor(model)
            predictor.set_image(np.asarray(decoded.image))
            masks, scores, _ = predictor.predict(
                point_coords=point_coords if len(point_coords) else None,
                point_labels=point_labels if len(point_labels) else None,
                box=boxes[0] if len(boxes) == 1 else None,
                multimask_output=False,
            )
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("inference_failed", f"SAM2 inference failed: {exc}") from exc

    response_masks: list[dict[str, Any]] = []
    for index, mask in enumerate(masks):
        mask_arr = np.asarray(mask).astype(bool)
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
            "confidence": clamp01(float(scores[index]) if index < len(scores) else 0.0),
            **box,
            "mask_base64": encode_mask_png(cropped_mask),
            "mask_width": int(cropped_mask.shape[1]),
            "mask_height": int(cropped_mask.shape[0]),
        })
    return result_response(str(request.get("id", "unknown")), {"masks": response_masks})


def handle_depth(request: dict[str, Any]) -> dict[str, Any]:
    decoded = decode_image(request.get("image"))
    model_path = depth_model_path()
    if model_path is None:
        raise RuntimeErrorCode("model_unavailable", "depth artifacts are required under /model/model/ or flat in /model")
    if is_da3_model(model_path):
        return handle_da3_depth(request, decoded, model_path)
    return handle_transformers_depth(request, decoded, model_path)


def handle_da3_depth(request: dict[str, Any], decoded: DecodedImage, model_path: Path) -> dict[str, Any]:
    try:
        with contextlib.redirect_stdout(sys.stderr):
            import torch
            from depth_anything_3.api import DepthAnything3
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("model_unavailable", "depth-anything-3 is required for DA3 inference") from exc
    try:
        with contextlib.redirect_stdout(sys.stderr):
            model = DepthAnything3.from_pretrained(str(model_path)).to(device=torch.device("cpu"))
            prediction = model.inference([np.asarray(decoded.image)], export_dir=None)
        predicted = np.asarray(prediction.depth)[0]
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("inference_failed", f"DA3 inference failed: {exc}") from exc
    return result_response(str(request.get("id", "unknown")), {"depth": prepare_depth_response(predicted)})


def handle_transformers_depth(request: dict[str, Any], decoded: DecodedImage, model_path: Path) -> dict[str, Any]:
    try:
        import torch
        from transformers import AutoImageProcessor, AutoModelForDepthEstimation
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("model_unavailable", "Torch and Transformers are required for depth inference") from exc
    try:
        processor = AutoImageProcessor.from_pretrained(str(model_path), local_files_only=True, trust_remote_code=True)
        model = AutoModelForDepthEstimation.from_pretrained(str(model_path), local_files_only=True, trust_remote_code=True)
        inputs = processor(images=decoded.image, return_tensors="pt")
        with torch.no_grad():
            outputs = model(**inputs)
        predicted = outputs.predicted_depth.detach().cpu().numpy()[0]
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("inference_failed", f"depth inference failed: {exc}") from exc
    return result_response(str(request.get("id", "unknown")), {"depth": prepare_depth_response(predicted)})


def depth_model_path() -> Path | None:
    nested = MODEL_DIR / "model"
    if nested.is_dir():
        return nested
    required = ("config.json", "model.safetensors", "preprocessor_config.json")
    if all((MODEL_DIR / filename).is_file() for filename in required):
        return MODEL_DIR
    da3_required = ("config.json", "model.safetensors")
    if all((MODEL_DIR / filename).is_file() for filename in da3_required):
        return MODEL_DIR
    return None


def is_da3_model(model_path: Path) -> bool:
    config_path = model_path / "config.json"
    if not config_path.is_file():
        return False
    try:
        config = json.loads(config_path.read_text(encoding="utf-8"))
    except Exception:  # noqa: BLE001
        return False
    serialized = json.dumps(config)
    return (
        config.get("model_type") in {"depth-anything-3", "depth_anything_3"}
        or config.get("model_name", "").startswith("da3-")
        or "depth_anything_3" in serialized
        or "DepthAnything3" in serialized
    )


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
