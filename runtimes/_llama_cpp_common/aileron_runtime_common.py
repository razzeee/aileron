import json
import os
import shutil
import subprocess
import sys
import traceback
from collections.abc import Callable

from llama_cpp import Llama


def detect_gpu_layers(log_prefix: str, n_threads: int) -> int:
    explicit = os.environ.get("N_GPU_LAYERS")
    if explicit is not None:
        layers = int(explicit)
        sys.stderr.write(f"[{log_prefix}] N_GPU_LAYERS={layers} (explicit)\n")
        return layers

    if shutil.which("nvidia-smi"):
        try:
            out = subprocess.check_output(
                ["nvidia-smi", "--query-gpu=name", "--format=csv,noheader"],
                stderr=subprocess.DEVNULL,
            ).decode().strip()
            if out:
                sys.stderr.write(
                    f"[{log_prefix}] CUDA GPU detected: {out.splitlines()[0]} - offloading all layers\n"
                )
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
                sys.stderr.write(f"[{log_prefix}] ROCm GPU detected - offloading all layers\n")
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
                            f"[{log_prefix}] Vulkan device detected: {name} - offloading all layers\n"
                        )
                        break
                return -1
        except Exception:
            pass

    sys.stderr.write(f"[{log_prefix}] no GPU detected - using CPU ({n_threads} threads)\n")
    return 0


def load_llama(
    *,
    log_prefix: str,
    model_path: str,
    n_ctx: int,
    n_threads: int,
    loading_suffix: str = "",
    **kwargs,
) -> Llama:
    n_gpu_layers = detect_gpu_layers(log_prefix, n_threads)
    suffix = f" {loading_suffix}" if loading_suffix else ""
    sys.stderr.write(
        f"[{log_prefix}] loading {model_path}{suffix} "
        f"(ctx={n_ctx}, gpu_layers={n_gpu_layers})\n"
    )
    sys.stderr.flush()
    return Llama(
        model_path=model_path,
        n_ctx=n_ctx,
        n_gpu_layers=n_gpu_layers,
        n_threads=n_threads,
        verbose=False,
        **kwargs,
    )


def send(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def serve_requests(
    *,
    llm: Llama,
    handlers: dict[str, Callable[[Llama, dict], None]],
    log_prefix: str,
    unsupported_done: bool,
) -> None:
    sys.stderr.write(f"[{log_prefix}] ready\n")
    sys.stderr.flush()

    for raw_line in sys.stdin:
        line = raw_line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError as e:
            sys.stderr.write(f"[{log_prefix}] bad request JSON: {e}\n")
            sys.stderr.flush()
            continue

        req_type = req.get("type", "")
        req_id = req.get("id", "unknown")
        try:
            handler = handlers.get(req_type)
            if handler is None:
                response = {"id": req_id, "error": "unsupported_type", "reason": req_type}
                if unsupported_done:
                    response["done"] = True
                send(response)
                continue
            handler(llm, req)
        except Exception:
            reason = traceback.format_exc()
            sys.stderr.write(reason)
            sys.stderr.flush()
            send({"id": req_id, "error": "internal_error", "reason": reason, "done": True})
