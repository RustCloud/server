# RustCloud image: a static binary with the web UI inside, on a small Alpine base.
#
#   docker build -t rustcloud .
#   docker run -p 8080:8080 -v rustcloud-data:/data rustcloud
#
# Data (SQLite database, files and the config file rustcloud.toml) lives in /data.
# Configure on the admin page or with RUSTCLOUD_* variables, e.g.
# RUSTCLOUD_DATABASE_URL for PostgreSQL (see compose.yml).

# 1. Web UI
FROM node:26-alpine@sha256:c6ad9e9c1d80f6eb607c26b4b0a132ff7a2344fad1c78ea3baa2a7d4c8de6d94 AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

# 2. Server (static musl binary; the UI from stage 1 is embedded at compile time)
FROM clux/muslrust:stable@sha256:12e02bbe193e3bd9255f9b5a9312f0039a0c79b9e015cf5e81eeab83a1eb8e0f AS server
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY --from=web /web/dist web/dist
# The commit, shown on the admin page: docker build --build-arg REVISION=$(git rev-parse HEAD)
ARG REVISION=""
ENV RUSTCLOUD_REVISION=$REVISION
RUN cargo build --release --locked -p rustcloud-server \
 && cp target/x86_64-unknown-linux-musl/release/rustcloud /rustcloud

# 3. Apps shipped with the server (WebAssembly components, see apps/)
FROM clux/muslrust:stable@sha256:12e02bbe193e3bd9255f9b5a9312f0039a0c79b9e015cf5e81eeab83a1eb8e0f AS apps
RUN rustup target add wasm32-wasip2
WORKDIR /src
COPY crates/plugin-api/wit crates/plugin-api/wit
COPY apps apps
RUN for dir in apps/*/; do \
      id=$(basename "$dir"); \
      cargo build --release --locked --target wasm32-wasip2 --manifest-path "$dir/Cargo.toml" \
      && mkdir -p "/apps/$id" && cp "$dir/manifest.toml" "/apps/$id/" \
      && cp "$dir/target/wasm32-wasip2/release/$id.wasm" "/apps/$id/app.wasm" || exit 1; \
    done

# 4. Tools of the media engine: our own static ffmpeg and ffprobe, only with what
#    the engine uses (see docker/build-ffmpeg.sh). Cached unless the script changes.
FROM alpine:3.22@sha256:2e0b81d60597f449baa56dc25d355367869d2dca4cd57e866f1e2b52af3eb55f AS media
COPY docker/build-ffmpeg.sh /build-ffmpeg.sh
RUN PREFIX=/opt/ffmpeg sh /build-ffmpeg.sh

# 5. Runtime
FROM alpine:3.22@sha256:2e0b81d60597f449baa56dc25d355367869d2dca4cd57e866f1e2b52af3eb55f
LABEL org.opencontainers.image.licenses="AGPL-3.0-or-later"
RUN adduser -D -H -u 10001 rustcloud && mkdir /data && chown rustcloud /data
COPY --from=server /rustcloud /usr/local/bin/rustcloud
COPY --from=apps /apps /usr/share/rustcloud/apps
COPY --from=media /opt/ffmpeg/bin/ffmpeg /opt/ffmpeg/bin/ffprobe /usr/libexec/rustcloud/
COPY --from=media /opt/ffmpeg/share/licenses /usr/share/doc/rustcloud/third-party
COPY LICENSE NOTICE /usr/share/doc/rustcloud/
USER rustcloud
WORKDIR /data
ENV RUSTCLOUD_CONFIG=/data/rustcloud.toml \
    RUSTCLOUD_LISTEN=0.0.0.0:8080 \
    RUSTCLOUD_DATA_DIR=/data \
    RUSTCLOUD_DATABASE_URL=sqlite:///data/rustcloud.db?mode=rwc \
    RUSTCLOUD_BUILTIN_APPS_DIR=/usr/share/rustcloud/apps \
    RUSTCLOUD_MEDIA_TOOLS_DIR=/usr/libexec/rustcloud
EXPOSE 8080
VOLUME /data
HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://127.0.0.1:8080/health >/dev/null || exit 1
ENTRYPOINT ["rustcloud"]
