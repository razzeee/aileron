ARG BASE_IMAGE="python:3.13-slim"
ARG RUNTIME_ID="llm-llamafile"
ARG RUNTIME_VARIANT="cpu"
ARG RUNTIME_STAGE="cpu"
ARG RUNTIME_DESCRIPTION="Aileron llamafile runtime for local inference."
ARG LLAMAFILE_URL="https://github.com/mozilla-ai/llamafile/releases/download/0.10.3/llamafile-0.10.3"
ARG LLAMAFILE_SHA256="e6d4041a82ca37cee15aab62e6826d7a61c6a3ea83bca68387958970df250883"
ARG LLAMAFILE_SOURCE_REF="0.10.3"
ARG RUNTIME_APT_PACKAGES="libgomp1"

FROM ${BASE_IMAGE} AS cpu

ARG RUNTIME_ID
ARG RUNTIME_VARIANT
ARG RUNTIME_DESCRIPTION
ARG LLAMAFILE_URL
ARG LLAMAFILE_SHA256
ARG RUNTIME_APT_PACKAGES

ENV PIP_BREAK_SYSTEM_PACKAGES=1

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl ${RUNTIME_APT_PACKAGES} \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL "${LLAMAFILE_URL}" -o /usr/local/bin/llamafile \
    && echo "${LLAMAFILE_SHA256}  /usr/local/bin/llamafile" | sha256sum -c - \
    && chmod 0755 /usr/local/bin/llamafile \
    && python -m pip install --no-cache-dir jsonschema

COPY runtimes/llm-llamafile/entrypoint.py /entrypoint.py

LABEL org.aileron.runtime="true" \
      org.aileron.runtime_id="${RUNTIME_ID}" \
      org.aileron.variant="${RUNTIME_VARIANT}" \
      org.opencontainers.image.description="${RUNTIME_DESCRIPTION}" \
      org.opencontainers.image.licenses="Apache-2.0"

ENTRYPOINT ["python", "/entrypoint.py"]

FROM fedora:latest AS gpu-builder

ARG LLAMAFILE_SOURCE_REF

RUN dnf install -y \
        cmake \
        curl \
        gcc \
        gcc-c++ \
        glslc \
        git \
        make \
        patch \
        pkgconf-pkg-config \
        spirv-headers-devel \
        vulkan-headers \
        vulkan-loader-devel \
    && mkdir -p /out \
    && git clone --depth 1 --branch "${LLAMAFILE_SOURCE_REF}" --recurse-submodules --shallow-submodules https://github.com/mozilla-ai/llamafile.git /src \
    && /src/llama.cpp.patches/apply-patches.sh \
    && python3 - <<'PY'
from pathlib import Path

path = Path("/src/llama.cpp/tools/server/server.cpp")
text = path.read_text()
old = "int main(int argc, char ** argv) {\n#ifdef COSMOCC"
new = "int llama_server(int argc, char ** argv) {\n#ifdef COSMOCC"
if old not in text:
    raise SystemExit("expected standalone server main not found")
path.write_text(text.replace(old, new, 1))
PY
RUN cmake -S /src/llama.cpp -B /build \
        -DCMAKE_BUILD_TYPE=Release \
        -DGGML_VULKAN=ON \
        -DGGML_LLAMAFILE=OFF \
        -DGGML_NATIVE=OFF \
        -DBUILD_SHARED_LIBS=OFF \
        -DLLAMA_BUILD_TESTS=OFF \
        -DLLAMA_BUILD_EXAMPLES=OFF \
        -DLLAMA_BUILD_SERVER=ON \
    && cmake --build /build --target llama-server -j"$(nproc)" \
    && install -m 0755 /build/bin/llama-server /out/llama-server

FROM fedora:latest AS gpu

ARG RUNTIME_ID
ARG RUNTIME_VARIANT
ARG RUNTIME_DESCRIPTION

RUN dnf install -y \
        libgomp \
        mesa-vulkan-drivers \
        python3 \
        python3-jsonschema \
        vulkan-loader \
        vulkan-tools \
    && ln -s /usr/bin/python3 /usr/local/bin/python \
    && dnf clean all

COPY --from=gpu-builder /out/llama-server /usr/local/bin/llama-server
COPY runtimes/llm-llamafile/entrypoint.py /entrypoint.py

ENV LLAMAFILE_PATH=/usr/local/bin/llama-server \
    LLAMAFILE_RUNNER= \
    LLAMAFILE_SERVER_KIND=llama-server

LABEL org.aileron.runtime="true" \
      org.aileron.runtime_id="${RUNTIME_ID}" \
      org.aileron.variant="${RUNTIME_VARIANT}" \
      org.opencontainers.image.description="${RUNTIME_DESCRIPTION}" \
      org.opencontainers.image.licenses="Apache-2.0"

ENTRYPOINT ["python3", "/entrypoint.py"]

FROM ${RUNTIME_STAGE}
