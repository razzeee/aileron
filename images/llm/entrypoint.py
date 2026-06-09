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
from typing import Optional

import instructor
from llama_cpp import Llama
from pydantic import BaseModel

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


def load_model() -> tuple[Llama, instructor.Instructor]:
    n_gpu_layers = detect_gpu_layers()
    sys.stderr.write(f"[aileron-llm] loading {MODEL_PATH}"
                     f" (ctx={N_CTX}, gpu_layers={n_gpu_layers})\n")
    sys.stderr.flush()
    llm = Llama(
        model_path=MODEL_PATH,
        n_ctx=N_CTX,
        n_gpu_layers=n_gpu_layers,
        n_threads=N_THREADS,
        verbose=False,
    )
    client = instructor.patch(
        create=llm.create_chat_completion_openai_v1,
        mode=instructor.Mode.JSON_SCHEMA,
    )
    return llm, client


def send(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


# ── Dynamic Pydantic model builder ───────────────────────────────────────────

def _build_model(schema: dict) -> type[BaseModel]:
    """Build a Pydantic model class from a JSON Schema dict at runtime."""
    from pydantic import create_model
    import pydantic

    def _annotation(prop: dict) -> type:
        t = prop.get("type")
        if t == "string":
            return str
        if t == "integer":
            return int
        if t == "number":
            return float
        if t == "boolean":
            return bool
        if t == "array":
            inner = _annotation(prop.get("items", {}))
            return list[inner]  # type: ignore[valid-type]
        # object or unknown → dict
        return dict

    props   = schema.get("properties", {})
    required = set(schema.get("required", list(props.keys())))
    fields: dict = {}
    for name, prop in props.items():
        ann = _annotation(prop)
        if name in required:
            fields[name] = (ann, ...)
        else:
            fields[name] = (Optional[ann], None)

    return create_model("DynamicModel", **fields)


# ── Request handlers ──────────────────────────────────────────────────────────

def handle_generate(client: instructor.Instructor, req: dict) -> None:
    """Stream a plain text response as tokens using a single-field model."""
    req_id     = req["id"]
    prompt     = req.get("prompt", "")
    max_tokens = int(req.get("max_tokens", 512))
    system     = req.get("system", DEFAULT_SYSTEM)

    class Response(BaseModel):
        response: str

    # Partial streaming: each partial object arrives with the `response` field
    # filled in progressively. We diff successive values to emit tokens.
    prev = ""
    for partial in client.chat.completions.create_partial(
        response_model=Response,
        messages=[
            {"role": "system", "content": system},
            {"role": "user",   "content": prompt},
        ],
        max_tokens=max_tokens,
    ):
        current = partial.response or ""
        if current and current != prev:
            token = current[len(prev):]
            send({"id": req_id, "token": token})
            prev = current

    send({"id": req_id, "done": True})


def handle_generate_structured(client: instructor.Instructor, req: dict) -> None:
    """Return a single structured JSON result constrained to the caller's schema."""
    req_id     = req["id"]
    prompt     = req.get("prompt", "")
    max_tokens = int(req.get("max_tokens", 1024))
    schema     = req.get("response_format", {}).get("schema", {})
    system     = req.get("system", DEFAULT_SYSTEM)

    try:
        model_cls = _build_model(schema)
    except Exception as e:
        send({"id": req_id, "error": "schema_validation_failed",
              "reason": f"could not build model from schema: {e}"})
        return

    try:
        result = client.chat.completions.create(
            response_model=model_cls,
            messages=[
                {"role": "system", "content": system},
                {"role": "user",   "content": prompt},
            ],
            max_tokens=max_tokens,
        )
        send({"id": req_id, "result": result.model_dump_json(), "done": True})
    except Exception as e:
        send({"id": req_id, "error": "schema_validation_failed", "reason": str(e)})


def main() -> None:
    _llm, client = load_model()
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
                handle_generate(client, req)
            elif req_type == "generate_structured":
                handle_generate_structured(client, req)
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
