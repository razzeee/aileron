ARG PYTHON_IMAGE="python:3.12-slim-bookworm"
FROM ${PYTHON_IMAGE} AS python-deps

ENV PIP_DISABLE_PIP_VERSION_CHECK=1
RUN set -eux; \
    pip install --no-cache-dir --prefix=/install --index-url https://download.pytorch.org/whl/cpu \
      torch==2.7.1; \
    PYTHONPATH="$(python -c 'import sysconfig; print(sysconfig.get_path("purelib", vars={"base":"/install", "platbase":"/install"}))')" \
      pip install --no-cache-dir --prefix=/install \
      transformers==4.53.2 \
      safetensors==0.5.3 \
      numpy==2.3.1 \
      phonemizer==3.3.0 \
      pycountry==26.2.16 \
      uroman==1.3.1.1

FROM ${PYTHON_IMAGE}
ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    HF_HUB_OFFLINE=1 \
    TRANSFORMERS_OFFLINE=1 \
    XDG_CACHE_HOME=/tmp

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends espeak-ng; \
    rm -rf /var/lib/apt/lists/*

COPY --from=python-deps /install /usr/local
WORKDIR /app
COPY runtimes/tts-vits/tts_vits ./tts_vits
COPY runtimes/tts-vits/entrypoint.sh /entrypoint
RUN chmod 0755 /entrypoint

LABEL org.aileron.runtime="true" \
      org.aileron.runtime_id="tts-vits" \
      org.aileron.variant="cpu" \
      org.opencontainers.image.description="Aileron offline CPU VITS text-to-speech runtime." \
      org.opencontainers.image.licenses="GPL-3.0-or-later"

ENTRYPOINT ["/entrypoint"]
