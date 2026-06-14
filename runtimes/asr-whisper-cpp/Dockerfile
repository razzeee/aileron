ARG PYTHON_IMAGE="python:3.13-slim"
FROM ${PYTHON_IMAGE} AS builder

# CPU-only whisper.cpp build.
# For GPU acceleration use Dockerfile.cuda.

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake \
    && rm -rf /var/lib/apt/lists/*

RUN python -m pip install --root=/runtime --no-cache-dir "pywhispercpp>=1.2.0"

FROM ${PYTHON_IMAGE}

RUN apt-get update && apt-get install -y --no-install-recommends \
        libgomp1 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /runtime/usr /usr

COPY entrypoint.py /entrypoint.py

LABEL org.aileron.runtime="true" \
      org.aileron.runtime_id="asr-whisper-cpp" \
      org.aileron.variant="cpu" \
      org.opencontainers.image.description="Aileron Whisper.cpp runtime for local speech transcription." \
      org.opencontainers.image.licenses="GPL-3.0-or-later"

ENTRYPOINT ["python", "/entrypoint.py"]
