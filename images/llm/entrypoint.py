#!/usr/bin/env python3
"""
Aileron LLM container entrypoint.

Reads newline-delimited JSON requests from stdin, writes responses to stdout.

Supported request types:
  generate            – stream tokens
  generate_structured – return a single JSON result constrained to a schema
"""

import json
import os
import sys
import traceback
from typing import Any

from jsonschema import validate as jsonschema_validate, ValidationError
from llama_cpp import Llama

MODEL_PATH = os.environ.get("MODEL_PATH", "/model/model.gguf")
N_CTX      = int(os.environ.get("N_CTX", "4096"))
N_GPU_LAYERS = int(os.environ.get("N_GPU_LAYERS", "0"))

def load_model() -> Llama:
    sys.stderr.write(f"[aileron-llm] loading {MODEL_PATH}\n")
    sys.stderr.flush()
    return Llama(
        model_path=MODEL_PATH,
        n_ctx=N_CTX,
        n_gpu_layers=N_GPU_LAYERS,
        verbose=False,
    )


def send(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def handle_generate(llm: Llama, req: dict) -> None:
    req_id     = req["id"]
    prompt     = req.get("prompt", "")
    max_tokens = int(req.get("max_tokens", 512))

    for chunk in llm(
        prompt,
        max_tokens=max_tokens,
        stream=True,
        echo=False,
    ):
        token = chunk["choices"][0]["text"]
        send({"id": req_id, "token": token})

    send({"id": req_id, "done": True})


def handle_generate_structured(llm: Llama, req: dict) -> None:
    req_id     = req["id"]
    prompt     = req.get("prompt", "")
    max_tokens = int(req.get("max_tokens", 1024))
    schema     = req.get("response_format", {}).get("schema", {})

    # Build a grammar string from the JSON Schema so llama.cpp constrains
    # sampling to valid JSON matching the schema.
    # llama-cpp-python >= 0.2.56 supports json_schema in create_chat_completion.
    # We use the lower-level __call__ with grammar= for the raw completion API.
    try:
        from llama_cpp import LlamaGrammar
        grammar = LlamaGrammar.from_json_schema(json.dumps(schema))
    except Exception:
        # Fall back to free-form JSON grammar if schema grammar fails.
        grammar = LlamaGrammar.from_string('root ::= value\n', verbose=False)

    result_text = llm(
        prompt,
        max_tokens=max_tokens,
        grammar=grammar,
        stream=False,
        echo=False,
    )["choices"][0]["text"].strip()

    # Validate before sending.
    try:
        parsed = json.loads(result_text)
        if schema:
            jsonschema_validate(instance=parsed, schema=schema)
    except (json.JSONDecodeError, ValidationError) as e:
        # Send a structured error the daemon can surface as SchemaValidationFailed.
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
