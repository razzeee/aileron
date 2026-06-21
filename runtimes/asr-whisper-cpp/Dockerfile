ARG BUILDER_IMAGE="rust:1-bookworm"
ARG FINAL_IMAGE="debian:bookworm-slim"
FROM ${BUILDER_IMAGE} AS builder

# CPU-only whisper.cpp build.
# For GPU acceleration use Dockerfile.cuda.

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates build-essential cmake clang libclang-dev pkg-config git ninja-build \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release -p aileron-runtime --bin aileron-runtime-asr-whisper-cpp \
        --no-default-features --features whisper \
    && cp target/release/aileron-runtime-asr-whisper-cpp /entrypoint

FROM ${FINAL_IMAGE}

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libgomp1 libstdc++6 libgcc-s1 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /entrypoint /entrypoint
RUN chmod 0755 /entrypoint

LABEL org.aileron.runtime="true" \
      org.aileron.runtime_id="asr-whisper-cpp" \
      org.aileron.variant="cpu" \
      org.opencontainers.image.description="Aileron Whisper.cpp runtime for local speech transcription." \
      org.opencontainers.image.licenses="GPL-3.0-or-later"

ENTRYPOINT ["/entrypoint"]
