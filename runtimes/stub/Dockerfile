FROM python:3.12-slim
COPY entrypoint.py /entrypoint.py
LABEL org.aileron.runtime="true" \
      org.aileron.runtime_id="stub" \
      org.aileron.variant="cpu"
ENTRYPOINT ["python", "/entrypoint.py"]
