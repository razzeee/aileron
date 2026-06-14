FROM python:3.13-slim
COPY entrypoint.py /entrypoint.py
LABEL org.aileron.runtime="true" \
      org.aileron.runtime_id="stub" \
      org.aileron.variant="cpu" \
      org.opencontainers.image.description="Aileron stub runtime for development and protocol testing." \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
ENTRYPOINT ["python", "/entrypoint.py"]
