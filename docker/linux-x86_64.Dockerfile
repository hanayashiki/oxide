# syntax=docker/dockerfile:1
#
# Reproducible Linux x86_64 build of the Oxide compiler.
#
# Usage:
#   docker build --output docs/src/dist \
#                --file docker/linux-x86_64.Dockerfile \
#                --target export .
#
# The `export` stage is a `FROM scratch` whose only contents are the
# built `oxide` binary; `docker build --output` extracts that file
# directly to the host without leaving a named image around.
#
# LLVM 18 is pinned to match `inkwell = { features = ["llvm18-0"] }`
# in Cargo.toml. If inkwell's LLVM version changes, update the apt
# package versions and the LLVM_SYS_180_PREFIX env var here.

FROM rust:1-bookworm AS builder

# Debian Bookworm caps at LLVM 14 in its default apt; LLVM 18 lives in
# the upstream LLVM apt repo (https://apt.llvm.org). Add the repo, then
# install the LLVM 18 dev packages inkwell links against.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg \
    && curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key \
        | gpg --dearmor -o /usr/share/keyrings/llvm-snapshot.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/llvm-snapshot.gpg] https://apt.llvm.org/bookworm/ llvm-toolchain-bookworm-18 main" \
        > /etc/apt/sources.list.d/llvm.list \
    && apt-get update && apt-get install -y --no-install-recommends \
        llvm-18-dev \
        libpolly-18-dev \
        zlib1g-dev \
        cmake \
    && rm -rf /var/lib/apt/lists/*

ENV LLVM_SYS_180_PREFIX=/usr/lib/llvm-18

WORKDIR /src
COPY . .

# `--platform=linux/amd64` above already pins the container's host triple
# to x86_64-unknown-linux-gnu, so a plain `cargo build --release` produces
# the desired binary natively (no cross-compile toolchain required).
RUN cargo build --release

FROM scratch AS export
COPY --from=builder /src/target/release/oxide /oxide
