# syntax=docker/dockerfile:1
FROM rust:1.88.0-slim-trixie AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY ingress ./ingress
COPY worker ./worker

RUN cargo build --release -p worker

FROM debian:bookworm-slim AS runner

WORKDIR /app
COPY --from=builder /app/target/release/worker ./worker

RUN useradd --system --no-create-home appuser
USER appuser

ENTRYPOINT ["./worker"]
