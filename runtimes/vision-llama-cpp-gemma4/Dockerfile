FROM python:3.12-slim

# CPU-only llama.cpp multimodal image.
# Build with a GGUF vision-language model and matching mmproj file.

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake git ninja-build \
    && rm -rf /var/lib/apt/lists/*

ARG LLAMA_CPP_PYTHON_REF="b5eefc82e0fd415d5547c81367c29b159c0268d3"
RUN pip install --no-cache-dir "git+https://github.com/abetlen/llama-cpp-python.git@${LLAMA_CPP_PYTHON_REF}"

COPY entrypoint.py /entrypoint.py

LABEL aileron.runtime_id="vision-llama-cpp-gemma4"

ENTRYPOINT ["python", "/entrypoint.py"]
