FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release -p aileron-runtime --bin aileron-runtime-stub \
    && cp target/release/aileron-runtime-stub /entrypoint

FROM debian:bookworm-slim
COPY --from=builder /entrypoint /entrypoint
RUN chmod 0755 /entrypoint
LABEL org.aileron.runtime="true" \
      org.aileron.runtime_id="stub" \
      org.aileron.variant="cpu" \
      org.opencontainers.image.description="Aileron stub runtime for development and protocol testing." \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
ENTRYPOINT ["/entrypoint"]
