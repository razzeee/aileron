FROM python:3.12-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ffmpeg \
    && rm -rf /var/lib/apt/lists/*

# faster-whisper uses CTranslate2 under the hood — significantly faster than
# the original whisper.cpp Python bindings and supports int8 quantisation.
RUN pip install --no-cache-dir \
    "faster-whisper>=1.0.0"

# Whisper model size: tiny, base, small, medium, large-v3
# The model is downloaded at first run and cached in /model.
# To bake it in, override MODEL_SIZE and run:
#   docker build --build-arg MODEL_SIZE=small -t aileron/whisper-small .
ARG MODEL_SIZE="small"
ENV MODEL_SIZE=${MODEL_SIZE}

RUN python -c "
from faster_whisper import WhisperModel
import os
os.makedirs('/model', exist_ok=True)
WhisperModel('${MODEL_SIZE}', device='cpu', compute_type='int8',
             download_root='/model')
print('model cached')
"

COPY entrypoint.py /entrypoint.py

ENTRYPOINT ["python", "/entrypoint.py"]
