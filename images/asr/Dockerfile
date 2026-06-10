FROM python:3.12-slim

# CPU-only whisper.cpp build.
# For GPU acceleration use Dockerfile.cuda.

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir "pywhispercpp>=1.2.0"

COPY download_model.py /download_model.py

ARG MODEL_SIZE=base
ENV MODEL_SIZE=${MODEL_SIZE}
RUN python /download_model.py "$MODEL_SIZE"

COPY entrypoint.py /entrypoint.py

LABEL aileron.use_cases="asr.transcribe"

ENTRYPOINT ["python", "/entrypoint.py"]
