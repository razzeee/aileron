#!/usr/bin/env python3
"""
Aileron vision container entrypoint.

Reads newline-delimited JSON requests from stdin, writes responses to stdout.

Supported request types:
  describe  - describe a PNG/JPEG image using a llama.cpp multimodal model
"""

import base64
import os
import sys

from llama_cpp import Llama
from llama_cpp.llama_chat_format import Gemma4ChatHandler

COMMON_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "_llama_cpp_common"))
if os.path.isdir(COMMON_DIR):
    sys.path.insert(0, COMMON_DIR)

from aileron_runtime_common import load_llama, send, serve_requests

MODEL_PATH = os.environ.get("MODEL_PATH", "/model/model.gguf")
MMPROJ_PATH = os.environ.get("MMPROJ_PATH", "/model/mmproj.gguf")
N_CTX = int(os.environ.get("N_CTX", "4096"))
N_THREADS = int(os.environ.get("N_THREADS", str(os.cpu_count() or 4)))

DEFAULT_PROMPT = os.environ.get(
    "VISION_PROMPT",
    "Describe this image clearly and concisely. Include visible objects, people, text, and relevant context.",
)
def load_model() -> Llama:
    return load_llama(
        log_prefix="aileron-vision",
        model_path=MODEL_PATH,
        loading_suffix=f"with {MMPROJ_PATH} (chat_format=gemma4)",
        chat_handler=Gemma4ChatHandler(
            clip_model_path=MMPROJ_PATH,
            verbose=False,
        ),
        n_ctx=N_CTX,
        n_threads=N_THREADS,
    )


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
    serve_requests(
        llm=llm,
        handlers={"describe": handle_describe},
        log_prefix="aileron-vision",
        unsupported_done=True,
    )


if __name__ == "__main__":
    main()
