# ---- Backend API (Rust/axum) ----
FROM rust:1-bookworm AS builder
WORKDIR /usr/src/app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked
# kubectl untuk verifikasi misi race (server-side, ke cluster peserta)
ARG KUBECTL_VERSION=v1.31.4
RUN curl -fsSLo /usr/local/bin/kubectl https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/amd64/kubectl \
 && chmod +x /usr/local/bin/kubectl

FROM debian:bookworm-slim
RUN useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin app
WORKDIR /app
COPY --from=builder /usr/src/app/target/release/civo-workshop-backend /app/civo-workshop-backend
COPY --from=builder /usr/local/bin/kubectl /usr/local/bin/kubectl
USER 10001
ENV PORT=8080 STATIC_DIR=/app/static
EXPOSE 8080
CMD ["/app/civo-workshop-backend"]
