# syntax=docker/dockerfile:1
# Run under QEMU arm64 using RaspiOS; install the native armhf compiler via multiarch
# and a native armhf Rust toolchain so cargo builds directly for arm32 with no
# cross-compilation indirection.
FROM --platform=linux/arm/v6 vascoguita/raspios:armhf AS builder

RUN apt-get update && apt-get install -y \
        curl \
        gcc \
        htop \
        libc6-dev \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable
ENV PATH="/root/.cargo/bin:$PATH"

RUN cargo install cargo-deb

WORKDIR /build
COPY . .

#RUN cargo deb --variant=rpi --compress-type gz

#FROM scratch
#COPY --from=builder /build/target/debian/*.deb /
