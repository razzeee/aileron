FROM python:3.12-slim

# CPU-only build of llama-cpp-python.
# For GPU acceleration use Dockerfile.cuda, Dockerfile.rocm, or Dockerfile.vulkan.

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake ninja-build \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir \
    "llama-cpp-python>=0.2.90" \
    jsonschema

COPY entrypoint.py /entrypoint.py

LABEL aileron.runtime_id="llm-llama-cpp"

ENTRYPOINT ["python", "/entrypoint.py"]
