#!/usr/bin/env python3
"""Aileron runtime wrapper for llamafile's local HTTP server."""

import json
import os
import socket
import subprocess
import sys
import time
import traceback
from typing import Any
from collections.abc import Iterator
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

from jsonschema import ValidationError
from jsonschema import validate as jsonschema_validate


MODEL_PATH = os.environ.get("MODEL_PATH", "/model/model.gguf")
LLAMAFILE_PATH = os.environ.get("LLAMAFILE_PATH", "/usr/local/bin/llamafile")
LLAMAFILE_RUNNER = os.environ.get("LLAMAFILE_RUNNER", "/bin/sh")
LLAMAFILE_SERVER_KIND = os.environ.get("LLAMAFILE_SERVER_KIND", "llamafile").strip().lower()
HOST = os.environ.get("LLAMAFILE_HOST", "127.0.0.1")
PORT = int(os.environ.get("LLAMAFILE_PORT", "0"))
N_CTX = int(os.environ.get("N_CTX", "4096"))
N_THREADS = int(os.environ.get("N_THREADS", str(os.cpu_count() or 4)))
STARTUP_TIMEOUT = float(os.environ.get("LLAMAFILE_STARTUP_TIMEOUT", "120"))
REQUEST_TIMEOUT = float(os.environ.get("LLAMAFILE_REQUEST_TIMEOUT", "600"))
AILERON_DEVICE = os.environ.get("AILERON_DEVICE", "cpu").strip().lower() or "cpu"

DEFAULT_SYSTEM = (
    "You are a helpful assistant. "
    "Always respond in the same language as the user's message. "
    "Be concise and accurate."
)


class LlamafileRuntime:
    def __init__(self, base_url: str, process: subprocess.Popen | None = None) -> None:
        self.base_url = base_url.rstrip("/")
        self.process = process
        self.should_exit = False

    def request_json(
        self,
        path: str,
        payload: dict[str, Any] | None = None,
        *,
        timeout: float = REQUEST_TIMEOUT,
    ) -> dict[str, Any]:
        data = None if payload is None else json.dumps(payload).encode("utf-8")
        request = Request(
            self.base_url + path,
            data=data,
            headers={"Content-Type": "application/json"},
            method="GET" if payload is None else "POST",
        )
        try:
            with urlopen(request, timeout=timeout) as response:
                body = response.read().decode("utf-8")
        except (HTTPError, URLError, TimeoutError) as exc:
            self.mark_failed_if_server_exited()
            raise RuntimeError(f"llamafile HTTP request failed: {exc}") from exc
        try:
            return json.loads(body)
        except json.JSONDecodeError as exc:
            raise RuntimeError(f"malformed llamafile JSON response: {body[:500]}") from exc

    def stream_json(self, path: str, payload: dict[str, Any]) -> Iterator[dict[str, Any]]:
        request = Request(
            self.base_url + path,
            data=json.dumps(payload).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urlopen(request, timeout=REQUEST_TIMEOUT) as response:
                for raw_line in response:
                    line = raw_line.decode("utf-8").strip()
                    if not line or not line.startswith("data:"):
                        continue
                    data = line.removeprefix("data:").strip()
                    if data == "[DONE]":
                        break
                    yield json.loads(data)
        except (HTTPError, URLError, TimeoutError, json.JSONDecodeError) as exc:
            self.mark_failed_if_server_exited()
            raise RuntimeError(f"llamafile streaming request failed: {exc}") from exc

    def mark_failed_if_server_exited(self) -> None:
        if self.process is not None and self.process.poll() is not None:
            self.should_exit = True

    def health_ready(self) -> bool:
        request = Request(self.base_url + "/health", method="GET")
        try:
            with urlopen(request, timeout=1) as response:
                return 200 <= response.status < 500
        except Exception:
            self.mark_failed_if_server_exited()
            return False


def send(obj: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def reserve_port() -> int:
    if PORT != 0:
        return PORT
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind((HOST, 0))
        return int(sock.getsockname()[1])


def accelerator_requested() -> bool:
    return AILERON_DEVICE in {"cuda", "rocm", "vulkan", "gpu"}


def gpu_layers() -> int:
    explicit = os.environ.get("N_GPU_LAYERS")
    if explicit is not None:
        return int(explicit)
    return -1 if accelerator_requested() else 0


def gpu_mode() -> str:
    explicit = os.environ.get("LLAMAFILE_GPU_MODE")
    if explicit is not None:
        return explicit
    if AILERON_DEVICE == "cuda":
        return "nvidia"
    if AILERON_DEVICE in {"rocm", "vulkan", "gpu"}:
        return "amd"
    return ""


def llamafile_command(port: int) -> list[str]:
    layers = gpu_layers()
    if accelerator_requested() and layers == 0:
        raise RuntimeError(
            f"AILERON_DEVICE={AILERON_DEVICE} selected an accelerator but N_GPU_LAYERS=0 disables offload"
        )
    if LLAMAFILE_SERVER_KIND == "llama-server":
        return llama_server_command(port, layers)
    if LLAMAFILE_SERVER_KIND != "llamafile":
        raise RuntimeError(f"unsupported LLAMAFILE_SERVER_KIND={LLAMAFILE_SERVER_KIND}")
    return portable_llamafile_command(port, layers)


def portable_llamafile_command(port: int, layers: int) -> list[str]:
    cmd = [] if LLAMAFILE_RUNNER == "" else [LLAMAFILE_RUNNER]
    cmd += [
        LLAMAFILE_PATH,
        "-m",
        MODEL_PATH,
        "--server",
        "--host",
        HOST,
        "--port",
        str(port),
        "-c",
        str(N_CTX),
        "--threads",
        str(N_THREADS),
        "-ngl",
        str(layers),
    ]
    mode = gpu_mode()
    if accelerator_requested() and mode:
        cmd.extend(["--gpu", mode])
    return cmd


def llama_server_command(port: int, layers: int) -> list[str]:
    cmd = [] if LLAMAFILE_RUNNER == "" else [LLAMAFILE_RUNNER]
    cmd += [
        LLAMAFILE_PATH,
        "--model",
        MODEL_PATH,
        "--host",
        HOST,
        "--port",
        str(port),
        "--ctx-size",
        str(N_CTX),
        "--threads",
        str(N_THREADS),
        "--n-gpu-layers",
        str(layers),
    ]
    return cmd


def start_llamafile() -> LlamafileRuntime:
    port = reserve_port()
    cmd = llamafile_command(port)
    sys.stderr.write(
        "[aileron-llamafile] starting device="
        f"{AILERON_DEVICE} gpu_layers={gpu_layers()} command={cmd}\n"
    )
    sys.stderr.flush()
    process = subprocess.Popen(cmd, stdout=sys.stderr, stderr=sys.stderr)
    runtime = LlamafileRuntime(f"http://{HOST}:{port}", process)

    deadline = time.monotonic() + STARTUP_TIMEOUT
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"llamafile exited during startup with code {process.returncode}")
        try:
            if runtime.health_ready():
                return runtime
        except Exception:
            pass
        time.sleep(0.25)

    process.terminate()
    raise RuntimeError(
        "llamafile startup timed out "
        f"device={AILERON_DEVICE} gpu_layers={gpu_layers()} command={cmd}"
    )


def chat_payload(req: dict[str, Any], *, stream: bool) -> dict[str, Any]:
    system = req.get("system", DEFAULT_SYSTEM)
    return {
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": req.get("prompt", "")},
        ],
        "max_tokens": int(req.get("max_tokens", 512)),
        "stream": stream,
    }


def completion_payload(req: dict[str, Any], *, stream: bool = False) -> dict[str, Any]:
    return {
        "prompt": req.get("prompt", ""),
        "n_predict": int(req.get("max_tokens", 4)),
        "temperature": float(req.get("temperature", 0.0)),
        "stop": ["\n", "\t"],
        "stream": stream,
    }


def structured_payload(req: dict[str, Any]) -> dict[str, Any]:
    schema = req.get("response_format", {}).get("schema", {})
    system = req.get("system", DEFAULT_SYSTEM)
    prompt = req.get("prompt", "")
    tool_results = req.get("tool_results", [])
    if tool_results:
        rendered = []
        for result in tool_results:
            content = result.get("content_json") or result.get("content") or ""
            rendered.append(f"{result.get('id', 'tool')}: {content}")
        prompt = prompt + "\n\nTool results:\n" + "\n".join(rendered)
    return {
        "prompt": f"{system}\n\n{prompt}",
        "n_predict": int(req.get("max_tokens", 1024)),
        "temperature": float(req.get("temperature", 0.0)),
        "json_schema": schema,
        "stream": False,
    }


def response_text(response: dict[str, Any]) -> str:
    if "content" in response:
        return str(response.get("content") or "")
    choices = response.get("choices") or []
    if choices:
        choice = choices[0]
        if "text" in choice:
            return str(choice.get("text") or "")
        message = choice.get("message") or {}
        return str(message.get("content") or "")
    return ""


def stream_token(event: dict[str, Any]) -> str:
    choices = event.get("choices") or []
    if choices:
        choice = choices[0]
        delta = choice.get("delta") or {}
        return str(delta.get("content") or choice.get("text") or "")
    return str(event.get("content") or "")


def clean_inline_completion(prefix: str, raw: str) -> str:
    suffix_mode = bool(prefix) and (prefix[-1].isalnum() or prefix[-1] in "_-")
    text = raw.strip() if suffix_mode else raw.lstrip()
    out = []
    started = False
    for ch in text:
        is_word = ch.isalnum() or ch in "_-'"
        if is_word:
            started = True
            out.append(ch)
        elif suffix_mode and not started:
            continue
        else:
            break
    completion = "".join(out)
    if completion and not suffix_mode and not prefix.endswith((" ", "\n", "\t")):
        completion = " " + completion
    return completion[:20]


def completion_choices(runtime: LlamafileRuntime, req: dict[str, Any]) -> list[str]:
    prefix = req.get("prompt", "")
    choices = max(1, min(3, int(req.get("choices", 1))))
    completions = []
    temperatures = [
        float(req.get("temperature", 0.0)),
        max(float(req.get("temperature", 0.0)), 0.4),
        max(float(req.get("temperature", 0.0)), 0.8),
    ]
    for temperature in temperatures:
        payload = completion_payload({**req, "temperature": temperature})
        response = runtime.request_json("/completion", payload)
        completion = clean_inline_completion(prefix, response_text(response))
        if completion and completion not in completions:
            completions.append(completion)
        if len(completions) >= choices:
            break
    return completions


def normalize_embedding(value: Any) -> list[float]:
    if isinstance(value, dict) and "embedding" in value:
        value = value["embedding"]
    if isinstance(value, dict) and "data" in value:
        data = value.get("data") or []
        value = data[0].get("embedding", []) if data else []
    if not value:
        return []
    if isinstance(value[0], list):
        cols = len(value[0])
        means = [0.0] * cols
        for row in value:
            for i, item in enumerate(row):
                means[i] += float(item)
        return [item / len(value) for item in means]
    return [float(item) for item in value]


def handle_generate(runtime: LlamafileRuntime, req: dict[str, Any]) -> None:
    req_id = req["id"]
    emitted = False
    for event in runtime.stream_json("/v1/chat/completions", chat_payload(req, stream=True)):
        token = stream_token(event)
        if token:
            emitted = True
            send({"id": req_id, "token": token})
    if not emitted:
        response = runtime.request_json("/v1/chat/completions", chat_payload(req, stream=False))
        token = response_text(response)
        if token:
            send({"id": req_id, "token": token})
    send({"id": req_id, "done": True})


def handle_predict_next(runtime: LlamafileRuntime, req: dict[str, Any]) -> None:
    send({"id": req["id"], "completions": completion_choices(runtime, req), "done": True})


def validated_structured_result(runtime: LlamafileRuntime, req: dict[str, Any]) -> str:
    schema = req.get("response_format", {}).get("schema", {})
    response = runtime.request_json("/completion", structured_payload(req))
    result = response_text(response).strip()
    parsed = json.loads(result)
    if schema:
        jsonschema_validate(instance=parsed, schema=schema)
    return result


def handle_generate_structured(runtime: LlamafileRuntime, req: dict[str, Any]) -> None:
    try:
        result = validated_structured_result(runtime, req)
    except (json.JSONDecodeError, ValidationError) as exc:
        send({"id": req["id"], "error": "schema_validation_failed", "reason": str(exc), "done": True})
        return
    send({"id": req["id"], "result": result, "done": True})


def handle_generate_structured_stream(runtime: LlamafileRuntime, req: dict[str, Any]) -> None:
    try:
        result = validated_structured_result(runtime, req)
    except (json.JSONDecodeError, ValidationError) as exc:
        send({"id": req["id"], "error": "schema_validation_failed", "reason": str(exc), "done": True})
        return
    send({"id": req["id"], "snapshot": result})
    send({"id": req["id"], "snapshot": result, "done": True})


def handle_embed(runtime: LlamafileRuntime, req: dict[str, Any]) -> None:
    response = runtime.request_json("/embedding", {"content": req.get("prompt", "")})
    send({"id": req["id"], "embedding": normalize_embedding(response), "done": True})


HANDLERS = {
    "generate": handle_generate,
    "predict_next": handle_predict_next,
    "generate_structured": handle_generate_structured,
    "generate_structured_stream": handle_generate_structured_stream,
    "embed": handle_embed,
}


def serve_requests(runtime: LlamafileRuntime) -> None:
    sys.stderr.write("[aileron-llamafile] ready\n")
    sys.stderr.flush()
    for raw_line in sys.stdin:
        line = raw_line.strip()
        if not line:
            continue
        req = {"id": "unknown"}
        try:
            req = json.loads(line)
            req_id = req.get("id", "unknown")
            handler = HANDLERS.get(req.get("type", ""))
            if handler is None:
                send({"id": req_id, "error": "unsupported_type", "reason": req.get("type", ""), "done": True})
                continue
            handler(runtime, req)
        except Exception:
            reason = traceback.format_exc()
            sys.stderr.write(reason)
            sys.stderr.flush()
            send({"id": req.get("id", "unknown"), "error": "internal_error", "reason": reason, "done": True})
        if runtime.should_exit:
            raise SystemExit(1)


def main() -> None:
    runtime = start_llamafile()
    serve_requests(runtime)


if __name__ == "__main__":
    main()
