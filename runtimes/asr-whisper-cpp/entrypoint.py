#!/usr/bin/env python3
"""
Aileron ASR container entrypoint — whisper.cpp backend (via pywhispercpp).

Reads newline-delimited JSON requests from stdin, writes responses to stdout.

Supported request types:
  transcribe  – transcribe base64-encoded raw PCM (16 kHz mono f32le)

Device auto-detection order (highest priority first):
  1. AILERON_DEVICE env var set explicitly → use as-is
  2. CUDA device present                   → enable whisper.cpp GPU context
  3. Vulkan device present                 → enable whisper.cpp GPU context
  4. No accelerator found                  → CPU only
"""

import base64
import json
import os
import shutil
import subprocess
import sys
import traceback

import numpy as np
from pywhispercpp.model import Model

MODEL_PATH = os.environ.get("MODEL_PATH", "/model/model.bin")
N_THREADS = int(os.environ.get("N_THREADS", str(os.cpu_count() or 4)))


def detect_device() -> str:
    """Return 'cuda', 'vulkan', or 'cpu'."""
    explicit = os.environ.get("AILERON_DEVICE")
    if explicit:
        sys.stderr.write(f"[aileron-asr] device: {explicit} (AILERON_DEVICE override)\n")
        return explicit

    if shutil.which("nvidia-smi"):
        try:
            out = subprocess.check_output(
                ["nvidia-smi", "--query-gpu=name", "--format=csv,noheader"],
                stderr=subprocess.DEVNULL,
            ).decode().strip()
            if out:
                sys.stderr.write(f"[aileron-asr] CUDA GPU detected: {out.splitlines()[0]}\n")
                return "cuda"
        except Exception:
            pass

    if shutil.which("vulkaninfo"):
        try:
            out = subprocess.check_output(
                ["vulkaninfo", "--summary"], stderr=subprocess.DEVNULL
            ).decode()
            if "deviceName" in out or "deviceType" in out:
                sys.stderr.write("[aileron-asr] Vulkan device detected\n")
                return "vulkan"
        except Exception:
            pass

    if has_dri_render_node():
        sys.stderr.write("[aileron-asr] Vulkan render node detected\n")
        return "vulkan"

    sys.stderr.write("[aileron-asr] no GPU detected — using CPU\n")
    return "cpu"


def has_dri_render_node() -> bool:
    dri_dir = "/dev/dri"
    try:
        return any(name.startswith("renderD") for name in os.listdir(dri_dir))
    except OSError:
        return False


def load_model() -> Model:
    device = detect_device()
    use_gpu = device != "cpu"
    sys.stderr.write(
        f"[aileron-asr] loading whisper model: {MODEL_PATH} "
        f"(device={device}, use_gpu={use_gpu}, threads={N_THREADS})\n"
    )
    sys.stderr.flush()
    return Model(
        MODEL_PATH,
        n_threads=N_THREADS,
        context_params={"use_gpu": use_gpu},
        print_progress=False,
    )


def send(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def decode_pcm_f32le(raw_bytes: bytes) -> np.ndarray:
    """Decode raw 16 kHz mono f32le PCM for pywhispercpp without temp WAV I/O."""
    if len(raw_bytes) % 4 != 0:
        raise ValueError("audio byte length is not a multiple of f32 sample size")
    return np.frombuffer(raw_bytes, dtype=np.float32).copy()


def handle_transcribe(model: Model, req: dict) -> None:
    req_id    = req["id"]
    audio_b64 = req.get("audio", "")
    language_hint = req.get("language_hint", "")

    try:
        raw_pcm = base64.b64decode(audio_b64)
    except Exception as e:
        send({"id": req_id, "error": "invalid_audio", "reason": str(e)})
        return

    try:
        audio = decode_pcm_f32le(raw_pcm)
        segments = model.transcribe(audio, language=language_hint or None)
        for seg in segments:
            text = seg.text if hasattr(seg, "text") else str(seg)
            send({"id": req_id, "token": text})
        send({"id": req_id, "done": True})
    except ValueError as e:
        send({"id": req_id, "error": "invalid_audio", "reason": str(e)})


def main() -> None:
    model = load_model()
    sys.stderr.write("[aileron-asr] ready\n")
    sys.stderr.flush()

    for raw_line in sys.stdin:
        line = raw_line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError as e:
            sys.stderr.write(f"[aileron-asr] bad request JSON: {e}\n")
            sys.stderr.flush()
            continue

        req_type = req.get("type", "")
        try:
            if req_type == "transcribe":
                handle_transcribe(model, req)
            else:
                send({"id": req.get("id", "unknown"),
                      "error": "unsupported_type", "reason": req_type})
        except Exception:
            sys.stderr.write(traceback.format_exc())
            sys.stderr.flush()
            send({"id": req.get("id", "unknown"), "error": "internal_error", "done": True})


if __name__ == "__main__":
    main()
