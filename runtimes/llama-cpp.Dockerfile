ARG BUILDER_IMAGE="rust:1-bookworm"
ARG FINAL_IMAGE="debian:bookworm-slim"
FROM ${BUILDER_IMAGE} AS builder

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG RUNTIME_ID
ARG RUNTIME_VARIANT="cpu"
ARG RUNTIME_DESCRIPTION="Aileron llama.cpp runtime for local inference."
ARG RUNTIME_BIN=""
ARG RUNTIME_FEATURES=""
ARG APT_PACKAGES=""
ARG CMAKE_ARGS=""
ARG CUDA_DOCKER_ARCH=""
ARG FORCE_CMAKE=""
ARG LDFLAGS=""
ARG ROCM_PATH="/opt/rocm"
ARG VULKAN_HEADERS_TAG="v1.4.354"

ENV CMAKE_ARGS="${CMAKE_ARGS}"
ENV CUDA_DOCKER_ARCH="${CUDA_DOCKER_ARCH}"
ENV FORCE_CMAKE="${FORCE_CMAKE}"
ENV LDFLAGS="${LDFLAGS}"
ENV ROCM_PATH="${ROCM_PATH}"
ENV PATH="${ROCM_PATH}/bin:${PATH}"

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl build-essential cmake clang libclang-dev pkg-config git ninja-build ${APT_PACKAGES} \
    && rm -rf /var/lib/apt/lists/* \
    && case " ${APT_PACKAGES} ${CMAKE_ARGS} " in \
    *[Vv][Uu][Ll][Kk][Aa][Nn]* ) \
    git clone --depth 1 --branch "${VULKAN_HEADERS_TAG}" https://github.com/KhronosGroup/Vulkan-Headers.git /tmp/vulkan-headers \
    && cp -r /tmp/vulkan-headers/include/* /usr/include/ \
    && rm -rf /tmp/vulkan-headers ;; \
    * ) ;; \
    esac \
    && if ! command -v cargo >/dev/null 2>&1; then \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal; \
    fi \
    && if [ -f /usr/local/cuda/lib64/stubs/libcuda.so ] && [ ! -e /usr/local/cuda/lib64/stubs/libcuda.so.1 ]; then \
    ln -s libcuda.so /usr/local/cuda/lib64/stubs/libcuda.so.1; \
    fi

ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN set -eux; \
    case "$RUNTIME_ID" in \
    llm-llama-cpp) bin="aileron-runtime-llm-llama-cpp" ;; \
    vision-llama-cpp-gemma4) bin="aileron-runtime-vision-llama-cpp" ;; \
    *) echo "unsupported RUNTIME_ID=$RUNTIME_ID" >&2; exit 1 ;; \
    esac; \
    if [ -n "$RUNTIME_BIN" ]; then bin="$RUNTIME_BIN"; fi; \
    case "$RUNTIME_ID:$RUNTIME_VARIANT" in \
    llm-llama-cpp:cpu) features="llama" ;; \
    llm-llama-cpp:cuda) features="llama-cuda" ;; \
    llm-llama-cpp:rocm) features="llama-rocm" ;; \
    llm-llama-cpp:vulkan) features="llama-vulkan" ;; \
    vision-llama-cpp-gemma4:cpu) features="vision" ;; \
    vision-llama-cpp-gemma4:cuda) features="vision-cuda" ;; \
    vision-llama-cpp-gemma4:rocm) features="vision-rocm" ;; \
    vision-llama-cpp-gemma4:vulkan) features="vision-vulkan" ;; \
    *) echo "unsupported RUNTIME_VARIANT=$RUNTIME_VARIANT" >&2; exit 1 ;; \
    esac; \
    if [ -n "$RUNTIME_FEATURES" ]; then features="$RUNTIME_FEATURES"; fi; \
    if [ "$RUNTIME_VARIANT" = "rocm" ]; then \
    cargo build --locked --release -p aileron-runtime --lib --no-default-features --features "$features"; \
    cargo rustc --locked --release -p aileron-runtime --bin "$bin" --no-default-features --features "$features" -- -C link-arg=-no-pie; \
    else \
    cargo build --locked --release -p aileron-runtime --bin "$bin" --no-default-features --features "$features"; \
    fi; \
    cp "target/release/$bin" /entrypoint

FROM ${FINAL_IMAGE}

ARG RUNTIME_ID
ARG RUNTIME_VARIANT="cpu"
ARG RUNTIME_DESCRIPTION="Aileron llama.cpp runtime for local inference."
ARG RUNTIME_APT_PACKAGES="libgomp1 libstdc++6 libgcc-s1 ca-certificates"
ARG ROCM_PATH="/opt/rocm"

ENV ROCM_PATH="${ROCM_PATH}"
ENV PATH="${ROCM_PATH}/bin:${PATH}"

RUN apt-get update && apt-get install -y --no-install-recommends ${RUNTIME_APT_PACKAGES} \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /entrypoint /entrypoint
RUN chmod 0755 /entrypoint

LABEL org.aileron.runtime="true" \
    org.aileron.runtime_id="${RUNTIME_ID}" \
    org.aileron.variant="${RUNTIME_VARIANT}" \
    org.opencontainers.image.description="${RUNTIME_DESCRIPTION}" \
    org.opencontainers.image.licenses="GPL-3.0-or-later"

ENTRYPOINT ["/entrypoint"]
