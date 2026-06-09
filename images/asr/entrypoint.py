#!/usr/bin/env python3
"""
Aileron ASR container entrypoint.

Reads newline-delimited JSON from stdin, writes responses to stdout.

Supported request types:
  transcribe  – transcribe base64-encoded raw PCM (16 kHz mono f32le)
"""

import base64
import json
import os
import struct
import sys
import tempfile
import traceback
import wave

from faster_whisper import WhisperModel

MODEL_SIZE   = os.environ.get("MODEL_SIZE", "small")
MODEL_PATH   = os.environ.get("MODEL_PATH", "/model")
DEVICE       = os.environ.get("DEVICE", "cpu")
COMPUTE_TYPE = os.environ.get("COMPUTE_TYPE", "int8")


def load_model() -> WhisperModel:
    sys.stderr.write(f"[aileron-asr] loading whisper-{MODEL_SIZE}\n")
    sys.stderr.flush()
    return WhisperModel(
        MODEL_SIZE,
        device=DEVICE,
        compute_type=COMPUTE_TYPE,
        download_root=MODEL_PATH,
    )


def send(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def pcm_f32le_to_wav(raw_bytes: bytes, sample_rate: int = 16000) -> bytes:
    """Wrap raw f32le PCM in a WAV container so faster-whisper can read it."""
    num_frames = len(raw_bytes) // 4  # 4 bytes per f32 sample
    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
        wav_path = f.name

    with wave.open(wav_path, "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)          # write as 16-bit PCM
        wf.setframerate(sample_rate)
        # Convert f32le → int16
        samples = struct.unpack(f"{num_frames}f", raw_bytes)
        int16 = struct.pack(
            f"{num_frames}h",
            *[max(-32768, min(32767, int(s * 32767))) for s in samples],
        )
        wf.writeframes(int16)

    return wav_path


def handle_transcribe(model: WhisperModel, req: dict) -> None:
    req_id     = req["id"]
    audio_b64  = req.get("audio", "")

    try:
        raw_pcm = base64.b64decode(audio_b64)
    except Exception as e:
        send({"id": req_id, "error": "invalid_audio", "reason": str(e)})
        return

    wav_path = pcm_f32le_to_wav(raw_pcm)
    try:
        segments, _ = model.transcribe(wav_path, beam_size=5)
        for segment in segments:
            send({"id": req_id, "token": segment.text})
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
                req_id = req.get("id", "unknown")
                send({"id": req_id, "error": "unsupported_type", "reason": req_type})
        except Exception:
            req_id = req.get("id", "unknown")
            sys.stderr.write(traceback.format_exc())
            sys.stderr.flush()
            send({"id": req_id, "error": "internal_error", "done": True})


if __name__ == "__main__":
    main()
