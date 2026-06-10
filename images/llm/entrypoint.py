#!/usr/bin/env python3
"""
Aileron LLM container entrypoint.

Reads newline-delimited JSON requests from stdin, writes responses to stdout.

Supported request types:
  generate            – stream tokens via instructor partial streaming
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
import shutil
import subprocess
import sys
import traceback

from llama_cpp import Llama

MODEL_PATH = os.environ.get("MODEL_PATH", "/model/model.gguf")
N_CTX      = int(os.environ.get("N_CTX", "4096"))
N_THREADS  = int(os.environ.get("N_THREADS", str(os.cpu_count() or 4)))

DEFAULT_SYSTEM = (
    "You are a helpful assistant. "
    "Always respond in the same language as the user's message. "
    "Be concise and accurate."
)


def detect_gpu_layers() -> int:
    """Return the number of layers to offload to GPU, or 0 for CPU-only."""

    explicit = os.environ.get("N_GPU_LAYERS")
    if explicit is not None:
        layers = int(explicit)
        sys.stderr.write(f"[aileron-llm] N_GPU_LAYERS={layers} (explicit)\n")
        return layers

    if shutil.which("nvidia-smi"):
        try:
            out = subprocess.check_output(
                ["nvidia-smi", "--query-gpu=name", "--format=csv,noheader"],
                stderr=subprocess.DEVNULL,
            ).decode().strip()
            if out:
                sys.stderr.write(f"[aileron-llm] CUDA GPU detected: {out.splitlines()[0]}"
                                 " — offloading all layers\n")
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
                sys.stderr.write("[aileron-llm] ROCm GPU detected — offloading all layers\n")
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
                        sys.stderr.write(
                            f"[aileron-llm] Vulkan device detected: {name}"
                            " — offloading all layers\n"
                        )
                        break
                return -1
        except Exception:
            pass

    sys.stderr.write(f"[aileron-llm] no GPU detected — using CPU ({N_THREADS} threads)\n")
    return 0


def load_model() -> Llama:
    n_gpu_layers = detect_gpu_layers()
    sys.stderr.write(f"[aileron-llm] loading {MODEL_PATH}"
                     f" (ctx={N_CTX}, gpu_layers={n_gpu_layers})\n")
    sys.stderr.flush()
    return Llama(
        model_path=MODEL_PATH,
        n_ctx=N_CTX,
        n_gpu_layers=n_gpu_layers,
        n_threads=N_THREADS,
        verbose=False,
    )


def send(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()




# ── Request handlers ──────────────────────────────────────────────────────────

def handle_generate(llm: Llama, req: dict) -> None:
    """Stream tokens using plain chat completion — no instructor overhead."""
    req_id     = req["id"]
    prompt     = req.get("prompt", "")
    max_tokens = int(req.get("max_tokens", 512))
    system     = req.get("system", DEFAULT_SYSTEM)

    for chunk in llm.create_chat_completion(
        messages=[
            {"role": "system", "content": system},
            {"role": "user",   "content": prompt},
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
        from jsonschema import validate as jsonschema_validate, ValidationError
        parsed = json.loads(result_text)
        if schema:
            jsonschema_validate(instance=parsed, schema=schema)
    except (json.JSONDecodeError, ValidationError) as e:
        send({"id": req_id, "error": "schema_validation_failed", "reason": str(e)})
        return

    send({"id": req_id, "result": result_text, "done": True})


def main() -> None:
    llm = load_model()
    sys.stderr.write("[aileron-llm] ready\n")
    sys.stderr.flush()

    for raw_line in sys.stdin:
        line = raw_line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError as e:
            sys.stderr.write(f"[aileron-llm] bad request JSON: {e}\n")
            sys.stderr.flush()
            continue

        req_type = req.get("type", "")
        try:
            if req_type == "generate":
                handle_generate(llm, req)
            elif req_type == "generate_structured":
                handle_generate_structured(llm, req)
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
