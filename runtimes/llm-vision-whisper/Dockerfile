ARG BUILDER_IMAGE="rust:1-bookworm"
ARG FINAL_IMAGE="debian:bookworm-slim"
FROM ${BUILDER_IMAGE} AS builder

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG RUNTIME_VARIANT="cpu"
ARG RUNTIME_FEATURES=""
ARG APT_PACKAGES=""
ARG CMAKE_ARGS=""
ARG CUDA_DOCKER_ARCH=""
ARG FORCE_CMAKE=""
ARG LDFLAGS=""
ARG ROCM_PATH="/opt/rocm"
ARG AMDGPU_TARGETS="gfx900;gfx906;gfx908;gfx90a;gfx942;gfx950;gfx1010;gfx1011;gfx1012;gfx1030;gfx1031;gfx1032;gfx1035;gfx1036;gfx1100;gfx1101;gfx1102;gfx1103;gfx1150;gfx1151;gfx1152;gfx1153;gfx1200;gfx1201"
ARG VULKAN_HEADERS_TAG="v1.4.354"

ENV CMAKE_ARGS="${CMAKE_ARGS}"
ENV CUDA_DOCKER_ARCH="${CUDA_DOCKER_ARCH}"
ENV FORCE_CMAKE="${FORCE_CMAKE}"
ENV LDFLAGS="${LDFLAGS}"
ENV ROCM_PATH="${ROCM_PATH}"
ENV HIP_PATH="${ROCM_PATH}"
ENV CMAKE_PREFIX_PATH="${ROCM_PATH}"
ENV LD_LIBRARY_PATH="${ROCM_PATH}/lib"
ENV AMDGPU_TARGETS="${AMDGPU_TARGETS}"
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
    case "$RUNTIME_VARIANT" in \
    cpu) features="vision whisper" ;; \
    cuda) features="vision-cuda whisper-cuda" ;; \
    rocm) features="vision-rocm whisper-rocm" ;; \
    vulkan) features="vision-vulkan whisper-vulkan" ;; \
    *) echo "unsupported RUNTIME_VARIANT=$RUNTIME_VARIANT" >&2; exit 1 ;; \
    esac; \
    if [ -n "$RUNTIME_FEATURES" ]; then features="$RUNTIME_FEATURES"; fi; \
    mkdir -p /runtime-bins; \
    if [ "$RUNTIME_VARIANT" = "rocm" ]; then \
    cargo build --locked --release -p aileron-runtime --lib --no-default-features --features "$features"; \
    fi; \
    for bin in aileron-runtime-llm-llama-cpp aileron-runtime-vision-llama-cpp aileron-runtime-asr-whisper-cpp; do \
    if [ "$RUNTIME_VARIANT" = "rocm" ]; then \
    cargo rustc --locked --release -p aileron-runtime --bin "$bin" --no-default-features --features "$features" -- -C link-arg=-no-pie; \
    else \
    cargo build --locked --release -p aileron-runtime --bin "$bin" --no-default-features --features "$features"; \
    fi; \
    cp "target/release/$bin" "/runtime-bins/$bin"; \
    done

FROM ${FINAL_IMAGE}

ARG RUNTIME_ID="llm-vision-whisper"
ARG RUNTIME_VARIANT="cpu"
ARG RUNTIME_DESCRIPTION="Aileron combined LLM, vision, and Whisper runtime."
ARG RUNTIME_APT_PACKAGES="libgomp1 libstdc++6 libgcc-s1 ca-certificates"
ARG ROCM_PATH="/opt/rocm"

ENV XDG_CACHE_HOME=/tmp
ENV ROCM_PATH="${ROCM_PATH}"
ENV HIP_PATH="${ROCM_PATH}"
ENV LD_LIBRARY_PATH="${ROCM_PATH}/lib"
ENV PATH="${ROCM_PATH}/bin:${PATH}"

RUN apt-get update && apt-get install -y --no-install-recommends ${RUNTIME_APT_PACKAGES} \
    && if [ "$RUNTIME_VARIANT" = "vulkan" ]; then \
    rm -f /usr/share/vulkan/icd.d/lvp_icd*.json /usr/share/vulkan/icd.d/lavapipe*.json; \
    fi \
    && mkdir -p /usr/local/nvidia/lib64 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /runtime-bins/* /usr/local/bin/
COPY runtimes/llm-vision-whisper/entrypoint.sh /entrypoint
RUN chmod 0755 /entrypoint /usr/local/bin/aileron-runtime-*

LABEL org.aileron.runtime="true" \
    org.aileron.runtime_id="${RUNTIME_ID}" \
    org.aileron.variant="${RUNTIME_VARIANT}" \
    org.opencontainers.image.description="${RUNTIME_DESCRIPTION}" \
    org.opencontainers.image.licenses="GPL-3.0-or-later"

ENTRYPOINT ["/entrypoint"]
