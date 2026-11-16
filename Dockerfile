# syntax=docker/dockerfile:1
# Easel: guests open http://<host>:3273/tv/<name> and send photos to that TV.
# Everything (TVs, password, settings, received photos) lives in the /config volume.

# The web UI (Vue + TypeScript), built to static files that get embedded into the binary.
FROM --platform=$BUILDPLATFORM node:26-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1-slim-trixie AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev libheif-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src

# Dependencies first, in their own layer: it only rebuilds when Cargo.toml/Cargo.lock change,
# and CI keeps it in the GitHub Actions cache. (No cache mounts: those aren't kept between CI
# runners, and the layer must hold everything the second build needs.)
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src/bin/frametv \
 && echo 'fn main() {}' > src/bin/frametv/main.rs \
 && touch src/lib.rs \
 && cargo build --release --locked --bin frametv \
 && rm -rf src

# Then the app itself, with the built web UI (build.rs embeds web/dist). Touch the sources so
# cargo rebuilds them over the placeholder build (COPY keeps the files' original timestamps).
COPY build.rs ./
COPY src ./src
COPY --from=web /web/dist ./web/dist
RUN find build.rs src web -type f -exec touch {} + \
 && cargo build --release --locked --bin frametv \
 && cp target/release/frametv /usr/local/bin/frametv

FROM debian:trixie-slim
# CI adds the version, revision and build date on top of these (see .github/workflows/docker.yml).
LABEL org.opencontainers.image.title="Easel"       org.opencontainers.image.description="Photo manager for Samsung The Frame TVs"       org.opencontainers.image.source="https://github.com/nickdarnell/easel"       org.opencontainers.image.licenses="MIT"       net.unraid.docker.webui="http://[IP]:[PORT:3273]/"       net.unraid.docker.icon="https://raw.githubusercontent.com/nickdarnell/easel/main/assets/icon.png"
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
