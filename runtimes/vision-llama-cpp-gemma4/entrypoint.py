#!/usr/bin/env python3
"""
Aileron vision container entrypoint.

Reads newline-delimited JSON requests from stdin, writes responses to stdout.

Supported request types:
  generate  - stream text tokens using the multimodal model as a text LLM
  generate_structured - return a JSON result constrained to a schema
  describe  - describe a PNG/JPEG image using a llama.cpp multimodal model
"""

import base64
import json
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
VISION_HANDLER = os.environ.get("VISION_HANDLER", "gemma4")
N_CTX = int(os.environ.get("N_CTX", "4096"))
N_THREADS = int(os.environ.get("N_THREADS", str(os.cpu_count() or 4)))

DEFAULT_PROMPT = os.environ.get(
    "VISION_PROMPT",
    "Describe this image clearly and concisely. Include visible objects, people, text, and relevant context.",
)
DEFAULT_SYSTEM = (
    "You are a helpful assistant. "
    "Always respond in the same language as the user's message. "
    "Be concise and accurate."
)


def load_model() -> Llama:
    if VISION_HANDLER != "gemma4":
        raise ValueError(f"unsupported VISION_HANDLER: {VISION_HANDLER}")

    return load_llama(
        log_prefix="aileron-vision",
        model_path=MODEL_PATH,
        loading_suffix=f"with {MMPROJ_PATH} (chat_format={VISION_HANDLER})",
        chat_handler=Gemma4ChatHandler(
            clip_model_path=MMPROJ_PATH,
            verbose=False,
        ),
        n_ctx=N_CTX,
        n_threads=N_THREADS,
    )


def handle_generate(llm: Llama, req: dict) -> None:
    req_id = req["id"]
    prompt = req.get("prompt", "")
    max_tokens = int(req.get("max_tokens", 512))
    system = req.get("system", DEFAULT_SYSTEM)

    for chunk in llm.create_chat_completion(
        messages=[
            {"role": "system", "content": system},
            {"role": "user", "content": prompt},
        ],
        max_tokens=max_tokens,
        stream=True,
    ):
        delta = chunk["choices"][0].get("delta", {})
        token = delta.get("content", "")
        if token:
            send({"id": req_id, "token": token})

    send({"id": req_id, "done": True})


def handle_generate_structured(llm: Llama, req: dict) -> None:
    req_id = req["id"]
    prompt = req.get("prompt", "")
    max_tokens = int(req.get("max_tokens", 1024))
    schema = req.get("response_format", {}).get("schema", {})
    system = req.get("system", DEFAULT_SYSTEM)

    try:
        from llama_cpp import LlamaGrammar

        grammar = LlamaGrammar.from_json_schema(json.dumps(schema))
    except Exception:
        grammar = LlamaGrammar.from_string("root ::= value\n", verbose=False)

    result_text = llm.create_chat_completion(
        messages=[
            {"role": "system", "content": system},
            {"role": "user", "content": prompt},
        ],
        max_tokens=max_tokens,
        grammar=grammar,
        stream=False,
    )["choices"][0]["message"]["content"].strip()

    try:
        from jsonschema import ValidationError
        from jsonschema import validate as jsonschema_validate

        parsed = json.loads(result_text)
        if schema:
            jsonschema_validate(instance=parsed, schema=schema)
    except (json.JSONDecodeError, ValidationError) as e:
        send({"id": req_id, "error": "schema_validation_failed", "reason": str(e), "done": True})
        return

    send({"id": req_id, "result": result_text, "done": True})


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
        handlers={
            "generate": handle_generate,
            "generate_structured": handle_generate_structured,
            "describe": handle_describe,
        },
        log_prefix="aileron-vision",
        unsupported_done=False,
    )


if __name__ == "__main__":
    main()
