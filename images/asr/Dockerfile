FROM python:3.12-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        cmake \
        curl \
    && rm -rf /var/lib/apt/lists/*

# pywhispercpp wraps the actual whisper.cpp C++ library.
RUN pip install --no-cache-dir "pywhispercpp>=1.2.0"

# Whisper model size: tiny, base, small, medium, large
# The model is downloaded at build time and baked into the image.
# Override MODEL_SIZE to choose a different model.
ARG MODEL_SIZE=base
ENV MODEL_SIZE=${MODEL_SIZE}

# Download the model weights at build time so the container starts instantly.
RUN python - <<'EOF'
import os, sys
from pywhispercpp.model import Model
os.makedirs("/model", exist_ok=True)
print(f"Downloading whisper.cpp model: {os.environ['MODEL_SIZE']}", flush=True)
Model(os.environ["MODEL_SIZE"], models_dir="/model")
print("Done.", flush=True)
EOF

COPY entrypoint.py /entrypoint.py

ENTRYPOINT ["python", "/entrypoint.py"]
