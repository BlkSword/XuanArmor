# syntax=docker/dockerfile:1

# -----------------------------------------------------------------------------
# Builder stage: compile eBPF + userspace static binary
# -----------------------------------------------------------------------------
FROM debian:bookworm-slim AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       ca-certificates curl xz-utils zstd \
       build-essential clang llvm libelf-dev \
    && rm -rf /var/lib/apt/lists/*

# Install Rust (stable + nightly + rust-src)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --no-modify-path
ENV PATH="/root/.cargo/bin:${PATH}"
RUN rustup toolchain install nightly-2026-07-31 --component rust-src \
    && rustup target add x86_64-unknown-linux-musl

# Install prebuilt bpf-linker（固定 v0.10.4：官方资产是 .tar.zst，新版 LLVM 产物可能被 verifier 拒绝）
RUN curl -fsSL -o /tmp/bpf-linker.tar.zst \
        https://github.com/aya-rs/bpf-linker/releases/download/v0.10.4/bpf-linker-x86_64-unknown-linux-musl.tar.zst \
    && zstd -d -f /tmp/bpf-linker.tar.zst -o /tmp/bpf-linker.tar \
    && tar -xf /tmp/bpf-linker.tar -C /root/.cargo/bin \
    && chmod +x /root/.cargo/bin/bpf-linker \
    && rm -f /tmp/bpf-linker.tar /tmp/bpf-linker.tar.zst

WORKDIR /build
COPY . .

RUN cargo +nightly-2026-07-31 build --package xuanarmor-ebpf --target bpfel-unknown-none -Z build-std=core --release \
    && cargo build --package xuanarmor --target x86_64-unknown-linux-musl --release

# -----------------------------------------------------------------------------
# Runtime stage: minimal distroless image
# -----------------------------------------------------------------------------
FROM gcr.io/distroless/static-debian12:latest

COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/xuanarmor /usr/local/bin/xuanarmor
COPY --from=builder /build/packaging/config.example.toml /etc/xuanarmor/config.toml

EXPOSE 8720

ENTRYPOINT ["/usr/local/bin/xuanarmor"]
CMD ["start", "--config", "/etc/xuanarmor/config.toml"]
