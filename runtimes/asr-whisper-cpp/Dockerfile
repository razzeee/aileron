FROM python:3.12-slim

# CPU-only whisper.cpp build.
# For GPU acceleration use Dockerfile.cuda.

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir "pywhispercpp>=1.2.0"

COPY entrypoint.py /entrypoint.py

LABEL org.aileron.runtime="true" \
      org.aileron.runtime_id="asr-whisper-cpp" \
      org.aileron.variant="cpu"

ENTRYPOINT ["python", "/entrypoint.py"]
