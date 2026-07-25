from __future__ import annotations

import base64
import contextlib
import json
import os
import sys
import time
import traceback
from pathlib import Path
from typing import Any, Protocol

import numpy as np
import pycountry


MODEL_DIR = Path(os.environ.get("MODEL_DIR", "/model"))
MAX_TEXT_BYTES = 16 * 1024
CHUNK_DURATION_MS = 50


def normalize_language(value: str) -> str:
    tag = value.strip().lower().replace("_", "-")
    primary = tag.split("-", 1)[0]
    if not primary:
        return ""
    language = None
    if len(primary) == 2 and primary.isalpha():
        language = pycountry.languages.get(alpha_2=primary)
    elif len(primary) == 3 and primary.isalpha():
        language = pycountry.languages.get(alpha_3=primary)
    if language is None:
        try:
            language = pycountry.languages.lookup(primary)
        except LookupError:
            return tag
    return getattr(language, "alpha_3", primary).lower()


SUPPORTED_LANGUAGES = frozenset(
    normalize_language(value)
    for value in os.environ.get("SUPPORTED_LANGUAGES", "").split(",")
    if value.strip()
)


class RuntimeErrorCode(Exception):
    def __init__(self, code: str, reason: str):
        super().__init__(reason)
        self.code = code
        self.reason = reason


class ModelAdapter(Protocol):
    sample_rate: int

    def synthesize(self, text: str) -> np.ndarray: ...


class TransformersVitsAdapter:
    def __init__(self, model_dir: Path):
        try:
            with contextlib.redirect_stdout(sys.stderr):
                import torch
                from transformers import AutoTokenizer, VitsModel

                self._torch = torch
                self._tokenizer = AutoTokenizer.from_pretrained(
                    str(model_dir), local_files_only=True
                )
                self._model = VitsModel.from_pretrained(
                    str(model_dir), local_files_only=True, use_safetensors=True
                ).eval()
        except Exception as exc:  # noqa: BLE001
            raise RuntimeErrorCode("model_unavailable", f"failed to load VITS model: {exc}") from exc
        self.sample_rate = int(getattr(self._model.config, "sampling_rate", 0))
        if not 8_000 <= self.sample_rate <= 192_000:
            raise RuntimeErrorCode("model_unavailable", "VITS config has an invalid sampling rate")

    def synthesize(self, text: str) -> np.ndarray:
        try:
            with contextlib.redirect_stdout(sys.stderr), self._torch.inference_mode():
                inputs = self._tokenizer(text, return_tensors="pt")
                waveform = self._model(**inputs).waveform
            return waveform.detach().cpu().numpy()
        except Exception as exc:  # noqa: BLE001
            raise RuntimeErrorCode("inference_failed", "VITS inference failed") from exc


def validate_model_layout(model_dir: Path) -> None:
    for filename in ("config.json", "vocab.json"):
        if not (model_dir / filename).is_file():
            raise RuntimeErrorCode("model_unavailable", f"/model/{filename} is required")
    single = (model_dir / "model.safetensors").is_file()
    index = model_dir / "model.safetensors.index.json"
    if single:
        return
    if not index.is_file():
        raise RuntimeErrorCode(
            "model_unavailable",
            "/model/model.safetensors or model.safetensors.index.json is required",
        )
    try:
        weight_map = json.loads(index.read_text(encoding="utf-8"))["weight_map"]
    except Exception as exc:  # noqa: BLE001
        raise RuntimeErrorCode("model_unavailable", "invalid Safetensors index") from exc
    shards = sorted(set(weight_map.values())) if isinstance(weight_map, dict) else []
    if not shards or any(
        not isinstance(name, str)
        or "/" in name
        or "\\" in name
        or not name.endswith(".safetensors")
        or not (model_dir / name).is_file()
        for name in shards
    ):
        raise RuntimeErrorCode("model_unavailable", "Safetensors index references missing or unsafe shards")


def validate_request(request: dict[str, Any]) -> tuple[str, str]:
    if request.get("type") != "synthesize":
        raise RuntimeErrorCode("unsupported_request", "this runtime only supports synthesize")
    text = request.get("text")
    if not isinstance(text, str) or not text.strip() or "\0" in text:
        raise RuntimeErrorCode("invalid_input", "text must be a non-empty string")
    if len(text.encode("utf-8")) > MAX_TEXT_BYTES:
        raise RuntimeErrorCode("invalid_input", "text exceeds the 16 KiB limit")
    voice_id = request.get("voice_id", "")
    if not isinstance(voice_id, str) or voice_id:
        raise RuntimeErrorCode("invalid_input", "only the default empty voice_id is supported")
    execution_mode = request.get("execution_mode", "interactive")
    if execution_mode not in ("", "interactive", "background"):
        raise RuntimeErrorCode("invalid_input", "execution_mode must be interactive or background")
    language_hint = request.get("language_hint", "")
    if not isinstance(language_hint, str):
        raise RuntimeErrorCode("invalid_input", "language_hint must be a string")
    language_hint = normalize_language(language_hint)
    if language_hint and SUPPORTED_LANGUAGES and language_hint not in SUPPORTED_LANGUAGES:
        raise RuntimeErrorCode("unsupported_language", f"language {language_hint} is not supported")
    return text, language_hint


def waveform_to_pcm(waveform: np.ndarray) -> bytes:
    samples = np.asarray(waveform, dtype=np.float32).squeeze()
    if samples.ndim != 1 or samples.size == 0 or not np.isfinite(samples).all():
        raise RuntimeErrorCode("invalid_input", "VITS output must be finite mono audio")
    return np.rint(np.clip(samples, -1.0, 1.0) * 32767.0).astype("<i2").tobytes()


def audio_events(request_id: str, pcm: bytes, sample_rate: int) -> list[dict[str, Any]]:
    bytes_per_chunk = max(2, sample_rate * 2 * CHUNK_DURATION_MS // 1000)
    bytes_per_chunk -= bytes_per_chunk % 2
    events = []
    for offset in range(0, len(pcm), bytes_per_chunk):
        event: dict[str, Any] = {
            "id": request_id,
            "audio": base64.b64encode(pcm[offset : offset + bytes_per_chunk]).decode("ascii"),
        }
        if not events:
            event.update(sample_rate=sample_rate, channels=1, sample_format="s16le")
        events.append(event)
    events.append({"id": request_id, "audio": "", "done": True})
    return events


def handle_request(request: dict[str, Any], adapter: ModelAdapter) -> list[dict[str, Any]]:
    request_id = str(request.get("id", "unknown"))
    text, _language_hint = validate_request(request)
    started = time.monotonic()
    pcm = waveform_to_pcm(adapter.synthesize(text))
    elapsed = time.monotonic() - started
    duration = len(pcm) / 2 / adapter.sample_rate
    print(
        f"[aileron-tts-vits] inference_ms={elapsed * 1000:.1f} sample_rate={adapter.sample_rate} output_seconds={duration:.3f}",
        file=sys.stderr,
        flush=True,
    )
    return audio_events(request_id, pcm, adapter.sample_rate)


def error_response(request_id: str, exc: RuntimeErrorCode) -> dict[str, Any]:
    return {"id": request_id, "error": exc.code, "reason": exc.reason, "done": True}


def main() -> int:
    started = time.monotonic()
    validate_model_layout(MODEL_DIR)
    adapter = TransformersVitsAdapter(MODEL_DIR)
    print(
        f"[aileron-tts-vits] ready model={MODEL_DIR.name} startup_ms={(time.monotonic() - started) * 1000:.1f} sample_rate={adapter.sample_rate}",
        file=sys.stderr,
        flush=True,
    )
    for line in sys.stdin:
        if not line.strip():
            continue
        request_id = "unknown"
        try:
            request = json.loads(line)
            if not isinstance(request, dict):
                raise RuntimeErrorCode("invalid_input", "request must be a JSON object")
            request_id = str(request.get("id", "unknown"))
            responses = handle_request(request, adapter)
        except RuntimeErrorCode as exc:
            responses = [error_response(request_id, exc)]
        except Exception as exc:  # noqa: BLE001
            traceback.print_exc(file=sys.stderr)
            responses = [error_response(request_id, RuntimeErrorCode("inference_failed", str(exc)))]
        for response in responses:
            print(json.dumps(response, separators=(",", ":")), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
