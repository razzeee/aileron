#!/usr/bin/env python3
"""
Aileron stub container — no ML, instant responses.

Implements the full aileron container stdio protocol for end-to-end testing
of the daemon, portal, and client without any model or GPU.

Behaviour per request type:
  generate            – streams the prompt back as tokens, word by word
  generate_structured – returns a minimal valid JSON object matching the schema
  embed               – returns a fixed embedding vector
  transcribe          – returns a fixed transcript/translation string
  describe            – returns a fixed image description string
  ocr                 – returns a fixed extracted-text string
  segment             – returns a fixed normalized bounding box
"""

import json
import sys
import time


def send(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def handle_generate(req: dict) -> None:
    req_id     = req["id"]
    prompt     = req.get("prompt", "(empty prompt)")
    # Echo the prompt back word by word to exercise the streaming path.
    words = prompt.split()
    for i, word in enumerate(words[:32]):          # cap at 32 tokens
        is_last = (i == len(words) - 1) or (i == 31)
        chunk = {"id": req_id, "token": word + (" " if not is_last else "")}
        if is_last:
            chunk["done"] = True
        send(chunk)
        time.sleep(0.02)                            # simulate inference latency
    if not words:
        send({"id": req_id, "token": "(stub: no prompt provided)", "done": True})


def handle_generate_structured(req: dict) -> None:
    req_id = req["id"]
    schema = req.get("response_format", {}).get("schema", {})

    # Build the simplest possible object that satisfies the schema's
    # required properties and their declared types.
    result = _stub_object(schema)
    result_str = json.dumps(result)
    send({"id": req_id, "result": result_str, "done": True})


def _stub_object(schema: dict) -> object:
    """Return a minimal value that matches a JSON Schema node."""
    t = schema.get("type")
    if t == "object":
        obj = {}
        props = schema.get("properties", {})
        required = schema.get("required", list(props.keys()))
        for key in required:
            prop_schema = props.get(key, {})
            obj[key] = _stub_object(prop_schema)
        return obj
    elif t == "array":
        items_schema = schema.get("items", {})
        return [_stub_object(items_schema)]
    elif t == "string":
        if "enum" in schema:
            return schema["enum"][0]
        return "stub"
    elif t == "integer":
        minimum = schema.get("minimum", 0)
        return int(minimum)
    elif t == "number":
        minimum = schema.get("minimum", 0.0)
        return float(minimum)
    elif t == "boolean":
        return True
    elif t == "null":
        return None
    else:
        return "stub"


def handle_transcribe(req: dict) -> None:
    req_id = req["id"]
    language_hint = req.get("language_hint", "")
    task = req.get("task", "transcribe")
    verb = "translation" if task == "translate" else "transcription"
    suffix = f" Language hint: {language_hint}." if language_hint else ""
    send({"id": req_id, "token": f"Stub {verb}: audio received.{suffix}", "done": True})


def handle_embed(req: dict) -> None:
    req_id = req["id"]
    # Deterministic fixed-size stub embedding vector.
    send({"id": req_id, "embedding": [0.0, 0.1, 0.2, 0.3], "done": True})


def handle_describe(req: dict) -> None:
    req_id = req["id"]
    send({"id": req_id, "token": "Stub description: an image was received.", "done": True})


def handle_ocr(req: dict) -> None:
    req_id = req["id"]
    send({"id": req_id, "token": "Stub OCR: extracted text from image.", "done": True})


def handle_segment(req: dict) -> None:
    req_id = req["id"]
    result = {
        "segments": [
            {
                "label": "stub object",
                "confidence": 1.0,
                "x": 0.1,
                "y": 0.1,
                "width": 0.8,
                "height": 0.8,
            }
        ]
    }
    send({"id": req_id, "result": json.dumps(result), "done": True})


def main() -> None:
    sys.stderr.write("[aileron-stub] ready\n")
    sys.stderr.flush()

    for raw_line in sys.stdin:
        line = raw_line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError as e:
            sys.stderr.write(f"[aileron-stub] bad JSON: {e}\n")
            sys.stderr.flush()
            continue

        req_type = req.get("type", "")
        req_id   = req.get("id", "unknown")
        try:
            if req_type == "generate":
                handle_generate(req)
            elif req_type == "generate_structured":
                handle_generate_structured(req)
            elif req_type == "embed":
                handle_embed(req)
            elif req_type == "transcribe":
                handle_transcribe(req)
            elif req_type == "describe":
                handle_describe(req)
            elif req_type == "ocr":
                handle_ocr(req)
            elif req_type == "segment":
                handle_segment(req)
            else:
                send({"id": req_id, "error": "unsupported_type", "reason": req_type})
        except Exception as e:
            sys.stderr.write(f"[aileron-stub] error handling {req_type}: {e}\n")
            sys.stderr.flush()
            send({"id": req_id, "error": "internal_error", "done": True})


if __name__ == "__main__":
    main()
