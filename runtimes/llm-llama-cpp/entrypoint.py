#!/usr/bin/env python3
"""
Aileron LLM container entrypoint.

Reads newline-delimited JSON requests from stdin, writes responses to stdout.

Supported request types:
  generate            – stream tokens via instructor partial streaming
  chat                – stream tokens from explicit chat messages
  generate_structured – return a single JSON result constrained to a schema

GPU auto-detection order (highest priority first):
  1. N_GPU_LAYERS env var set explicitly  → use as-is
  2. CUDA device present                 → offload all layers (-1)
  3. ROCm / HIP device present           → offload all layers (-1)
  4. Vulkan device present               → offload all layers (-1)
  5. No accelerator found                → CPU only (0)
"""

import json
import os
import sys

from llama_cpp import Llama
from jsonschema import ValidationError
from jsonschema import validate as jsonschema_validate

COMMON_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "_llama_cpp_common"))
if os.path.isdir(COMMON_DIR):
    sys.path.insert(0, COMMON_DIR)

from aileron_runtime_common import load_llama, send, serve_requests

MODEL_PATH = os.environ.get("MODEL_PATH", "/model/model.gguf")
N_CTX      = int(os.environ.get("N_CTX", "4096"))
N_THREADS  = int(os.environ.get("N_THREADS", str(os.cpu_count() or 4)))

DEFAULT_SYSTEM = (
    "You are a helpful assistant. "
    "Always respond in the same language as the user's message. "
    "Be concise and accurate."
)
def load_model() -> Llama:
    return load_llama(
        log_prefix="aileron-llm",
        model_path=MODEL_PATH,
        n_ctx=N_CTX,
        n_threads=N_THREADS,
    )




# ── Request handlers ──────────────────────────────────────────────────────────

def stream_chat_or_fallback(llm: Llama, req_id: str, messages: list[dict], max_tokens: int) -> None:
    emitted = False
    for chunk in llm.create_chat_completion(
        messages=messages,
        max_tokens=max_tokens,
        stream=True,
    ):
        choice = chunk.get("choices", [{}])[0]
        delta = choice.get("delta", {})
        token = delta.get("content", "") or choice.get("text", "")
        if token:
            emitted = True
            send({"id": req_id, "token": token})

    if not emitted:
        reply = llm.create_chat_completion(
            messages=messages,
            max_tokens=max_tokens,
            stream=False,
        )
        token = reply["choices"][0].get("message", {}).get("content", "")
        if token:
            send({"id": req_id, "token": token})

def handle_generate(llm: Llama, req: dict) -> None:
    """Stream tokens using plain chat completion — no instructor overhead."""
    req_id     = req["id"]
    prompt     = req.get("prompt", "")
    max_tokens = int(req.get("max_tokens", 512))
    system     = req.get("system", DEFAULT_SYSTEM)

    stream_chat_or_fallback(llm, req_id, [
        {"role": "system", "content": system},
        {"role": "user",   "content": prompt},
    ], max_tokens)

    send({"id": req_id, "done": True})


def handle_chat(llm: Llama, req: dict) -> None:
    """Stream tokens from an explicit stateless chat transcript."""
    req_id = req["id"]
    max_tokens = int(req.get("max_tokens", 512))
    system = req.get("system", DEFAULT_SYSTEM)
    messages = [{"role": "system", "content": system}]
    messages.extend(req.get("messages", []))

    stream_chat_or_fallback(llm, req_id, messages, max_tokens)

    send({"id": req_id, "done": True})


def handle_generate_structured(llm: Llama, req: dict) -> None:
    """Return a single structured JSON result constrained to the caller's schema
    using llama.cpp's native grammar-based sampling."""
    req_id     = req["id"]
    prompt     = req.get("prompt", "")
    max_tokens = int(req.get("max_tokens", 1024))
    schema     = req.get("response_format", {}).get("schema", {})
    system     = req.get("system", DEFAULT_SYSTEM)

    try:
        from llama_cpp import LlamaGrammar
        grammar = LlamaGrammar.from_json_schema(json.dumps(schema))
    except Exception:
        grammar = LlamaGrammar.from_string('root ::= value\n', verbose=False)

    result_text = llm.create_chat_completion(
        messages=[
            {"role": "system", "content": system},
            {"role": "user",   "content": prompt},
        ],
        max_tokens=max_tokens,
        grammar=grammar,
        stream=False,
    )["choices"][0]["message"]["content"].strip()

    try:
        parsed = json.loads(result_text)
        if schema:
            jsonschema_validate(instance=parsed, schema=schema)
    except (json.JSONDecodeError, ValidationError) as e:
        send({"id": req_id, "error": "schema_validation_failed", "reason": str(e), "done": True})
        return

    send({"id": req_id, "result": result_text, "done": True})


def main() -> None:
    llm = load_model()
    serve_requests(
        llm=llm,
        handlers={
            "generate": handle_generate,
            "chat": handle_chat,
            "generate_structured": handle_generate_structured,
        },
        log_prefix="aileron-llm",
        unsupported_done=False,
    )


if __name__ == "__main__":
    main()
