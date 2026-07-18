ARG PYTHON_IMAGE="python:3.12-slim-bookworm"
FROM ${PYTHON_IMAGE}

ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    HF_HUB_OFFLINE=1 \
    TRANSFORMERS_OFFLINE=1 \
    XDG_CACHE_HOME=/tmp

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates build-essential libgl1 libglib2.0-0; \
    rm -rf /var/lib/apt/lists/*; \
    pip install --no-cache-dir \
      numpy \
      pillow \
      depth-anything-3 \
      opencv-python-headless \
      onnxruntime \
      sam2 \
      torch \
      torchvision \
      transformers \
      ultralytics

WORKDIR /app
COPY runtimes/vision-foundation/vision_foundation ./vision_foundation
COPY runtimes/vision-foundation/entrypoint.sh /entrypoint
RUN chmod 0755 /entrypoint

LABEL org.aileron.runtime="true" \
      org.aileron.runtime_id="vision-foundation" \
      org.aileron.variant="cpu" \
      org.opencontainers.image.description="Aileron Torch-based vision foundation runtime for detection, segmentation, and depth." \
      org.opencontainers.image.licenses="GPL-3.0-or-later"

ENTRYPOINT ["/entrypoint"]
