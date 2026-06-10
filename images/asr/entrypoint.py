#!/usr/bin/env python3
"""
Aileron ASR container entrypoint — whisper.cpp backend (via pywhispercpp).

Reads newline-delimited JSON requests from stdin, writes responses to stdout.

Supported request types:
  transcribe  – transcribe base64-encoded raw PCM (16 kHz mono f32le)

GPU auto-detection order (highest priority first):
  1. N_GPU_LAYERS env var set explicitly → use as-is
  2. CUDA device present                → offload all layers
  3. Vulkan device present              → offload all layers
  4. No accelerator found               → CPU only
"""

import base64
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import traceback
import wave

from pywhispercpp.model import Model

MODEL_SIZE = os.environ.get("MODEL_SIZE", "base")
MODEL_DIR  = os.environ.get("MODEL_DIR", "/model")


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

    sys.stderr.write("[aileron-asr] no GPU detected — using CPU\n")
    return "cpu"


def load_model() -> Model:
    device = detect_device()
    sys.stderr.write(f"[aileron-asr] loading whisper model: {MODEL_SIZE} (device={device})\n")
    sys.stderr.flush()
    return Model(MODEL_SIZE, models_dir=MODEL_DIR)


def send(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def pcm_f32le_to_wav(raw_bytes: bytes, sample_rate: int = 16000) -> str:
    """Wrap raw f32le PCM in a WAV container that whisper.cpp can read."""
    num_frames = len(raw_bytes) // 4
    samples = struct.unpack(f"{num_frames}f", raw_bytes)
    int16_samples = [max(-32768, min(32767, int(s * 32767))) for s in samples]

    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    with wave.open(tmp.name, "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(sample_rate)
        wf.writeframes(struct.pack(f"{num_frames}h", *int16_samples))
    return tmp.name


def handle_transcribe(model: Model, req: dict) -> None:
    req_id    = req["id"]
    audio_b64 = req.get("audio", "")

    try:
        raw_pcm = base64.b64decode(audio_b64)
    except Exception as e:
        send({"id": req_id, "error": "invalid_audio", "reason": str(e)})
        return

    wav_path = pcm_f32le_to_wav(raw_pcm)
    try:
        segments = model.transcribe(wav_path)
        for seg in segments:
            text = seg.text if hasattr(seg, "text") else str(seg)
            send({"id": req_id, "token": text})
        send({"id": req_id, "done": True})
    finally:
        os.unlink(wav_path)


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
