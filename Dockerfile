FROM rust:1.86-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --shell /usr/sbin/nologin appuser
RUN mkdir -p /config && chown appuser:appuser /config

COPY --from=builder /app/target/release/dmarc2mqtt /usr/local/bin/dmarc2mqtt

USER appuser
WORKDIR /app
VOLUME ["/config"]
ENTRYPOINT ["/usr/local/bin/dmarc2mqtt", "--config", "/config/config.yaml"]
