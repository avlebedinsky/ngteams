# syntax=docker/dockerfile:1
# Builds ngteams-server only. The context is the repository root: docker build -f packaging/server/Dockerfile .
FROM docker.io/library/rust:1.100-trixie AS build
ARG CARGO_BUILD_JOBS=default
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# The image's rustc is only the bootstrap: the build uses the workspace's pinned toolchain.
COPY rust-toolchain.toml ./
RUN rustup toolchain install
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p ngteams-server \
 && install -D target/release/ngteams-server /out/ngteams-server

FROM docker.io/library/debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --user-group --home-dir /var/lib/ngteams-server --create-home ngteams
COPY --from=build /out/ngteams-server /usr/local/bin/ngteams-server
USER ngteams
WORKDIR /var/lib/ngteams-server
ENV NGTEAMS_SERVER_LISTEN=0.0.0.0:8080
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=3s --start-period=30s --retries=3 \
  CMD curl -fsS -o /dev/null "http://127.0.0.1:${NGTEAMS_SERVER_LISTEN##*:}/health/ready" || exit 1
ENTRYPOINT ["ngteams-server"]
CMD ["serve"]
