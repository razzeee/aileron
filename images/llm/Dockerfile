FROM python:3.12-slim

# llama-cpp-python with CPU inference. Override build args for GPU variants:
#   --build-arg CMAKE_ARGS="-DLLAMA_CUDA=on" for NVIDIA
#   --build-arg CMAKE_ARGS="-DLLAMA_VULKAN=on" for Vulkan
ARG CMAKE_ARGS=""
ENV CMAKE_ARGS=${CMAKE_ARGS}

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir \
    "llama-cpp-python>=0.2.90" \
    jsonschema

# The model file is baked into the image at /model/model.gguf.
# Build this image with:
#   docker build --build-arg MODEL_URL=<url> -t aileron/llama3.2-3b-instruct .
# or copy a local GGUF:
#   COPY model.gguf /model/model.gguf
ARG MODEL_URL=""
RUN if [ -n "$MODEL_URL" ]; then \
        pip install --no-cache-dir huggingface_hub && \
        python -c "from huggingface_hub import hf_hub_download; \
                   import urllib.parse, os; \
                   url='${MODEL_URL}'; \
                   os.makedirs('/model', exist_ok=True); \
                   open('/model/model.gguf','wb').write(open(hf_hub_download(*url.split('/')[3:5], filename=url.split('/')[-1]),'rb').read())"; \
    else \
        mkdir -p /model; \
    fi

COPY entrypoint.py /entrypoint.py

ENTRYPOINT ["python", "/entrypoint.py"]
