# syntax=docker/dockerfile:1
FROM rust:1.88.0-slim-trixie@sha256:9a7159329166b45f453351a077367f501aa3e98378f7e327530e7966a139d05f AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY ingress ./ingress
COPY worker ./worker

RUN cargo build --release -p ingress

FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241 AS runner

WORKDIR /app
COPY --from=builder /app/target/release/ingress ./ingress

RUN useradd --system --no-create-home appuser
USER appuser

EXPOSE 3001
ENTRYPOINT ["./ingress"]
