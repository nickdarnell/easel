# syntax=docker/dockerfile:1
# Easel: guests open http://<host>:3273/tv/<name> and send photos to that TV.
# Everything (TVs, password, settings, received photos) lives in the /config volume.

FROM rust:1-slim-trixie AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev libheif-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin frametv \
 && cp target/release/frametv /usr/local/bin/frametv

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3t64 libheif1 libheif-plugin-libde265 \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir /config
COPY --from=build /usr/local/bin/frametv /usr/local/bin/frametv
COPY --chmod=755 docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
ENV FRAMETV_DATA_DIR=/config \
    RUST_LOG=info
VOLUME /config
EXPOSE 3273/tcp 53317/tcp 53317/udp
ENTRYPOINT ["docker-entrypoint.sh"]
