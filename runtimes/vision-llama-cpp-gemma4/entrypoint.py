#!/usr/bin/env python3
"""
Aileron vision container entrypoint.

Reads newline-delimited JSON requests from stdin, writes responses to stdout.

Supported request types:
  describe  - describe a PNG/JPEG image using a llama.cpp multimodal model
"""

import base64
import json
import os
import shutil
import subprocess
import sys
import traceback

from llama_cpp import Llama
from llama_cpp.llama_chat_format import Gemma4ChatHandler

MODEL_PATH = os.environ.get("MODEL_PATH", "/model/model.gguf")
MMPROJ_PATH = os.environ.get("MMPROJ_PATH", "/model/mmproj.gguf")
N_CTX = int(os.environ.get("N_CTX", "4096"))
N_THREADS = int(os.environ.get("N_THREADS", str(os.cpu_count() or 4)))

DEFAULT_PROMPT = os.environ.get(
    "VISION_PROMPT",
    "Describe this image clearly and concisely. Include visible objects, people, text, and relevant context.",
)


def detect_gpu_layers() -> int:
    explicit = os.environ.get("N_GPU_LAYERS")
    if explicit is not None:
        layers = int(explicit)
        sys.stderr.write(f"[aileron-vision] N_GPU_LAYERS={layers} (explicit)\n")
        return layers

    if shutil.which("nvidia-smi"):
        try:
            out = subprocess.check_output(
                ["nvidia-smi", "--query-gpu=name", "--format=csv,noheader"],
                stderr=subprocess.DEVNULL,
            ).decode().strip()
            if out:
                sys.stderr.write(f"[aileron-vision] CUDA GPU detected: {out.splitlines()[0]}\n")
                return -1
        except Exception:
            pass

    if shutil.which("rocm-smi"):
        try:
            out = subprocess.check_output(
                ["rocm-smi", "--showproductname"],
                stderr=subprocess.DEVNULL,
            ).decode()
            if "GPU" in out or "Radeon" in out or "gfx" in out.lower():
                sys.stderr.write("[aileron-vision] ROCm GPU detected\n")
                return -1
        except Exception:
            pass

    if shutil.which("vulkaninfo"):
        try:
            out = subprocess.check_output(
                ["vulkaninfo", "--summary"],
                stderr=subprocess.DEVNULL,
            ).decode()
            if "deviceName" in out or "deviceType" in out:
                for line in out.splitlines():
                    if "deviceName" in line:
                        name = line.split("=")[-1].strip()
                        sys.stderr.write(f"[aileron-vision] Vulkan device detected: {name}\n")
                        break
                return -1
        except Exception:
            pass

    sys.stderr.write(f"[aileron-vision] no GPU detected - using CPU ({N_THREADS} threads)\n")
    return 0


def load_model() -> Llama:
    n_gpu_layers = detect_gpu_layers()
    sys.stderr.write(
        f"[aileron-vision] loading {MODEL_PATH} with {MMPROJ_PATH} "
        f"(ctx={N_CTX}, chat_format=gemma4, gpu_layers={n_gpu_layers})\n"
    )
    sys.stderr.flush()
    return Llama(
        model_path=MODEL_PATH,
        chat_handler=Gemma4ChatHandler(
            clip_model_path=MMPROJ_PATH,
            verbose=False,
        ),
        n_ctx=N_CTX,
        n_gpu_layers=n_gpu_layers,
        n_threads=N_THREADS,
        verbose=False,
    )


def send(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def image_to_data_url(value) -> str:
    if isinstance(value, str):
        raw = base64.b64decode(value)
    elif isinstance(value, list):
        raw = bytes(value)
    else:
        raise ValueError("image must be a base64 string or byte array")

    if raw.startswith(b"\xff\xd8\xff"):
        mime = "image/jpeg"
    elif raw.startswith(b"\x89PNG\r\n\x1a\n"):
        mime = "image/png"
    else:
        raise ValueError("image must be PNG or JPEG")

    encoded = base64.b64encode(raw).decode("ascii")
    return f"data:{mime};base64,{encoded}"


def handle_describe(llm: Llama, req: dict) -> None:
    req_id = req["id"]
    prompt = req.get("prompt") or DEFAULT_PROMPT

    try:
        image_url = image_to_data_url(req.get("image"))
    except Exception as e:
        send({"id": req_id, "error": "invalid_image", "reason": str(e), "done": True})
        return

    response = llm.create_chat_completion(
        messages=[
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": prompt},
                    {"type": "image_url", "image_url": {"url": image_url}},
                ],
            }
        ],
        max_tokens=int(req.get("max_tokens", 512)),
        stream=False,
    )
    text = response["choices"][0]["message"]["content"].strip()
    send({"id": req_id, "token": text, "done": True})


def main() -> None:
    llm = load_model()
    sys.stderr.write("[aileron-vision] ready\n")
    sys.stderr.flush()

    for raw_line in sys.stdin:
        line = raw_line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError as e:
            sys.stderr.write(f"[aileron-vision] bad request JSON: {e}\n")
            sys.stderr.flush()
            continue

        req_type = req.get("type", "")
        req_id = req.get("id", "unknown")
        try:
            if req_type == "describe":
                handle_describe(llm, req)
            else:
                send({"id": req_id, "error": "unsupported_type", "reason": req_type, "done": True})
        except Exception:
            sys.stderr.write(traceback.format_exc())
            sys.stderr.flush()
            send({"id": req_id, "error": "internal_error", "done": True})


if __name__ == "__main__":
    main()
