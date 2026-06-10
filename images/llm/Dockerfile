FROM python:3.12-slim

# CPU-only build of llama-cpp-python.
# For GPU acceleration use Dockerfile.cuda, Dockerfile.rocm, or Dockerfile.vulkan.

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir \
    "llama-cpp-python>=0.2.90" \
    jsonschema

COPY download_model.py /download_model.py

ARG MODEL_URL=""
RUN if [ -n "$MODEL_URL" ]; then \
        python /download_model.py "$MODEL_URL"; \
    else \
        mkdir -p /model; \
    fi

COPY entrypoint.py /entrypoint.py

# Declare which use-cases this image supports.
# The aileron daemon reads this label at pull time and auto-assigns unoccupied
# use-cases, prompting the user to resolve any conflicts.
LABEL aileron.use_cases="llm.summarize,llm.translate,llm.rephrase,llm.classify,llm.extract"

ENTRYPOINT ["python", "/entrypoint.py"]
